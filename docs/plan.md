# Recommendations: Features & Tests for `olha`

## Context

`olha` is a pre-1.0 (v0.1.0), minimalist Linux/Wayland notification daemon with
a SQLite-backed persistent history. The workspace has three crates — `olhad`
(daemon, D-Bus `org.freedesktop.Notifications` + `org.olha.Daemon`), `olha`
(CLI client), and `olha-popup` (iced + `iced_layershell` Wayland popup). The
project is well-architected but **under-tested** (~10 unit tests across the
workspace and zero integration tests), and the README's "Future Enhancements"
checklist shows several items the author already wants to tackle.

This document is a menu of recommendations — prioritized by impact and grouped
so the next piece of work can be picked off the top. Nothing here is
implemented yet; each item should be scoped into its own plan / PR before
code changes.

---

## Feature Recommendations

### Tier 1 — High Impact / Low Effort (do these first)

1. **systemd user service unit** *(already a README TODO at `README.md:690`, `:748`)*
   - Ship `olhad/systemd/olhad.service` (`Type=dbus`,
     `BusName=org.olha.Daemon`), plus a `ConditionEnvironment=WAYLAND_DISPLAY`
     variant for `olha-popup`.
   - Add `make install` / a `contrib/` dir and a README section replacing the
     `# TODO: provide systemd unit file` stub.
   - Also consider a D-Bus activation `.service` file at
     `/usr/share/dbus-1/services/org.olha.Daemon.service` so the daemon
     auto-starts when the first `notify-send` arrives.

2. **Exec-on-match rule action** *(README TODO: "Scripting/hook support")*
   - The rules engine in `olhad/src/rules.rs` already exposes `RuleAction`
     with only `Clear` / `Ignore`. Add `RuleAction::Exec(String)` driven by a
     config field like `action = "exec:notify-phone %s"` with `%s`, `%a`,
     `%b`, `%u` placeholders for summary/app/body/urgency.
   - Run via `tokio::process::Command`, cap concurrency, log failures; do not
     block the D-Bus handler.

3. **Notification grouping / deduplication**
   - When the same `app_name` + `summary` arrives within N seconds, update
     the existing row's `body` + `updated_at` and bump a new `repeat_count`
     column instead of inserting a duplicate. Surface as `(x3)` in `olha
     list`.
   - Schema migration needed in `olhad/src/db/schema.rs` — see Tier 2 #1.

4. **Full-text search (FTS5)**
   - SQLite FTS5 virtual table on `summary` + `body`; wire into the existing
     `--search` flag in `olhad/src/db/queries.rs`. Currently `--search` uses
     `LIKE`, which does not scale past ~10k rows.
   - Free win: `rusqlite` with `bundled` already supports FTS5.

5. **`olha watch` / tail-style TUI**
   - The `subscribe` subcommand already streams `notification_received`
     events. Add a compact `watch` mode that renders a live, scrolling table
     using the existing `comfy-table` output in `olha/src/output.rs`.

### Tier 2 — High Impact / Moderate Effort

1. **Schema versioning & migrations**
   - Add a `schema_version` table + migration runner in `olhad/src/db/`. This
     is needed before features 3 / 6 / 7 below land, and retrofitting later
     is painful. Use a simple `Vec<&str>` of SQL-per-version pattern.

2. **Archival / export commands**
   - `olha export --format json|csv [filters]` — reuse the existing query
     filters from `olhad/src/dbus/olha.rs::list`.
   - `olhad` side: opt-in move of rows older than `archive_after` into a
     `notifications_archive` table to keep the hot table small.

3. **Inline reply** *(README TODO)*
   - FDO spec defines an `"inline-reply"` hint + `NotificationReplied` signal.
     Wire it through the daemon and add a text input to `olha-popup`'s popup
     view. Needed by Slack, Telegram, KDE Connect.

4. **Sound / audio** *(README TODO)*
   - Config-driven sound per urgency (`[sounds] critical = "/path/to.wav"`)
     using `rodio` or shelling out to `paplay`. Respect the FDO
     `"suppress-sound"` hint.

5. **Action invocation + proper E2E harness**
   - See Testing section below — blocker for anything that changes D-Bus
     behavior.

6. **Per-app config overrides**
   - `[[app]] name = "Slack"` sections that override `default_timeout`,
     sound, popup position, etc. This is `mako`'s killer feature and it's
     missing here.

### Tier 3 — High Value / Higher Effort

1. **Web dashboard** *(README TODO)*
   - Add `olha-web` crate: `axum` + htmx (or Leptos) serving a localhost-only
     UI reading from the existing D-Bus interface. Keep it optional behind a
     Cargo feature so the core install stays minimal.

2. **Do-Not-Disturb mode**
   - `olha dnd on|off|toggle|status` flips a runtime flag in `olhad`; while
     on, all non-critical notifications are stored but the
     `notification_received` signal is suppressed so `olha-popup` stays
     silent. Time-windowed DND (`olha dnd on --for 1h`) is a natural
     extension.

3. **Rich body rendering** *(README TODO)*
   - Spec allows a small HTML subset (`<b>`, `<i>`, `<a>`, `<img>`). iced
     supports `rich_text` in 0.14 — render in the popup, strip to plain in
     the CLI table.

4. **Image / icon persistence**
   - FDO `image-data` / `image-path` hints currently get thrown into the
     `hints` JSON blob. Persist raw bytes to
     `~/.local/share/olha/images/<hash>.png`, store path in DB, display in
     popup and optionally in the CLI via kitty/iterm2 graphics protocol.

5. **Prometheus / stats endpoint**
   - Tiny `hyper` server exposing `olhad_notifications_total{app,urgency}`,
     `olhad_rule_matches_total{rule}`, DB row count. Natural for users
     already scraping node_exporter.

### Tier 4 — Polish

- Shell-completion for dynamic values (`--app <tab>` → list of known apps
  from the DB).
- `olha show <id> --open-action <key>` one-liner for keybind integration.
- Configurable log file + log level via config (currently env-only).
- `olhad --check-config` dry-run that validates rules and exits non-zero on
  bad regex.
- Colored diff in `olha status` when `max_count` / `max_age` thresholds are
  about to trigger cleanup.

---

## Test Recommendations

Current coverage:

- `olha/src/output.rs` — 7 unit tests (table/detail formatting, truncate).
- `olhad/src/rules.rs` — 2 unit tests (one match, one non-match).
- `olhad/src/config.rs` — 1 unit test (duration parsing).
- **Zero** integration tests, **zero** D-Bus tests, **zero** DB tests.

### Tier 1 — Critical Gaps

1. **DB layer unit tests** — `olhad/tests/db.rs` or `#[cfg(test)]` blocks in
   `olhad/src/db/queries.rs`.
   - Use `rusqlite::Connection::open_in_memory()` + `schema::init()` per
     test.
   - Cover: insert, list with every filter permutation (app / urgency /
     status / category / search / since / until / limit), count parity with
     list, mark-read idempotency, clear vs delete semantics, cleanup by age,
     cleanup by count, JSON round-trip for `actions` and `hints`.

2. **Rules engine expansion** — extend `olhad/src/rules.rs` tests.
   - Invalid regex → `RulesEngine::new` returns `Err`.
   - Rule ordering / first-match-wins.
   - All five match fields (`app_name`, `summary`, `body`, `urgency`,
     `category`) individually and combined.
   - Unicode body, empty body, multi-line body.

3. **Config loader** — `olhad/src/config.rs`.
   - Parse `config.example.toml` verbatim (asserts the doc stays valid).
   - Missing file → defaults; malformed TOML → typed error; unknown field →
     decide between warn vs fail (prefer `#[serde(deny_unknown_fields)]`).
   - `db_path` with `~`, with `$HOME`, absolute, relative.
   - Every `Duration` string variant including bad ones (`"30x"`, `"-1h"`).

### Tier 2 — Integration Tests (new `tests/` dirs)

4. **Private session-bus harness** — most valuable new test file.
   - `olhad/tests/common/mod.rs` spawns a private `dbus-daemon --session
     --print-address`, starts `olhad` against a temp DB + temp XDG dirs,
     yields a `zbus::Connection`. Cleans up in `Drop`.
   - Use this in every test below. Gate with
     `#[cfg_attr(not(feature = "dbus-tests"), ignore)]` so CI without dbus
     doesn't break.

5. **FDO interface round-trip** — `olhad/tests/fdo.rs`.
   - `Notify()` → row appears in DB → `GetServerInformation()` /
     `GetCapabilities()` return the advertised caps → `CloseNotification()`
     emits `NotificationClosed` signal.
   - Replace semantics (`replaces_id != 0`).
   - Expiration respected (`expire_timeout` `0` / `-1` / positive).

6. **`org.olha.Daemon` round-trip** — `olhad/tests/olha_daemon.rs`.
   - Every method: `list`, `count`, `show`, `mark_read`, `clear`, `delete`,
     `invoke_action`, `status`.
   - `invoke_action` emits `ActionInvoked(dbus_id, key)` (regression test
     for the CHANGELOG bug where `row_id` was dropped).
   - `notification_received` signal payload contains `row_id` (direct
     regression for the CHANGELOG fix).

7. **CLI black-box tests** — `olha/tests/cli.rs` using `assert_cmd` +
   `predicates`.
   - Point the CLI at the private bus from #4, run `olha list --json`,
     parse output with `serde_json`, assert shape.
   - Exit code 1 + friendly stderr on "daemon not running".
   - `olha completions bash | bash -n` — shell-completion scripts parse
     cleanly (run once per supported shell that's installed).

### Tier 3 — Behavioral / Long-Running

8. **Subscribe-stream test** — start daemon, subscribe via
   `futures_util::StreamExt`, fire N notifications on another task, assert
   all N events received in order with correct `row_id`s.

9. **Cleanup-loop test** — inject fake "old" rows by writing `created_at =
   now - 40d`, set `max_age = 30d` + short `cleanup_interval`, run daemon
   for 2 intervals, assert rows gone.

10. **Concurrency** — 10 CLI clients calling `list` / `count` in parallel
    against the daemon; assert no `database is locked` errors and identical
    results. Catches future regressions when busy-timeout / WAL mode is
    tuned.

11. **Popup logic** (`olha-popup/src/main.rs` eviction + stacking) —
    extract the pure state-transition fn (currently tangled inside
    `update()`) and unit-test it: enqueue > `max_visible`, critical never
    evicted, timeout dismissal, action-button click → `InvokeAction`
    message.

### Tier 4 — CI Hygiene

- `cargo clippy --workspace --all-targets -- -D warnings` in CI.
- `cargo fmt --check`.
- `cargo deny check` for license + advisory scanning.
- `cargo-hack --each-feature` once features land.
- A tiny `cargo-llvm-cov` target so coverage trends are visible.

---

## Recommended Next Step

Pick a scope and open an issue / branch for it:

- **"Testing foundation"** — Tier 1 tests #1–#3 + the private-bus harness
  (#4). Highest ROI; unblocks safe refactoring.
- **"Quality-of-life features"** — Tier 1 features #1 (systemd), #5 (watch),
  plus #3 (dedup) behind a schema migration (Tier 2 #1).
- **"Power-user features"** — exec-on-match (#2) + per-app overrides
  (Tier 2 #6) + DND mode (Tier 3 #2).
