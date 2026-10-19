//! liaison-hub: the background service that stores and routes messages between you and
//! your agents. Listens on a unix socket only.

mod db;
mod monitor;
mod notifier;
mod routes;
mod state;
mod tray;

use anyhow::{Context, Result};
use liaison_core::{config::Config, paths};
use std::os::unix::fs::PermissionsExt;

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("liaison-hub {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "liaison_hub=info".into()),
        )
        .with_target(false)
        .init();

    let socket = paths::socket_path();
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if socket.exists() {
        if tokio::net::UnixStream::connect(&socket).await.is_ok() {
            anyhow::bail!("another liaison-hub is already listening on {}", socket.display());
        }
        std::fs::remove_file(&socket)?;
    }

    let db_path = paths::db_path();
    let db = db::Db::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;
    // Waiters from a previous run are gone; their hooks re-park when they reconnect.
    let reset = db.reset_listening()?;
    if reset > 0 {
        tracing::info!("{reset} session(s) were listening before the restart; now idle until they wait again");
    }
    let hub = state::Hub::new(db, Config::load());
    notifier::start(hub.clone());
    monitor::start(hub.clone());
    tray::start(hub.clone());

    let listener = tokio::net::UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!("listening on {} (db: {})", socket.display(), db_path.display());

    let stopping = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = {
        let stopping = stopping.clone();
        async move {
            shutdown().await;
            stopping.notify_one();
        }
    };
    let server = axum::serve(listener, routes::router(hub)).with_graceful_shutdown(signal);
    // Long-polls (parked agents, the app's event stream) never finish on their own: give
    // in-flight requests a moment, then exit. Their clients reconnect to the next hub.
    tokio::select! {
        result = server => result?,
        _ = async {
            stopping.notified().await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        } => {}
    }
    let _ = std::fs::remove_file(&socket);
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[cfg(test)]
mod tests;
