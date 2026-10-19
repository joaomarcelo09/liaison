//! Everything on screen: the session sidebar, the chat, the composer.

use crate::{
    app::{App, Conn, MESSAGES_ID, Msg, ThemePref, Thread},
    style,
};
use chrono::{Datelike, Local, TimeZone};
use iced::{
    Alignment, Color, Element, Fill, Font, Shrink, Theme,
    font::Weight,
    widget::{
        Space, button, column, container, markdown, row, rule, scrollable, space, stack, text, text_editor,
        text_input, toggler, tooltip,
    },
};
use liaison_core::{
    Delivery, Direction, Harness, Message, MessageKind, NotifyLevel, QuestionStatus, ReconnectMode, Session,
    SessionStatus,
};

fn bold() -> Font {
    Font { weight: Weight::Semibold, ..style::ui_font() }
}

fn muted<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(content).style(|theme: &Theme| text::Style { color: Some(style::muted(theme)) })
}

fn local(ms: i64) -> Option<chrono::DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

fn clock(ms: i64) -> String {
    local(ms).map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

fn short_time(ms: i64) -> String {
    let (Some(t), now) = (local(ms), Local::now()) else { return String::new() };
    if t.date_naive() == now.date_naive() {
        t.format("%H:%M").to_string()
    } else if t.year() == now.year() {
        t.format("%b %d").to_string()
    } else {
        t.format("%Y-%m-%d").to_string()
    }
}

fn day_label(ms: i64) -> String {
    let (Some(t), now) = (local(ms), Local::now()) else { return String::new() };
    let days = (now.date_naive() - t.date_naive()).num_days();
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ if t.year() == now.year() => t.format("%A, %B %d").to_string(),
        _ => t.format("%B %d, %Y").to_string(),
    }
}

fn status_label(s: &Session) -> &'static str {
    match s.status {
        SessionStatus::Active => "working",
        SessionStatus::Idle => "idle",
        SessionStatus::Listening => "listening",
        SessionStatus::Disconnected => "offline",
        SessionStatus::Ended => "ended",
    }
}

fn dot<'a>(color: Color, size: f32) -> Element<'a, Msg> {
    container(Space::new().width(size).height(size)).style(style::dot(color)).into()
}

fn tip<'a>(content: impl Into<Element<'a, Msg>>, label: &'a str) -> Element<'a, Msg> {
    tooltip(
        content,
        container(text(label).size(style::fs(12))).padding([6, 10]).style(container::rounded_box),
        tooltip::Position::Bottom,
    )
    .gap(4)
    .into()
}

pub fn view(app: &App) -> Element<'_, Msg> {
    let theme = app.theme();
    let body = row![sidebar(app, &theme), rule::vertical(1).style(rule::weak), main(app, &theme)];
    let base: Element<'_, Msg> = if app.show_settings {
        stack![container(body).width(Fill).height(Fill), settings(app)].into()
    } else {
        container(body).width(Fill).height(Fill).into()
    };

    match &app.toast {
        Some(toast) => {
            let p = theme.extended_palette();
            let accent = if toast.error { p.danger.base.color } else { p.success.base.color };
            let card = container(
                row![
                    text(toast.text.clone()).size(style::fs(13)),
                    button(text("x").size(style::fs(13))).on_press(Msg::DismissToast).style(style::ghost).padding([0, 6])
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding([10, 14])
            .max_width(460)
            .style(style::banner(accent));
            stack![base, container(card).padding(18).width(Fill).height(Fill).align_right(Fill).align_bottom(Fill)]
                .into()
        }
        None => base,
    }
}

// ── settings ────────────────────────────────────────────────────────────────

fn settings(app: &App) -> Element<'_, Msg> {
    let stepper = |value: String, down: Msg, up: Msg, reset: Msg| {
        row![
            button(text("−").size(style::fs(14))).on_press(down).style(style::pill_button).padding([2, 12]),
            container(text(value).size(style::fs(14)).font(bold())).center_x(style::fs(14) * 4.0),
            button(text("+").size(style::fs(14))).on_press(up).style(style::pill_button).padding([2, 12]),
            button(text("Reset").size(style::fs(12))).on_press(reset).style(style::ghost).padding([2, 8]),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
    };
    let theme_button = |label: &'static str, pref: ThemePref| {
        button(text(label).size(style::fs(13)))
            .on_press(Msg::SetTheme(pref))
            .style(if app.theme_pref == pref { style::primary_button } else { style::pill_button })
            .padding([4, 12])
    };
    let line = |label: &'static str, control: Element<'static, Msg>| {
        row![text(label).size(style::fs(14)).width(style::fs(14) * 7.0), control].spacing(12).align_y(Alignment::Center)
    };
    let card = container(
        column![
            row![
                text("Settings").size(style::fs(18)).font(bold()),
                space::horizontal(),
                button(text("Close").size(style::fs(13))).on_press(Msg::ToggleSettings).style(style::ghost).padding([4, 8]),
            ]
            .align_y(Alignment::Center),
            line(
                "Theme",
                row![
                    theme_button("Auto", ThemePref::System),
                    theme_button("Dark", ThemePref::Dark),
                    theme_button("Light", ThemePref::Light)
                ]
                .spacing(6)
                .into()
            ),
            line(
                "Zoom",
                stepper(
                    format!("{:.0}%", app.zoom * 100.0),
                    Msg::Zoom(crate::app::Step::Down),
                    Msg::Zoom(crate::app::Step::Up),
                    Msg::Zoom(crate::app::Step::Reset)
                )
                .into()
            ),
            line(
                "Text size",
                stepper(
                    app.text_size.to_string(),
                    Msg::TextSize(crate::app::Step::Down),
                    Msg::TextSize(crate::app::Step::Up),
                    Msg::TextSize(crate::app::Step::Reset)
                )
                .into()
            ),
            muted("Zoom scales the whole window (Ctrl + / Ctrl − / Ctrl 0). Text size changes only the letters.")
                .size(style::fs(12)),
        ]
        .spacing(16),
    )
    .padding(22)
    .max_width(style::fs(14) * 34.0)
    .style(style::card(Color::TRANSPARENT));
    let backdrop = iced::widget::mouse_area(
        container(Space::new()).width(Fill).height(Fill).style(|_: &Theme| container::Style {
            background: Some(Color { a: 0.45, ..Color::BLACK }.into()),
            ..Default::default()
        }),
    )
    .on_press(Msg::ToggleSettings);
    stack![backdrop, container(iced::widget::opaque(card)).center(Fill)].into()
}

// ── sidebar ─────────────────────────────────────────────────────────────────

fn sidebar<'a>(app: &'a App, theme: &Theme) -> Element<'a, Msg> {
    let p = theme.extended_palette();
    let (conn_color, conn_label) = match &app.conn {
        Conn::Connected => (p.success.base.color, "Connected to the Liaison hub".to_string()),
        Conn::Connecting => (p.warning.base.color, "Connecting to the hub…".to_string()),
        Conn::Offline(e) => (p.danger.base.color, format!("Hub not running: {e}")),
    };
    let header = row![
        text("Liaison").size(style::fs(20)).font(bold()),
        tooltip(
            dot(conn_color, 8.0),
            container(text(conn_label).size(style::fs(12))).padding([6, 10]).style(container::rounded_box),
            tooltip::Position::Bottom
        ),
        space::horizontal(),
        tip(
            button(text("Settings").size(style::fs(12)))
                .on_press(Msg::ToggleSettings)
                .style(if app.show_settings { style::pill_button } else { style::ghost })
                .padding([4, 8]),
            "Theme, zoom and text size"
        ),
        tip(
            button(text("Archived").size(style::fs(12)))
                .on_press(Msg::ToggleArchived)
                .style(if app.show_archived { style::pill_button } else { style::ghost })
                .padding([4, 8]),
            if app.show_archived { "Hide archived chats" } else { "Show archived chats" }
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let search = text_input("Search chats", &app.search)
        .on_input(Msg::Search)
        .padding([7, 10])
        .size(style::fs(13))
        .style(style::input);

    let query = app.search.trim().to_lowercase();
    let matches = |s: &&Session| {
        query.is_empty()
            || s.display_name().to_lowercase().contains(&query)
            || s.cwd.as_deref().unwrap_or("").to_lowercase().contains(&query)
            || s.harness.label().to_lowercase().contains(&query)
    };
    let visible: Vec<&Session> = app.sessions.iter().filter(matches).collect();

    let groups: [(&str, Box<dyn Fn(&Session) -> bool>); 4] = [
        ("ONLINE", Box::new(|s: &Session| !s.archived && s.status.is_connected())),
        ("OFFLINE", Box::new(|s: &Session| !s.archived && s.status == SessionStatus::Disconnected)),
        ("ENDED", Box::new(|s: &Session| !s.archived && s.status == SessionStatus::Ended)),
        ("ARCHIVED", Box::new(|s: &Session| s.archived)),
    ];
    let mut list = column![].spacing(2);
    let mut any = false;
    for (label, include) in groups.iter() {
        if *label == "ARCHIVED" && !app.show_archived {
            continue;
        }
        let items: Vec<&&Session> = visible.iter().filter(|s| include(s)).collect();
        if items.is_empty() {
            continue;
        }
        any = true;
        list = list.push(
            container(muted(format!("{label} · {}", items.len())).size(style::fs(11)).font(bold())).padding([10, 8]),
        );
        for s in items {
            list = list.push(session_item(s, app.selected.as_deref() == Some(s.id.as_str()), theme));
        }
    }
    if !any {
        let hint = if app.sessions.is_empty() {
            "No agent sessions yet.\n\nRun `liaison connect` in a terminal, then start Claude Code or opencode."
        } else {
            "Nothing matches."
        };
        list = list.push(container(muted(hint).size(style::fs(13))).padding(12));
    }

    container(
        column![
            container(header).padding([14, 14]),
            container(search).padding([0, 12]),
            scrollable(container(list).padding([6, 8])).height(Fill),
        ]
        .spacing(8),
    )
    // Grows with the text size (310px at the default 16px).
    .width(style::fs(14) * 310.0 / f32::from(liaison_core::config::Desktop::DEFAULT_TEXT_SIZE))
    .height(Fill)
    .style(style::sidebar)
    .into()
}

fn session_item<'a>(s: &'a Session, selected: bool, theme: &Theme) -> Element<'a, Msg> {
    let p = theme.extended_palette();
    let time = s.last_message_at.unwrap_or(s.updated_at);
    let mut meta = row![dot(style::status_color(theme, s.status), 7.0), muted(s.harness.label()).size(style::fs(11))]
        .spacing(6)
        .align_y(Alignment::Center);
    if let Some(project) = s.project().filter(|p| Some(p.as_str()) != s.title.as_deref()) {
        meta = meta.push(muted(format!("· {project}")).size(style::fs(11)));
    }
    let mut meta_row = row![meta, space::horizontal()].align_y(Alignment::Center);
    if s.queued > 0 {
        meta_row = meta_row.push(tip(muted(format!("{} queued", s.queued)).size(style::fs(11)), "Your messages waiting for the agent"));
    }
    if s.unread > 0 {
        meta_row = meta_row.push(
            container(text(s.unread.to_string()).size(style::fs(11)).font(bold()))
                .padding([1, 7])
                .style(style::badge(p.primary.base.color)),
        );
    }
    let preview = s.status_text.clone().or_else(|| s.last_message.clone()).unwrap_or_default();
    let name_font = if s.unread > 0 { bold() } else { style::ui_font() };
    let content = column![
        row![
            text(s.display_name()).size(style::fs(14)).font(name_font).wrapping(text::Wrapping::None),
            space::horizontal(),
            muted(short_time(time)).size(style::fs(11))
        ]
        .spacing(6)
        .align_y(Alignment::Center),
        meta_row.spacing(6),
        muted(preview).size(style::fs(12)).wrapping(text::Wrapping::None),
    ]
    .spacing(3);
    button(container(content).clip(true))
        .on_press(Msg::Select(s.id.clone()))
        .style(style::session_item(selected))
        .width(Fill)
        .padding([8, 10])
        .into()
}

// ── main area ───────────────────────────────────────────────────────────────

fn main<'a>(app: &'a App, theme: &Theme) -> Element<'a, Msg> {
    let Some(session) = app.selected_session() else { return welcome(app) };
    let thread = app.threads.get(&session.id);
    column![
        chat_header(app, session, theme),
        rule::horizontal(1).style(rule::weak),
        messages(app, session, thread, theme),
        composer(app, session),
    ]
    .width(Fill)
    .height(Fill)
    .into()
}

fn welcome(app: &App) -> Element<'_, Msg> {
    let status = match &app.conn {
        Conn::Connected => format!(
            "{} session{} · {} online",
            app.sessions.len(),
            if app.sessions.len() == 1 { "" } else { "s" },
            app.sessions.iter().filter(|s| s.status.is_connected()).count()
        ),
        Conn::Connecting => "Connecting to the hub…".into(),
        Conn::Offline(_) => "The hub is not running. Start it with: systemctl --user enable --now liaison-hub".into(),
    };
    let steps = column![
        muted("1.  In a terminal:  liaison connect").size(style::fs(14)),
        muted("2.  Start Claude Code or opencode as usual").size(style::fs(14)),
        muted("3.  Each session gets its own chat here").size(style::fs(14)),
    ]
    .spacing(6);
    container(
        column![
            text("Liaison").size(style::fs(34)).font(bold()),
            muted("Your AI agents, one chat per session.").size(style::fs(16)),
            Space::new().height(12),
            steps,
            Space::new().height(12),
            muted(status).size(style::fs(13)),
        ]
        .spacing(6)
        .align_x(Alignment::Center),
    )
    .center(Fill)
    .into()
}

fn chat_header<'a>(app: &'a App, s: &'a Session, theme: &Theme) -> Element<'a, Msg> {
    let title: Element<'a, Msg> = match &app.rename {
        Some(value) => row![
            text_input("Session title", value)
                .id("rename")
                .on_input(Msg::RenameInput)
                .on_submit(Msg::RenameSave)
                .size(style::fs(16))
                .padding([5, 8])
                .width(320)
                .style(style::input),
            button(text("Save").size(style::fs(13))).on_press(Msg::RenameSave).style(style::primary_button).padding([6, 12]),
            button(text("Cancel").size(style::fs(13))).on_press(Msg::RenameCancel).style(style::ghost).padding([6, 10]),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        None => tip(
            button(text(s.display_name()).size(style::fs(18)).font(bold())).on_press(Msg::RenameStart).style(style::ghost).padding([2, 6]),
            "Click to rename",
        ),
    };

    let mut subtitle = row![
        dot(style::status_color(theme, s.status), 8.0),
        text(status_label(s)).size(style::fs(12)),
        muted("·").size(style::fs(12)),
        muted(s.harness.label()).size(style::fs(12)),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some(cwd) = &s.cwd {
        subtitle = subtitle
            .push(muted("·").size(style::fs(12)))
            .push(muted(cwd.as_str()).size(style::fs(12)).wrapping(text::Wrapping::None));
    }
    if let Some(status_text) = &s.status_text {
        subtitle = subtitle.push(
            container(text(status_text.as_str()).size(style::fs(11)).wrapping(text::Wrapping::None)).padding([2, 8]).style(style::pill),
        );
    }

    let listen = tip(
        toggler(s.listen).label("Listen").text_size(style::fs(13)).size(16).on_toggle_maybe(
            (s.status != SessionStatus::Ended && s.harness != Harness::Custom).then_some(Msg::SetListen),
        ),
        "When its turn ends, the agent keeps waiting for your messages instead of going idle",
    );
    let mirror = tip(
        toggler(s.mirror).label("All replies").text_size(style::fs(13)).size(16).on_toggle(Msg::SetMirror),
        "Show every final reply here, not only replies to messages you sent from Liaison",
    );
    let archive = button(text(if s.archived { "Unarchive" } else { "Archive" }).size(style::fs(13)))
        .on_press(Msg::SetArchived(!s.archived))
        .style(style::ghost)
        .padding([6, 10]);
    let delete: Element<'a, Msg> = if app.confirm_delete {
        row![
            muted("Delete this conversation?").size(style::fs(13)),
            button(text("Delete").size(style::fs(13))).on_press(Msg::DeleteConfirm).style(style::danger_button).padding([6, 12]),
            button(text("Cancel").size(style::fs(13))).on_press(Msg::DeleteCancel).style(style::ghost).padding([6, 10]),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
    } else {
        button(text("Delete").size(style::fs(13))).on_press(Msg::DeleteAsk).style(style::ghost).padding([6, 10]).into()
    };

    let controls = row![listen, mirror, archive, delete].spacing(14).align_y(Alignment::Center);
    // The title side takes what the controls leave and clips (long paths, large text).
    let top = row![column![title, subtitle].spacing(4).width(Fill).clip(true), controls]
        .spacing(16)
        .align_y(Alignment::Center);

    let mut col = column![top].spacing(10);
    if let Some(banner) = status_banner(app, s, theme) {
        col = col.push(banner);
    }
    container(col).padding([12, 18]).width(Fill).style(style::header).into()
}

fn status_banner<'a>(app: &'a App, s: &'a Session, theme: &Theme) -> Option<Element<'a, Msg>> {
    let p = theme.extended_palette();
    let (accent, message, actions): (Color, String, Option<Element<'a, Msg>>) = match s.status {
        SessionStatus::Disconnected => {
            let reason = s.ended_reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default();
            let queued = if s.queued > 0 {
                format!(" {} message{} waiting.", s.queued, if s.queued == 1 { "" } else { "s" })
            } else {
                String::new()
            };
            let actions = s.harness.can_reconnect().then(|| {
                let enabled = !app.reconnecting;
                row![
                    tip(
                        button(text(if app.reconnecting { "Starting…" } else { "Reconnect" }).size(style::fs(13)))
                            .on_press_maybe(enabled.then_some(Msg::Reconnect(ReconnectMode::Background)))
                            .style(style::primary_button)
                            .padding([6, 12]),
                        "Resume the session in the background; it stays reachable from Liaison",
                    ),
                    tip(
                        button(text("Open in terminal").size(style::fs(13)))
                            .on_press_maybe(enabled.then_some(Msg::Reconnect(ReconnectMode::Terminal)))
                            .style(style::pill_button)
                            .padding([6, 12]),
                        "Resume the session in a new terminal window",
                    ),
                ]
                .spacing(8)
                .into()
            });
            (
                p.warning.base.color,
                format!("The agent is offline{reason}. Messages you send are kept and delivered when it reconnects.{queued}"),
                actions,
            )
        }
        SessionStatus::Ended => (
            style::muted(theme),
            format!(
                "This session ended ({}). Its context is gone, so the conversation is read-only.",
                s.ended_reason.as_deref().unwrap_or("closed")
            ),
            None,
        ),
        SessionStatus::Listening => {
            (p.success.base.color, "Listening — your messages go straight to the agent.".into(), None)
        }
        SessionStatus::Idle if s.queued > 0 && s.harness == Harness::ClaudeCode => (
            p.primary.base.color,
            "Idle — your messages are delivered when the agent's next turn starts. Turn on Listen to keep it waiting for you."
                .into(),
            None,
        ),
        _ => return None,
    };
    let mut content = row![text(message).size(style::fs(13)).width(Fill)].spacing(12).align_y(Alignment::Center);
    if let Some(actions) = actions {
        content = content.push(actions);
    }
    Some(container(content).padding([8, 12]).width(Fill).style(style::banner(accent)).into())
}

fn messages<'a>(app: &'a App, s: &'a Session, thread: Option<&'a Thread>, theme: &Theme) -> Element<'a, Msg> {
    let Some(thread) = thread.filter(|t| t.loaded) else {
        return container(muted("Loading…")).center(Fill).into();
    };
    let mut md_style = markdown::Style::from_palette(theme.palette());
    md_style.font = style::ui_font();
    let settings = markdown::Settings::with_text_size(style::fs(14), md_style);

    let mut col = column![].spacing(10).padding([16, 22]).width(Fill);
    if thread.has_more {
        col = col.push(
            container(button(text("Load older messages").size(style::fs(12))).on_press(Msg::LoadOlder).style(style::pill_button))
                .center_x(Fill),
        );
    }
    if thread.messages.is_empty() {
        col = col.push(container(muted("No messages yet.").size(style::fs(13))).center_x(Fill).padding(30));
    }
    let mut last_day = None;
    for m in &thread.messages {
        let day = local(m.created_at).map(|t| t.date_naive());
        if day != last_day {
            last_day = day;
            col = col.push(
                container(container(muted(day_label(m.created_at)).size(style::fs(11))).padding([2, 10]).style(style::pill))
                    .center_x(Fill)
                    .padding([6, 0]),
            );
        }
        col = col.push(message_view(app, s, m, thread.markdown.get(&m.id), settings, theme));
    }
    scrollable(col).id(MESSAGES_ID).on_scroll(Msg::Scrolled).height(Fill).width(Fill).into()
}

fn md<'a>(items: Option<&'a Vec<markdown::Item>>, body: &'a str, settings: markdown::Settings) -> Element<'a, Msg> {
    match items {
        Some(items) => markdown::view(items, settings).map(Msg::Link),
        None => text(body).size(style::fs(14)).into(),
    }
}

fn copy_button<'a>(body: &str) -> Element<'a, Msg> {
    tip(
        button(muted("Copy").size(style::fs(11))).on_press(Msg::Copy(body.to_string())).style(style::ghost).padding([0, 6]),
        "Copy the message text",
    )
}

fn message_view<'a>(
    app: &'a App,
    s: &'a Session,
    m: &'a Message,
    items: Option<&'a Vec<markdown::Item>>,
    settings: markdown::Settings,
    theme: &Theme,
) -> Element<'a, Msg> {
    let p = theme.extended_palette();
    match (m.direction, m.kind) {
        (Direction::System, _) | (_, MessageKind::Event) => container(
            muted(format!("{} · {}", m.body, clock(m.created_at))).size(style::fs(12)),
        )
        .center_x(Fill)
        .into(),

        (Direction::ToAgent, _) => {
            let (status, failed) = match m.delivery {
                Delivery::Queued => ("queued", false),
                Delivery::Delivered | Delivery::Read => ("delivered", false),
                Delivery::Undeliverable => ("not delivered", true),
            };
            let footer = text(format!("{} · {status}", clock(m.created_at))).size(style::fs(11)).style(move |theme: &Theme| {
                let p = theme.extended_palette();
                text::Style {
                    color: Some(if failed { p.danger.base.color } else { p.primary.base.text.scale_alpha(0.75) }),
                }
            });
            let bubble = container(column![text(m.body.as_str()).size(style::fs(14)), footer].spacing(4))
                .padding([8, 12])
                .max_width(560)
                .style(if failed { style::bubble_failed } else { style::bubble_user });
            row![space::horizontal(), bubble].into()
        }

        (Direction::FromAgent, MessageKind::Question | MessageKind::Permission) => question_view(app, s, m, items, settings, theme),

        (Direction::FromAgent, MessageKind::Notify) => {
            let level = m.meta_str("level").and_then(|l| l.parse().ok()).unwrap_or(NotifyLevel::Info);
            let accent = style::level_color(theme, level);
            let footer = row![muted(format!("{} · notification", clock(m.created_at))).size(style::fs(11)), space::horizontal(), copy_button(&m.body)]
                .align_y(Alignment::Center);
            let card = container(
                row![dot(accent, 10.0), column![md(items, &m.body, settings), footer].spacing(6).width(Fill)]
                    .spacing(10)
                    .align_y(Alignment::Start),
            )
            .padding([10, 14])
            .max_width(680)
            .style(style::card(accent));
            row![card, space::horizontal()].into()
        }

        (Direction::FromAgent, _) => {
            let mut footer = row![muted(clock(m.created_at)).size(style::fs(11))].spacing(6).align_y(Alignment::Center);
            if m.kind == MessageKind::Reply {
                footer = footer.push(tip(
                    container(muted("reply").size(style::fs(10))).padding([0, 6]).style(style::pill),
                    "The agent's final answer, mirrored from its session",
                ));
            }
            footer = footer.push(space::horizontal()).push(copy_button(&m.body));
            let unread_mark: Element<'a, Msg> = if m.read_at.is_none() {
                dot(p.primary.base.color, 6.0)
            } else {
                Space::new().width(6).into()
            };
            let bubble = container(column![md(items, &m.body, settings), footer].spacing(6))
                .padding([10, 14])
                .max_width(680)
                .style(style::bubble_agent);
            row![bubble, unread_mark, space::horizontal()].spacing(6).align_y(Alignment::Center).into()
        }
    }
}

fn question_view<'a>(
    app: &'a App,
    s: &'a Session,
    m: &'a Message,
    items: Option<&'a Vec<markdown::Item>>,
    settings: markdown::Settings,
    theme: &Theme,
) -> Element<'a, Msg> {
    let p = theme.extended_palette();
    let permission = m.kind == MessageKind::Permission;
    let accent = if permission { p.warning.base.color } else { p.primary.base.color };
    let label = if permission { "Permission request" } else { "Question" };

    let mut col = column![
        text(label).size(style::fs(12)).font(bold()).style(move |_: &Theme| text::Style { color: Some(accent) }),
        md(items, &m.body, settings),
    ]
    .spacing(8);

    match &m.question {
        Some(q) if q.status == QuestionStatus::Pending && s.status != SessionStatus::Ended => {
            let mut buttons = row![].spacing(8);
            for option in &q.options {
                let style = match option.as_str() {
                    "Allow" | "Yes" => style::success_button,
                    "Deny" | "No" => style::danger_button,
                    _ => style::pill_button,
                };
                buttons = buttons.push(
                    button(text(option.as_str()).size(style::fs(13)))
                        .on_press(Msg::Answer { question: q.id.clone(), answer: option.clone() })
                        .style(style)
                        .padding([6, 14]),
                );
            }
            col = col.push(buttons);
            if !permission {
                let draft = app.answers.get(&q.id).map(String::as_str).unwrap_or("");
                let qid = q.id.clone();
                let qid2 = q.id.clone();
                col = col.push(
                    row![
                        text_input("Type an answer…", draft)
                            .on_input(move |text| Msg::AnswerDraft { question: qid.clone(), text })
                            .on_submit(Msg::Answer { question: qid2.clone(), answer: draft.to_string() })
                            .size(style::fs(13))
                            .padding([6, 10])
                            .style(style::input),
                        button(text("Answer").size(style::fs(13)))
                            .on_press_maybe((!draft.trim().is_empty()).then(|| Msg::Answer {
                                question: q.id.clone(),
                                answer: draft.to_string(),
                            }))
                            .style(style::primary_button)
                            .padding([6, 12]),
                    ]
                    .spacing(8),
                );
            }
            if let Some(expires) = q.expires_at {
                col = col.push(muted(format!("Waiting for you until {}", clock(expires))).size(style::fs(11)));
            }
        }
        Some(q) if q.status == QuestionStatus::Answered => {
            col = col.push(
                text(format!("Answered: {}", q.answer.as_deref().unwrap_or("")))
                    .size(style::fs(13))
                    .style(|theme: &Theme| text::Style { color: Some(theme.extended_palette().success.base.color) }),
            );
        }
        Some(_) => {
            let note = if permission {
                "No answer in time — the agent asked in its terminal instead."
            } else {
                "No answer — the question expired."
            };
            col = col.push(muted(note).size(style::fs(12)));
        }
        None => {}
    }
    col = col.push(muted(clock(m.created_at)).size(style::fs(11)));

    let card = container(col).padding([12, 14]).max_width(680).style(style::card(accent));
    row![card, space::horizontal()].into()
}

fn composer<'a>(app: &'a App, s: &'a Session) -> Element<'a, Msg> {
    if s.status == SessionStatus::Ended {
        return container(muted("This conversation is read-only.").size(style::fs(13)))
            .padding([14, 20])
            .width(Fill)
            .style(style::composer_box)
            .into();
    }
    let placeholder = match s.status {
        SessionStatus::Active => "Message the agent — delivered after its current step…",
        SessionStatus::Idle if s.harness == Harness::ClaudeCode => "Message — delivered on the agent's next turn…",
        SessionStatus::Disconnected => "Message — kept until the agent reconnects…",
        _ => "Message the agent…",
    };
    let can_send = !app.composer.text().trim().is_empty();
    let editor = text_editor(&app.composer)
        .id("composer")
        .placeholder(placeholder)
        .on_action(Msg::Composer)
        .key_binding(|press| {
            use iced::keyboard::{Key, key::Named};
            if let Some(step) = crate::app::zoom_shortcut(&press.key, press.modifiers) {
                return Some(text_editor::Binding::Custom(Msg::Zoom(step)));
            }
            if matches!(press.key, Key::Named(Named::Enter)) && !press.modifiers.shift()
                && let text_editor::Status::Focused { .. } = press.status {
                    return Some(text_editor::Binding::Custom(Msg::Send));
                }
            text_editor::Binding::from_key_press(press)
        })
        .padding([10, 12])
        .size(style::fs(14))
        .min_height(44)
        .max_height(200)
        .height(Shrink)
        .style(style::editor);
    let send = button(text("Send").size(style::fs(14)).font(bold()))
        .on_press_maybe(can_send.then_some(Msg::Send))
        .style(style::primary_button)
        .padding([11, 20]);
    container(
        column![
            row![editor, send].spacing(10).align_y(Alignment::End),
            muted("Enter to send · Shift+Enter for a new line").size(style::fs(11)),
        ]
        .spacing(6),
    )
    .padding([12, 18])
    .width(Fill)
    .style(style::composer_box)
    .into()
}
