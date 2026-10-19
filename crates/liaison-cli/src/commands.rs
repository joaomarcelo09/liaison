//! Terminal helpers: status, sessions, messages, send, reconnect, open, agent.

use crate::connect;
use anyhow::{Context, Result, bail};
use chrono::{Local, TimeZone};
use liaison_core::{
    Direction, Harness, HubClient, Message, MessageKind, QuestionStatus, ReconnectMode, Session, SessionStatus,
    api::*, launch, procs,
};
use std::time::Duration;

fn client() -> HubClient {
    HubClient::default()
}

fn time(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// Finds a session by id, id prefix, external id prefix or title.
async fn find(query: &str) -> Result<Session> {
    let all: SessionsResponse = client().get("/v1/sessions?archived=true").await?;
    let q = query.to_lowercase();
    let mut matches: Vec<Session> = all
        .sessions
        .iter()
        .filter(|s| s.id.to_lowercase() == q || s.external_id.to_lowercase() == q)
        .cloned()
        .collect();
    if matches.is_empty() {
        matches = all
            .sessions
            .into_iter()
            .filter(|s| {
                s.id.to_lowercase().starts_with(&q)
                    || s.external_id.to_lowercase().starts_with(&q)
                    || s.display_name().to_lowercase().contains(&q)
            })
            .collect();
    }
    match matches.len() {
        0 => bail!("no session matches `{query}`"),
        1 => Ok(matches.remove(0)),
        n => bail!(
            "{n} sessions match `{query}`: {}",
            matches.iter().map(|s| format!("{} ({})", s.display_name(), &s.id[..10])).collect::<Vec<_>>().join(", ")
        ),
    }
}

pub fn connect(target: Option<connect::Target>) -> Result<()> {
    let targets = match target {
        Some(t) => vec![t],
        None => {
            let found = connect::detected();
            if found.is_empty() {
                bail!("neither `claude` nor `opencode` was found in PATH");
            }
            found
        }
    };
    for t in targets {
        connect::connect(t)?;
    }
    Ok(())
}

pub async fn status() -> Result<()> {
    match client().get::<HealthResponse>("/v1/health").await {
        Ok(h) => {
            println!("hub        running (v{}) on {}", h.version, client().socket().display());
            println!("database   {}", h.db_path);
            println!("sessions   {} ({} connected)", h.sessions, h.connected);
            println!("app        {}", if h.ui_connected { "open" } else { "closed" });
        }
        Err(e) => {
            println!("hub        NOT running — {e}");
            println!("           start it with: systemctl --user enable --now liaison-hub");
        }
    }
    let mark = |b: bool| if b { "connected" } else { "not connected (run `liaison connect`)" };
    println!("claude     {}", mark(connect::claude_connected()));
    println!("opencode   {}", mark(connect::opencode_connected()));
    Ok(())
}

pub async fn sessions(all: bool) -> Result<()> {
    let r: SessionsResponse = client().get(&format!("/v1/sessions?archived={all}")).await?;
    if r.sessions.is_empty() {
        println!("No sessions yet. Start Claude Code or opencode after `liaison connect`.");
        return Ok(());
    }
    println!("{:<12} {:<12} {:<13} {:>6} {:>6}  NAME", "ID", "HARNESS", "STATUS", "UNREAD", "QUEUED");
    for s in r.sessions {
        println!(
            "{:<12} {:<12} {:<13} {:>6} {:>6}  {}",
            &s.id[..10],
            s.harness.as_str(),
            s.status.as_str(),
            s.unread,
            s.queued,
            s.display_name()
        );
    }
    Ok(())
}

pub async fn messages(query: &str, count: i64) -> Result<()> {
    let session = find(query).await?;
    let r: MessagesResponse = client().get(&format!("/v1/sessions/{}/messages?limit={count}", session.id)).await?;
    println!("── {} · {} · {}", session.display_name(), session.harness.label(), session.status);
    for m in r.messages {
        print_message(&m);
    }
    Ok(())
}

fn print_message(m: &Message) {
    let who = match (m.direction, m.kind) {
        (Direction::System, _) => "·".to_string(),
        (Direction::ToAgent, _) => format!("you [{}]", m.delivery),
        (Direction::FromAgent, MessageKind::Reply) => "agent (reply)".into(),
        (Direction::FromAgent, k) if k != MessageKind::Text => format!("agent ({k})"),
        _ => "agent".into(),
    };
    println!("{} {who}: {}", time(m.created_at), m.body.trim());
    if let Some(q) = &m.question {
        match q.status {
            QuestionStatus::Answered => println!("    → answered: {}", q.answer.as_deref().unwrap_or("")),
            QuestionStatus::Expired => println!("    → expired"),
            QuestionStatus::Pending => println!("    → waiting for an answer: {}", q.options.join(" / ")),
        }
    }
}

pub async fn send(query: &str, text: &str) -> Result<()> {
    let session = find(query).await?;
    let m: Message = client().post(&format!("/v1/sessions/{}/send", session.id), &SendRequest { text: text.into() }).await?;
    let note = match session.status {
        SessionStatus::Disconnected => " — the session is disconnected; `liaison reconnect` delivers it",
        SessionStatus::Idle if session.harness == Harness::ClaudeCode => " — delivered on the session's next turn",
        _ => "",
    };
    println!("queued message #{}{note}", m.id);
    Ok(())
}

pub async fn reconnect(query: &str, terminal: bool) -> Result<()> {
    let session = find(query).await?;
    let mode = if terminal { ReconnectMode::Terminal } else { ReconnectMode::Background };
    let output = launch::reconnect(&client(), &session, mode).await.map_err(anyhow::Error::msg)?;
    println!("started: {output}");
    Ok(())
}

pub async fn open(query: Option<&str>) -> Result<()> {
    let session_id = match query {
        Some(q) => Some(find(q).await?.id),
        None => None,
    };
    let health: HealthResponse = client().get("/v1/health").await?;
    if health.ui_connected || session_id.is_some() {
        let _: serde_json::Value =
            client().post("/v1/ui/focus", &serde_json::json!({ "session_id": session_id })).await?;
    } else {
        std::process::Command::new("liaison-desktop").spawn().context("starting liaison-desktop")?;
    }
    Ok(())
}

// ── agent side, for your own scripts ────────────────────────────────────────

#[derive(clap::Subcommand)]
pub enum AgentCommand {
    /// Create (or reopen) a chat box; prints its session id. It disconnects when the
    /// calling process exits.
    Register {
        /// A stable name for this agent, e.g. "backup-script".
        name: String,
        /// Process to watch instead of the caller.
        #[arg(long)]
        pid: Option<i64>,
    },
    /// Send a message.
    Say { session: String, text: Vec<String> },
    /// Send a notification.
    Notify {
        session: String,
        title: String,
        body: Option<String>,
        #[arg(long, default_value = "info")]
        level: String,
    },
    /// Ask a question and print the answer (exit code 2 when unanswered).
    Ask {
        session: String,
        question: String,
        #[arg(long = "option")]
        options: Vec<String>,
        #[arg(long, default_value_t = 900)]
        timeout: u64,
    },
    /// Wait for the user's next message and print it (exit code 2 on timeout).
    Wait {
        session: String,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Mark the session disconnected (or ended with --gone).
    End {
        session: String,
        #[arg(long)]
        gone: bool,
    },
}

pub async fn agent(cmd: AgentCommand) -> Result<()> {
    let c = client();
    match cmd {
        AgentCommand::Register { name, pid } => {
            let pid = pid.or_else(|| Some(std::os::unix::process::parent_id() as i64));
            let session: Session = c
                .post(
                    "/v1/sessions/register",
                    &RegisterRequest {
                        harness: Harness::Custom,
                        external_id: name.clone(),
                        cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
                        title: Some(name),
                        pid: pid.filter(|p| procs::is_alive(*p, None)),
                        transcript_path: None,
                        source: None,
                    },
                )
                .await?;
            println!("{}", session.id);
        }
        AgentCommand::Say { session, text } => {
            let _: Message = c
                .post(
                    &format!("/v1/sessions/{session}/messages"),
                    &AgentMessageRequest {
                        kind: MessageKind::Text,
                        body: text.join(" "),
                        meta: serde_json::Value::Null,
                    },
                )
                .await?;
        }
        AgentCommand::Notify { session, title, body, level } => {
            let text = match body {
                Some(b) => format!("**{title}**\n\n{b}"),
                None => format!("**{title}**"),
            };
            let _: Message = c
                .post(
                    &format!("/v1/sessions/{session}/messages"),
                    &AgentMessageRequest {
                        kind: MessageKind::Notify,
                        body: text,
                        meta: serde_json::json!({ "title": title, "level": level }),
                    },
                )
                .await?;
        }
        AgentCommand::Ask { session, question, options, timeout } => {
            let q: liaison_core::Question = c
                .post(
                    &format!("/v1/sessions/{session}/questions"),
                    &QuestionRequest {
                        kind: MessageKind::Question,
                        prompt: question,
                        options,
                        meta: serde_json::Value::Null,
                        timeout_secs: Some(timeout),
                    },
                )
                .await?;
            let q: liaison_core::Question = c
                .get_long(&format!("/v1/questions/{}/wait?timeout={}", q.id, timeout + 1), Duration::from_secs(timeout + 30))
                .await?;
            match q.answer {
                Some(a) if q.status == QuestionStatus::Answered => println!("{a}"),
                _ => std::process::exit(2),
            }
        }
        AgentCommand::Wait { session, timeout } => {
            let r: MessagesResponse = c
                .get_long(&format!("/v1/sessions/{session}/wait?timeout={timeout}"), Duration::from_secs(timeout + 30))
                .await?;
            if r.messages.is_empty() {
                std::process::exit(2);
            }
            for m in r.messages {
                println!("{}", m.body);
            }
        }
        AgentCommand::End { session, gone } => {
            let _: Session = c
                .post(&format!("/v1/sessions/{session}/end"), &EndRequest { reason: "ended by agent".into(), gone })
                .await?;
        }
    }
    Ok(())
}
