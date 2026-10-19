//! Behaviour tests for the hub's message routing rules.

use crate::{
    db::Db,
    state::{Hub, HubError, SharedHub},
};
use liaison_core::{
    Delivery, Direction, Harness, MessageKind, QuestionStatus, ReconnectMode, SessionStatus, api::*, config::Config,
};
use std::time::Duration;

fn hub() -> SharedHub {
    let mut config = Config::default();
    config.notifications.enabled = false;
    Hub::new(Db::open(std::path::Path::new(":memory:")).unwrap(), config)
}

fn register(hub: &Hub, harness: Harness, ext: &str, pid: Option<i64>) -> liaison_core::Session {
    hub.register(&RegisterRequest {
        harness,
        external_id: ext.into(),
        cwd: Some("/tmp/project".into()),
        title: None,
        pid,
        transcript_path: None,
        source: Some("startup".into()),
    })
    .unwrap()
}

fn say(hub: &Hub, id: &str, kind: MessageKind, body: &str) {
    hub.agent_message(id, &AgentMessageRequest { kind, body: body.into(), meta: serde_json::Value::Null }).unwrap();
}

#[test]
fn register_is_idempotent_and_keeps_the_same_chat() {
    let hub = hub();
    let a = register(&hub, Harness::ClaudeCode, "abc", None);
    let b = register(&hub, Harness::ClaudeCode, "abc", None);
    assert_eq!(a.id, b.id);
    let other = register(&hub, Harness::Opencode, "abc", None);
    assert_ne!(a.id, other.id, "the same id in another harness is another session");
    assert_eq!(hub.db.sessions(true).unwrap().len(), 2);
}

#[test]
fn messages_queue_until_taken_and_are_taken_once() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    hub.send_from_user(&s.id, "first").unwrap();
    hub.send_from_user(&s.id, "second").unwrap();
    assert_eq!(hub.session(&s.id).unwrap().queued, 2);

    let taken = hub.take(&s.id).unwrap();
    assert_eq!(taken.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    assert!(taken.iter().all(|m| m.delivery == Delivery::Delivered));
    assert!(hub.take(&s.id).unwrap().is_empty());
    let s = hub.session(&s.id).unwrap();
    assert_eq!(s.queued, 0);
    assert!(s.awaiting_reply);
}

#[test]
fn messages_to_a_disconnected_session_are_kept() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    hub.end(&s.id, &EndRequest { reason: "exit".into(), gone: false }).unwrap();
    assert_eq!(hub.session(&s.id).unwrap().status, SessionStatus::Disconnected);
    hub.send_from_user(&s.id, "while away").unwrap();
    // The agent comes back (resume) and receives it.
    let back = register(&hub, Harness::ClaudeCode, "s1", None);
    assert_eq!(back.id, s.id);
    assert_eq!(back.status, SessionStatus::Idle);
    assert_eq!(hub.take(&s.id).unwrap()[0].body, "while away");
}

#[test]
fn ended_sessions_are_read_only_and_fail_queued_messages() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    let queued = hub.send_from_user(&s.id, "never delivered").unwrap();
    hub.end(&s.id, &EndRequest { reason: "context deleted".into(), gone: true }).unwrap();

    let s = hub.session(&s.id).unwrap();
    assert_eq!(s.status, SessionStatus::Ended);
    assert_eq!(hub.db.message(queued.id).unwrap().unwrap().delivery, Delivery::Undeliverable);
    assert!(matches!(hub.send_from_user(&s.id, "hello?"), Err(HubError::Gone(_))));
    assert!(matches!(
        hub.prepare_reconnect(&s.id, &ReconnectRequest { mode: ReconnectMode::Background }),
        Err(HubError::Gone(_))
    ));
}

#[test]
fn final_reply_is_mirrored_only_for_turns_started_from_the_app() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    let idle = |reply: &str| StateRequest { status: SessionStatus::Idle, last_reply: Some(reply.into()) };
    let replies = |hub: &Hub| {
        hub.messages(&s.id, None, 100).unwrap().into_iter().filter(|m| m.kind == MessageKind::Reply).count()
    };

    // A turn the user started in the terminal: not mirrored.
    hub.set_state(&s.id, &idle("terminal answer")).unwrap();
    assert_eq!(replies(&hub), 0);

    // A turn started by a Liaison message: mirrored once.
    hub.send_from_user(&s.id, "question from the app").unwrap();
    hub.take(&s.id).unwrap();
    hub.set_state(&s.id, &idle("app answer")).unwrap();
    assert_eq!(replies(&hub), 1);
    hub.set_state(&s.id, &idle("next terminal answer")).unwrap();
    assert_eq!(replies(&hub), 1);

    // The agent already answered through chat_send: no duplicate.
    hub.send_from_user(&s.id, "another").unwrap();
    hub.take(&s.id).unwrap();
    say(&hub, &s.id, MessageKind::Text, "answered with chat_send");
    hub.set_state(&s.id, &idle("same answer again")).unwrap();
    assert_eq!(replies(&hub), 1);

    // "All replies" mirrors everything.
    hub.patch(&s.id, &PatchSessionRequest { mirror: Some(true), ..Default::default() }, true).unwrap();
    hub.set_state(&s.id, &idle("mirrored anyway")).unwrap();
    assert_eq!(replies(&hub), 2);
}

#[tokio::test]
async fn wait_returns_as_soon_as_the_user_writes() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    let waiter = {
        let hub = hub.clone();
        let id = s.id.clone();
        tokio::spawn(async move { hub.wait_messages(&id, Duration::from_secs(10)).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hub.session(&s.id).unwrap().status, SessionStatus::Listening);
    hub.send_from_user(&s.id, "ping").unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), waiter).await.unwrap().unwrap().unwrap();
    assert_eq!(got[0].body, "ping");
    assert_eq!(hub.session(&s.id).unwrap().status, SessionStatus::Active);
}

#[tokio::test]
async fn wait_times_out_back_to_idle() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    let got = hub.wait_messages(&s.id, Duration::from_millis(200)).await.unwrap();
    assert!(got.is_empty());
    assert_eq!(hub.session(&s.id).unwrap().status, SessionStatus::Idle);
}

#[tokio::test]
async fn questions_are_answered_once() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    let q = hub
        .ask(&s.id, &QuestionRequest {
            kind: MessageKind::Permission,
            prompt: "Allow Bash?".into(),
            options: vec!["Allow".into(), "Deny".into()],
            meta: serde_json::Value::Null,
            timeout_secs: Some(60),
        })
        .unwrap();
    let waiter = {
        let hub = hub.clone();
        let id = q.id.clone();
        tokio::spawn(async move { hub.wait_answer(&id, Duration::from_secs(10)).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    hub.answer(&q.id, "Allow").unwrap();
    let answered = waiter.await.unwrap().unwrap();
    assert_eq!(answered.status, QuestionStatus::Answered);
    assert_eq!(answered.answer.as_deref(), Some("Allow"));
    assert!(matches!(hub.answer(&q.id, "Deny"), Err(HubError::Conflict(_))));
}

#[test]
fn disconnecting_expires_pending_questions() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    let q = hub
        .ask(&s.id, &QuestionRequest {
            kind: MessageKind::Question,
            prompt: "Deploy?".into(),
            options: vec![],
            meta: serde_json::Value::Null,
            timeout_secs: None,
        })
        .unwrap();
    hub.end(&s.id, &EndRequest { reason: "exit".into(), gone: false }).unwrap();
    assert_eq!(hub.db.question(&q.id).unwrap().unwrap().status, QuestionStatus::Expired);
}

#[test]
fn reconnect_needs_a_disconnected_session_and_keeps_messages_queued() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    let req = ReconnectRequest { mode: ReconnectMode::Background };
    assert!(matches!(hub.prepare_reconnect(&s.id, &req), Err(HubError::Conflict(_))));

    hub.end(&s.id, &EndRequest { reason: "exit".into(), gone: false }).unwrap();
    let ticket = hub.prepare_reconnect(&s.id, &req).unwrap();
    assert!(ticket.prompt.is_none(), "nothing waiting, no prompt");
    assert!(ticket.session.listen, "background sessions keep listening");

    hub.send_from_user(&s.id, "hi").unwrap();
    let ticket = hub.prepare_reconnect(&s.id, &req).unwrap();
    assert!(ticket.prompt.unwrap().contains("1 message"));
    // Delivery happens only when the restarted agent takes them.
    assert_eq!(hub.session(&s.id).unwrap().queued, 1);

    let custom = register(&hub, Harness::Custom, "script", None);
    hub.end(&custom.id, &EndRequest { reason: "exit".into(), gone: false }).unwrap();
    assert!(matches!(hub.prepare_reconnect(&custom.id, &req), Err(HubError::BadRequest(_))));
}

#[tokio::test]
async fn pending_lists_idle_sessions_of_a_process() {
    let hub = hub();
    let pid = std::process::id() as i64;
    let a = register(&hub, Harness::Opencode, "a", Some(pid));
    let b = register(&hub, Harness::Opencode, "b", Some(pid));
    hub.set_state(&b.id, &StateRequest { status: SessionStatus::Active, last_reply: None }).unwrap();
    hub.send_from_user(&a.id, "for a").unwrap();
    hub.send_from_user(&b.id, "for busy b").unwrap();
    let pending = hub.pending_for_pid(pid, Duration::from_millis(100)).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].external_id, "a");
}

#[test]
fn unread_counts_agent_messages_until_read() {
    let hub = hub();
    let s = register(&hub, Harness::ClaudeCode, "s1", None);
    say(&hub, &s.id, MessageKind::Text, "one");
    say(&hub, &s.id, MessageKind::Notify, "two");
    assert_eq!(hub.session(&s.id).unwrap().unread, 2);
    hub.mark_read(&s.id).unwrap();
    assert_eq!(hub.session(&s.id).unwrap().unread, 0);
    let last = hub.messages(&s.id, None, 10).unwrap().pop().unwrap();
    assert_eq!(last.direction, Direction::FromAgent);
    assert!(last.read_at.is_some());
}
