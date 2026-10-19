//! Starting a disconnected agent session again (see PLAN.md §5 "Reconnect").
//!
//! The hub only decides *what* to deliver; the app or the CLI starts the process, because
//! they run inside your desktop session (PATH, Wayland display, …) while the hub runs under
//! systemd with a minimal environment.

use crate::{
    config::Config,
    model::{Harness, ReconnectMode, Session},
};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Directory added to PATH (the agent's own bin dir, e.g. for `#!/usr/bin/env node`).
    pub path_prepend: Option<PathBuf>,
    /// Wait for the command and report its output (`claude --bg` returns immediately).
    pub wait: bool,
    /// Where the output of a detached command goes.
    pub log: Option<PathBuf>,
}

impl LaunchPlan {
    /// The command line, shell-quoted, for display.
    pub fn display(&self) -> String {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().cloned())
            .map(|a| shell_quote(&a))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

const DEFAULT_PROMPT: &str = "You were reconnected from the Liaison app. Say briefly that you are back online and \
     ready; the user will write to you through Liaison.";

/// Builds the command that resumes `session`.
pub fn plan(session: &Session, mode: ReconnectMode, prompt: Option<&str>, config: &Config) -> Result<LaunchPlan, String> {
    let cwd = session.cwd.as_deref().map(PathBuf::from).filter(|p| p.is_dir());
    let ext = session.external_id.clone();
    let (program, mut agent_args) = match session.harness {
        Harness::ClaudeCode => (find_program("claude", config.commands.claude.as_deref())?, vec![]),
        Harness::Opencode => (find_program("opencode", config.commands.opencode.as_deref())?, vec![]),
        Harness::Custom => return Err("custom sessions cannot be restarted by Liaison".into()),
    };
    let path_prepend = program.parent().map(Path::to_path_buf);
    let prompt = prompt.map(str::to_owned);

    match (session.harness, mode) {
        (Harness::ClaudeCode, ReconnectMode::Background) => {
            agent_args.extend(["--bg".into(), "--resume".into(), ext]);
            agent_args.push(prompt.unwrap_or_else(|| DEFAULT_PROMPT.into()));
            Ok(LaunchPlan { program, args: agent_args, cwd, path_prepend, wait: true, log: None })
        }
        (Harness::ClaudeCode, ReconnectMode::Terminal) => {
            agent_args.extend(["--resume".into(), ext]);
            agent_args.extend(prompt);
            in_terminal(program, agent_args, cwd, path_prepend, config)
        }
        (Harness::Opencode, ReconnectMode::Background) => {
            agent_args.extend(["run".into(), "--session".into(), ext]);
            if let Some(dir) = &cwd {
                agent_args.extend(["--dir".into(), dir.to_string_lossy().into_owned()]);
            }
            agent_args.push(prompt.unwrap_or_else(|| DEFAULT_PROMPT.into()));
            let log = crate::paths::data_dir().join("logs").join(format!("{}.log", session.id));
            Ok(LaunchPlan { program, args: agent_args, cwd, path_prepend, wait: false, log: Some(log) })
        }
        (Harness::Opencode, ReconnectMode::Terminal) => {
            if let Some(dir) = &cwd {
                agent_args.push(dir.to_string_lossy().into_owned());
            }
            agent_args.extend(["--session".into(), ext]);
            if let Some(p) = prompt {
                agent_args.extend(["--prompt".into(), p]);
            }
            in_terminal(program, agent_args, cwd, path_prepend, config)
        }
        (Harness::Custom, _) => unreachable!(),
    }
}

/// Starts the plan. For `wait` plans, returns the combined output.
pub fn spawn(plan: &LaunchPlan) -> Result<String, String> {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args).stdin(Stdio::null());
    if let Some(dir) = &plan.cwd {
        cmd.current_dir(dir);
    }
    if let Some(extra) = &plan.path_prepend {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![extra.clone()];
        paths.extend(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    // Detach from our process group so closing the app does not take the agent with it.
    cmd.process_group(0);

    if plan.wait {
        let out = cmd.output().map_err(|e| format!("could not start {}: {e}", plan.program.display()))?;
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if out.status.success() {
            Ok(text.trim().to_string())
        } else {
            Err(format!("{} exited with {}: {}", plan.program.display(), out.status, text.trim()))
        }
    } else {
        match &plan.log {
            Some(log) => {
                if let Some(dir) = log.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log)
                    .map_err(|e| format!("could not open {}: {e}", log.display()))?;
                let err = file.try_clone().map_err(|e| e.to_string())?;
                cmd.stdout(file).stderr(err);
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", plan.program.display()))?;
        // Reap the child when it exits so it never lingers as a zombie.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(String::new())
    }
}

fn in_terminal(
    program: PathBuf,
    agent_args: Vec<String>,
    cwd: Option<PathBuf>,
    path_prepend: Option<PathBuf>,
    config: &Config,
) -> Result<LaunchPlan, String> {
    let dir = cwd
        .clone()
        .unwrap_or_else(crate::paths::home_dir)
        .to_string_lossy()
        .into_owned();
    let mut command: Vec<String> = match &config.commands.terminal {
        Some(template) if !template.is_empty() => template.iter().map(|a| a.replace("{cwd}", &dir)).collect(),
        _ => detect_terminal(&dir).ok_or("no terminal emulator found; set [commands] terminal in config.toml")?,
    };
    command.push(program.to_string_lossy().into_owned());
    command.extend(agent_args);
    let terminal = command.remove(0);
    let terminal = which(&terminal).unwrap_or_else(|| PathBuf::from(&terminal));
    Ok(LaunchPlan { program: terminal, args: command, cwd, path_prepend, wait: false, log: None })
}

fn detect_terminal(cwd: &str) -> Option<Vec<String>> {
    let preferred = std::env::var("TERMINAL").ok().filter(|t| !t.is_empty());
    let candidates = ["kitty", "alacritty", "foot", "wezterm", "ghostty", "konsole", "gnome-terminal", "xterm"];
    let name = preferred
        .into_iter()
        .chain(candidates.iter().map(|s| s.to_string()))
        .find(|t| which(t).is_some())?;
    let base = Path::new(&name).file_name()?.to_string_lossy().into_owned();
    let args: Vec<String> = match base.as_str() {
        "kitty" => vec!["--directory".into(), cwd.into(), "--".into()],
        "alacritty" => vec!["--working-directory".into(), cwd.into(), "-e".into()],
        "foot" => vec![format!("--working-directory={cwd}")],
        "wezterm" => vec!["start".into(), "--cwd".into(), cwd.into(), "--".into()],
        "ghostty" => vec![format!("--working-directory={cwd}"), "-e".into()],
        "konsole" => vec!["--workdir".into(), cwd.into(), "-e".into()],
        "gnome-terminal" => vec![format!("--working-directory={cwd}"), "--".into()],
        _ => vec!["-e".into()],
    };
    Some(std::iter::once(name).chain(args).collect())
}

/// Finds an agent program: the configured path, then PATH, then common install folders.
pub fn find_program(name: &str, configured: Option<&str>) -> Result<PathBuf, String> {
    if let Some(path) = configured.map(PathBuf::from).filter(|p| p.is_file()) {
        return Ok(path);
    }
    if let Some(path) = which(name) {
        return Ok(path);
    }
    let home = crate::paths::home_dir();
    for dir in [".local/bin", ".bun/bin", ".opencode/bin", ".npm-global/bin", ".cargo/bin"] {
        let path = home.join(dir).join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(format!("`{name}` not found; run `liaison connect` again from a terminal where `{name}` works"))
}

pub fn which(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.is_absolute() {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file())
}

fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// The whole reconnect flow: ask the hub how to start the agent, then start it.
/// Returns a description of what was started.
pub async fn reconnect(client: &crate::HubClient, session: &Session, mode: ReconnectMode) -> Result<String, String> {
    use crate::api::{ReconnectRequest, ReconnectTicket};

    let ticket: ReconnectTicket = client
        .post(&format!("/v1/sessions/{}/reconnect", session.id), &ReconnectRequest { mode })
        .await
        .map_err(|e| e.to_string())?;
    let plan = plan(&ticket.session, mode, ticket.prompt.as_deref(), &Config::load())?;
    let shown = plan.display();
    let output = tokio::task::spawn_blocking(move || spawn(&plan)).await.map_err(|e| e.to_string())??;
    Ok(if output.is_empty() { shown } else { format!("{shown}\n{output}") })
}
