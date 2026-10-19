//! `liaison`: the command line side of Liaison — MCP server and hooks for agents,
//! connectors for harnesses, and a few terminal helpers.

mod commands;
mod connect;
mod hooks;
mod mcp;

use clap::{Parser, Subcommand};
use liaison_core::Harness;

#[derive(Parser)]
#[command(name = "liaison", version, about = "Talk with your AI agents from the Liaison desktop app")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install Liaison into an agent harness (all installed ones when omitted).
    Connect { target: Option<connect::Target> },
    /// Remove Liaison from an agent harness.
    Disconnect { target: connect::Target },
    /// Show whether the hub is running and which harnesses are connected.
    Status,
    /// List sessions.
    Sessions {
        /// Include archived sessions.
        #[arg(long)]
        all: bool,
    },
    /// Show a session's messages.
    Messages {
        /// Session id, id prefix or title.
        session: String,
        #[arg(short = 'n', long, default_value_t = 20)]
        count: i64,
    },
    /// Send a message to a session as the user.
    Send {
        /// Session id, id prefix or title.
        session: String,
        text: Vec<String>,
    },
    /// Start a disconnected session again.
    Reconnect {
        /// Session id, id prefix or title.
        session: String,
        /// Open it in a terminal instead of the background.
        #[arg(long)]
        terminal: bool,
    },
    /// Open the desktop app (on a session, when given).
    Open { session: Option<String> },
    /// Talk to the user from your own scripts or agents.
    #[command(subcommand)]
    Agent(commands::AgentCommand),
    /// MCP server for agents (started by the harness).
    Mcp {
        #[arg(long, value_parser = parse_harness, default_value = "claude-code")]
        harness: Harness,
    },
    /// Claude Code hook (started by Claude Code).
    Hook { event: hooks::HookEvent },
}

fn parse_harness(s: &str) -> Result<Harness, String> {
    s.parse()
}

#[tokio::main]
async fn main() {
    // Exit quietly when the output pipe closes (`liaison sessions | head`) instead of panicking.
    // SAFETY: only changes the process-wide disposition of SIGPIPE back to the default.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Mcp { harness } => mcp::run(harness).await,
        Command::Hook { event } => hooks::run(event).await,
        Command::Connect { target } => commands::connect(target),
        Command::Disconnect { target } => connect::disconnect(target),
        Command::Status => commands::status().await,
        Command::Sessions { all } => commands::sessions(all).await,
        Command::Messages { session, count } => commands::messages(&session, count).await,
        Command::Send { session, text } => commands::send(&session, &text.join(" ")).await,
        Command::Reconnect { session, terminal } => commands::reconnect(&session, terminal).await,
        Command::Open { session } => commands::open(session.as_deref()).await,
        Command::Agent(cmd) => commands::agent(cmd).await,
    };
    if let Err(e) = result {
        eprintln!("liaison: {e:#}");
        std::process::exit(1);
    }
}
