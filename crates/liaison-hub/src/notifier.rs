//! Desktop notifications (freedesktop, via D-Bus). Sent by the hub so they arrive even when
//! the app window is closed. Clicking one opens the app on that session; permission requests
//! can be answered straight from the notification.

use crate::state::{Hub, SharedHub};
use liaison_core::{Message, MessageKind, Session};
use notify_rust::{Notification, Timeout, Urgency};
use tokio::sync::mpsc;

pub struct Job {
    session: Session,
    message: Message,
}

static JOBS: std::sync::OnceLock<mpsc::UnboundedSender<Job>> = std::sync::OnceLock::new();

/// Queues a notification for an agent message, if the user wants one.
pub fn message(hub: &Hub, session: &Session, message: &Message) {
    let config = hub.config();
    if !config.notifications.enabled {
        return;
    }
    let important = matches!(message.kind, MessageKind::Notify | MessageKind::Question | MessageKind::Permission);
    let chatty = matches!(message.kind, MessageKind::Text | MessageKind::Reply);
    if !(important || (chatty && config.notifications.messages)) {
        return;
    }
    {
        let presence = hub.presence.lock().unwrap_or_else(|e| e.into_inner());
        if !important && presence.focused && presence.session_id.as_deref() == Some(session.id.as_str()) {
            return; // the user is looking at this chat already
        }
    }
    if let Some(tx) = JOBS.get() {
        let _ = tx.send(Job { session: session.clone(), message: message.clone() });
    }
}

pub fn start(hub: SharedHub) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    let _ = JOBS.set(tx);
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let hub = hub.clone();
            // Each notification waits for its action on its own thread (blocking D-Bus call).
            std::thread::spawn(move || show(hub, job));
        }
    });
}

fn show(hub: SharedHub, job: Job) {
    let Job { session, message } = job;
    let name = session.display_name();
    let (summary, urgency) = match message.kind {
        MessageKind::Permission => (format!("🔐 {name} needs permission"), Urgency::Critical),
        MessageKind::Question => (format!("❓ {name} asks"), Urgency::Critical),
        MessageKind::Notify => {
            let title = message.meta_str("title").unwrap_or("Notification");
            let icon = match message.meta_str("level") {
                Some("success") => "✅",
                Some("warning") => "⚠️",
                Some("error") => "❌",
                _ => "🔔",
            };
            (format!("{icon} {name} · {title}"), Urgency::Normal)
        }
        _ => (name.clone(), Urgency::Normal),
    };
    let body = truncate(&message.body, 300);

    let mut n = Notification::new();
    n.appname("Liaison")
        .summary(&summary)
        .body(&body)
        .icon("liaison")
        .urgency(urgency)
        .action("default", "Open");
    let question = message.question.clone().filter(|q| q.status == liaison_core::QuestionStatus::Pending);
    if let Some(q) = &question {
        // Offer up to three answers directly in the notification.
        for option in q.options.iter().take(3) {
            n.action(&format!("answer:{option}"), option);
        }
        n.timeout(Timeout::Never);
    }

    let handle = match n.show() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("notification failed: {e}");
            return;
        }
    };
    let session_id = session.id.clone();
    handle.wait_for_action(|action| {
        if let Some(answer) = action.strip_prefix("answer:") {
            if let Some(q) = &question
                && let Err(e) = hub.answer(&q.id, answer) {
                    tracing::info!("answer from notification ignored: {e:?}");
                }
        } else if action == "default" {
            open_app(&hub, Some(&session_id));
        }
    });
}

/// Focuses the running app (on a session, if given), or starts it.
pub fn open_app(hub: &Hub, session_id: Option<&str>) {
    if hub.ui_connected() {
        hub.focus_ui(session_id.map(str::to_string));
        return;
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("liaison-desktop")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "liaison-desktop".into());
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(&exe);
    if let Some(id) = session_id {
        command.args(["--session", id]);
    }
    let result = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    match result {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::warn!("could not start {}: {e}", exe.display()),
    }
}

fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}
