//! Background checks: notices dead harness processes, deleted transcripts and expired
//! questions, so the UI always shows the truth even when no hook fired.

use crate::state::SharedHub;
use liaison_core::{Harness, SessionStatus, procs};
use std::time::Duration;

pub fn start(hub: SharedHub) {
    tokio::spawn(async move {
        let mut tick: u64 = 0;
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            if let Err(e) = check(&hub, tick.is_multiple_of(10)) {
                tracing::warn!("monitor: {e:?}");
            }
            tick = tick.wrapping_add(1);
        }
    });
}

fn check(hub: &SharedHub, slow: bool) -> Result<(), crate::state::HubError> {
    for session in hub.db.connected_sessions()? {
        if let Some(pid) = session.pid
            && !procs::is_alive(pid, session.pid_start) {
                tracing::info!("session {} lost its process {pid}", session.id);
                hub.mark_disconnected(&session, "process exited")?;
            }
    }
    if slow {
        for session in hub.db.sessions_with_status(SessionStatus::Disconnected)? {
            let gone = session.harness == Harness::ClaudeCode
                && session.transcript_path.as_deref().is_some_and(|p| !std::path::Path::new(p).exists());
            if gone {
                hub.mark_ended(&session, "transcript deleted")?;
            }
        }
    }
    hub.expire_questions()?;
    hub.check_reconnects()?;
    Ok(())
}
