# olha — A Minimalist Notification Center

**olha** (Portuguese for "look!") is a lightweight, persistent notification daemon for Linux/Wayland with first-class support for CLI and scripting integrations.

Unlike traditional notification daemons that show popups and forget, **olha** stores all notifications in a local SQLite database with full history, filtering, and lifecycle management. Perfect for integration with status bars (Waybar, eww), scripts, and window managers like Hyprland.

## Features

- **Persistent History**: All notifications stored in SQLite; never lose important messages
- **Full Lifecycle Management**: Track notifications as unread → read → cleared → deleted
- **Rich Filtering**: Query by app name, urgency, status, category, text search, and time range
- **D-Bus Standard**: Implements `org.freedesktop.Notifications` so `notify-send` and all desktop apps work
- **Minimal & Fast**: Written in Rust, lightweight daemon with zero dependencies on GTK/heavy UI libraries
- **Scriptable**: JSON output, subscribe mode for real-time event streaming
- **Notification Rules**: Auto-mute, auto-clear, or ignore notifications based on regex patterns
- **Do Not Disturb**: Silence popups on demand; history is still recorded, and critical notifications break through by default
- **Automatic Cleanup**: Configurable retention policies (max age, max count)

## Installation

### From Source

```bash
git clone https://github.com/yourusername/olha
cd olha
cargo build --release
cargo install --path olhad
cargo install --path olha
```

This installs two binaries:
- `olhad` — the daemon
- `olha` — the CLI client

### Shell Completions

`olha` can generate completion scripts for your shell.

```bash
# zsh — install into any directory on $fpath
olha completions zsh > ~/.zfunc/_olha
# ensure ~/.zfunc is on $fpath, then in your .zshrc:
#   autoload -Uz compinit && compinit

# bash
olha completions bash > ~/.local/share/bash-completion/completions/olha

# fish
olha completions fish > ~/.config/fish/completions/olha.fish
```

The zsh script also registers an `ola` alias (`olha` spelled in Portuguese,
meaning "look"). Add `alias ola=olha` to your shell rc and `ola <TAB>` will
complete the same subcommands and flags as `olha <TAB>`.

## Quick Start

1. **Start the daemon**:
   ```bash
   olhad
   ```

2. **Send a test notification**:
   ```bash
   notify-send "Hello" "This is a test notification"
   ```

3. **Query notifications**:
   ```bash
   olha list
   olha list --json
   olha count
   olha show 1
   ```

4. **Manage notifications**:
   ```bash
   olha mark-read 1          # Mark as read
   olha clear 1              # Dismiss/clear
   olha delete 1             # Permanently delete
   olha clear --all          # Clear all
   ```

## Configuration

Configuration file: `~/.config/olha/config.toml`

### Paths

- **Config**: `$XDG_CONFIG_HOME/olha/config.toml` (default: `~/.config/olha/config.toml`)
- **Database**: `$XDG_DATA_HOME/olha/notifications.db` (default: `~/.local/share/olha/notifications.db`)

### Configuration Options

```toml
[general]
# Optional custom database path
# db_path = "~/.local/share/olha/notifications.db"

[retention]
# Maximum age of notifications (d=days, h=hours, m=minutes, s=seconds)
max_age = "30d"

# Maximum number of notifications to keep
max_count = 10000

# How often to check for cleanup
cleanup_interval = "1h"

[notifications]
# Timeout in seconds before notifications auto-expire
default_timeout = 10
timeout_low = 5
timeout_critical = 0  # Critical notifications don't expire
```

#### Understanding Timeouts

When an application sends a notification, it can request a specific timeout (how long before the notification expires). If the application doesn't specify one (or sends `-1`), olha applies a default based on the notification's urgency level:

- **`default_timeout`** (10s) — Used for normal-urgency notifications. Most chat messages, download completions, and general app notifications fall here.
- **`timeout_low`** (5s) — Used for low-urgency notifications like Spotify track changes or routine status updates. These disappear quickly.
- **`timeout_critical`** (0 = never) — Used for critical notifications like low battery warnings or system errors. A value of `0` means the notification never auto-expires and stays until the user explicitly dismisses it.

These timeouts control the D-Bus `expire_timeout` value sent back to the notification source. They do **not** affect how long notifications are kept in the database — that's controlled by the `[retention]` section above.

**Example**: If Firefox sends a "Download complete" notification without a timeout, olha assigns the `default_timeout` of 10 seconds. After 10 seconds, the notification is considered expired, but it remains in the database as part of your notification history until the retention policy cleans it up.

### Do Not Disturb

`olha dnd` toggles DND at runtime. While on, the daemon still stores
incoming notifications in history, but the `notification_received`
signal is not emitted — so `olha-popup` stays quiet and
`olha subscribe` doesn't tick. Turn it off and new notifications
resume as normal. State persists across daemon restarts.

```bash
olha dnd            # show current state
olha dnd on         # silence popups
olha dnd off        # resume popups
olha dnd toggle
olha dnd --json     # machine-readable: {"enabled":..., "allow_critical":...}
```

Critical-urgency notifications (battery warnings, system errors)
break through DND by default. To make DND silence *everything*, set
this in your config:

```toml
[dnd]
allow_critical = false
```

Notes:

- DND only affects the real-time signal stream. Missed notifications
  are always recoverable via `olha list`.
- Clients can also react to the `dnd_changed` D-Bus signal on
  `org.olha.Daemon` if you want a status-bar indicator that flips
  without polling.

### Notification Rules

Rules live in two independent lists under two different TOML sections,
and they run at two different moments in a notification's life. Keeping
this distinction in your head is the single biggest win when writing
rules, because almost every "why didn't my rule fire?" question traces
back to confusing the two lists, or to confusing the two differently
named things called `action` inside a single daemon rule. This section
spells both out.

#### The pipeline at a glance

```
notify-send  ─────►  [ [[rules]] ]  ─────►  stored in SQLite  ─────►  [ [[popup.rules]] ]  ─────►  popup renders
                      (olhad)                                          (olha-popup only)
```

| Stage         | TOML section      | Binary       | Controls                                                         |
| ------------- | ----------------- | ------------ | ---------------------------------------------------------------- |
| Daemon rules  | `[[rules]]`       | `olhad`      | Whether the notification is stored, and what a click does later. |
| Popup rules   | `[[popup.rules]]` | `olha-popup` | Whether, and how, the popup is *displayed*.                      |

Daemon rules run first. Popup rules only ever run if you're using
`olha-popup`; the daemon itself never reads `[[popup.rules]]`. A popup
rule cannot prevent storage (the daemon has already stored the
notification by then), and a daemon rule cannot hide the popup without
also touching storage — if you just want "don't pop this, but keep it in
history," that's a job for `[[popup.rules]]`.

Each list is evaluated in the order it appears in `config.toml` and
**first match wins**.

---

#### Daemon rules (`[[rules]]`)

A daemon rule has a **match** part (regex/urgency/category conditions)
and a **verdict** part (the `action` field, plus an optional nested
`[rules.on_action]` table). If every match condition is satisfied, the
verdict takes effect.

##### The two `action`s — not the same field, not the same moment

The name collision is the single biggest source of confusion, so it's
worth stating directly:

- **`action`** — top-level scalar, **required**. This is the *storage
  verdict*, evaluated **once, when the notification arrives**. It decides
  whether the notification gets stored, silently dropped, or auto-cleared.
  Exactly three values are valid: `"clear"`, `"ignore"`, `"none"`.
- **`[rules.on_action]`** — nested table, optional. This is a map from
  *notification action key* → shell command, evaluated **later, when the
  user clicks a button on the popup**. The keys are action identifiers
  that the sending application attached to the notification — things like
  `"default"` (body click), `"reply"`, `"open"` — not storage verdicts.

These two fields do not replace each other and are not alternatives. A
rule that exists only to bind click handlers sets `action = "none"`
(don't touch storage) and then fills in `[rules.on_action]`.

##### Match fields

All specified fields must match for the rule to fire. Any field you omit
is ignored (not "must be empty").

- `app_name`, `summary`, `body`, `category` — regex; see [Regex syntax](#regex-syntax).
- `urgency` — exact string, one of `"low"`, `"normal"`, `"critical"`.

##### The `action` verdict

| Value    | What `olhad` does when the rule matches                                                 |
| -------- | --------------------------------------------------------------------------------------- |
| `clear`  | Store the notification, but immediately mark it as *cleared*. It appears in history but not as unread. |
| `ignore` | Do not store the notification at all. It vanishes — not in `olha list`, not in the popup. |
| `none`   | Store the notification normally (as unread). Use this when the rule exists only to attach `on_action` handlers. |

Examples:

```toml
# Auto-clear Slack thread notifications — kept in history, never unread.
[[rules]]
name     = "mute-slack-threads"
app_name = "Slack"
summary  = "Thread:.*"
action   = "clear"

# Silently drop every Spotify notification.
[[rules]]
name     = "ignore-spotify"
app_name = "Spotify"
action   = "ignore"

# Store normally, but watch for update-ish summaries across any app.
[[rules]]
name    = "system-updates"
summary = "^(Update|Upgrade|Install)"
action  = "clear"
```

##### The `[rules.on_action]` click handlers

`olhad` emits the standard `ActionInvoked` D-Bus signal whenever the user
clicks a popup button or the notification body. When the sending app is
still subscribed to the FDO notifications interface, it handles that
signal itself — typical for long-running GUI apps. When it isn't (the
app exited, or never listened), the daemon can run something local
instead:

1. **Default desktop-entry activation.** For the implicit `"default"`
   action (a body click), if the notification carries a `desktop-entry`
   hint, `olhad` runs `gtk-launch <entry>` to focus or launch the app.
   No config required.
2. **Per-rule shell commands via `[rules.on_action]`.** A map from action
   key to shell command. On a click, `olhad` walks the rule list in
   order, finds the first rule that matches *and* has an entry for the
   clicked action key, and runs that command under `sh -c`. This
   overrides step 1.

```toml
[[rules]]
name     = "focus-signal"
app_name = '^Signal$'
action   = "none"                                       # leave storage alone

[rules.on_action]
default = "signal-desktop --activate"                    # click on body
reply   = "notify-send 'Replied to $OLHA_SUMMARY'"       # click "Reply" button
```

The spawned command inherits these environment variables:

| Variable               | Value                                        |
| ---------------------- | -------------------------------------------- |
| `OLHA_APP_NAME`        | The notification's `app_name`                |
| `OLHA_SUMMARY`         | The notification's summary (title)           |
| `OLHA_BODY`            | The notification's body                      |
| `OLHA_URGENCY`         | `low` / `normal` / `critical`                |
| `OLHA_ACTION_KEY`      | The action key that was clicked              |
| `OLHA_DESKTOP_ENTRY`   | The notification's `desktop-entry` hint      |
| `OLHA_NOTIFICATION_ID` | Database row id                              |

A rule command always beats desktop-entry activation. FDO signal emission
is unchanged — a subscribed sender still receives `ActionInvoked`, and
the rule's command also runs.

> **Pitfall: keys under `[rules.on_action]` are *action keys*, not
> verdicts.** It's tempting to write something like
>
> ```toml
> [rules.on_action]
> none    = "hyprctl dispatch focuswindow class:signal"   # DEAD — never fires
> default = "hyprctl dispatch focuswindow class:signal"   # the live one
> ```
>
> because `"none"` is a valid value for the top-level `action` field.
> But `[rules.on_action]` is keyed by *action keys the sending app
> registered* (`default`, `reply`, `open`, etc.). No real app emits
> `"clear"`, `"ignore"`, or `"none"` as action keys, so entries under
> those names load but never match a click. `olhad` logs a warning
> (`[rules.on_action] key "<key>" will never fire`) at startup when it
> sees one. Rule of thumb: use `default` for body-click, and otherwise
> use exactly what the app advertises — check `olha show <id>` to see
> the action keys a given notification carries.

##### How `olha-popup` uses `[[rules]]`

`olha-popup` reads the same `[[rules]]` list as the daemon so it can
render buttons and dispatch clicks through `on_action` commands. But
**storage verdicts (`clear`/`ignore`/`none`) have already been applied by
the time the popup sees anything.** Filters that only affect the popup
itself live under `[[popup.rules]]` — described next.

---

#### Popup rules (`[[popup.rules]]`)

A second, independent list consumed only by `olha-popup`. These run when
the popup is about to display a notification that the daemon has already
stored. Popup rules cannot stop storage; they can only filter or rewrite
what the popup does with the notification.

| Field                   | Effect on the popup                                       |
| ----------------------- | --------------------------------------------------------- |
| `suppress = true`       | Don't show the popup at all (notification still in DB).   |
| `override_urgency`      | Re-label for rendering only. One of `low`/`normal`/`critical`. |
| `override_timeout_secs` | Force a specific auto-dismiss timeout, in seconds.        |

A single rule may combine multiple of these. Match fields are the same
as daemon rules (`app_name`, `summary`, `body`, `urgency`), with the
same regex semantics. Rules are evaluated in order; first match wins. A
rule with **no** match fields is a catch-all and fires on every
notification — include at least one match field unless that's really
what you want.

```toml
# Demote every Teams "critical" to "normal" so it auto-dismisses.
[[popup.rules]]
name             = "demote-teams"
app_name         = '^Microsoft Teams$'
override_urgency = "normal"

# Never pop a Spotify notification (daemon still stores it).
[[popup.rules]]
name     = "hide-spotify"
app_name = '^Spotify$'
suppress = true

# Force a 4-second dismiss for Slack popups regardless of urgency.
[[popup.rules]]
name                  = "short-timeout-for-slack"
app_name              = '^Slack$'
override_timeout_secs = 4
```

Broken regexes are logged (`skipping popup rule "<name>": …`) and
dropped — they don't stop the popup from starting or affect the other
rules. See `olha-popup/src/rules.rs` for the matching semantics and unit
tests.

---

#### Regex syntax

Patterns are compiled with Rust's [`regex`](https://docs.rs/regex) crate.
A few things to know:

- **Unanchored by default.** `'Slack'` matches `Slack`, `Slack Desktop`,
  and `slackware`. Use `^…$` when you need an exact match.
- **Case-sensitive by default.** Prepend `(?i)` for case-insensitive —
  e.g. `'(?i)^slack$'` matches both `Slack` and `SLACK`.
- **No lookaround or backreferences.** The `regex` crate is linear-time
  and omits `(?=…)`, `(?!…)`, `\1`, etc. Everything else in standard
  Perl-ish syntax works: `|`, `?`, `*`, `+`, `{m,n}`, `[…]`, `\d \w \s`,
  `(?i) (?m) (?s)` flags, Unicode classes `\p{…}`.
- **Prefer TOML literal (single-quoted) strings for regexes.** Basic
  strings (`"…"`) eat one layer of backslash escapes before the pattern
  ever reaches the regex engine, which is a common foot-gun:

  | What you want to match | Basic string (`"…"`) | Literal string (`'…'`) |
  | ---------------------- | -------------------- | ---------------------- |
  | literal `.`            | `"\\."`              | `'\.'`                 |
  | digit                  | `"\\d"`              | `'\d'`                 |
  | whitespace             | `"\\s"`              | `'\s'`                 |
  | path separator         | `"home/\\w+"`        | `'home/\w+'`           |

  Literal strings can't contain a single quote; for those rare patterns
  use a basic string and double each backslash.

Broken regexes are logged (`skipping popup rule "<name>": …`) and
dropped — they don't stop the popup from starting or affect the other
rules. See `olha-popup/src/rules.rs` for the matching semantics and unit
tests covering each of these cases.

## CLI Reference

### List Notifications

```bash
olha list                          # Show recent notifications (default: 50)
olha list --limit 20               # Limit results
olha list --app Slack              # Filter by app
olha list --urgency critical       # Filter by urgency
olha list --status unread          # Filter by status
olha list --category network       # Filter by category
olha list --search "important"     # Full-text search
olha list --since "2024-01-15T10:00:00Z"   # Since timestamp
olha list --until "2024-01-15T11:00:00Z"   # Until timestamp
olha list --json                   # Output as JSON (machine-readable)
```

By default, `olha list` prints a compact table:

```
ID     App              Summary                                   Status    Urgency   Created
─────  ───────────────  ────────────────────────────────────────  ────────  ────────  ────────────────
3      Firefox          Download complete                         unread    normal    10:30
2      Slack            New message in #general                   read      normal    Mon 09:15
1      System           Battery low                               unread    critical  Jan 14 23:45

3 notification(s)
```

Use `--json` when piping to other tools like `jq`, scripting, or integrating with status bars.

### Count Notifications

```bash
olha count                    # Show unread and total counts
olha count --status unread    # Count by status
olha count --json             # JSON output
```

### Show Single Notification

```bash
olha show 1              # Show notification with row ID 1
olha show 1 --json       # As JSON (machine-readable)
```

By default, `olha show` prints a detailed key-value view of a single notification:

```
ID:        3
App:       Firefox
Summary:   Download complete
Body:      document.pdf has finished downloading
Urgency:   normal
Status:    unread
Category:  transfer.complete
Desktop:   firefox
Actions:   open (Open), show-in-folder (Show in Folder)
Created:   2024-01-15T10:30:45Z
Updated:   2024-01-15T10:30:45Z
D-Bus ID:  42
```

This is useful for inspecting a notification's full details, including its available actions (see [Invoke Actions](#invoke-actions) below).

### Update Notifications

```bash
olha mark-read 1 2 3      # Mark multiple as read
olha mark-read --all      # Mark all as read
olha clear 1              # Clear/dismiss specific notification
olha clear --all          # Clear all
olha delete 1             # Permanently delete
olha delete 1 2 3 --all   # Delete permanently
```

### Invoke Actions

Some applications attach **action buttons** to their notifications. For example, when Firefox finishes a download, the notification might include an "Open" button and a "Show in Folder" button. A chat application might include "Reply" or "Mark as Read". These are called **notification actions**.

Each action has two parts:
- An **action key** (a machine-readable identifier like `"open"`, `"reply"`, or `"default"`)
- A **label** (what the user sees, like `"Open"`, `"Reply"`, or `"Mark as Read"`)

When you invoke an action, olha sends the action key back to the originating application over D-Bus, telling it the user wants to perform that action. What happens next depends entirely on the application — Firefox might open the downloaded file, a chat app might open a reply window, etc.

#### Discovering available actions

Use `olha show <id>` to see what actions a notification has:

```bash
$ olha show 3
ID:        3
App:       Firefox
Summary:   Download complete
Body:      document.pdf
Actions:   open (Open), show-in-folder (Show in Folder)
...
```

Or with JSON for scripting:

```bash
$ olha show 3 --json | jq '.actions'
[
  { "id": "open", "label": "Open" },
  { "id": "show-in-folder", "label": "Show in Folder" }
]
```

#### Invoking an action

Pass the notification's row ID and the action key:

```bash
olha invoke 3 "open"             # Tell Firefox to open the downloaded file
olha invoke 3 "show-in-folder"   # Tell Firefox to reveal it in the file manager
olha invoke 7 "reply"            # Tell a chat app to open the reply window
olha invoke 5 "default"          # Trigger the default action (usually "open/focus the app")
```

**Note**: The special action key `"default"` is a convention in the D-Bus notification spec. Most applications treat it as "the user clicked on the notification body itself" and will typically focus the relevant window.

#### Example: scripting with actions

```bash
# Find unread Firefox download notifications and open them all
olha list --app Firefox --status unread --json \
  | jq -r '.[] | select(.actions[]?.id == "open") | .row_id' \
  | xargs -I{} olha invoke {} "open"
```

### Subscribe to Events

Listen for new notifications in real time. The command blocks and prints each notification as it arrives, one per line. This is the foundation for building desktop popups, status bar integrations, and automation scripts.

```bash
olha subscribe           # One-line summary per notification
olha subscribe --json    # Full JSON per notification (one JSON object per line)
```

The default output looks like:

```
[normal] [Firefox] Download complete — document.pdf
[critical] [System] Battery low
[low] [Spotify] Now playing — Never Gonna Give You Up
```

With `--json`, each line is a complete notification object (suitable for piping to `jq`):

```bash
# Pipe to jq for filtering
olha subscribe --json | jq 'select(.urgency == "critical")'

# Pipe to a popup script (see Hyprland integration below)
olha subscribe --json | while IFS= read -r line; do
  echo "$line" | jq -r '.summary'
done
```

### Daemon Status

```bash
olha status              # Show daemon status
olha status --json       # JSON output
```

The non-JSON output includes the current DND toggle. With `--json`,
there's a `dnd` object: `{"enabled": bool, "allow_critical": bool}`.

### Do Not Disturb

```bash
olha dnd                 # Show current state
olha dnd on              # Silence popups
olha dnd off             # Resume popups
olha dnd toggle          # Flip the current state
olha dnd --json          # {"enabled": bool, "allow_critical": bool}
```

While DND is on, the daemon still writes every incoming notification
to the database — the `notification_received` signal is simply not
emitted, so `olha-popup` and `olha subscribe` stay quiet. History is
always recoverable via `olha list`. Critical-urgency notifications
break through by default; see the [Do Not Disturb config
section](#do-not-disturb) to silence everything instead.

## Notification Lifecycle

Unlike traditional notification daemons that show a popup and then forget about it, olha stores every notification in a database and tracks its state. This means you can go back and see what you missed, search through old notifications, or script workflows around them.

Notifications move through these states:

```
[New from D-Bus]
     ↓
  UNREAD  ← Just arrived, you haven't looked at it yet
     ↓
  READ    ← You've acknowledged it (mark-read)
     ↓
  CLEARED ← You've dismissed it (clear) — still in the database
     ↓
[DELETED] ← Permanently removed from the database (delete or auto-cleanup)
```

### When to use each state

| Action | Command | What it means | Still in DB? |
|--------|---------|---------------|:------------:|
| **Mark as read** | `olha mark-read 1` | "I've seen this" — like marking an email as read. Useful for tracking what's new. | Yes |
| **Clear** | `olha clear 1` | "I'm done with this" — the notification is dismissed but kept in history. Most notification daemons call this "closing" or "dismissing". | Yes |
| **Delete** | `olha delete 1` | "Remove this permanently" — gone from the database entirely. Use this for sensitive notifications or cleanup. | No |

### Practical examples

```bash
# Morning routine: see what came in overnight
olha list --status unread

# Mark everything as read after reviewing
olha mark-read --all

# Dismiss a noisy app's notifications without deleting history
olha list --app Slack --status unread --json \
  | jq -r '.[].row_id' \
  | xargs olha clear

# Hard-delete old cleared notifications (retention policy also does this automatically)
olha list --status cleared --json \
  | jq -r '.[].row_id' \
  | xargs olha delete
```

### Automatic state changes

Notifications can also change state automatically:
- **Rules** can auto-clear or ignore notifications on arrival (see [Notification Rules](#notification-rules))
- **Retention policy** deletes old notifications based on age (`max_age`) and count (`max_count`)
- **CloseNotification** — when an application programmatically closes its own notification (e.g., a timer app clearing its alert), olha marks it as cleared

## Integration Examples

### Waybar

Add to your `~/.config/waybar/config.json`:

```json
{
  "modules-right": ["custom/notifications"],
  "custom/notifications": {
    "format": "{}",
    "exec": "olha count --json | jq -r '\"\\(.unread)/\\(.total)\"'",
    "exec-on-event": false,
    "interval": 5,
    "on-click": "olha list --json | jq '.[0].row_id' | xargs -I{} olha mark-read {}"
  }
}
```

### eww (Elkowars Wacky Widgets)

Create `~/.config/eww/eww.yuck`:

```lisp
(defwidget notifications []
  (box :class "notifications"
    (label :text "${notifications-count}"
           :onclick "eww open notification-center")))

(defpoll notifications-count :interval "5s" "olha count --json | jq -r '.unread'")

(defwindow notification-center
  :monitor 0
  :geometry (geometry :x "50%"
                      :y "50%"
                      :width "600px"
                      :height "400px"
                      :anchor "center")
  (box (label :text (exec "olha list --json"))))
```

### olha-popup (Wayland, Hyprland / Sway)

`olha-popup` is a native Wayland popup notifier built on `wlr-layer-shell`. It shows stacked, actionable popups at a screen corner, invokes notification actions (Open, Reply, …) on click, and dismisses itself on timeout or close. No terminal spawning, no `jq`, no window rules — it speaks directly to the daemon over D-Bus.

**Requirements**: a compositor that implements `wlr-layer-shell` (Hyprland, Sway, River, KDE Plasma 6 Wayland, etc.). GNOME Mutter is not supported — see the shell fallback below.

#### 1. Build and install

```bash
cargo install --path olha-popup
```

This installs the `olha-popup` binary to `~/.cargo/bin/`.

#### 2. Autostart

For Hyprland (`~/.config/hypr/hyprland.conf`):

```ini
exec-once = olhad
exec-once = olha-popup
```

For Sway (`~/.config/sway/config`):

```
exec olhad
exec olha-popup
```

#### 3. Configure

All options live under the `[popup]` section of `~/.config/olha/config.toml`. Defaults are sane; override any subset:

```toml
[popup]
position    = "top-right"   # top-right | top-left | bottom-right | bottom-left
max_visible = 5             # oldest non-critical popup is evicted when exceeded
margin      = 12            # px from screen edge
gap         = 8             # px between stacked popups
width       = 380
height      = 120
```

Per-urgency timeouts come from the existing `[notifications]` section:

```toml
[notifications]
default_timeout  = 10   # seconds; normal urgency
timeout_low      = 5
timeout_critical = 0    # 0 = sticky until dismissed or replaced
```

The notifier also honors `expire_timeout` when the sender specifies it (in milliseconds, per the FDO spec); `0` from the sender means "never expire", `-1` means "use server default".

#### 4. Actions

When a notification carries actions (e.g., Firefox download complete with an **Open** button), `olha-popup` renders them as clickable buttons. Clicking one calls `org.olha.Daemon.InvokeAction`, which in turn emits the standard `ActionInvoked` signal back to the originating app — the normal D-Bus round-trip any notification daemon would perform. The popup also marks the notification as read.

Try it:

```bash
notify-send --urgency=normal --app-name=Test \
  "Hello" "Click a button" -A "open=Open" -A "dismiss=Dismiss"
```

### Shell-script fallback (non-layer-shell compositors)

If your compositor does not implement `wlr-layer-shell` (e.g., GNOME Mutter), you can still drive popups from `olha subscribe --json` with a small script that spawns an Alacritty window per notification. This mode does *not* support clickable actions.

<details>
<summary>Show Alacritty+Hyprland shell recipe</summary>

Add window rules to `~/.config/hypr/hyprland.conf`:

```ini
windowrule {
  name = olha-popup
  match:class = ^(olha-popup-.*)$

  float = on
  pin = on
  size = 600 100
  move = (monitor_w-620) 40
  no_initial_focus = on
  no_focus = on
  no_shadow = on
  animation = slide
  rounding = 8
}

windowrule = border_color rgb(7aa2f7), match:class ^(olha-popup-normal)$
windowrule = border_color rgb(e0af68), match:class ^(olha-popup-low)$
windowrule = border_color rgb(f7768e), match:class ^(olha-popup-critical)$
```

Create `~/.config/olha/popup.sh`:

```bash
#!/bin/bash
cleanup() { pkill -f 'alacritty.*--class olha-popup' 2>/dev/null; }
trap cleanup EXIT

olha subscribe --json | while IFS= read -r line; do
  pkill -f 'alacritty.*--class olha-popup' 2>/dev/null
  sleep 0.05

  summary=$(echo "$line" | jq -r '.summary // ""')
  app=$(echo "$line" | jq -r '.app_name // ""')
  body=$(echo "$line" | jq -r '.body // ""')
  urgency=$(echo "$line" | jq -r '.urgency // "normal"')

  case "$urgency" in
    low)      timeout=5 ;;
    critical) timeout=0 ;;
    *)        timeout=10 ;;
  esac

  text=""
  [ -n "$app" ] && text="[$app] "
  text="${text}${summary}"
  [ -n "$body" ] && text="${text}\n${body}"

  if [ "$timeout" -eq 0 ]; then
    alacritty --class "olha-popup-${urgency}" --title "olha" \
      -e bash -c "echo -e '${text//\'/\\\'}'; cat" &
  else
    alacritty --class "olha-popup-${urgency}" --title "olha" \
      -e bash -c "echo -e '${text//\'/\\\'}'; sleep $timeout" &
  fi
done
```

Then `chmod +x ~/.config/olha/popup.sh` and `exec-once = ~/.config/olha/popup.sh`. Dependencies: `alacritty`, `jq`.

</details>

### Polybar

Add to your polybar config:

```ini
[module/olha]
type = custom/script
exec = olha count --json | jq -r '"unread: \(.unread)"'
interval = 5
click-left = olha list
```

## Database Schema

The SQLite database has two tables: `notifications` (per-notification
rows) and `meta` (small daemon-wide state, currently just the DND
toggle).

### `notifications`

| Column | Type | Description |
|--------|------|-------------|
| `id` | INTEGER | Internal row ID (primary key) |
| `dbus_id` | INTEGER | D-Bus notification ID |
| `app_name` | TEXT | Application that sent the notification |
| `app_icon` | TEXT | Icon name or path |
| `summary` | TEXT | Notification title |
| `body` | TEXT | Notification message |
| `urgency` | INTEGER | 0=low, 1=normal, 2=critical |
| `category` | TEXT | D-Bus category (e.g., "im.received") |
| `desktop_entry` | TEXT | .desktop file identifier |
| `actions` | TEXT | JSON array of {id, label} action buttons |
| `hints` | TEXT | JSON object of raw D-Bus hints |
| `status` | TEXT | "unread", "read", or "cleared" |
| `expire_timeout` | INTEGER | Timeout in milliseconds |
| `created_at` | TEXT | ISO 8601 creation timestamp |
| `updated_at` | TEXT | ISO 8601 last update timestamp |
| `closed_reason` | INTEGER | Why it was closed (1=expired, 2=dismissed, etc.) |

When `[encryption].enabled = true`, the `summary`, `body`, and `hints`
columns are empty and the authoritative ciphertext lives in the
`summary_enc` / `body_enc` / `hints_enc` BLOB columns, alongside an
`enc_version` and `key_id` pinning the row to the DEK that encrypted
it.

### `meta`

| Column | Type | Description |
|--------|------|-------------|
| `key` | TEXT | Primary key. Currently only `dnd_enabled` is used. |
| `value` | TEXT | Opaque string value (e.g., `"true"` / `"false"`). |

This table is a tiny KV store for daemon state that needs to survive
restarts without a config-file round-trip. The DND toggle set by
`olha dnd on/off` lives here.

## Filtering & Queries

The `list` command uses flexible filtering. All filters can be combined:

```bash
# Complex example: critical Firefox notifications from the last hour, unread
olha list --app Firefox --urgency critical --status unread \
  --since "$(date -d '1 hour ago' -Iseconds)"

# Search for "meeting" in notifications from today
olha list --search "meeting" \
  --since "$(date -d 'today 00:00:00' -Iseconds)"
```

## JSON Output Format

When using `--json` flag, output is a JSON array of notifications:

```json
[
  {
    "row_id": 1,
    "dbus_id": 1,
    "app_name": "Firefox",
    "summary": "Download complete",
    "body": "document.pdf",
    "urgency": "normal",
    "status": "unread",
    "created_at": "2024-01-15T10:30:45Z",
    "updated_at": "2024-01-15T10:30:45Z",
    "category": "transfer.complete",
    "desktop_entry": "firefox",
    "actions": [
      {"id": "open", "label": "Open"},
      {"id": "show", "label": "Show in folder"}
    ]
  }
]
```

## Architecture

```
notify-send / Apps ──(D-Bus)──→ olhad ──→ SQLite DB
                                  ↓
                                  │
                     ┌────────────┴────────────┐
                     ↓                        ↓
                  Rules Engine          Retention
                  (auto-mute)           Cleanup
                                        (background)

olha CLI ←─(D-Bus)─ olhad ←─(DB Query)─ SQLite DB
waybar / eww / scripts ←─ JSON output
```

**olhad** is the central daemon:
- Receives notifications via standard D-Bus `org.freedesktop.Notifications` interface
- Applies user rules to auto-mute/auto-clear matching notifications
- Stores all notifications in SQLite with timestamps and metadata
- Provides a control interface (`org.olha.Daemon`) for querying and managing notifications

**olha** CLI is the user-facing client:
- Queries the daemon via D-Bus to list, filter, and manage notifications
- Outputs results as human-readable text or JSON
- Can be integrated into status bars, scripts, and automation workflows

## Building from Source

### Prerequisites

- Rust 1.70+
- pkg-config
- dbus development headers

### Build

```bash
cargo build --release

# Install
cargo install --path olhad
cargo install --path olha
```

### Running the Daemon

```bash
# Start in foreground (good for debugging)
RUST_LOG=debug olhad

# Start as background service
olhad &

# Or via systemd user service:
# TODO: provide systemd unit file
```

## Troubleshooting

### Daemon won't start

1. Check if another notification daemon is running:
   ```bash
   ps aux | grep -E "(mako|dunst|swaync|olhad)"
   ```

2. Check logs:
   ```bash
   RUST_LOG=debug olhad
   ```

3. Verify D-Bus connectivity:
   ```bash
   gdbus introspect --session --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications
   ```

### CLI can't connect to daemon

1. Ensure daemon is running:
   ```bash
   olha status
   ```

2. Check D-Bus session:
   ```bash
   echo $DBUS_SESSION_BUS_ADDRESS
   ```

3. Check if olha can see the daemon:
   ```bash
   gdbus introspect --session --dest org.olha.Daemon --object-path /org/olha/Daemon
   ```

### Database issues

Database is stored in `~/.local/share/olha/notifications.db`. To reset:

```bash
rm ~/.local/share/olha/notifications.db
olhad  # Recreates on startup
```

## Future Enhancements

- [ ] Scripting/hook support (run commands on notification match)
- [x] Subscribe mode with real-time event streaming
- [ ] Web UI dashboard
- [ ] Notification archival (move old to separate table)
- [ ] Rich notification body with markdown/HTML rendering
- [ ] Sound/audio integration
- [ ] Inline reply support
- [ ] Action button execution
- [ ] systemd user service

## License

MIT

## Contributing

Pull requests welcome! Areas needing help:
- Web UI / dashboard
- Additional integrations (Polybar, lemonbar, etc.)
- Better error handling and edge cases
- Performance optimizations
- Documentation

## Related Projects

- **SwayNotificationCenter** — Full-featured notification center for Sway/Wayland (GTK-based)
- **Dunst** — Lightweight notification daemon (X11/Wayland)
- **Mako** — Minimal Wayland notification daemon
- **Systemd-user-units** — systemd user service examples

---

Made with ❤️ for minimalists who want their notifications to stick around and be queryable.
