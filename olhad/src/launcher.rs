//! Spawning helpers for local click-action handling.
//!
//! `olha` is a notification daemon, but the FDO spec stops at emitting
//! `ActionInvoked` — actually *doing* something on click is a daemon-side
//! extension. This module isolates the two mechanisms that translate a click
//! into a running process:
//!
//! * [`spawn_shell_command`] — run an arbitrary user-configured shell command
//!   (from a `rules.on_action` entry).
//! * [`activate_desktop_entry`] — resolve a `desktop-entry` hint to a running
//!   app by shelling out to `gtk-launch` (which internally prefers D-Bus
//!   `org.freedesktop.Application.Activate` when available and falls back to
//!   the `.desktop` file's `Exec` line otherwise).
//!
//! Both detach from the daemon — a tiny reaper thread waits on the child so
//! zombies are cleaned up, and we don't block the caller.
//!
//! Children inherit the current *graphical session* environment — not just
//! whatever env `olhad` happened to start with. GUI env vars like
//! `WAYLAND_DISPLAY`, `GDK_SCALE`, or `XDG_CURRENT_DESKTOP` often live only
//! in the user's systemd session manager (populated by the compositor via
//! `systemctl --user import-environment` at session start). [`init_session_env`]
//! snapshots that env at daemon startup via `systemctl --user show-environment`;
//! every subsequent spawn layers the snapshot over `olhad`'s inherited env so
//! Electron apps (Signal, Discord) render at the correct scale.
//!
//! We use `std::process` rather than `tokio::process` because D-Bus interface
//! handlers run on zbus's own executor thread, which has no tokio reactor —
//! calling `tokio::process::Command` from there panics.

use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

static SESSION_ENV: OnceLock<Vec<(String, String)>> = OnceLock::new();

/// Snapshot the systemd user-session env into the module cache. Call once
/// at daemon startup; every spawn afterwards reads the cache, so the D-Bus
/// handler thread never shells out.
pub fn init_session_env() {
    let env = load_session_env();
    tracing::debug!("session env loaded: {} entries", env.len());
    let _ = SESSION_ENV.set(env);
}

fn load_session_env() -> Vec<(String, String)> {
    let output = match Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            tracing::debug!(
                "systemctl --user show-environment exited {}; session env empty",
                o.status,
            );
            return Vec::new();
        }
        Err(e) => {
            tracing::debug!("could not invoke systemctl: {}; session env empty", e);
            return Vec::new();
        }
    };

    // `systemctl show-environment` prints KEY=value lines. Values with
    // special chars are ANSI-C quoted ($'…'); the GUI vars we care about
    // (DISPLAY, WAYLAND_DISPLAY, XDG_*, GDK_*) don't need quoting in
    // practice, so a naive split_once is good enough. Unparseable lines
    // are dropped.
    String::from_utf8_lossy(&output)
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

fn session_env() -> &'static [(String, String)] {
    SESSION_ENV.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// Hand a spawned `Child` to a one-shot reaper thread so the kernel can
/// clean up the zombie when the process exits. We never care about the exit
/// status — actions are fire-and-forget.
fn reap(mut child: Child, label: &'static str) {
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    tracing::debug!("{} spawned (pid {})", label, pid);
}

/// Spawn `cmd` under `sh -c` with the given env vars attached. Env
/// precedence: `olhad`'s inherited env → session env (overrides) → caller's
/// `env` (`OLHA_*`, most specific, wins).
pub fn spawn_shell_command(cmd: &str, env: &[(&str, String)]) -> io::Result<()> {
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .envs(session_env().iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    reap(child, "shell command");
    Ok(())
}

/// Resolve `entry` (the `desktop-entry` hint value, e.g. "signal") to a
/// running app via `gtk-launch`. Best-effort: missing binary or missing
/// `.desktop` file yields a warn at the call site.
pub fn activate_desktop_entry(entry: &str) -> io::Result<()> {
    let child = Command::new("gtk-launch")
        .arg(entry)
        .envs(session_env().iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    reap(child, "gtk-launch");
    Ok(())
}
