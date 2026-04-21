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

### Notification Rules

Rules allow you to automatically mute, clear, or ignore notifications based on patterns:

```toml
[[rules]]
name = "mute-slack-threads"
app_name = "Slack"
summary = "Thread:.*"
action = "clear"  # Automatically clear matched notifications

[[rules]]
name = "ignore-spotify"
app_name = "Spotify"
action = "ignore"  # Don't store in database at all

[[rules]]
name = "system-updates"
summary = "^(Update|Upgrade|Install)"
action = "clear"
```

Rules are regex-based and support matching on:
- `app_name` — the application name
- `summary` — the notification title
- `body` — the notification message
- `urgency` — "low", "normal", or "critical"
- `category` — the D-Bus category hint

When a notification matches **all** specified fields in a rule, the action is taken:
- `clear` — automatically dismiss the notification (marks as cleared)
- `ignore` — don't store the notification in the database at all

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

### Hyprland

olha integrates with Hyprland to show desktop notification popups using a small floating Alacritty window. No GTK, no extra daemons — just `olha subscribe` piped to a shell script that spawns a terminal popup with Hyprland window rules for positioning and styling.

**How it works**: `olha subscribe --json` streams notifications in real time. A small shell script reads each one, kills any previous popup, and spawns a new Alacritty window with a class that encodes the urgency level. Hyprland window rules position the window and set the border color based on that class.

#### 1. Add window rules to `~/.config/hypr/hyprland.conf`

```ini
# Base positioning and behavior for all olha notification popups
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

# Urgency-based border colors (Tokyo Night palette)
windowrule = border_color rgb(7aa2f7), match:class ^(olha-popup-normal)$    # Normal: blue
windowrule = border_color rgb(e0af68), match:class ^(olha-popup-low)$       # Low: amber
windowrule = border_color rgb(f7768e), match:class ^(olha-popup-critical)$  # Critical: red
```

Adjust `size`, `move`, and `border_color` values to taste. The `move` expression places the popup 20px from the right edge and 40px from the top. `pin` keeps it visible across workspaces. `no_initial_focus` and `no_focus` prevent the popup from stealing keyboard focus.

#### 2. Create the popup script at `~/.config/olha/popup.sh`

```bash
#!/bin/bash
# olha notification popup for Hyprland + Alacritty
# Spawns a small floating terminal for each notification.
# Previous popup is killed when a new one arrives.

cleanup() { pkill -f 'alacritty.*--class olha-popup' 2>/dev/null; }
trap cleanup EXIT

olha subscribe --json | while IFS= read -r line; do
  # Kill previous popup
  pkill -f 'alacritty.*--class olha-popup' 2>/dev/null
  sleep 0.05  # Brief pause to let the old window close

  # Extract fields
  summary=$(echo "$line" | jq -r '.summary // ""')
  app=$(echo "$line" | jq -r '.app_name // ""')
  body=$(echo "$line" | jq -r '.body // ""')
  urgency=$(echo "$line" | jq -r '.urgency // "normal"')

  # Timeout based on urgency (matches olha config defaults)
  case "$urgency" in
    low)      timeout=5 ;;
    critical) timeout=0 ;;
    *)        timeout=10 ;;
  esac

  # Build display text
  text=""
  [ -n "$app" ] && text="[$app] "
  text="${text}${summary}"
  [ -n "$body" ] && text="${text}\n${body}"

  # Spawn popup (class encodes urgency for Hyprland border color rules)
  if [ "$timeout" -eq 0 ]; then
    # Critical: stays until next notification replaces it
    alacritty --class "olha-popup-${urgency}" --title "olha" \
      -e bash -c "echo -e '${text//\'/\\\'}'; cat" &
  else
    alacritty --class "olha-popup-${urgency}" --title "olha" \
      -e bash -c "echo -e '${text//\'/\\\'}'; sleep $timeout" &
  fi
done
```

Make it executable:

```bash
chmod +x ~/.config/olha/popup.sh
```

**Dependencies**: `alacritty` and `jq`.

#### 3. Add startup lines to `~/.config/hypr/hyprland.conf`

```ini
exec-once = olhad
exec-once = ~/.config/olha/popup.sh
```

#### 4. Customize

**Timeout**: Edit the `case` block in the script. The defaults match olha's config: 5s for low, 10s for normal, and critical notifications stay until replaced.

**Position/size**: Edit the `move` and `size` values in the window rule. For example, to place popups in the top-left corner: `move = 20 40`.

**Colors**: Edit the `border_color` values. Some palettes:

| Palette | Low | Normal | Critical |
|---------|-----|--------|----------|
| Tokyo Night | `rgb(e0af68)` | `rgb(7aa2f7)` | `rgb(f7768e)` |
| Catppuccin Mocha | `rgb(f9e2af)` | `rgb(b4befe)` | `rgb(f38ba8)` |
| Gruvbox | `rgb(d8a657)` | `rgb(7daea3)` | `rgb(ea6962)` |

**Hotkey to dismiss**: Add a keybind to kill the popup on demand:

```ini
bind = $mainMod, Escape, exec, pkill -f 'alacritty.*--class olha-popup'
```

**Hotkey to open notification list**:

```ini
bind = $mainMod, n, exec, alacritty --class olha-center --title "Notifications" -e bash -c "olha list; read -r -p 'Press Enter to close...'"
```

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

The SQLite database stores notifications with the following fields:

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
