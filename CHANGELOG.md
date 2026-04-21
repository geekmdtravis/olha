# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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

[Unreleased]: https://github.com/yourusername/olha/compare/HEAD
