//! Hub state and the operations shared by the HTTP routes and the background monitor.

use crate::{
    db::{Db, Registration},
    notifier,
};
use anyhow::Result;
use liaison_core::{
    Delivery, Direction, Event, Message, MessageKind, Question, QuestionStatus, Session, SessionStatus, api::*,
    config::Config, now_ms, procs,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, watch};

#[derive(Debug, Default, Clone)]
pub struct Presence {
    pub focused: bool,
    pub session_id: Option<String>,
}

pub struct Hub {
    pub db: Db,
    pub config: RwLock<Config>,
    events: broadcast::Sender<Event>,
    /// Bumped on every change a long-poll may be waiting for.
    changes: watch::Sender<u64>,
    pub presence: Mutex<Presence>,
    pub ui_clients: AtomicUsize,
    /// Sessions being restarted from the app, and when that started.
    reconnecting: Mutex<HashMap<String, Instant>>,
}

pub type SharedHub = Arc<Hub>;

#[derive(Debug)]
pub enum HubError {
    NotFound(&'static str),
    Gone(String),
    Conflict(String),
    BadRequest(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for HubError {
    fn from(e: anyhow::Error) -> Self {
        HubError::Internal(e)
    }
}

pub type HubResult<T> = Result<T, HubError>;

impl Hub {
    pub fn new(db: Db, config: Config) -> SharedHub {
        let (events, _) = broadcast::channel(1024);
        let (changes, _) = watch::channel(0);
        Arc::new(Hub {
            db,
            config: RwLock::new(config),
            events,
            changes,
            presence: Mutex::new(Presence::default()),
            ui_clients: AtomicUsize::new(0),
            reconnecting: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn ui_connected(&self) -> bool {
        self.ui_clients.load(Ordering::Relaxed) > 0
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn bump(&self) {
        self.changes.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub fn emit_session(&self, id: &str) {
        if let Ok(Some(session)) = self.db.session(id) {
            self.emit(Event::Session { session });
        }
    }

    fn emit_message(&self, message: &Message) {
        self.emit(Event::Message { message: message.clone() });
    }

    fn emit_question(&self, question: &Question) {
        self.emit(Event::Question { question: question.clone() });
        if let Ok(Some(message)) = self.db.message(question.message_id) {
            self.emit_message(&message);
        }
    }

    pub fn session(&self, id: &str) -> HubResult<Session> {
        self.db.session(id)?.ok_or(HubError::NotFound("session"))
    }

    fn system_event(&self, session_id: &str, text: &str) {
        if let Ok(m) =
            self.db
                .insert_message(session_id, Direction::System, MessageKind::Event, text, &serde_json::Value::Null, Delivery::Delivered)
        {
            self.emit_message(&m);
        }
    }

    /// Waits until `check` yields something or the timeout passes.
    async fn wait_for<T>(&self, timeout: Duration, mut check: impl FnMut() -> HubResult<Option<T>>) -> HubResult<Option<T>> {
        let deadline = Instant::now() + timeout;
        let mut rx = self.changes.subscribe();
        loop {
            rx.borrow_and_update();
            if let Some(found) = check()? {
                return Ok(Some(found));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            // Wake on changes, and at least every few seconds to notice expiries.
            let _ = tokio::time::timeout(left.min(Duration::from_secs(5)), rx.changed()).await;
        }
    }

    // ── lifecycle ───────────────────────────────────────────────────────────

    pub fn register(&self, req: &RegisterRequest) -> HubResult<Session> {
        if req.external_id.trim().is_empty() {
            return Err(HubError::BadRequest("external_id is required".into()));
        }
        let pid_start = req.pid.and_then(procs::start_time);
        let (session, previous) = self.db.upsert_session(&Registration {
            harness: req.harness,
            external_id: &req.external_id,
            cwd: req.cwd.as_deref(),
            title: req.title.as_deref(),
            pid: req.pid,
            pid_start,
            transcript_path: req.transcript_path.as_deref(),
            status: SessionStatus::Idle,
        })?;
        self.reconnecting.lock().unwrap_or_else(|e| e.into_inner()).remove(&session.id);
        match previous {
            None => self.system_event(&session.id, &format!("{} session connected", req.harness.label())),
            Some(prev) if !prev.is_connected() => self.system_event(&session.id, "Reconnected"),
            _ => {}
        }
        self.bump();
        self.emit_session(&session.id);
        self.session(&session.id)
    }

    pub fn set_state(&self, id: &str, req: &StateRequest) -> HubResult<Session> {
        let session = self.session(id)?;
        if !req.status.is_connected() {
            return Err(HubError::BadRequest("use /end to disconnect a session".into()));
        }
        if session.status == SessionStatus::Ended {
            return Err(HubError::Gone("session has ended".into()));
        }
        if !session.status.is_connected() {
            self.system_event(id, "Reconnected");
        }
        self.db.set_status(id, req.status, None)?;
        if let Some(reply) = req.last_reply.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            self.mirror_reply(&session, reply)?;
        }
        self.bump();
        self.emit_session(id);
        self.session(id)
    }

    /// Shows the agent's final answer in the chat when the turn was started from the app
    /// (or mirroring is on), unless the agent already wrote to the chat itself.
    fn mirror_reply(&self, session: &Session, reply: &str) -> HubResult<()> {
        if !(session.awaiting_reply || session.mirror) {
            return Ok(());
        }
        self.db.set_awaiting_reply(&session.id, false)?;
        if session.awaiting_reply && self.db.agent_spoke_since_last_delivery(&session.id)? {
            return Ok(());
        }
        self.agent_message(
            &session.id,
            &AgentMessageRequest { kind: MessageKind::Reply, body: reply.to_string(), meta: serde_json::Value::Null },
        )?;
        Ok(())
    }

    pub fn end(&self, id: &str, req: &EndRequest) -> HubResult<Session> {
        let session = self.session(id)?;
        if req.gone {
            self.mark_ended(&session, &req.reason)?;
        } else {
            self.mark_disconnected(&session, &req.reason)?;
        }
        self.session(id)
    }

    pub fn mark_disconnected(&self, session: &Session, reason: &str) -> HubResult<()> {
        if !session.status.is_connected() {
            return Ok(());
        }
        self.db.set_status(&session.id, SessionStatus::Disconnected, Some(reason))?;
        for q in self.db.expire_questions(Some(&session.id))? {
            self.emit_question(&q);
        }
        self.system_event(&session.id, &format!("Disconnected ({reason})"));
        self.bump();
        self.emit_session(&session.id);
        Ok(())
    }

    pub fn mark_ended(&self, session: &Session, reason: &str) -> HubResult<()> {
        if session.status == SessionStatus::Ended {
            return Ok(());
        }
        self.db.set_status(&session.id, SessionStatus::Ended, Some(reason))?;
        for m in self.db.fail_queued(&session.id)? {
            self.emit_message(&m);
        }
        for q in self.db.expire_questions(Some(&session.id))? {
            self.emit_question(&q);
        }
        self.system_event(&session.id, &format!("Session ended ({reason}). The conversation is read-only."));
        self.bump();
        self.emit_session(&session.id);
        Ok(())
    }

    pub fn patch(&self, id: &str, req: &PatchSessionRequest, from_user: bool) -> HubResult<Session> {
        self.session(id)?;
        self.db.patch_session(
            id,
            req.title.as_deref(),
            from_user,
            req.status_text.as_deref(),
            req.listen,
            req.mirror,
            req.archived,
        )?;
        self.bump();
        self.emit_session(id);
        self.session(id)
    }

    pub fn delete(&self, id: &str) -> HubResult<()> {
        if !self.db.delete_session(id)? {
            return Err(HubError::NotFound("session"));
        }
        self.bump();
        self.emit(Event::SessionRemoved { id: id.to_string() });
        Ok(())
    }

    // ── messages ────────────────────────────────────────────────────────────

    pub fn agent_message(&self, id: &str, req: &AgentMessageRequest) -> HubResult<Message> {
        let session = self.session(id)?;
        if req.body.trim().is_empty() {
            return Err(HubError::BadRequest("message body is empty".into()));
        }
        let message =
            self.db.insert_message(id, Direction::FromAgent, req.kind, &req.body, &req.meta, Delivery::Delivered)?;
        self.db.touch(id)?;
        self.emit_message(&message);
        self.emit_session(id);
        notifier::message(self, &session, &message);
        Ok(message)
    }

    pub fn send_from_user(&self, id: &str, text: &str) -> HubResult<Message> {
        let session = self.session(id)?;
        if !session.status.accepts_messages() {
            return Err(HubError::Gone("this session has ended; its context is gone".into()));
        }
        if text.trim().is_empty() {
            return Err(HubError::BadRequest("message is empty".into()));
        }
        let message = self.db.insert_message(
            id,
            Direction::ToAgent,
            MessageKind::Text,
            text,
            &serde_json::Value::Null,
            Delivery::Queued,
        )?;
        self.db.mark_read(id)?;
        self.bump();
        self.emit_message(&message);
        self.emit_session(id);
        Ok(message)
    }

    /// Hands queued messages to the agent.
    pub fn take(&self, id: &str) -> HubResult<Vec<Message>> {
        let session = self.session(id)?;
        if session.status == SessionStatus::Ended {
            return Ok(vec![]);
        }
        let messages = self.db.take_queued(id)?;
        if !messages.is_empty() {
            self.db.set_awaiting_reply(id, true)?;
            for m in &messages {
                self.emit_message(m);
            }
            self.emit_session(id);
        }
        Ok(messages)
    }

    /// Parks the agent until the user writes (listen mode), then hands the messages over.
    pub async fn wait_messages(self: &Arc<Self>, id: &str, timeout: Duration) -> HubResult<Vec<Message>> {
        let session = self.session(id)?;
        if session.status == SessionStatus::Ended {
            return Err(HubError::Gone("session has ended".into()));
        }
        if session.status.is_connected() {
            self.db.set_status(id, SessionStatus::Listening, None)?;
            self.emit_session(id);
        }
        // If the agent stops waiting early (cancelled hook, closed connection), drop back to idle.
        let guard = ListenGuard { hub: self.clone(), id: id.to_string(), armed: true };
        let found = self
            .wait_for(timeout, || {
                let messages = self.take(id)?;
                Ok((!messages.is_empty()).then_some(messages))
            })
            .await;
        let mut guard = guard;
        guard.armed = false;
        let messages = found?.unwrap_or_default();
        let next = if messages.is_empty() { SessionStatus::Idle } else { SessionStatus::Active };
        if self.db.set_status_if(id, SessionStatus::Listening, next)? {
            self.emit_session(id);
        }
        Ok(messages)
    }

    pub fn messages(&self, id: &str, before: Option<i64>, limit: i64) -> HubResult<Vec<Message>> {
        self.session(id)?;
        Ok(self.db.messages(id, before, limit.clamp(1, 1000))?)
    }

    pub fn mark_read(&self, id: &str) -> HubResult<()> {
        if self.db.mark_read(id)? > 0 {
            self.emit_session(id);
        }
        Ok(())
    }

    /// Idle sessions of a harness process that have messages waiting (opencode plugin).
    pub async fn pending_for_pid(&self, pid: i64, timeout: Duration) -> HubResult<Vec<PendingSession>> {
        let found = self
            .wait_for(timeout, || {
                let sessions = self.db.pending_for_pid(pid)?;
                Ok((!sessions.is_empty()).then_some(sessions))
            })
            .await?;
        Ok(found
            .unwrap_or_default()
            .into_iter()
            .map(|s| PendingSession { id: s.id, external_id: s.external_id })
            .collect())
    }

    // ── questions ───────────────────────────────────────────────────────────

    pub fn ask(&self, id: &str, req: &QuestionRequest) -> HubResult<Question> {
        let session = self.session(id)?;
        if !matches!(req.kind, MessageKind::Question | MessageKind::Permission) {
            return Err(HubError::BadRequest("kind must be question or permission".into()));
        }
        let config = self.config();
        let timeout = req.timeout_secs.unwrap_or(match req.kind {
            MessageKind::Permission => config.timeouts.permission_secs,
            _ => config.timeouts.question_secs,
        });
        let mut meta = req.meta.clone();
        if !meta.is_object() {
            meta = serde_json::json!({});
        }
        meta["options"] = serde_json::json!(req.options);
        let message = self.db.insert_message(id, Direction::FromAgent, req.kind, &req.prompt, &meta, Delivery::Delivered)?;
        let now = now_ms();
        let question = Question {
            id: ulid::Ulid::generate().to_string(),
            session_id: id.to_string(),
            message_id: message.id,
            kind: req.kind,
            prompt: req.prompt.clone(),
            options: req.options.clone(),
            answer: None,
            status: QuestionStatus::Pending,
            created_at: now,
            answered_at: None,
            expires_at: Some(now + (timeout as i64) * 1000),
        };
        self.db.insert_question(&question)?;
        self.db.touch(id)?;
        let message = self.db.message(message.id)?.unwrap_or(message);
        self.emit_message(&message);
        self.emit_session(id);
        notifier::message(self, &session, &message);
        Ok(question)
    }

    pub async fn wait_answer(&self, question_id: &str, timeout: Duration) -> HubResult<Question> {
        self.db.question(question_id)?.ok_or(HubError::NotFound("question"))?;
        let found = self
            .wait_for(timeout, || {
                let q = self.db.question(question_id)?.ok_or(HubError::NotFound("question"))?;
                if q.status == QuestionStatus::Pending && q.expires_at.is_some_and(|t| t < now_ms()) {
                    for q in self.db.expire_questions(None)? {
                        self.emit_question(&q);
                    }
                    return Ok(self.db.question(question_id)?);
                }
                Ok((q.status != QuestionStatus::Pending).then_some(q))
            })
            .await?;
        match found {
            Some(q) => Ok(q),
            None => self.db.question(question_id)?.ok_or(HubError::NotFound("question")),
        }
    }

    pub fn answer(&self, question_id: &str, answer: &str) -> HubResult<Question> {
        let question = self.db.question(question_id)?.ok_or(HubError::NotFound("question"))?;
        if !self.db.answer_question(question_id, answer)? {
            return Err(HubError::Conflict(format!("question is already {}", question.status)));
        }
        let question = self.db.question(question_id)?.ok_or(HubError::NotFound("question"))?;
        self.bump();
        self.emit_question(&question);
        self.emit_session(&question.session_id);
        Ok(question)
    }

    pub fn expire_questions(&self) -> Result<()> {
        let expired = self.db.expire_questions(None)?;
        if !expired.is_empty() {
            self.bump();
        }
        for q in expired {
            self.emit_question(&q);
        }
        Ok(())
    }

    // ── reconnect ───────────────────────────────────────────────────────────

    /// Prepares a reconnect: the client starts the agent with the returned prompt, and the
    /// agent's hooks/plugin take the queued messages once it is really running.
    pub fn prepare_reconnect(&self, id: &str, req: &ReconnectRequest) -> HubResult<ReconnectTicket> {
        let session = self.session(id)?;
        if session.status == SessionStatus::Ended {
            return Err(HubError::Gone("this session has ended; its context is gone".into()));
        }
        if session.status.is_connected() {
            return Err(HubError::Conflict("the session is already connected".into()));
        }
        if !session.harness.can_reconnect() {
            return Err(HubError::BadRequest(format!("{} sessions cannot be restarted by Liaison", session.harness)));
        }
        if req.mode == liaison_core::ReconnectMode::Background {
            // Without a terminal the user can only reach it through Liaison, so keep it listening.
            self.db.patch_session(id, None, false, None, Some(true), None, None)?;
        }
        let queued = self.db.queued(id)?.len();
        let prompt = (queued > 0).then(|| {
            format!(
                "[Liaison] You were reconnected from the Liaison app because the user wrote to you while you were \
                 offline ({queued} message{}). Their messages are attached; read them and respond.",
                if queued == 1 { "" } else { "s" }
            )
        });
        self.reconnecting.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), Instant::now());
        let what = match req.mode {
            liaison_core::ReconnectMode::Background => "in the background",
            liaison_core::ReconnectMode::Terminal => "in a terminal",
        };
        self.system_event(id, &format!("Reconnecting {what}…"));
        self.emit_session(id);
        Ok(ReconnectTicket { session: self.session(id)?, prompt })
    }

    /// Reports reconnects that never came back (the agent crashed or refused to start).
    pub fn check_reconnects(&self) -> Result<()> {
        let stale: Vec<String> = {
            let mut pending = self.reconnecting.lock().unwrap_or_else(|e| e.into_inner());
            let stale: Vec<String> = pending
                .iter()
                .filter(|(_, since)| since.elapsed() > Duration::from_secs(45))
                .map(|(id, _)| id.clone())
                .collect();
            for id in &stale {
                pending.remove(id);
            }
            stale
        };
        for id in stale {
            if let Some(session) = self.db.session(&id)?
                && !session.status.is_connected() {
                    let hint = match session.harness {
                        liaison_core::Harness::ClaudeCode => " Check `claude agents` / `~/.claude/daemon.log`.",
                        _ => "",
                    };
                    self.system_event(&id, &format!("The agent did not come back. Your messages are still queued.{hint}"));
                    self.emit_session(&id);
                }
        }
        Ok(())
    }

    pub fn focus_ui(&self, session_id: Option<String>) {
        self.emit(Event::Focus { session_id });
    }

    pub fn health(&self) -> Result<HealthResponse> {
        let (sessions, connected) = self.db.count_sessions()?;
        Ok(HealthResponse {
            ok: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
            db_path: liaison_core::paths::db_path().display().to_string(),
            sessions,
            connected,
            ui_connected: self.ui_connected(),
        })
    }
}

struct ListenGuard {
    hub: SharedHub,
    id: String,
    armed: bool,
}

impl Drop for ListenGuard {
    fn drop(&mut self) {
        if self.armed && matches!(self.hub.db.set_status_if(&self.id, SessionStatus::Listening, SessionStatus::Idle), Ok(true)) {
            self.hub.emit_session(&self.id);
        }
    }
}
