//! HTTP API (see PLAN.md §7).

use crate::state::{HubError, SharedHub};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use liaison_core::{Event, Harness, api::*};
use serde::Deserialize;
use std::{convert::Infallible, sync::atomic::Ordering, time::Duration};
use tokio::sync::broadcast::error::RecvError;

impl IntoResponse for HubError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            HubError::NotFound(what) => (StatusCode::NOT_FOUND, format!("{what} not found")),
            HubError::Gone(msg) => (StatusCode::GONE, msg),
            HubError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            HubError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            HubError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        };
        (status, Json(ErrorResponse { error })).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, HubError>;

const MAX_WAIT_SECS: u64 = 3600;

#[derive(Deserialize)]
struct WaitQuery {
    timeout: Option<u64>,
}

impl WaitQuery {
    fn duration(&self, default: u64) -> Duration {
        Duration::from_secs(self.timeout.unwrap_or(default).min(MAX_WAIT_SECS))
    }
}

pub fn router(hub: SharedHub) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/config", get(config))
        .route("/v1/config/reload", post(reload_config))
        .route("/v1/events", get(events))
        .route("/v1/resolve", get(resolve))
        .route("/v1/sessions", get(list_sessions))
        .route("/v1/sessions/register", post(register))
        .route("/v1/sessions/{id}", get(get_session).patch(patch_session).delete(delete_session))
        .route("/v1/sessions/{id}/state", post(set_state))
        .route("/v1/sessions/{id}/end", post(end_session))
        .route("/v1/sessions/{id}/messages", get(list_messages).post(agent_message))
        .route("/v1/sessions/{id}/send", post(send))
        .route("/v1/sessions/{id}/take", post(take))
        .route("/v1/sessions/{id}/wait", get(wait))
        .route("/v1/sessions/{id}/read", post(read))
        .route("/v1/sessions/{id}/questions", post(ask))
        .route("/v1/sessions/{id}/reconnect", post(reconnect))
        .route("/v1/questions/{id}", get(get_question))
        .route("/v1/questions/{id}/wait", get(wait_question))
        .route("/v1/questions/{id}/answer", post(answer))
        .route("/v1/pids/{pid}/pending", get(pending))
        .route("/v1/ui/presence", post(presence))
        .route("/v1/ui/focus", post(focus))
        .with_state(hub)
}

async fn health(State(hub): State<SharedHub>) -> ApiResult<HealthResponse> {
    Ok(Json(hub.health()?))
}

async fn config(State(hub): State<SharedHub>) -> Json<liaison_core::config::Config> {
    Json(hub.config())
}

async fn reload_config(State(hub): State<SharedHub>) -> Json<liaison_core::config::Config> {
    let config = liaison_core::config::Config::load();
    *hub.config.write().unwrap_or_else(|e| e.into_inner()) = config.clone();
    Json(config)
}

#[derive(Deserialize)]
struct ResolveQuery {
    pid: Option<i64>,
    harness: Option<Harness>,
    external_id: Option<String>,
}

async fn resolve(State(hub): State<SharedHub>, Query(q): Query<ResolveQuery>) -> ApiResult<liaison_core::Session> {
    let found = match (q.harness, q.external_id.as_deref(), q.pid) {
        (Some(h), Some(ext), _) => hub.db.session_by_external(h, ext)?,
        (_, _, Some(pid)) => hub.db.session_by_pid(pid)?,
        _ => return Err(HubError::BadRequest("pass pid, or harness and external_id".into())),
    };
    found.map(Json).ok_or(HubError::NotFound("session"))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    archived: bool,
}

async fn list_sessions(State(hub): State<SharedHub>, Query(q): Query<ListQuery>) -> ApiResult<SessionsResponse> {
    Ok(Json(SessionsResponse { sessions: hub.db.sessions(q.archived)? }))
}

async fn register(State(hub): State<SharedHub>, Json(req): Json<RegisterRequest>) -> ApiResult<liaison_core::Session> {
    Ok(Json(hub.register(&req)?))
}

async fn get_session(State(hub): State<SharedHub>, Path(id): Path<String>) -> ApiResult<liaison_core::Session> {
    Ok(Json(hub.session(&id)?))
}

#[derive(Deserialize)]
struct PatchQuery {
    /// `agent` when the agent itself renames the session (does not lock the title).
    source: Option<String>,
}

async fn patch_session(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Query(q): Query<PatchQuery>,
    Json(req): Json<PatchSessionRequest>,
) -> ApiResult<liaison_core::Session> {
    let from_user = q.source.as_deref() != Some("agent");
    Ok(Json(hub.patch(&id, &req, from_user)?))
}

async fn delete_session(State(hub): State<SharedHub>, Path(id): Path<String>) -> Result<StatusCode, HubError> {
    hub.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_state(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Json(req): Json<StateRequest>,
) -> ApiResult<liaison_core::Session> {
    Ok(Json(hub.set_state(&id, &req)?))
}

async fn end_session(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Json(req): Json<EndRequest>,
) -> ApiResult<liaison_core::Session> {
    Ok(Json(hub.end(&id, &req)?))
}

#[derive(Deserialize)]
struct MessagesQuery {
    before: Option<i64>,
    limit: Option<i64>,
}

async fn list_messages(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Query(q): Query<MessagesQuery>,
) -> ApiResult<MessagesResponse> {
    Ok(Json(MessagesResponse { messages: hub.messages(&id, q.before, q.limit.unwrap_or(200))? }))
}

async fn agent_message(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Json(req): Json<AgentMessageRequest>,
) -> ApiResult<liaison_core::Message> {
    Ok(Json(hub.agent_message(&id, &req)?))
}

async fn send(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Json(req): Json<SendRequest>,
) -> ApiResult<liaison_core::Message> {
    Ok(Json(hub.send_from_user(&id, &req.text)?))
}

async fn take(State(hub): State<SharedHub>, Path(id): Path<String>) -> ApiResult<MessagesResponse> {
    Ok(Json(MessagesResponse { messages: hub.take(&id)? }))
}

async fn wait(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<MessagesResponse> {
    Ok(Json(MessagesResponse { messages: hub.wait_messages(&id, q.duration(300)).await? }))
}

async fn read(State(hub): State<SharedHub>, Path(id): Path<String>) -> Result<StatusCode, HubError> {
    hub.mark_read(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn ask(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Json(req): Json<QuestionRequest>,
) -> ApiResult<liaison_core::Question> {
    Ok(Json(hub.ask(&id, &req)?))
}

async fn reconnect(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Json(req): Json<ReconnectRequest>,
) -> ApiResult<ReconnectTicket> {
    Ok(Json(hub.prepare_reconnect(&id, &req)?))
}

async fn get_question(State(hub): State<SharedHub>, Path(id): Path<String>) -> ApiResult<liaison_core::Question> {
    hub.db.question(&id)?.map(Json).ok_or(HubError::NotFound("question"))
}

async fn wait_question(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<liaison_core::Question> {
    Ok(Json(hub.wait_answer(&id, q.duration(300)).await?))
}

async fn answer(
    State(hub): State<SharedHub>,
    Path(id): Path<String>,
    Json(req): Json<AnswerRequest>,
) -> ApiResult<liaison_core::Question> {
    Ok(Json(hub.answer(&id, &req.answer)?))
}

async fn pending(
    State(hub): State<SharedHub>,
    Path(pid): Path<i64>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<PendingResponse> {
    Ok(Json(PendingResponse { sessions: hub.pending_for_pid(pid, q.duration(25)).await? }))
}

async fn presence(State(hub): State<SharedHub>, Json(req): Json<PresenceRequest>) -> StatusCode {
    *hub.presence.lock().unwrap_or_else(|e| e.into_inner()) =
        crate::state::Presence { focused: req.focused, session_id: req.session_id };
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct FocusRequest {
    session_id: Option<String>,
}

async fn focus(State(hub): State<SharedHub>, Json(req): Json<FocusRequest>) -> StatusCode {
    match req.session_id {
        Some(id) => crate::notifier::open_app(&hub, Some(&id)),
        None => hub.focus_ui(None),
    }
    StatusCode::NO_CONTENT
}

/// Newline-delimited JSON events for the desktop app, with a blank keep-alive line every
/// few seconds (which also lets the hub notice a closed app quickly).
async fn events(State(hub): State<SharedHub>) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(256);
    let mut events = hub.subscribe();
    hub.ui_clients.fetch_add(1, Ordering::Relaxed);
    let hub_task = hub.clone();
    tokio::spawn(async move {
        let line = |event: &Event| {
            let mut bytes = serde_json::to_vec(event).unwrap_or_default();
            bytes.push(b'\n');
            Ok(Bytes::from(bytes))
        };
        let hello = Event::Hello { version: env!("CARGO_PKG_VERSION").to_string() };
        let mut alive = tx.send(line(&hello)).await.is_ok();
        let mut keepalive = tokio::time::interval(Duration::from_secs(5));
        while alive {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => alive = tx.send(line(&event)).await.is_ok(),
                    Err(RecvError::Lagged(n)) => tracing::warn!("event stream lagged by {n}"),
                    Err(RecvError::Closed) => break,
                },
                _ = keepalive.tick() => alive = tx.send(Ok(Bytes::from_static(b"\n"))).await.is_ok(),
            }
        }
        if hub_task.ui_clients.fetch_sub(1, Ordering::Relaxed) == 1 {
            *hub_task.presence.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
        }
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Response::builder()
        .header("content-type", "application/x-ndjson")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
