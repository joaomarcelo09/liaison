//! Application state and update logic (Elm style: `Msg` in, state change + `Task` out).

use crate::hub::{self, Feed};
use iced::{
    Subscription, Task, Theme,
    theme,
    widget::{markdown, operation, scrollable, text_editor},
    keyboard, window,
};
use liaison_core::{
    Direction, Event, Message, MessageKind, Question, ReconnectMode, Session, SessionStatus, api::PatchSessionRequest,
    config::{Config, Desktop as DesktopConfig},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub const MESSAGES_ID: &str = "messages";
const PAGE: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub enum Conn {
    Connecting,
    Connected,
    Offline(String),
}

/// The messages of one session, with their parsed Markdown.
#[derive(Default)]
pub struct Thread {
    pub messages: Vec<Message>,
    pub markdown: HashMap<i64, Vec<markdown::Item>>,
    pub has_more: bool,
    pub loaded: bool,
}

impl Thread {
    fn upsert(&mut self, message: Message) {
        self.markdown.insert(message.id, markdown::parse(&message.body).collect());
        match self.messages.binary_search_by_key(&message.id, |m| m.id) {
            Ok(i) => self.messages[i] = message,
            Err(i) => self.messages.insert(i, message),
        }
    }

    fn set_question(&mut self, question: Question) {
        if let Some(m) = self.messages.iter_mut().find(|m| m.id == question.message_id) {
            m.question = Some(question);
        }
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub since: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePref {
    System,
    Light,
    Dark,
}

pub struct App {
    pub conn: Conn,
    pub sessions: Vec<Session>,
    pub selected: Option<String>,
    pub threads: HashMap<String, Thread>,
    pub composer: text_editor::Content,
    /// Unsent text per session, kept while switching chats.
    pub drafts: HashMap<String, String>,
    /// Free-text answers being typed for questions.
    pub answers: HashMap<String, String>,
    pub search: String,
    pub show_archived: bool,
    pub rename: Option<String>,
    pub confirm_delete: bool,
    pub reconnecting: bool,
    pub toast: Option<Toast>,
    pub focused: bool,
    pub window: Option<window::Id>,
    pub at_bottom: bool,
    pub theme_pref: ThemePref,
    pub system_mode: theme::Mode,
    /// Whole-window scale (1.0 = 100%).
    pub zoom: f32,
    /// Chat text size in pixels.
    pub text_size: u16,
    pub show_settings: bool,
    /// Highest message id seen, so only new messages mark the window urgent.
    last_message_id: i64,
    start_session: Option<String>,
}

/// A step for the zoom and text-size controls.
#[derive(Debug, Clone, Copy)]
pub enum Step {
    Up,
    Down,
    Reset,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Feed(Feed),
    SessionsLoaded(Result<Vec<Session>, String>),
    Select(String),
    ThreadLoaded { session: String, older: bool, result: Result<Vec<Message>, String> },
    LoadOlder,
    Scrolled(scrollable::Viewport),
    Composer(text_editor::Action),
    Send,
    Sent(Result<Message, String>),
    AnswerDraft { question: String, text: String },
    Answer { question: String, answer: String },
    Answered(Result<Question, String>),
    SetListen(bool),
    SetMirror(bool),
    SetArchived(bool),
    Patched(Result<Session, String>),
    Reconnect(ReconnectMode),
    Reconnected(Result<String, String>),
    RenameStart,
    RenameInput(String),
    RenameSave,
    RenameCancel,
    DeleteAsk,
    DeleteCancel,
    DeleteConfirm,
    Deleted(Result<(), String>),
    Search(String),
    ToggleArchived,
    Copy(String),
    Link(markdown::Uri),
    Window(window::Id, window::Event),
    SystemTheme(theme::Mode),
    SetTheme(ThemePref),
    ToggleSettings,
    Zoom(Step),
    TextSize(Step),
    Key(keyboard::Event),
    DismissToast,
    Tick,
    Done(Result<(), String>),
}

impl App {
    pub fn new(start_session: Option<String>) -> (App, Task<Msg>) {
        let desktop = Config::load().desktop;
        let text_size = desktop.text_size.clamp(TEXT_SIZE_RANGE.0, TEXT_SIZE_RANGE.1);
        crate::style::set_text_size(text_size);
        let theme_pref = match desktop.theme.as_str() {
            "light" => ThemePref::Light,
            "dark" => ThemePref::Dark,
            _ => ThemePref::System,
        };
        let app = App {
            conn: Conn::Connecting,
            sessions: Vec::new(),
            selected: None,
            threads: HashMap::new(),
            composer: text_editor::Content::new(),
            drafts: HashMap::new(),
            answers: HashMap::new(),
            search: String::new(),
            show_archived: false,
            rename: None,
            confirm_delete: false,
            reconnecting: false,
            toast: None,
            focused: true,
            window: None,
            at_bottom: true,
            theme_pref,
            system_mode: theme::Mode::Dark,
            zoom: clamp_zoom(desktop.zoom),
            text_size,
            show_settings: false,
            last_message_id: 0,
            start_session,
        };
        let tasks = Task::batch([
            iced::system::theme().map(Msg::SystemTheme),
            window::latest().and_then(window::gain_focus),
        ]);
        (app, tasks)
    }

    pub fn title(&self) -> String {
        let unread: i64 = self.sessions.iter().filter(|s| !s.archived).map(|s| s.unread).sum();
        if unread > 0 { format!("({unread}) Liaison") } else { "Liaison".into() }
    }

    pub fn theme(&self) -> Theme {
        let dark = match self.theme_pref {
            ThemePref::Dark => true,
            ThemePref::Light => false,
            ThemePref::System => self.system_mode != theme::Mode::Light,
        };
        if dark { crate::style::dark() } else { crate::style::light() }
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let mut subs = vec![
            Subscription::run(hub::feed).map(Msg::Feed),
            window::events().map(|(id, e)| Msg::Window(id, e)),
            iced::system::theme_changes().map(Msg::SystemTheme),
            keyboard::listen().map(Msg::Key),
        ];
        if self.toast.is_some() {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Msg::Tick));
        }
        Subscription::batch(subs)
    }

    pub fn scale_factor(&self) -> f32 {
        self.zoom
    }

    pub fn selected_session(&self) -> Option<&Session> {
        let id = self.selected.as_deref()?;
        self.sessions.iter().find(|s| s.id == id)
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast { text: text.into(), error, since: Instant::now() });
    }

    fn sort_sessions(&mut self) {
        self.sessions.sort_by_key(|s| std::cmp::Reverse(s.last_message_at.unwrap_or(s.updated_at)));
    }

    fn upsert_session(&mut self, session: Session) {
        match self.sessions.iter_mut().find(|s| s.id == session.id) {
            Some(s) => *s = session,
            None => self.sessions.push(session),
        }
        self.sort_sessions();
    }

    fn load_thread(&self, session: String) -> Task<Msg> {
        let id = session.clone();
        Task::perform(hub::messages(id, None), move |result| Msg::ThreadLoaded {
            session: session.clone(),
            older: false,
            result,
        })
    }

    fn mark_read_if_visible(&mut self, session_id: &str) -> Task<Msg> {
        let visible = self.focused && self.selected.as_deref() == Some(session_id);
        let unread = self.sessions.iter().any(|s| s.id == session_id && s.unread > 0);
        if !(visible && unread) {
            return Task::none();
        }
        // The hub only reports the new counters; mark the visible messages read here too.
        let now = liaison_core::now_ms();
        if let Some(thread) = self.threads.get_mut(session_id) {
            for m in thread.messages.iter_mut().filter(|m| m.direction == Direction::FromAgent && m.read_at.is_none()) {
                m.read_at = Some(now);
            }
        }
        Task::perform(hub::mark_read(session_id.to_string()), Msg::Done)
    }

    fn presence(&self) -> Task<Msg> {
        Task::perform(hub::presence(self.focused, self.selected.clone()), Msg::Done)
    }

    fn snap(&mut self) -> Task<Msg> {
        self.at_bottom = true;
        operation::snap_to_end(MESSAGES_ID)
    }

    fn select(&mut self, id: String) -> Task<Msg> {
        if self.selected.as_deref() == Some(id.as_str()) {
            return Task::none();
        }
        if let Some(prev) = self.selected.take() {
            self.drafts.insert(prev, self.composer.text());
        }
        let draft = self.drafts.remove(&id).unwrap_or_default();
        self.composer = text_editor::Content::with_text(draft.trim_end_matches('\n'));
        self.selected = Some(id.clone());
        self.rename = None;
        self.confirm_delete = false;
        Task::batch([
            self.load_thread(id.clone()),
            self.mark_read_if_visible(&id),
            self.presence(),
            operation::focus("composer"),
        ])
    }

    fn patch(&self, req: PatchSessionRequest) -> Task<Msg> {
        match &self.selected {
            Some(id) => Task::perform(hub::patch(id.clone(), req), Msg::Patched),
            None => Task::none(),
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Feed(Feed::Connected) => {
                self.conn = Conn::Connected;
                let mut tasks = vec![Task::perform(hub::sessions(), Msg::SessionsLoaded), self.presence()];
                if let Some(id) = &self.selected {
                    tasks.push(self.load_thread(id.clone()));
                }
                Task::batch(tasks)
            }
            Msg::Feed(Feed::Disconnected(reason)) => {
                self.conn = Conn::Offline(reason);
                Task::none()
            }
            Msg::Feed(Feed::Event(event)) => self.on_event(*event),

            Msg::SessionsLoaded(Ok(sessions)) => {
                self.sessions = sessions;
                self.sort_sessions();
                if let Some(id) = self.selected.clone()
                    && !self.sessions.iter().any(|s| s.id == id) {
                        self.selected = None;
                    }
                match self.start_session.take() {
                    Some(id) if self.sessions.iter().any(|s| s.id == id) => self.select(id),
                    _ => Task::none(),
                }
            }
            Msg::SessionsLoaded(Err(e)) => {
                self.toast(format!("Could not load sessions: {e}"), true);
                Task::none()
            }
            Msg::Select(id) => self.select(id),
            Msg::ThreadLoaded { session, older, result } => match result {
                Ok(messages) => {
                    let thread = self.threads.entry(session.clone()).or_default();
                    let full_page = messages.len() >= PAGE;
                    if !older {
                        *thread = Thread::default();
                    }
                    thread.loaded = true;
                    thread.has_more = full_page;
                    for m in messages {
                        thread.upsert(m);
                    }
                    if !older && self.selected.as_deref() == Some(session.as_str()) {
                        return self.snap();
                    }
                    Task::none()
                }
                Err(e) => {
                    self.toast(format!("Could not load messages: {e}"), true);
                    Task::none()
                }
            },
            Msg::LoadOlder => {
                let Some(id) = self.selected.clone() else { return Task::none() };
                let before = self.threads.get(&id).and_then(|t| t.messages.first()).map(|m| m.id);
                let session = id.clone();
                Task::perform(hub::messages(id, before), move |result| Msg::ThreadLoaded {
                    session: session.clone(),
                    older: true,
                    result,
                })
            }
            Msg::Scrolled(viewport) => {
                self.at_bottom = viewport.relative_offset().y > 0.98;
                Task::none()
            }
            Msg::Composer(action) => {
                self.composer.perform(action);
                Task::none()
            }
            Msg::Send => {
                let text = self.composer.text().trim().to_string();
                let Some(session) = self.selected_session() else { return Task::none() };
                if text.is_empty() || session.status == SessionStatus::Ended {
                    return Task::none();
                }
                let id = session.id.clone();
                self.composer = text_editor::Content::new();
                Task::perform(hub::send(id, text), Msg::Sent)
            }
            Msg::Sent(Ok(_)) => self.snap(),
            Msg::Sent(Err(e)) => {
                self.toast(format!("Not sent: {e}"), true);
                Task::none()
            }
            Msg::AnswerDraft { question, text } => {
                self.answers.insert(question, text);
                Task::none()
            }
            Msg::Answer { question, answer } => {
                let answer = answer.trim().to_string();
                if answer.is_empty() {
                    return Task::none();
                }
                self.answers.remove(&question);
                Task::perform(hub::answer(question, answer), Msg::Answered)
            }
            Msg::Answered(Ok(_)) => Task::none(),
            Msg::Answered(Err(e)) => {
                self.toast(format!("Answer not delivered: {e}"), true);
                Task::none()
            }
            Msg::SetListen(on) => self.patch(PatchSessionRequest { listen: Some(on), ..Default::default() }),
            Msg::SetMirror(on) => self.patch(PatchSessionRequest { mirror: Some(on), ..Default::default() }),
            Msg::SetArchived(on) => {
                self.toast(if on { "Archived" } else { "Restored from archive" }, false);
                self.patch(PatchSessionRequest { archived: Some(on), ..Default::default() })
            }
            Msg::Patched(Ok(session)) => {
                self.upsert_session(session);
                Task::none()
            }
            Msg::Patched(Err(e)) => {
                self.toast(e, true);
                Task::none()
            }
            Msg::Reconnect(mode) => {
                let Some(session) = self.selected_session().cloned() else { return Task::none() };
                self.reconnecting = true;
                Task::perform(hub::reconnect(session, mode), Msg::Reconnected)
            }
            Msg::Reconnected(result) => {
                self.reconnecting = false;
                match result {
                    Ok(_) => self.toast("Starting the agent… it reconnects in a few seconds.", false),
                    Err(e) => self.toast(format!("Reconnect failed: {e}"), true),
                }
                Task::none()
            }
            Msg::RenameStart => {
                self.rename = self.selected_session().map(|s| s.display_name());
                operation::focus("rename")
            }
            Msg::RenameInput(text) => {
                self.rename = Some(text);
                Task::none()
            }
            Msg::RenameSave => {
                let Some(title) = self.rename.take() else { return Task::none() };
                self.patch(PatchSessionRequest { title: Some(title), ..Default::default() })
            }
            Msg::RenameCancel => {
                self.rename = None;
                Task::none()
            }
            Msg::DeleteAsk => {
                self.confirm_delete = true;
                Task::none()
            }
            Msg::DeleteCancel => {
                self.confirm_delete = false;
                Task::none()
            }
            Msg::DeleteConfirm => {
                self.confirm_delete = false;
                match self.selected.clone() {
                    Some(id) => Task::perform(hub::delete(id), Msg::Deleted),
                    None => Task::none(),
                }
            }
            Msg::Deleted(Ok(())) => {
                self.toast("Conversation deleted", false);
                Task::none()
            }
            Msg::Deleted(Err(e)) | Msg::Done(Err(e)) => {
                self.toast(e, true);
                Task::none()
            }
            Msg::Done(Ok(())) => Task::none(),
            Msg::Search(text) => {
                self.search = text;
                Task::none()
            }
            Msg::ToggleArchived => {
                self.show_archived = !self.show_archived;
                Task::none()
            }
            Msg::Copy(text) => {
                self.toast("Copied", false);
                iced::clipboard::write(text)
            }
            Msg::Link(url) => {
                let _ = std::process::Command::new("xdg-open")
                    .arg(&url)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map(|mut c| std::thread::spawn(move || c.wait()));
                Task::none()
            }
            Msg::Window(id, event) => {
                self.window = Some(id);
                match event {
                    window::Event::Focused => {
                        self.focused = true;
                        let read = self.selected.clone().map(|s| self.mark_read_if_visible(&s)).unwrap_or_else(Task::none);
                        // Clear the urgent mark set by `attention` (most WMs also do this on focus).
                        Task::batch([self.presence(), read, window::request_user_attention(id, None)])
                    }
                    window::Event::Unfocused => {
                        self.focused = false;
                        self.presence()
                    }
                    _ => Task::none(),
                }
            }
            Msg::SystemTheme(mode) => {
                self.system_mode = mode;
                Task::none()
            }
            Msg::SetTheme(pref) => {
                self.theme_pref = pref;
                self.save_view_settings();
                Task::none()
            }
            Msg::ToggleSettings => {
                self.show_settings = !self.show_settings;
                Task::none()
            }
            Msg::Zoom(step) => {
                // 10% steps, rounded so repeated steps don't drift.
                let percent = (self.zoom * 100.0).round() as i32;
                self.zoom = clamp_zoom(match step {
                    Step::Up => (percent + 10) as f32 / 100.0,
                    Step::Down => (percent - 10) as f32 / 100.0,
                    Step::Reset => 1.0,
                });
                self.save_view_settings();
                Task::none()
            }
            Msg::TextSize(step) => {
                self.text_size = match step {
                    Step::Up => self.text_size + 1,
                    Step::Down => self.text_size.saturating_sub(1),
                    Step::Reset => DesktopConfig::DEFAULT_TEXT_SIZE,
                }
                .clamp(TEXT_SIZE_RANGE.0, TEXT_SIZE_RANGE.1);
                crate::style::set_text_size(self.text_size);
                self.save_view_settings();
                Task::none()
            }
            Msg::Key(event) => match event {
                keyboard::Event::KeyPressed { key, modifiers, .. } => match zoom_shortcut(&key, modifiers) {
                    Some(step) => self.update(Msg::Zoom(step)),
                    None if self.show_settings && key == keyboard::Key::Named(keyboard::key::Named::Escape) => {
                        self.show_settings = false;
                        Task::none()
                    }
                    None => Task::none(),
                },
                _ => Task::none(),
            },
            Msg::DismissToast => {
                self.toast = None;
                Task::none()
            }
            Msg::Tick => {
                if self.toast.as_ref().is_some_and(|t| t.since.elapsed() > Duration::from_secs(if t.error { 8 } else { 3 })) {
                    self.toast = None;
                }
                Task::none()
            }
        }
    }

    fn on_event(&mut self, event: Event) -> Task<Msg> {
        match event {
            Event::Hello { .. } => Task::none(),
            Event::Session { session } => {
                let id = session.id.clone();
                self.upsert_session(session);
                self.mark_read_if_visible(&id)
            }
            Event::SessionRemoved { id } => {
                self.sessions.retain(|s| s.id != id);
                self.threads.remove(&id);
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
                Task::none()
            }
            Event::Message { message } => {
                let attention = self.attention(&message);
                let session = message.session_id.clone();
                let is_selected = self.selected.as_deref() == Some(session.as_str());
                if let Some(thread) = self.threads.get_mut(&session).filter(|t| t.loaded) {
                    let is_new = thread.messages.binary_search_by_key(&message.id, |m| m.id).is_err();
                    thread.upsert(message);
                    if is_selected && is_new && self.at_bottom {
                        return Task::batch([self.snap(), attention]);
                    }
                }
                attention
            }
            Event::Question { question } => {
                if let Some(thread) = self.threads.get_mut(&question.session_id) {
                    thread.set_question(question);
                }
                Task::none()
            }
            Event::Focus { session_id } => {
                let select = match session_id {
                    Some(id) if self.sessions.iter().any(|s| s.id == id) => self.select(id),
                    Some(id) => {
                        self.start_session = Some(id);
                        Task::perform(hub::sessions(), Msg::SessionsLoaded)
                    }
                    None => Task::none(),
                };
                let focus = match self.window {
                    Some(id) => window::gain_focus(id),
                    None => window::latest().and_then(window::gain_focus),
                };
                Task::batch([select, focus])
            }
        }
    }
}

const TEXT_SIZE_RANGE: (u16, u16) = DesktopConfig::TEXT_SIZE_RANGE;

fn clamp_zoom(zoom: f32) -> f32 {
    let (min, max) = DesktopConfig::ZOOM_RANGE;
    if zoom.is_finite() { zoom.clamp(min, max) } else { 1.0 }
}

/// Ctrl + / Ctrl - / Ctrl 0, as in a browser.
pub fn zoom_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<Step> {
    if !modifiers.command() {
        return None;
    }
    match key.as_ref() {
        keyboard::Key::Character("+" | "=") => Some(Step::Up),
        keyboard::Key::Character("-" | "_") => Some(Step::Down),
        keyboard::Key::Character("0") => Some(Step::Reset),
        _ => None,
    }
}

impl App {
    /// Marks the window urgent for a new agent message while it is not focused, like Teams or
    /// Discord: i3 (X11 urgency hint) and Hyprland (xdg-activation) highlight the window and its
    /// workspace until it gets focus.
    fn attention(&mut self, message: &Message) -> Task<Msg> {
        let new = message.id > self.last_message_id;
        self.last_message_id = self.last_message_id.max(message.id);
        let from_agent = message.direction == Direction::FromAgent;
        // Events re-sent for old messages (delivery or read updates) must not ring.
        let recent = liaison_core::now_ms() - message.created_at < 60_000;
        match self.window {
            Some(id) if new && from_agent && recent && !self.focused => {
                let level = match message.kind {
                    MessageKind::Question | MessageKind::Permission => window::UserAttention::Critical,
                    _ => window::UserAttention::Informational,
                };
                window::request_user_attention(id, Some(level))
            }
            _ => Task::none(),
        }
    }

    /// Saves theme, zoom and text size to config.toml (other settings there are kept).
    fn save_view_settings(&self) {
        let mut config = Config::load();
        config.desktop.theme = match self.theme_pref {
            ThemePref::System => "system",
            ThemePref::Dark => "dark",
            ThemePref::Light => "light",
        }
        .into();
        config.desktop.zoom = self.zoom;
        config.desktop.text_size = self.text_size;
        let _ = config.save();
    }
}
