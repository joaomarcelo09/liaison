//! Talking to the hub: the live event subscription and one-shot API calls.

use futures::{SinkExt, Stream, channel::mpsc};
use liaison_core::{
    Event, HubClient, Message, Question, ReconnectMode, Session,
    api::{AnswerRequest, MessagesResponse, PatchSessionRequest, PresenceRequest, SendRequest, SessionsResponse},
};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Feed {
    Connected,
    Event(Box<Event>),
    Disconnected(String),
}

/// Follows the hub's event stream forever, reconnecting when the hub restarts.
pub fn feed() -> impl Stream<Item = Feed> {
    iced::stream::channel(1024, async |mut output: mpsc::Sender<Feed>| {
        let client = HubClient::default();
        loop {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
            let follow = {
                let client = client.clone();
                tokio::spawn(async move { client.follow_events(move |e| { let _ = tx.send(e); }).await })
            };
            while let Some(event) = rx.recv().await {
                let item = match event {
                    Event::Hello { .. } => Feed::Connected,
                    other => Feed::Event(Box::new(other)),
                };
                if output.send(item).await.is_err() {
                    return;
                }
            }
            let reason = match follow.await {
                Ok(Ok(())) => "the hub closed the connection".to_string(),
                Ok(Err(e)) => e.to_string(),
                Err(e) => e.to_string(),
            };
            if output.send(Feed::Disconnected(reason)).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

fn client() -> HubClient {
    HubClient::default()
}

pub async fn sessions() -> Result<Vec<Session>, String> {
    client()
        .get::<SessionsResponse>("/v1/sessions?archived=true")
        .await
        .map(|r| r.sessions)
        .map_err(|e| e.to_string())
}

pub async fn messages(session_id: String, before: Option<i64>) -> Result<Vec<Message>, String> {
    let path = match before {
        Some(b) => format!("/v1/sessions/{session_id}/messages?limit=200&before={b}"),
        None => format!("/v1/sessions/{session_id}/messages?limit=200"),
    };
    client().get::<MessagesResponse>(&path).await.map(|r| r.messages).map_err(|e| e.to_string())
}

pub async fn send(session_id: String, text: String) -> Result<Message, String> {
    client()
        .post(&format!("/v1/sessions/{session_id}/send"), &SendRequest { text })
        .await
        .map_err(|e| e.to_string())
}

pub async fn answer(question_id: String, answer: String) -> Result<Question, String> {
    client()
        .post(&format!("/v1/questions/{question_id}/answer"), &AnswerRequest { answer })
        .await
        .map_err(|e| e.to_string())
}

pub async fn patch(session_id: String, req: PatchSessionRequest) -> Result<Session, String> {
    client().patch(&format!("/v1/sessions/{session_id}"), &req).await.map_err(|e| e.to_string())
}

pub async fn delete(session_id: String) -> Result<(), String> {
    client()
        .delete::<serde_json::Value>(&format!("/v1/sessions/{session_id}"))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub async fn mark_read(session_id: String) -> Result<(), String> {
    client()
        .post::<_, serde_json::Value>(&format!("/v1/sessions/{session_id}/read"), &())
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub async fn presence(focused: bool, session_id: Option<String>) -> Result<(), String> {
    client()
        .post::<_, serde_json::Value>("/v1/ui/presence", &PresenceRequest { focused, session_id })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub async fn reconnect(session: Session, mode: ReconnectMode) -> Result<String, String> {
    liaison_core::launch::reconnect(&client(), &session, mode).await
}
