# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

- `cargo build` / `cargo build --release` — build the workspace.
- `cargo build -p olhad` / `-p olha` / `-p olha-popup` — build one crate.
- `cargo test` — all tests. `cargo test -p olhad` — one crate. `cargo test -p olhad <name>` — single test (e.g. `cargo test -p olhad roundtrip_summary`).
- `cargo check` / `cargo clippy` — fast feedback.
- `RUST_LOG=debug olhad` or `olhad -vvv` — run daemon with verbose logging. Verbosity counter in `olhad/src/main.rs:73`: `-v` WARN, `-vv` INFO, `-vvv` DEBUG.
- `cargo install --path olhad` / `--path olha` / `--path olha-popup` — install binaries to `~/.cargo/bin/`.
- `olha unlock` / `olha lock` — load / zeroize the X25519 secret that reads need. Daemon starts locked; writes seal under the always-loaded public key regardless.
- Another notification daemon already holding `org.freedesktop.Notifications` will cause `olhad` startup to fail with an owner-process diagnostic. Isolate tests with `dbus-run-session -- bash`.

## Workspace layout

Rust workspace (`Cargo.toml` at root) with three crates that share `zbus` (D-Bus) and `tokio`. Shared dependency versions live under `[workspace.dependencies]`.

### `olhad/` — the daemon

Registers **two** D-Bus interfaces on one session connection:

- `org.freedesktop.Notifications` at `/org/freedesktop/Notifications` (`olhad/src/dbus/freedesktop.rs`): the FDO spec methods (`Notify`, `CloseNotification`, `GetCapabilities`, `GetServerInformation`) and the `ActionInvoked` / `NotificationClosed` signals.
- `org.olha.Daemon` at `/org/olha/Daemon` (`olhad/src/dbus/olha.rs`): custom control interface (`list`, `count`, `mark_read`, `clear`, `delete`, `*_all`, `get_notification`, `invoke_action`, `dismiss`, `get_dnd`, `set_dnd`, `unlock`, `lock`, `is_unlocked`, `status`) + signals `notification_received`, `dnd_changed`, and `locked_changed` (fires on every manual or auto-lock transition).

Both interfaces share `Arc<DaemonState>` (declared in `olhad/src/main.rs`) holding `Config`, `db_path`, `RulesEngine`, an `Arc<EncryptionState>`, and a runtime `dnd_enabled: AtomicBool`. `DaemonState::open_db()` opens a fresh `rusqlite::Connection` per call — cheap with the `bundled` feature. A small `meta(key, value)` KV table in SQLite persists DND, the public key, and the DEK-wrapped X25519 secret across restarts; `db::queries::{get_meta, set_meta}` are the helpers. Meta keys owned by the encryption layer: `enc_public_key`, `enc_wrapped_secret`, `enc_key_id`, `enc_dek_kid` (names also exported as constants in `olhad/src/main.rs`).

Key modules:

- `rules.rs` — compiles `[[rules]]` to `CompiledRule`s once at startup. First-match-wins evaluation returns a `RuleAction::{Clear,Ignore,None}` storage verdict plus an `on_action: HashMap<action_key, shell_cmd>` for click dispatch.
- `launcher.rs` — spawns `sh -c` for rule `on_action` commands (with `OLHA_*` env vars set from the notification) and `gtk-launch` for the `desktop-entry` hint fallback. `init_session_env()` snapshots `WAYLAND_DISPLAY`/`GDK_SCALE`/etc. at startup so spawned handlers inherit GUI-correct env regardless of how `olhad` was launched.
- `db/schema.rs` and `db/queries.rs` — SQLite schema + filter/insert/update/delete. Queries take `&EncMode` (Plaintext / Locked / Unlocked) rather than an optional `EncryptionContext`; writes succeed under `Locked`, reads of encrypted rows return placeholders until `EncMode::Unlocked`.
- `db/encryption.rs` — sealed-box crypto + `EncryptionState` (see below).
- `cleanup_loop` in `main.rs` — background tokio task enforcing `max_age` / `max_count` / `cleanup_interval`.
- `auto_lock_loop` in `main.rs` — background task that zeroizes the in-memory X25519 secret after `[encryption].auto_lock_secs` of idle.

### `olha/` — the CLI client

- `main.rs` is the `clap` command tree.
- `client.rs` declares `ControlDaemonProxy` (a `zbus::proxy` trait over `org.olha.Daemon`) and calls through it; `output.rs` does pretty-printing (comfy-table + owo-colors).
- `encryption.rs` is a **separate** side of the CLI that opens the SQLite DB **directly** (not through the daemon) for `olha encryption init/enable/disable/status/rewrap/rotate-key`. The daemon-stopped subcommands (`rotate-key`, `disable --rekey-to-plaintext`) probe `org.olha.Daemon.is_unlocked` first and refuse to run while olhad is up. `unlock` / `lock` at the top level of the CLI talk to the running daemon over D-Bus (see `client.rs`).

### `olha-popup/` — the Wayland popup

A separate binary built on `iced` + `iced_layershell` (`wlr-layer-shell`). Subscribes to the daemon's `notification_received` signal (`dbus.rs::connect`), renders stacked popups at a screen corner, and calls `InvokeAction` / `Dismiss` back over D-Bus on click. Reads the same `config.toml` but cares about sections the daemon ignores:

- `[popup]` — position, sizes, gap, `max_visible`, `hide_content_when_locked` (global opt-in to iPhone-style "show previews" gating).
- `[[popup.rules]]` — `suppress` / `override_urgency` / `override_timeout_secs` / `hide_content_when_locked` (per-rule override, `Some(true|false)` trumps the global). Matcher in `olha-popup/src/rules.rs`.
- The popup *also* reads `[[rules]]` to find `on_action` commands when rendering action buttons, but invocation still round-trips through the daemon's `InvokeAction` (the daemon owns the launcher).
- Popup subscribes to `locked_changed` on startup (plus one-shot `is_unlocked` probe) so the privacy toggle updates reactively. The daemon always emits plaintext on `notification_received` — the popup chooses whether to render it.

## Two-rule system (biggest footgun)

The README's "Notification Rules" section is the full reference. Short version:

- `[[rules]]` runs in **olhad on arrival**. Top-level `action` is the *storage verdict*: `clear` | `ignore` | `none`. `[rules.on_action]` is a *separate* nested table mapping *application-defined action keys* (e.g. `default`, `reply`) to shell commands. `"none"` is a valid storage verdict but is **never** a valid `on_action` key — the engine logs a warning at startup when one appears there.
- `[[popup.rules]]` runs in **olha-popup at display time**. Cannot affect storage. Fields: `suppress`, `override_urgency`, `override_timeout_secs`.

First-match-wins in both lists. Patterns are Rust `regex`, unanchored, case-sensitive (`(?i)` to flip). Prefer TOML literal strings (`'…'`) to avoid double-escaping.

## Encryption (`olhad/src/db/encryption.rs`)

At-rest encryption of the `summary`, `body`, and `hints` columns only. **Sealed-box** (X25519 + XChaCha20-Poly1305): writes only need the public key, reads need the secret key, and the daemon only holds the secret between `Unlock` and `Lock` / idle auto-lock.

On-disk field layout (`enc_version = 1`):

```
version_byte(1) = 0x01  ||  field_tag(1)  ||  ephemeral_pk(32)  ||  nonce(24)  ||  ciphertext || tag(16)
```

`field_tag` is `0x01`/`0x02`/`0x03` for summary/body/hints. The symmetric key is `SHA-256(b"olha/kdf" || shared || epk || pk)`; AAD is `b"olha/{summary|body|hints}"`. The outer field-tag byte is a redundant pre-crypto check against column mixups.

The X25519 secret key is stored in `meta.enc_wrapped_secret`, encrypted under a DEK derived from `SHA-256(pass_output)`. Wrapped-sk layout: `version(0x01) || nonce(24) || ct(32) || tag(16)`, AAD `b"olha/wrapped-sk"`. The DEK lives in memory only during the Unlock code path; the long-lived secret-in-memory between unlock/lock is the 32-byte X25519 sk inside `Zeroizing`.

Startup flow (`olhad/src/main.rs`):

  1. `[encryption].enabled = false` → `EncryptionState::plaintext()`.
  2. enabled + `meta.enc_public_key` present → `EncryptionState::with_public_key(...)`; daemon starts locked, writes seal, reads return `[encrypted]` placeholders until `olha unlock`.
  3. enabled + no pk in meta → fail closed. Tell the user to run `olha encryption init`.

`EncMode<'a>` (threaded into every `queries::*` call):

- `Plaintext` — encryption off.
- `Locked { pk, key_id }` — encrypted writes OK, encrypted reads → placeholders.
- `Unlocked { pk, key_id, sk, activity }` — full read + write; successful decrypts bump the `activity` atomic (shared with `EncryptionState.last_activity`) so the idle auto-lock timer resets.

`auto_lock_loop` in `main.rs` polls every 30s; when `EncryptionState::should_auto_lock()` fires, it zeroizes the sk and emits `locked_changed(false)`. Disable by setting `[encryption].auto_lock_secs = 0`.

## Notification lifecycle

`Unread → Read → Cleared → Deleted`. `mark_read`, `clear`, `delete` are distinct D-Bus methods. `InvokeAction` emits FDO `ActionInvoked` + `NotificationClosed(reason=2)` then flips the row to `Read`. `Dismiss` emits `NotificationClosed(reason=2)` and flips to `Cleared` without running any action.

## Do Not Disturb

`org.olha.Daemon.SetDnd(bool)` flips an `AtomicBool` in `DaemonState` and persists the value to `meta` (key `dnd_enabled`). When DND is on, `emit_notification_signal` in `olhad/src/dbus/freedesktop.rs` swallows the `notification_received` signal so subscribers (`olha-popup`, `olha subscribe`) stay quiet — but storage runs as usual. Critical-urgency notifications bypass DND when `[dnd].allow_critical = true` (the default). `dnd_changed(bool)` is emitted on every toggle for reactive clients. State is reloaded from `meta` at daemon startup in `olhad/src/main.rs`.

## Config and paths

- Config file: `$XDG_CONFIG_HOME/olha/config.toml` (default `~/.config/olha/config.toml`). On first run, `Config::load` writes `olhad/src/config.template.toml` to disk so users discover knobs by reading the file. The `default_config_template_parses_to_defaults` test fails if the template drifts from `Config::default()` — update both together.
- DB: `$XDG_DATA_HOME/olha/notifications.db` (default `~/.local/share/olha/notifications.db`). Overridable via `[general].db_path`.
