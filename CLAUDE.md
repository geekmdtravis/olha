# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

- `cargo build` / `cargo build --release` — build the workspace.
- `cargo build -p olhad` / `-p olha` / `-p olha-popup` — build one crate.
- `cargo test` — all tests. `cargo test -p olhad` — one crate. `cargo test -p olhad <name>` — single test (e.g. `cargo test -p olhad roundtrip_summary`).
- `cargo check` / `cargo clippy` — fast feedback.
- `RUST_LOG=debug olhad` or `olhad -vvv` — run daemon with verbose logging. Verbosity counter in `olhad/src/main.rs:73`: `-v` WARN, `-vv` INFO, `-vvv` DEBUG.
- `olhad --allow-degraded-read` — start the daemon when `pass` can't unlock the DEK. Encrypted rows come back as `[encrypted]` placeholders and any new write that would need encryption is rejected. Recovery mode only.
- `cargo install --path olhad` / `--path olha` / `--path olha-popup` — install binaries to `~/.cargo/bin/`.
- Another notification daemon already holding `org.freedesktop.Notifications` will cause `olhad` startup to fail with an owner-process diagnostic. Isolate tests with `dbus-run-session -- bash`.

## Workspace layout

Rust workspace (`Cargo.toml` at root) with three crates that share `zbus` (D-Bus) and `tokio`. Shared dependency versions live under `[workspace.dependencies]`.

### `olhad/` — the daemon

Registers **two** D-Bus interfaces on one session connection:

- `org.freedesktop.Notifications` at `/org/freedesktop/Notifications` (`olhad/src/dbus/freedesktop.rs`): the FDO spec methods (`Notify`, `CloseNotification`, `GetCapabilities`, `GetServerInformation`) and the `ActionInvoked` / `NotificationClosed` signals.
- `org.olha.Daemon` at `/org/olha/Daemon` (`olhad/src/dbus/olha.rs`): custom control interface (`list`, `count`, `mark_read`, `clear`, `delete`, `*_all`, `get_notification`, `invoke_action`, `dismiss`, `get_dnd`, `set_dnd`, `status`) + signals `notification_received` (on every stored notification, unless suppressed by DND) and `dnd_changed` (on every DND toggle).

Both interfaces share `Arc<DaemonState>` (declared in `olhad/src/main.rs`) holding `Config`, `db_path`, `RulesEngine`, an optional `EncryptionContext`, and a runtime `dnd_enabled: AtomicBool`. `DaemonState::open_db()` opens a fresh `rusqlite::Connection` per call — cheap with the `bundled` feature. A small `meta(key, value)` KV table in SQLite persists the DND toggle across restarts; `db::queries::{get_meta, set_meta}` are the helpers.

Key modules:

- `rules.rs` — compiles `[[rules]]` to `CompiledRule`s once at startup. First-match-wins evaluation returns a `RuleAction::{Clear,Ignore,None}` storage verdict plus an `on_action: HashMap<action_key, shell_cmd>` for click dispatch.
- `launcher.rs` — spawns `sh -c` for rule `on_action` commands (with `OLHA_*` env vars set from the notification) and `gtk-launch` for the `desktop-entry` hint fallback. `init_session_env()` snapshots `WAYLAND_DISPLAY`/`GDK_SCALE`/etc. at startup so spawned handlers inherit GUI-correct env regardless of how `olhad` was launched.
- `db/schema.rs` and `db/queries.rs` — SQLite schema + filter/insert/update/delete.
- `db/encryption.rs` — field-level AEAD (see below).
- `cleanup_loop` in `main.rs` — background tokio task enforcing `max_age` / `max_count` / `cleanup_interval`.

### `olha/` — the CLI client

- `main.rs` is the `clap` command tree.
- `client.rs` declares `ControlDaemonProxy` (a `zbus::proxy` trait over `org.olha.Daemon`) and calls through it; `output.rs` does pretty-printing (comfy-table + owo-colors).
- `encryption.rs` is a **separate** side of the CLI that opens the SQLite DB **directly** (not through the daemon) for `olha encryption init/enable/status/rotate`. `rotate` requires the daemon to be stopped — concurrent writes during rotation corrupt the DB.

### `olha-popup/` — the Wayland popup

A separate binary built on `iced` + `iced_layershell` (`wlr-layer-shell`). Subscribes to the daemon's `notification_received` signal (`dbus.rs::connect`), renders stacked popups at a screen corner, and calls `InvokeAction` / `Dismiss` back over D-Bus on click. Reads the same `config.toml` but cares about sections the daemon ignores:

- `[popup]` — position, sizes, gap, `max_visible`.
- `[[popup.rules]]` — `suppress` / `override_urgency` / `override_timeout_secs`. Matcher in `olha-popup/src/rules.rs`.
- The popup *also* reads `[[rules]]` to find `on_action` commands when rendering action buttons, but invocation still round-trips through the daemon's `InvokeAction` (the daemon owns the launcher).

## Two-rule system (biggest footgun)

The README's "Notification Rules" section is the full reference. Short version:

- `[[rules]]` runs in **olhad on arrival**. Top-level `action` is the *storage verdict*: `clear` | `ignore` | `none`. `[rules.on_action]` is a *separate* nested table mapping *application-defined action keys* (e.g. `default`, `reply`) to shell commands. `"none"` is a valid storage verdict but is **never** a valid `on_action` key — the engine logs a warning at startup when one appears there.
- `[[popup.rules]]` runs in **olha-popup at display time**. Cannot affect storage. Fields: `suppress`, `override_urgency`, `override_timeout_secs`.

First-match-wins in both lists. Patterns are Rust `regex`, unanchored, case-sensitive (`(?i)` to flip). Prefer TOML literal strings (`'…'`) to avoid double-escaping.

## Encryption (`olhad/src/db/encryption.rs`)

At-rest encryption of the `summary`, `body`, and `hints` columns only.

- XChaCha20-Poly1305 AEAD. Per-field random 24-byte nonce. AAD = `"olha/v1/<field>"` so a ciphertext cannot be pasted into a different column. Stored layout per field: `nonce(24) || ciphertext || tag(16)`.
- The DEK is derived by `SHA-256` over whatever bytes `pass show <pass_entry>` returns (default entry `olha/db-key`). DEK is held in `Zeroizing<[u8;32]>`, never logged. `key_id = SHA-256(dek)[..4]`.
- Startup flow (`olhad/src/main.rs` around line 105):
  1. `[encryption].enabled = false` → plaintext mode, no DEK.
  2. enabled + pass unlocks → encrypted mode.
  3. enabled + pass fails + `--allow-degraded-read` → degraded read, new encrypted writes refused.
  4. enabled + pass fails, no flag → **fail closed**, daemon exits.

## Notification lifecycle

`Unread → Read → Cleared → Deleted`. `mark_read`, `clear`, `delete` are distinct D-Bus methods. `InvokeAction` emits FDO `ActionInvoked` + `NotificationClosed(reason=2)` then flips the row to `Read`. `Dismiss` emits `NotificationClosed(reason=2)` and flips to `Cleared` without running any action.

## Do Not Disturb

`org.olha.Daemon.SetDnd(bool)` flips an `AtomicBool` in `DaemonState` and persists the value to `meta` (key `dnd_enabled`). When DND is on, `emit_notification_signal` in `olhad/src/dbus/freedesktop.rs` swallows the `notification_received` signal so subscribers (`olha-popup`, `olha subscribe`) stay quiet — but storage runs as usual. Critical-urgency notifications bypass DND when `[dnd].allow_critical = true` (the default). `dnd_changed(bool)` is emitted on every toggle for reactive clients. State is reloaded from `meta` at daemon startup in `olhad/src/main.rs`.

## Config and paths

- Config file: `$XDG_CONFIG_HOME/olha/config.toml` (default `~/.config/olha/config.toml`). On first run, `Config::load` writes `olhad/src/config.template.toml` to disk so users discover knobs by reading the file. The `default_config_template_parses_to_defaults` test fails if the template drifts from `Config::default()` — update both together.
- DB: `$XDG_DATA_HOME/olha/notifications.db` (default `~/.local/share/olha/notifications.db`). Overridable via `[general].db_path`.
