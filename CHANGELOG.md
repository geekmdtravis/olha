# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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
