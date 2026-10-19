//! Request and response bodies of the hub HTTP API (see PLAN.md §7).

use crate::model::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub harness: Harness,
    pub external_id: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    /// PID of the harness process (used for liveness checks and MCP routing).
    pub pid: Option<i64>,
    pub transcript_path: Option<String>,
    /// Why the session (re)started: startup, resume, clear, compact, …
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateRequest {
    pub status: SessionStatus,
    /// The final assistant text of the turn that just ended, if any.
    #[serde(default)]
    pub last_reply: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndRequest {
    pub reason: String,
    /// The context is gone for good (the session can never be resumed).
    #[serde(default)]
    pub gone: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessageRequest {
    #[serde(default = "default_kind")]
    pub kind: MessageKind,
    pub body: String,
    #[serde(default)]
    pub meta: serde_json::Value,
}

fn default_kind() -> MessageKind {
    MessageKind::Text
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessagesResponse {
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRequest {
    pub kind: MessageKind,
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub meta: serde_json::Value,
    /// Seconds before the question expires (hub default when absent).
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerRequest {
    pub answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSession {
    pub id: String,
    pub external_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PendingResponse {
    pub sessions: Vec<PendingSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendRequest {
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchSessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectRequest {
    pub mode: ReconnectMode,
}

/// How a reconnecting client should start the agent again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectTicket {
    pub session: Session,
    /// The first prompt, when messages are waiting. The messages themselves are handed over
    /// by the agent's hooks/plugin once it is really running, so a crash cannot lose them.
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceRequest {
    pub focused: bool,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsResponse {
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
    pub db_path: String,
    pub sessions: i64,
    pub connected: i64,
    pub ui_connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}
