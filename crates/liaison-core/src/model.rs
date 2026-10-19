//! Domain model shared by the hub, the CLI and the desktop app.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok($name::$variant),)+
                    other => Err(format!("unknown {}: {other}", stringify!($name))),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum! {
    /// The program an agent session runs in.
    Harness {
        ClaudeCode => "claude-code",
        Opencode => "opencode",
        Custom => "custom",
    }
}

impl Harness {
    pub fn label(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Opencode => "opencode",
            Harness::Custom => "Custom",
        }
    }

    /// Whether the hub knows how to start this harness again for a stored session.
    pub fn can_reconnect(self) -> bool {
        matches!(self, Harness::ClaudeCode | Harness::Opencode)
    }
}

string_enum! {
    /// Where a session is in its lifecycle. See PLAN.md §5.
    SessionStatus {
        Active => "active",
        Idle => "idle",
        Listening => "listening",
        Disconnected => "disconnected",
        Ended => "ended",
    }
}

impl SessionStatus {
    /// The harness process is alive and attached to the hub.
    pub fn is_connected(self) -> bool {
        matches!(self, Self::Active | Self::Idle | Self::Listening)
    }

    /// Messages can still be queued for this session.
    pub fn accepts_messages(self) -> bool {
        self != Self::Ended
    }
}

string_enum! {
    Direction {
        ToAgent => "to_agent",
        FromAgent => "from_agent",
        System => "system",
    }
}

string_enum! {
    MessageKind {
        /// Plain chat text.
        Text => "text",
        /// The agent's final answer for a turn, mirrored automatically.
        Reply => "reply",
        /// A highlighted notification.
        Notify => "notify",
        /// A question waiting for an answer.
        Question => "question",
        /// A permission request waiting for Allow/Deny.
        Permission => "permission",
        /// A lifecycle event (connected, disconnected, …).
        Event => "event",
    }
}

string_enum! {
    Delivery {
        Queued => "queued",
        Delivered => "delivered",
        Read => "read",
        Undeliverable => "undeliverable",
    }
}

string_enum! {
    QuestionStatus {
        Pending => "pending",
        Answered => "answered",
        Expired => "expired",
    }
}

string_enum! {
    NotifyLevel {
        Info => "info",
        Success => "success",
        Warning => "warning",
        Error => "error",
    }
}

string_enum! {
    ReconnectMode {
        Background => "background",
        Terminal => "terminal",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub harness: Harness,
    pub external_id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
    pub pid: Option<i64>,
    pub pid_start: Option<i64>,
    pub status: SessionStatus,
    pub status_text: Option<String>,
    /// Keep the agent parked waiting for messages when its turn ends.
    pub listen: bool,
    /// Mirror every final reply into the chat, not only replies to app messages.
    pub mirror: bool,
    pub archived: bool,
    /// A message from the app was delivered and the next final reply should be mirrored.
    pub awaiting_reply: bool,
    pub ended_reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: i64,
    /// Agent messages the user has not read yet.
    #[serde(default)]
    pub unread: i64,
    /// User messages waiting for delivery.
    #[serde(default)]
    pub queued: i64,
    #[serde(default)]
    pub last_message: Option<String>,
    #[serde(default)]
    pub last_message_at: Option<i64>,
}

impl Session {
    /// A human-friendly name: the title, else the project folder, else the id.
    pub fn display_name(&self) -> String {
        if let Some(title) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return title.trim().to_string();
        }
        if let Some(project) = self.project() {
            return project;
        }
        format!("{} {}", self.harness.label(), short_id(&self.external_id))
    }

    /// The last path component of the working directory.
    pub fn project(&self) -> Option<String> {
        let cwd = self.cwd.as_deref()?;
        std::path::Path::new(cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| Some(cwd.to_string()))
    }
}

pub fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub session_id: String,
    pub direction: Direction,
    pub kind: MessageKind,
    pub body: String,
    #[serde(default)]
    pub meta: serde_json::Value,
    pub delivery: Delivery,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
    pub read_at: Option<i64>,
    /// The question attached to `question`/`permission` messages.
    #[serde(default)]
    pub question: Option<Question>,
}

impl Message {
    pub fn meta_str(&self, key: &str) -> Option<&str> {
        self.meta.get(key).and_then(|v| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub session_id: String,
    pub message_id: i64,
    pub kind: MessageKind,
    pub prompt: String,
    pub options: Vec<String>,
    pub answer: Option<String>,
    pub status: QuestionStatus,
    pub created_at: i64,
    pub answered_at: Option<i64>,
    pub expires_at: Option<i64>,
}

/// Streamed by `GET /v1/events`, one JSON object per line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Sent once when the stream opens.
    Hello { version: String },
    Session { session: Session },
    SessionRemoved { id: String },
    Message { message: Message },
    Question { question: Question },
    /// Ask a running UI to come to the front (e.g. notification clicked).
    Focus { session_id: Option<String> },
}
