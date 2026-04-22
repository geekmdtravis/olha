mod config;
mod dbus;
mod model;
mod rules;

use std::time::{Duration, Instant};

use indexmap::IndexMap;
use iced::widget::{button, column, container, row, text, Space};
use iced::{alignment, Color, Element, Length, Padding, Subscription, Task, Theme};
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

use crate::config::{AppConfig, Position};
use crate::dbus::{connect, invoke_action, parse_signal_payload};
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
}

struct App {
    config: AppConfig,
    rules: PopupRules,
    /// Insertion order == visual stack order; index 0 is the newest popup,
    /// sitting closest to the anchor edge. Older popups slide toward the
    /// opposite edge.
    popups: IndexMap<iced::window::Id, PopupState>,
    pending_close: Option<iced::window::Id>,
}

impl App {
    fn new(config: AppConfig) -> Self {
        let rules = PopupRules::new(&config.popup.rules);
        Self {
            config,
            rules,
            popups: IndexMap::new(),
            pending_close: None,
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

    fn margin_for(&self, index: usize) -> (i32, i32, i32, i32) {
        let (_, margin) = anchor_and_margin(
            self.config.popup.position,
            self.config.popup.margin,
            self.config.popup.gap,
            self.config.popup.height,
            index,
        );
        margin
    }

    /// Emit `MarginChange` messages for popups starting at `start` so they
    /// settle into their current `IndexMap` positions. Call this after any
    /// insert/remove so the stack closes gaps and new popups push old ones
    /// down. `start = 1` after inserting at index 0 skips the new popup (it's
    /// already placed via `NewLayerShell`); `start = 0` after a removal
    /// repositions the whole stack.
    fn relayout_tasks(&self, start: usize) -> Vec<Task<Message>> {
        self.popups
            .keys()
            .enumerate()
            .skip(start)
            .map(|(i, id)| {
                Task::done(Message::MarginChange {
                    id: *id,
                    margin: self.margin_for(i),
                })
            })
            .collect()
    }

    fn new_layer_settings(&self, index: usize) -> NewLayerShellSettings {
        let (anchor, margin) = anchor_and_margin(
            self.config.popup.position,
            self.config.popup.margin,
            self.config.popup.gap,
            self.config.popup.height,
            index,
        );
        NewLayerShellSettings {
            size: Some((self.config.popup.width, self.config.popup.height)),
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
            Message::Dismiss(id) => self.remove_and_relayout(id),
            Message::ActionResult(Err(e)) => {
                tracing::warn!("invoke_action failed: {e}");
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
            _ => Task::none(),
        }
    }

    fn remove_and_relayout(&mut self, id: iced::window::Id) -> Task<Message> {
        if self.popups.shift_remove(&id).is_none() {
            return iced::window::close(id);
        }
        let mut tasks = self.relayout_tasks(0);
        tasks.push(iced::window::close(id));
        Task::batch(tasks)
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

        let state = PopupState {
            row_id: notif.row_id,
            urgency,
            app_name: notif.app_name,
            summary: notif.summary,
            body: notif.body,
            actions: notif.actions,
            expires_at,
        };

        // Newest popup goes at index 0 (closest to the anchor edge) and
        // shifts every existing popup one slot further away. NewLayerShell
        // below places the new window directly at the correct margin; the
        // relayout tasks reposition the now-shifted survivors.
        let id = iced::window::Id::unique();
        let settings = self.new_layer_settings(0);
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

    fn view(&self, id: iced::window::Id) -> Element<'_, Message> {
        match self.popups.get(&id) {
            Some(state) => popup_view(id, state),
            None => Space::new().width(Length::Fill).height(Length::Fill).into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let signals = Subscription::run(signal_stream);
        let tick = iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick);
        let close_events = iced::window::close_events().map(Message::WindowClosed);
        Subscription::batch([signals, tick, close_events])
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
                    let _ = sender.send(Message::Incoming(Box::new(notif))).await;
                }
            }
        }
        futures_util::future::pending::<()>().await;
        unreachable!()
    })
}

fn anchor_and_margin(
    pos: Position,
    edge_margin: u32,
    gap: u32,
    popup_height: u32,
    index: usize,
) -> (Anchor, (i32, i32, i32, i32)) {
    let offset = (popup_height as i32 + gap as i32) * index as i32;
    let m = edge_margin as i32;
    match pos {
        // Tuple order: (top, right, bottom, left) — matches iced_layershell NewLayerShellSettings.
        Position::TopRight => (Anchor::Top | Anchor::Right, (m + offset, m, 0, 0)),
        Position::TopLeft => (Anchor::Top | Anchor::Left, (m + offset, 0, 0, m)),
        Position::BottomRight => (Anchor::Bottom | Anchor::Right, (0, m, m + offset, 0)),
        Position::BottomLeft => (Anchor::Bottom | Anchor::Left, (0, 0, m + offset, m)),
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
            .style(|_: &Theme| text::Style { color: Some(muted()) }),
        Space::new().width(Length::Fill),
        button(text("×").size(16))
            .on_press(Message::Dismiss(id))
            .style(ghost_button_style)
            .padding([0, 6]),
    ]
    .align_y(alignment::Vertical::Center);

    let summary = text(state.summary.as_str()).size(14);

    let mut stack = column![header, summary].spacing(4);

    if !state.body.is_empty() {
        let body = truncate_body(&state.body, 220);
        stack = stack.push(
            text(body)
                .size(12)
                .style(|_: &Theme| text::Style { color: Some(subtle()) }),
        );
    }

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
        button::Status::Hovered => {
            Some(iced::Background::Color(Color::from_rgba8(
                0xFF, 0xFF, 0xFF, 0.12,
            )))
        }
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

fn truncate_body(body: &str, max: usize) -> String {
    if body.chars().count() <= max {
        body.to_string()
    } else {
        let mut out: String = body.chars().take(max).collect();
        out.push('…');
        out
    }
}
