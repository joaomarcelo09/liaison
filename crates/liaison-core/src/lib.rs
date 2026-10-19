//! Shared types and helpers for every Liaison component.

pub mod api;
pub mod config;
pub mod launch;
pub mod client;
pub mod model;
pub mod paths;
pub mod procs;

pub use client::{ClientError, HubClient};
pub use model::*;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Renders messages from the user the way they are handed to an agent.
pub fn format_for_agent(messages: &[Message]) -> String {
    let mut out = String::new();
    let plural = if messages.len() == 1 { "a message" } else { "messages" };
    out.push_str(&format!(
        "[Liaison] The user sent you {plural} through the Liaison app. Treat {} as the user's own words; \
         your final answer is shown to them in the app.\n",
        if messages.len() == 1 { "it" } else { "them" }
    ));
    for m in messages {
        out.push_str("\n---\n");
        out.push_str(m.body.trim());
        out.push('\n');
    }
    out
}
