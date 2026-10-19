//! Small `/proc` helpers used for liveness checks and session routing.

use std::fs;

/// Programs that sit between a harness and the processes it spawns.
const WRAPPERS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "env", "timeout", "nohup", "liaison"];

/// Fields of `/proc/<pid>/stat` after the `(comm)` field, so index 0 is field 3 (state).
fn stat_fields(pid: i64) -> Option<Vec<String>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    Some(rest.split_whitespace().map(str::to_owned).collect())
}

/// The process start time in clock ticks since boot (field 22), which together with the
/// PID identifies a process uniquely.
pub fn start_time(pid: i64) -> Option<i64> {
    stat_fields(pid)?.get(19)?.parse().ok()
}

pub fn parent_pid(pid: i64) -> Option<i64> {
    stat_fields(pid)?.get(1)?.parse().ok()
}

pub fn comm(pid: i64) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
}

/// True when `pid` is running and, if `start` is known, is still the same process.
pub fn is_alive(pid: i64, start: Option<i64>) -> bool {
    let Some(fields) = stat_fields(pid) else { return false };
    if fields.first().map(String::as_str) == Some("Z") {
        return false;
    }
    match start {
        Some(expected) => fields.get(19).and_then(|s| s.parse::<i64>().ok()) == Some(expected),
        None => true,
    }
}

/// The PID of the harness that spawned this process: the first ancestor that is not a
/// shell or other wrapper. Claude Code and opencode run hooks and MCP servers as children.
pub fn harness_pid() -> Option<i64> {
    let mut pid = std::os::unix::process::parent_id() as i64;
    for _ in 0..16 {
        if pid <= 1 {
            return None;
        }
        let name = comm(pid)?;
        if !WRAPPERS.contains(&name.as_str()) {
            return Some(pid);
        }
        pid = parent_pid(pid)?;
    }
    None
}

/// Whether the process reads from a terminal (someone can answer prompts there).
pub fn has_tty(pid: i64) -> bool {
    fs::read_link(format!("/proc/{pid}/fd/0"))
        .map(|p| {
            let p = p.to_string_lossy();
            p.starts_with("/dev/pts/") || p.starts_with("/dev/tty")
        })
        .unwrap_or(false)
}
