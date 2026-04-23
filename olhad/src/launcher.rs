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
//! Both detach from the daemon — spawn, log the pid, and drop the handle.
//! Tokio's runtime reaps children via its SIGCHLD handler, so there are no
//! zombies.

use std::io;
use std::process::Stdio;
use tokio::process::Command;

/// Spawn `cmd` under `sh -c` with the given env vars attached. Detaches
/// immediately; the daemon does not wait for completion.
pub async fn spawn_shell_command(cmd: &str, env: &[(&str, String)]) -> io::Result<()> {
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    tracing::debug!("spawned shell command (pid {:?}): {}", child.id(), cmd);
    Ok(())
}

/// Resolve `entry` (the `desktop-entry` hint value, e.g. "signal") to a
/// running app. Uses `gtk-launch`; if the binary is missing or the `.desktop`
/// file can't be found, `gtk-launch` exits non-zero and the daemon just
/// logs a warn — activation is best-effort.
pub async fn activate_desktop_entry(entry: &str) -> io::Result<()> {
    let child = Command::new("gtk-launch")
        .arg(entry)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    tracing::debug!("gtk-launch {} (pid {:?})", entry, child.id());
    Ok(())
}
