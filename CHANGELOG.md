# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **X25519 sealed-box at-rest encryption.** The daemon no longer
  requires an unlocked secret at startup: it loads the long-lived
  public key from the DB's `meta` table and seals every incoming
  notification. Reads of encrypted rows require running `olha unlock`
  (which derives the DEK via `pass show`, unwraps the X25519 secret
  key, and holds it in memory). `olha lock` zeroes the secret
  immediately; the daemon also auto-locks after `[encryption].auto_lock_secs`
  of idle (default 300). `org.olha.Daemon` gains `Unlock`, `Lock`,
  `IsUnlocked`, and a `LockedChanged(bool)` signal.
- New `olha encryption` subcommands: `disable --rekey-to-plaintext`
  (intentional plaintext downgrade, requires daemon stopped),
  `rewrap` (re-wrap the X25519 secret under a new DEK — fast, only
  touches `meta`), and `rotate-key` (new X25519 keypair, re-seal
  every row).
- Top-level `olha unlock` / `olha lock` subcommands.
- `[popup].hide_content_when_locked` (global, default `false`) and
  per-rule `hide_content_when_locked: Option<bool>` — iPhone-style
  "show previews" gating for popups while the daemon is locked.
- `[encryption].auto_lock_secs` config knob (default 300; 0
  disables).
- Do Not Disturb mode. `olha dnd [status|on|off|toggle]` flips a
  runtime flag that the daemon persists to a new `meta` KV table in
  SQLite, so it survives restarts. While DND is on, notifications are
  still stored in history but the `notification_received` signal is
  suppressed — `olha-popup` and `olha subscribe` stay quiet. Critical
  urgency breaks through by default; set `[dnd].allow_critical = false`
  in `config.toml` to silence everything. `org.olha.Daemon` gains
  `GetDnd`, `SetDnd`, and a `DndChanged` signal; `olha status` shows
  the current DND state.
- New workspace crate `olha-popup`: a native Wayland (wlr-layer-shell) notifier
  built on iced + iced_layershell. Replaces the Alacritty-per-notification
  shell recipe with stacked, actionable popups — clicking an action button
  invokes it over D-Bus and the popup dismisses. Per-urgency accent colors,
  configurable screen corner, `max_visible` eviction, and per-popup timeouts
  honoring the existing `[notifications]` settings.
- New `[popup]` section in `config.toml` (`position`, `max_visible`, `margin`,
  `gap`, `width`, `height`) read by `olha-popup`; ignored by `olhad`.
- Pretty table rendering for `olha list` using `comfy-table`, with bold headers
  and colorized `status` / `urgency` cells. Honors `NO_COLOR` and disables
  color on non-TTY output.
- Bold labels and per-line bulleted actions in `olha show <id>` output.
- `olha completions <SHELL>` subcommand (bash, zsh, fish, powershell, elvish).
  The zsh script also binds the `ola` alias to the same completion function,
  so `alias ola=olha` gives identical tab-completion for both names.
- `ActionInvoked` signal on the `org.freedesktop.Notifications` interface, so
  the originating app is actually notified when `olha invoke` runs.
- "Shell Completions" section in the README with install instructions.

### Changed
- **Encryption model reworked.** Writes now always work when
  encryption is enabled (they seal under the public key regardless of
  whether anyone has unlocked). Reads of encrypted rows require an
  unlock. The daemon no longer holds a DEK in memory across its
  lifetime — only the 32-byte X25519 secret lives between unlock and
  lock/auto-lock. The `--allow-degraded-read` flag is removed; the
  equivalent behavior ("daemon is running but can't read encrypted
  rows") is now the default locked state.
- `olha encryption init` now generates an X25519 keypair in addition
  to seeding the `pass` entry; stores pk + wrapped sk in `meta`.
- `olha encryption rotate` is split into `rewrap` (rotate DEK only;
  cheap) and `rotate-key` (rotate X25519 keypair; re-seals every row).
- `olha invoke <id> <key>` now looks up the notification, emits the FDO
  `ActionInvoked(dbus_id, key)` signal, and marks the notification `read`.
  Previously the daemon method was a no-op stub that only logged.
- `olha list` summaries now content-wrap instead of being hard-truncated, so
  long titles stay readable.
- CLI errors print via `eprintln!` and exit with status 1 instead of using
  Rust's default `Result` debug-quoted output.

### Fixed
- `olha mark-read` / `clear` / `delete` now error when given no IDs and no
  `--all`, instead of silently succeeding.
- Friendly "olhad is not running" message when the daemon isn't reachable,
  instead of a raw zbus error.
- `truncate()` helper in `olha/src/output.rs` now counts characters rather
  than bytes, fixing alignment for multibyte strings.
- `olha invoke` on a missing notification id or unknown action key now returns
  a clear error instead of a fake success.
- `notification_received` D-Bus signal now carries `row_id` in its JSON
  payload. Previously `store_notification`'s returned id was discarded and the
  field was always omitted, which made it impossible for signal subscribers
  (such as `olha-popup`) to invoke actions on the newly received notification.

[Unreleased]: https://github.com/yourusername/olha/compare/HEAD
