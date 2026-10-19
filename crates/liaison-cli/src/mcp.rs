//! `liaison mcp`: the MCP server (stdio) that gives agents tools to talk to the user.
//!
//! Each harness starts one server per process. The session a tool call belongs to is found
//! from the harness process id (Claude Code: one session per process) or from the
//! `liaison_session` argument the opencode plugin fills in.

use liaison_core::{
    ClientError, Harness, HubClient, Message, MessageKind, NotifyLevel, QuestionStatus, Session,
    api::{AgentMessageRequest, MessagesResponse, PatchSessionRequest, QuestionRequest},
    client::encode_query,
    procs,
};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use std::time::Duration;

const INSTRUCTIONS: &str = "Liaison is the user's desktop messenger for AI agents. Each of your sessions has \
its own chat box in the Liaison app. Use chat_send to tell the user something, chat_notify when you finish \
something important or need attention, chat_ask to ask a question and wait for the answer, and chat_status \
to show what you are doing. Messages the user writes in Liaison reach you automatically, marked [Liaison]; \
chat_inbox fetches any that are still waiting and chat_wait waits for the next one.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendArgs {
    /// The message, in Markdown.
    pub text: String,
    #[schemars(description = "Internal: filled in automatically by the client. Leave empty.")]
    #[serde(default)]
    pub liaison_session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotifyArgs {
    /// Short headline shown in the desktop notification.
    pub title: String,
    /// Details, in Markdown.
    #[serde(default)]
    pub body: Option<String>,
    /// One of: info, success, warning, error. Defaults to info.
    #[serde(default)]
    pub level: Option<String>,
    #[schemars(description = "Internal: filled in automatically by the client. Leave empty.")]
    #[serde(default)]
    pub liaison_session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AskArgs {
    /// The question, in Markdown.
    pub question: String,
    /// Optional answers shown as buttons. The user can always type a free answer too.
    #[serde(default)]
    pub options: Vec<String>,
    /// How long to wait for the answer, in seconds (default 900).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[schemars(description = "Internal: filled in automatically by the client. Leave empty.")]
    #[serde(default)]
    pub liaison_session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitArgs {
    /// How long to wait for a message, in seconds (default 300, max 3600).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[schemars(description = "Internal: filled in automatically by the client. Leave empty.")]
    #[serde(default)]
    pub liaison_session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionOnlyArgs {
    #[schemars(description = "Internal: filled in automatically by the client. Leave empty.")]
    #[serde(default)]
    pub liaison_session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TextArgs {
    /// The text. Empty clears it.
    pub text: String,
    #[schemars(description = "Internal: filled in automatically by the client. Leave empty.")]
    #[serde(default)]
    pub liaison_session: Option<String>,
}

#[derive(Clone)]
pub struct LiaisonMcp {
    client: HubClient,
    harness: Harness,
    harness_pid: Option<i64>,
    #[allow(dead_code)] // read by the #[tool_handler] macro
    tool_router: ToolRouter<Self>,
}

fn tool_error(text: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(text.into())]))
}

fn ok(text: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(text.into())]))
}

fn describe(e: &ClientError) -> String {
    match e {
        ClientError::Unavailable(_) => {
            "Liaison is not running, so the user cannot be reached through it right now.".to_string()
        }
        other => format!("Liaison error: {other}"),
    }
}

fn render_inbox(messages: &[Message]) -> String {
    if messages.is_empty() {
        return "No new messages from the user.".into();
    }
    liaison_core::format_for_agent(messages)
}

#[tool_router]
impl LiaisonMcp {
    pub fn new(harness: Harness) -> Self {
        Self {
            client: HubClient::default(),
            harness,
            harness_pid: procs::harness_pid(),
            tool_router: Self::tool_router(),
        }
    }

    /// Finds the Liaison session this call belongs to.
    async fn session(&self, explicit: Option<&str>) -> Result<Session, String> {
        let explicit = explicit.map(str::trim).filter(|s| !s.is_empty() && *s != "null");
        let result = match (explicit, self.harness_pid) {
            (Some(ext), _) => {
                let path = format!(
                    "/v1/resolve?harness={}&external_id={}",
                    self.harness.as_str(),
                    encode_query(ext)
                );
                self.client.get::<Session>(&path).await
            }
            (None, Some(pid)) => self.client.get::<Session>(&format!("/v1/resolve?pid={pid}")).await,
            (None, None) => return Err("could not identify the agent process".into()),
        };
        result.map_err(|e| {
            if e.is_not_found() {
                "This session is not registered with Liaison yet (are the Liaison hooks/plugin installed? \
                 run `liaison connect`)."
                    .to_string()
            } else {
                describe(&e)
            }
        })
    }

    async fn post(&self, session: &Session, kind: MessageKind, body: String, meta: serde_json::Value) -> Result<(), String> {
        self.client
            .post::<_, Message>(
                &format!("/v1/sessions/{}/messages", session.id),
                &AgentMessageRequest { kind, body, meta },
            )
            .await
            .map(|_| ())
            .map_err(|e| describe(&e))
    }

    #[tool(description = "Send a message to the user's Liaison chat for this session. Use it to report \
        progress, results, or anything the user should read even when they are away from the terminal.")]
    async fn chat_send(&self, Parameters(args): Parameters<SendArgs>) -> Result<CallToolResult, McpError> {
        let session = match self.session(args.liaison_session.as_deref()).await {
            Ok(s) => s,
            Err(e) => return tool_error(e),
        };
        match self.post(&session, MessageKind::Text, args.text, serde_json::Value::Null).await {
            Ok(()) => ok("Sent."),
            Err(e) => tool_error(e),
        }
    }

    #[tool(description = "Send the user a desktop notification (also shown highlighted in the chat). Use it \
        when you finish a task, hit a blocker, or need the user's attention.")]
    async fn chat_notify(&self, Parameters(args): Parameters<NotifyArgs>) -> Result<CallToolResult, McpError> {
        let session = match self.session(args.liaison_session.as_deref()).await {
            Ok(s) => s,
            Err(e) => return tool_error(e),
        };
        let level: NotifyLevel = args.level.as_deref().unwrap_or("info").parse().unwrap_or(NotifyLevel::Info);
        let body = match args.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            Some(b) => format!("**{}**\n\n{b}", args.title.trim()),
            None => format!("**{}**", args.title.trim()),
        };
        let meta = serde_json::json!({ "title": args.title.trim(), "level": level.as_str() });
        match self.post(&session, MessageKind::Notify, body, meta).await {
            Ok(()) => ok("Notification sent."),
            Err(e) => tool_error(e),
        }
    }

    #[tool(description = "Ask the user a question in Liaison and wait for the answer. Offer `options` for \
        quick replies. Returns the user's answer, or says that nobody answered in time.")]
    async fn chat_ask(&self, Parameters(args): Parameters<AskArgs>) -> Result<CallToolResult, McpError> {
        let session = match self.session(args.liaison_session.as_deref()).await {
            Ok(s) => s,
            Err(e) => return tool_error(e),
        };
        let timeout = args.timeout_secs.unwrap_or(900).clamp(10, 3600);
        let question = self
            .client
            .post::<_, liaison_core::Question>(
                &format!("/v1/sessions/{}/questions", session.id),
                &QuestionRequest {
                    kind: MessageKind::Question,
                    prompt: args.question,
                    options: args.options,
                    meta: serde_json::Value::Null,
                    timeout_secs: Some(timeout),
                },
            )
            .await;
        let question = match question {
            Ok(q) => q,
            Err(e) => return tool_error(describe(&e)),
        };
        let answered = self
            .client
            .get_long::<liaison_core::Question>(
                &format!("/v1/questions/{}/wait?timeout={}", question.id, timeout + 2),
                Duration::from_secs(timeout + 30),
            )
            .await;
        match answered {
            Ok(q) if q.status == QuestionStatus::Answered => {
                ok(format!("The user answered: {}", q.answer.unwrap_or_default()))
            }
            Ok(_) => ok(format!("The user did not answer within {timeout} seconds.")),
            Err(e) => tool_error(describe(&e)),
        }
    }

    #[tool(description = "Fetch messages the user sent from Liaison that you have not received yet.")]
    async fn chat_inbox(&self, Parameters(args): Parameters<SessionOnlyArgs>) -> Result<CallToolResult, McpError> {
        let session = match self.session(args.liaison_session.as_deref()).await {
            Ok(s) => s,
            Err(e) => return tool_error(e),
        };
        match self.client.post::<_, MessagesResponse>(&format!("/v1/sessions/{}/take", session.id), &()).await {
            Ok(r) => ok(render_inbox(&r.messages)),
            Err(e) => tool_error(describe(&e)),
        }
    }

    #[tool(description = "Wait until the user sends a message from Liaison, then return it. Use it when you \
        have nothing left to do and should stay available for the user's next instruction.")]
    async fn chat_wait(&self, Parameters(args): Parameters<WaitArgs>) -> Result<CallToolResult, McpError> {
        let session = match self.session(args.liaison_session.as_deref()).await {
            Ok(s) => s,
            Err(e) => return tool_error(e),
        };
        let timeout = args.timeout_secs.unwrap_or(300).clamp(1, 3600);
        let result = self
            .client
            .get_long::<MessagesResponse>(
                &format!("/v1/sessions/{}/wait?timeout={timeout}", session.id),
                Duration::from_secs(timeout + 30),
            )
            .await;
        match result {
            Ok(r) if r.messages.is_empty() => ok(format!("No message from the user in {timeout} seconds.")),
            Ok(r) => ok(render_inbox(&r.messages)),
            Err(e) => tool_error(describe(&e)),
        }
    }

    #[tool(description = "Show a short status line (what you are doing right now) in the Liaison chat header. \
        An empty text clears it.")]
    async fn chat_status(&self, Parameters(args): Parameters<TextArgs>) -> Result<CallToolResult, McpError> {
        self.patch(args.liaison_session.as_deref(), PatchSessionRequest {
            status_text: Some(args.text),
            ..Default::default()
        })
        .await
    }

    #[tool(description = "Rename this session's chat box in Liaison (a short title for the task).")]
    async fn chat_set_title(&self, Parameters(args): Parameters<TextArgs>) -> Result<CallToolResult, McpError> {
        self.patch(args.liaison_session.as_deref(), PatchSessionRequest { title: Some(args.text), ..Default::default() })
            .await
    }

    async fn patch(&self, explicit: Option<&str>, req: PatchSessionRequest) -> Result<CallToolResult, McpError> {
        let session = match self.session(explicit).await {
            Ok(s) => s,
            Err(e) => return tool_error(e),
        };
        match self.client.patch::<_, Session>(&format!("/v1/sessions/{}?source=agent", session.id), &req).await {
            Ok(_) => ok("Updated."),
            Err(e) => tool_error(describe(&e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for LiaisonMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("liaison", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

pub async fn run(harness: Harness) -> anyhow::Result<()> {
    let service = LiaisonMcp::new(harness).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
