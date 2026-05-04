mod config;
mod dbus;
mod model;
mod rules;

use std::time::{Duration, Instant};

use iced::widget::{button, column, container, row, text, Space};
use iced::{alignment, Color, Element, Length, Padding, Subscription, Task, Theme};
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use indexmap::IndexMap;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

use crate::config::{AppConfig, Position};
use crate::dbus::{connect, dismiss, invoke_action, parse_signal_payload};
use crate::model::{Notification, PopupState, Urgency};
use crate::rules::PopupRules;

fn main() -> iced_layershell::Result {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = AppConfig::load();
    let init_config = config.clone();

    iced_layershell::daemon(
        move || (App::new(init_config.clone()), Task::none()),
        App::namespace,
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .theme(App::theme)
    .settings(Settings {
        layer_settings: LayerShellSettings {
            start_mode: StartMode::Background,
            ..Default::default()
        },
        ..Default::default()
    })
    .run()
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    Incoming(Box<Notification>),
    Tick,
    ActionClicked(iced::window::Id, String),
    Dismiss(iced::window::Id),
    ActionResult(Result<(), String>),
    WindowClosed(iced::window::Id),
    DaemonUnlockedChanged(bool),
}

struct App {
    config: AppConfig,
    rules: PopupRules,
    /// Insertion order == visual stack order; index 0 is the newest popup,
    /// sitting closest to the anchor edge. Older popups slide toward the
    /// opposite edge.
    popups: IndexMap<iced::window::Id, PopupState>,
    pending_close: Option<iced::window::Id>,
    /// Daemon-reported unlock state. `true` means the X25519 sk is
    /// loaded; `false` means locked or encryption disabled. Tracked
    /// only for the `hide_content_when_locked` privacy toggle — the
    /// popup always gets plaintext on the signal. Optimistically
    /// `true` at startup so we err on the side of showing content
    /// until the daemon tells us otherwise.
    daemon_unlocked: bool,
}

impl App {
    fn new(config: AppConfig) -> Self {
        let rules = PopupRules::new(&config.popup.rules);
        Self {
            config,
            rules,
            popups: IndexMap::new(),
            pending_close: None,
            daemon_unlocked: true,
        }
    }

    fn namespace() -> String {
        "olha-popup".to_string()
    }

    fn theme(&self, _id: iced::window::Id) -> Option<Theme> {
        Some(Theme::Dark)
    }

    fn timeout_for(&self, urgency: Urgency, override_secs: Option<u32>) -> Option<Duration> {
        // Policy is owned by olha: the sender's expire_timeout is ignored.
        // Precedence: matched rule's override_timeout_secs > per-urgency config.
        // In both, a value of 0 means "never expire".
        let secs = override_secs.unwrap_or_else(|| match urgency {
            Urgency::Low => self.config.notifications.timeout_low,
            Urgency::Normal => self.config.notifications.default_timeout,
            Urgency::Critical => self.config.notifications.timeout_critical,
        });
        if secs == 0 {
            None
        } else {
            Some(Duration::from_secs(secs as u64))
        }
    }

    /// Emit `MarginChange` messages for popups starting at `start` so they
    /// settle into their current `IndexMap` positions. Call this after any
    /// insert/remove so the stack closes gaps and new popups push old ones
    /// down. `start = 1` after inserting at index 0 skips the new popup (it's
    /// already placed via `NewLayerShell`); `start = 0` after a removal
    /// repositions the whole stack.
    ///
    /// Walks once and accumulates height + gap so popups with different
    /// heights stack correctly without overlap.
    fn relayout_tasks(&self, start: usize) -> Vec<Task<Message>> {
        let pos = self.config.popup.position;
        let edge = self.config.popup.margin;
        let gap = self.config.popup.gap;
        let mut tasks = Vec::new();
        let mut offset: u32 = 0;
        for (i, (id, state)) in self.popups.iter().enumerate() {
            if i >= start {
                let (_, margin) = anchor_and_margin(pos, edge, offset);
                tasks.push(Task::done(Message::MarginChange { id: *id, margin }));
            }
            offset = offset.saturating_add(state.height).saturating_add(gap);
        }
        tasks
    }

    fn new_layer_settings(&self, height: u32, offset: u32) -> NewLayerShellSettings {
        let (anchor, margin) =
            anchor_and_margin(self.config.popup.position, self.config.popup.margin, offset);
        NewLayerShellSettings {
            size: Some((self.config.popup.width, height)),
            exclusive_zone: None,
            anchor,
            layer: Layer::Overlay,
            margin: Some(margin),
            keyboard_interactivity: KeyboardInteractivity::None,
            output_option: OutputOption::None,
            ..Default::default()
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Incoming(notif) => self.handle_incoming(*notif),
            Message::Tick => self.handle_tick(),
            Message::ActionClicked(id, key) => self.handle_action(id, key),
            Message::Dismiss(id) => self.handle_dismiss(id),
            Message::ActionResult(Err(e)) => {
                tracing::warn!("action/dismiss D-Bus call failed: {e}");
                Task::none()
            }
            Message::ActionResult(Ok(())) => Task::none(),
            Message::WindowClosed(id) => {
                // Compositor closed (or we closed) the window — only relayout
                // the survivors if this was actually one of ours.
                if self.popups.shift_remove(&id).is_some() {
                    Task::batch(self.relayout_tasks(0))
                } else {
                    Task::none()
                }
            }
            Message::DaemonUnlockedChanged(unlocked) => {
                tracing::debug!("daemon unlocked changed: {}", unlocked);
                self.daemon_unlocked = unlocked;
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn handle_incoming(&mut self, notif: Notification) -> Task<Message> {
        let decision = self.rules.evaluate(&notif);
        if decision.suppress {
            tracing::debug!(
                "suppressing popup for app={:?} summary={:?} per rule {:?}",
                notif.app_name,
                notif.summary,
                decision.matched.as_deref().unwrap_or("?"),
            );
            return Task::none();
        }
        let urgency = decision.override_urgency.unwrap_or(notif.urgency);

        let evict_task = self.evict_if_needed();

        let timeout = self.timeout_for(urgency, decision.override_timeout_secs);
        let expires_at = timeout.map(|d| Instant::now() + d);

        let hide_now = decision
            .hide_content_when_locked
            .unwrap_or(self.config.popup.hide_content_when_locked)
            && !self.daemon_unlocked;

        let (display_summary, display_body) = if hide_now {
            (String::new(), "New notification".to_string())
        } else {
            (notif.summary, notif.body)
        };

        let (height, body_for_render) = compute_layout(
            &display_body,
            !notif.actions.is_empty(),
            self.config.popup.width,
            self.config.popup.height,
            self.config.popup.max_height,
        );

        let state = PopupState {
            row_id: notif.row_id,
            urgency,
            app_name: notif.app_name,
            summary: display_summary,
            body: body_for_render,
            actions: notif.actions,
            expires_at,
            height,
        };

        // Newest popup goes at index 0 (closest to the anchor edge) and
        // shifts every existing popup one slot further away. NewLayerShell
        // below places the new window directly at the correct margin; the
        // relayout tasks reposition the now-shifted survivors.
        let id = iced::window::Id::unique();
        let settings = self.new_layer_settings(height, 0);
        self.popups.shift_insert(0, id, state);

        let mut tasks: Vec<Task<Message>> = Vec::new();
        if let Some(t) = evict_task {
            tasks.push(t);
        }
        tasks.extend(self.relayout_tasks(1));
        tasks.push(Task::done(Message::NewLayerShell { settings, id }));
        Task::batch(tasks)
    }

    fn evict_if_needed(&mut self) -> Option<Task<Message>> {
        if self.popups.len() < self.config.popup.max_visible {
            return None;
        }
        // Oldest non-critical sits at the highest index now that newest is
        // at index 0.
        let victim = self
            .popups
            .iter()
            .rev()
            .find(|(_, s)| s.urgency != Urgency::Critical)
            .map(|(k, _)| *k);
        let id = victim?;
        self.popups.shift_remove(&id);
        Some(iced::window::close(id))
    }

    fn handle_tick(&mut self) -> Task<Message> {
        let now = Instant::now();
        let expired: Vec<iced::window::Id> = self
            .popups
            .iter()
            .filter_map(|(id, state)| match state.expires_at {
                Some(t) if t <= now => Some(*id),
                _ => None,
            })
            .collect();

        if expired.is_empty() && self.pending_close.is_none() {
            return Task::none();
        }

        let mut tasks: Vec<Task<Message>> = Vec::new();
        if let Some(id) = self.pending_close.take() {
            tasks.push(iced::window::close(id));
        }
        let expired_count = expired.len();
        for id in expired {
            self.popups.shift_remove(&id);
            tasks.push(iced::window::close(id));
        }
        if expired_count > 0 {
            tasks.extend(self.relayout_tasks(0));
        }
        Task::batch(tasks)
    }

    fn handle_action(&mut self, id: iced::window::Id, key: String) -> Task<Message> {
        let row_id = self.popups.get(&id).and_then(|s| s.row_id);
        tracing::debug!(
            "handle_action: window_id={:?} key={} row_id={:?}",
            id,
            key,
            row_id,
        );
        let existed = self.popups.shift_remove(&id).is_some();
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if existed {
            tasks.extend(self.relayout_tasks(0));
        }
        tasks.push(iced::window::close(id));
        match row_id {
            Some(row_id) => {
                tasks.push(Task::perform(
                    async move { invoke_action(row_id, key).await },
                    Message::ActionResult,
                ));
            }
            None => {
                tracing::warn!(
                    "action '{key}' clicked but notification has no row_id; cannot invoke"
                );
            }
        }
        Task::batch(tasks)
    }

    fn handle_dismiss(&mut self, id: iced::window::Id) -> Task<Message> {
        let row_id = self.popups.get(&id).and_then(|s| s.row_id);
        tracing::debug!("handle_dismiss: window_id={:?} row_id={:?}", id, row_id);
        let existed = self.popups.shift_remove(&id).is_some();
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if existed {
            tasks.extend(self.relayout_tasks(0));
        }
        tasks.push(iced::window::close(id));
        if let Some(row_id) = row_id {
            tasks.push(Task::perform(
                async move { dismiss(row_id).await },
                Message::ActionResult,
            ));
        }
        Task::batch(tasks)
    }

    fn view(&self, id: iced::window::Id) -> Element<'_, Message> {
        match self.popups.get(&id) {
            Some(state) => popup_view(id, state),
            None => Space::new().width(Length::Fill).height(Length::Fill).into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let signals = Subscription::run(signal_stream);
        let locked = Subscription::run(locked_stream);
        let tick = iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick);
        let close_events = iced::window::close_events().map(Message::WindowClosed);
        Subscription::batch([signals, locked, tick, close_events])
    }
}

fn signal_stream() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::channel::mpsc;
    iced::stream::channel(32, |mut sender: mpsc::Sender<Message>| async move {
        use futures_util::{SinkExt, StreamExt};
        let proxy = match connect().await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("failed to connect to olhad: {e}");
                futures_util::future::pending::<()>().await;
                unreachable!()
            }
        };
        let mut stream = match proxy.receive_notification_received().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("failed to subscribe to daemon signal: {e}");
                futures_util::future::pending::<()>().await;
                unreachable!()
            }
        };
        while let Some(sig) = stream.next().await {
            if let Ok(args) = sig.args() {
                if let Some(notif) = parse_signal_payload(args.notification()) {
                    tracing::debug!(
                        "incoming popup: row_id={:?} dbus_id={} actions={:?}",
                        notif.row_id,
                        notif.dbus_id,
                        notif.actions.iter().map(|a| &a.id).collect::<Vec<_>>(),
                    );
                    let _ = sender.send(Message::Incoming(Box::new(notif))).await;
                }
            }
        }
        futures_util::future::pending::<()>().await;
        unreachable!()
    })
}

/// Track the daemon's unlock state so the privacy toggle can decide
/// whether to hide content in popups. Reads once at startup, then
/// listens for `locked_changed` signals.
fn locked_stream() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::channel::mpsc;
    iced::stream::channel(4, |mut sender: mpsc::Sender<Message>| async move {
        use futures_util::{SinkExt, StreamExt};
        let proxy = match connect().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("locked_stream: failed to connect to olhad: {e}");
                futures_util::future::pending::<()>().await;
                unreachable!()
            }
        };
        match proxy.is_unlocked().await {
            Ok(b) => {
                let _ = sender.send(Message::DaemonUnlockedChanged(b)).await;
            }
            Err(e) => tracing::debug!("is_unlocked probe failed at startup: {e}"),
        }
        let mut stream = match proxy.receive_locked_changed().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("locked_stream: failed to subscribe: {e}");
                futures_util::future::pending::<()>().await;
                unreachable!()
            }
        };
        while let Some(sig) = stream.next().await {
            if let Ok(args) = sig.args() {
                let _ = sender
                    .send(Message::DaemonUnlockedChanged(*args.unlocked()))
                    .await;
            }
        }
        futures_util::future::pending::<()>().await;
        unreachable!()
    })
}

/// `offset` is the cumulative pixel distance from the screen edge to where
/// this popup's anchor edge should sit — i.e. the sum of the heights of
/// every popup stacked closer to the anchor, plus one `gap` between each.
/// Callers compute it once while walking the popup list (see
/// `App::relayout_tasks`).
fn anchor_and_margin(
    pos: Position,
    edge_margin: u32,
    offset: u32,
) -> (Anchor, (i32, i32, i32, i32)) {
    let m = edge_margin as i32;
    let o = offset as i32;
    match pos {
        // Tuple order: (top, right, bottom, left) — matches iced_layershell NewLayerShellSettings.
        Position::TopRight => (Anchor::Top | Anchor::Right, (m + o, m, 0, 0)),
        Position::TopLeft => (Anchor::Top | Anchor::Left, (m + o, 0, 0, m)),
        Position::BottomRight => (Anchor::Bottom | Anchor::Right, (0, m, m + o, 0)),
        Position::BottomLeft => (Anchor::Bottom | Anchor::Left, (0, 0, m + o, m)),
    }
}

// -----------------------------------------------------------------------------
// View
// -----------------------------------------------------------------------------

fn popup_view(id: iced::window::Id, state: &PopupState) -> Element<'_, Message> {
    let accent = urgency_accent(state.urgency);

    let header = row![
        text(state.app_name.as_str())
            .size(11)
            .style(|_: &Theme| text::Style {
                color: Some(muted())
            }),
        Space::new().width(Length::Fill),
        button(text("×").size(16))
            .on_press(Message::Dismiss(id))
            .style(ghost_button_style)
            .padding([0, 6]),
    ]
    .align_y(alignment::Vertical::Center);

    let summary = text(state.summary.as_str()).size(14);

    // Summary + body are clickable and invoke the "default" action, which is
    // implicit for any sender that advertises the default-action capability
    // (most libnotify clients do). Action buttons and the × stay outside this
    // region so they keep their own on_press behavior.
    let mut default_stack = column![summary].spacing(4);
    if !state.body.is_empty() {
        default_stack =
            default_stack.push(text(state.body.as_str()).size(12).style(|_: &Theme| {
                text::Style {
                    color: Some(subtle()),
                }
            }));
    }
    let default_region: Element<'_, Message> = button(default_stack)
        .on_press(Message::ActionClicked(id, "default".to_string()))
        .style(default_region_style)
        .padding(0)
        .width(Length::Fill)
        .into();

    let mut stack = column![header, default_region].spacing(4);

    if !state.actions.is_empty() {
        let mut action_row = row![].spacing(6);
        for action in &state.actions {
            let key = action.id.clone();
            action_row = action_row.push(
                button(text(action.label.as_str()).size(12))
                    .on_press(Message::ActionClicked(id, key))
                    .style(move |theme: &Theme, status| action_button_style(theme, status, accent))
                    .padding([4, 10]),
            );
        }
        stack = stack.push(Space::new().height(Length::Fixed(2.0)));
        stack = stack.push(action_row);
    }

    let body = row![
        accent_bar(accent),
        Space::new().width(Length::Fixed(10.0)),
        stack,
    ]
    .height(Length::Fill);

    container(body)
        .padding(Padding {
            top: 10.0,
            right: 14.0,
            bottom: 10.0,
            left: 10.0,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(surface())),
            border: iced::Border {
                color: border_color(),
                width: 1.0,
                radius: 6.0.into(),
            },
            text_color: Some(foreground()),
            ..Default::default()
        })
        .into()
}

fn accent_bar(color: Color) -> Element<'static, Message> {
    container(Space::new().width(Length::Fixed(4.0)).height(Length::Fill))
        .width(Length::Fixed(4.0))
        .height(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(color)),
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn urgency_accent(u: Urgency) -> Color {
    match u {
        Urgency::Low => Color::from_rgb8(0x5B, 0x8F, 0xB9),
        Urgency::Normal => Color::from_rgb8(0x2F, 0x7D, 0xE1),
        Urgency::Critical => Color::from_rgb8(0xD8, 0x4C, 0x4C),
    }
}

fn surface() -> Color {
    Color::from_rgb8(0x1E, 0x22, 0x2A)
}
fn border_color() -> Color {
    Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.08)
}
fn foreground() -> Color {
    Color::from_rgb8(0xEC, 0xEF, 0xF4)
}
fn muted() -> Color {
    Color::from_rgba8(0xEC, 0xEF, 0xF4, 0.55)
}
fn subtle() -> Color {
    Color::from_rgba8(0xEC, 0xEF, 0xF4, 0.80)
}

fn ghost_button_style(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Some(iced::Background::Color(Color::from_rgba8(
            0xFF, 0xFF, 0xFF, 0.12,
        ))),
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: foreground(),
        border: iced::Border {
            radius: 4.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn default_region_style(_theme: &Theme, status: button::Status) -> button::Style {
    // Transparent button that still acts as a click target. A very faint
    // hover tint makes the body feel interactive without making it look like
    // a button.
    let bg = match status {
        button::Status::Hovered => Some(iced::Background::Color(Color::from_rgba8(
            0xFF, 0xFF, 0xFF, 0.04,
        ))),
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: foreground(),
        border: iced::Border {
            radius: 4.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn action_button_style(_theme: &Theme, status: button::Status, accent: Color) -> button::Style {
    let bg = match status {
        button::Status::Hovered => iced::Background::Color(lighten(accent, 0.10)),
        button::Status::Pressed => iced::Background::Color(darken(accent, 0.10)),
        _ => iced::Background::Color(accent),
    };
    button::Style {
        background: Some(bg),
        text_color: Color::WHITE,
        border: iced::Border {
            radius: 4.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn lighten(c: Color, amt: f32) -> Color {
    Color {
        r: (c.r + amt).min(1.0),
        g: (c.g + amt).min(1.0),
        b: (c.b + amt).min(1.0),
        a: c.a,
    }
}
fn darken(c: Color, amt: f32) -> Color {
    Color {
        r: (c.r - amt).max(0.0),
        g: (c.g - amt).max(0.0),
        b: (c.b - amt).max(0.0),
        a: c.a,
    }
}

// -----------------------------------------------------------------------------
// Layout estimation
// -----------------------------------------------------------------------------
//
// iced_layershell needs a fixed surface size at window creation time, so we
// estimate how tall the popup wants to be from its content, clamp it into the
// configured `[height, max_height]` range, and pre-truncate the body if it
// won't fit. Numbers below mirror the chrome assembled in `popup_view`.

const CONTAINER_PAD_V: u32 = 20; // top 10 + bottom 10
const CONTAINER_PAD_LEFT: u32 = 10;
const CONTAINER_PAD_RIGHT: u32 = 14;
const ACCENT_BAR_W: u32 = 4;
const ACCENT_GAP_W: u32 = 10;
const OUTER_SPACING: u32 = 4; // column![header, default_region].spacing(4)
const INNER_SPACING: u32 = 4; // column![summary, body].spacing(4)
const HEADER_H: u32 = 18; // text(11) + × button(text 16) row
const SUMMARY_H: u32 = 18; // text(14) at line height ~1.3
const BODY_LINE_H: u32 = 16; // text(12) at line height ~1.3
const ACTION_SPACER_H: u32 = 2;
const ACTION_ROW_H: u32 = 24; // button(text 12, padding [4,10]) ≈ 16 + 8
const AVG_GLYPH_W: f32 = 7.0; // size-12 average; deliberately overestimated

/// Decide the popup's pixel height and the body string to render.
///
/// Returns `(height, body)` where `height` is clamped into
/// `[min_height, max_height]` and `body` is `body_in` truncated with an
/// ellipsis if the chosen height can't fit every wrapped line.
fn compute_layout(
    body_in: &str,
    has_actions: bool,
    popup_width: u32,
    min_height: u32,
    max_height: u32,
) -> (u32, String) {
    let max_height = max_height.max(min_height);

    let body_region_w = popup_width
        .saturating_sub(CONTAINER_PAD_LEFT + CONTAINER_PAD_RIGHT + ACCENT_BAR_W + ACCENT_GAP_W);
    let chars_per_line = ((body_region_w as f32 / AVG_GLYPH_W).floor() as usize).max(20);

    let action_block = if has_actions {
        OUTER_SPACING + ACTION_SPACER_H + OUTER_SPACING + ACTION_ROW_H
    } else {
        0
    };
    let chrome = CONTAINER_PAD_V + HEADER_H + OUTER_SPACING + SUMMARY_H + action_block;

    let body_chars = body_in.chars().count();
    let body_lines_natural = if body_chars == 0 {
        0
    } else {
        body_in
            .split('\n')
            .map(|line| {
                let n = line.chars().count();
                if n == 0 {
                    1
                } else {
                    n.div_ceil(chars_per_line)
                }
            })
            .sum()
    };
    let body_block_natural = if body_lines_natural > 0 {
        INNER_SPACING + body_lines_natural as u32 * BODY_LINE_H
    } else {
        0
    };

    let chosen = (chrome + body_block_natural).clamp(min_height, max_height);

    // How many body lines actually fit in the chosen height?
    let body_budget = chosen.saturating_sub(chrome);
    let max_body_lines = if body_budget > INNER_SPACING {
        ((body_budget - INNER_SPACING) / BODY_LINE_H) as usize
    } else {
        0
    };

    let body = if body_chars == 0 || max_body_lines == 0 {
        String::new()
    } else if body_lines_natural <= max_body_lines {
        body_in.to_string()
    } else {
        // Reserve one slot for the ellipsis at the end of the last visible line.
        let cap = (max_body_lines * chars_per_line).saturating_sub(1).max(1);
        let mut out: String = body_in.chars().take(cap).collect();
        out.push('…');
        out
    };

    (chosen, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 380;
    const MIN: u32 = 120;
    const MAX: u32 = 240;

    #[test]
    fn empty_body_no_actions_uses_min_height() {
        let (h, body) = compute_layout("", false, W, MIN, MAX);
        assert_eq!(h, MIN);
        assert!(body.is_empty());
    }

    #[test]
    fn long_body_grows_up_to_max() {
        let body = "x".repeat(2000);
        let (h, out) = compute_layout(&body, true, W, MIN, MAX);
        assert_eq!(h, MAX);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() < body.chars().count());
    }

    #[test]
    fn short_body_with_actions_grows_above_min() {
        let body = "Short body that wraps once or twice across this popup width.";
        let (h, out) = compute_layout(body, true, W, MIN, MAX);
        assert!((MIN..=MAX).contains(&h));
        assert_eq!(out, body, "no truncation expected at this length");
    }

    #[test]
    fn min_height_is_floor_even_when_content_is_tiny() {
        let (h, _) = compute_layout("hi", false, W, MIN, MAX);
        assert_eq!(h, MIN);
    }

    #[test]
    fn max_below_min_is_clamped() {
        // Misconfiguration: max_height < height. Floor wins.
        let (h, _) = compute_layout("hi", false, W, 200, 100);
        assert_eq!(h, 200);
    }
}
