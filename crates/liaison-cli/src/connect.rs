//! `liaison connect` / `liaison disconnect`: installs the MCP server, hooks and plugin into
//! each harness' own configuration.

use crate::hooks::HookEvent;
use anyhow::{Context, Result, bail};
use liaison_core::{config::Config, launch, paths};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

const OPENCODE_PLUGIN: &str = include_str!("../../../connectors/opencode/liaison.js");
const MCP_NAME: &str = "liaison";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Target {
    ClaudeCode,
    Opencode,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Target::ClaudeCode => "Claude Code",
            Target::Opencode => "opencode",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude",
            Target::Opencode => "opencode",
        }
    }
}

/// Targets whose program is installed.
pub fn detected() -> Vec<Target> {
    [Target::ClaudeCode, Target::Opencode]
        .into_iter()
        .filter(|t| launch::find_program(t.program(), None).is_ok())
        .collect()
}

fn liaison_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the liaison executable")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.contains([' ', '\'', '"']) { format!("'{}'", s.replace('\'', r"'\''")) } else { s.into_owned() }
}

pub fn connect(target: Target) -> Result<()> {
    let program = launch::find_program(target.program(), None)
        .map_err(|e| anyhow::anyhow!("{} is not installed: {e}", target.label()))?;
    let exe = liaison_exe()?;
    match target {
        Target::ClaudeCode => connect_claude(&exe, &program)?,
        Target::Opencode => connect_opencode(&exe)?,
    }
    // Remember where the agent lives so the app can restart sessions with a desktop PATH.
    let mut config = Config::load();
    let path = Some(program.to_string_lossy().into_owned());
    match target {
        Target::ClaudeCode => config.commands.claude = path,
        Target::Opencode => config.commands.opencode = path,
    }
    config.save().context("saving config.toml")?;
    println!("✓ {} connected to Liaison. Start a new session (or restart running ones) to use it.", target.label());
    Ok(())
}

pub fn disconnect(target: Target) -> Result<()> {
    match target {
        Target::ClaudeCode => disconnect_claude()?,
        Target::Opencode => disconnect_opencode()?,
    }
    println!("✓ {} disconnected from Liaison.", target.label());
    Ok(())
}

// ── Claude Code ─────────────────────────────────────────────────────────────

fn claude_settings_path() -> PathBuf {
    paths::home_dir().join(".claude").join("settings.json")
}

fn hook_timeout(event: HookEvent) -> u64 {
    match event {
        HookEvent::Stop => 3600,
        HookEvent::PermissionRequest => 900,
        HookEvent::SessionEnd => 5,
        _ => 15,
    }
}

fn is_liaison_hook(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|h| {
                h.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(" hook ") && c.contains("liaison"))
            })
        })
}

fn connect_claude(exe: &Path, claude: &Path) -> Result<()> {
    let path = claude_settings_path();
    let mut settings = read_json(&path)?;
    backup(&path)?;
    let root = settings.as_object_mut().context("settings.json is not a JSON object")?;

    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().context("settings.json `hooks` is not an object")?;
    for event in HookEvent::ALL {
        let groups = hooks.entry(event.claude_name()).or_insert_with(|| json!([]));
        let groups = groups.as_array_mut().context("hook list is not an array")?;
        groups.retain(|g| !is_liaison_hook(g));
        groups.push(json!({
            "hooks": [{
                "type": "command",
                "command": format!("{} hook {}", quote(exe), event.arg()),
                "timeout": hook_timeout(event),
            }]
        }));
    }

    // Let agents message the user without a permission prompt for every message.
    let permissions = root.entry("permissions").or_insert_with(|| json!({}));
    if let Some(permissions) = permissions.as_object_mut() {
        let allow = permissions.entry("allow").or_insert_with(|| json!([]));
        if let Some(allow) = allow.as_array_mut() {
            let rule = format!("mcp__{MCP_NAME}");
            if !allow.iter().any(|r| r.as_str() == Some(&rule)) {
                allow.push(json!(rule));
            }
        }
    }
    write_json(&path, &settings)?;
    println!("  hooks + permission rule → {}", path.display());

    // MCP servers live in ~/.claude.json; let Claude Code manage that file itself.
    let _ = std::process::Command::new(claude)
        .args(["mcp", "remove", "--scope", "user", MCP_NAME])
        .output();
    let out = std::process::Command::new(claude)
        .args(["mcp", "add", "--scope", "user", MCP_NAME, "--"])
        .arg(exe)
        .args(["mcp", "--harness", "claude-code"])
        .output()
        .context("running `claude mcp add`")?;
    if !out.status.success() {
        bail!("`claude mcp add` failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }
    println!("  MCP server `{MCP_NAME}` → claude mcp (user scope)");
    Ok(())
}

fn disconnect_claude() -> Result<()> {
    let path = claude_settings_path();
    if path.exists() {
        let mut settings = read_json(&path)?;
        backup(&path)?;
        if let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) {
            for event in HookEvent::ALL {
                if let Some(groups) = hooks.get_mut(event.claude_name()).and_then(Value::as_array_mut) {
                    groups.retain(|g| !is_liaison_hook(g));
                    if groups.is_empty() {
                        hooks.remove(event.claude_name());
                    }
                }
            }
            if hooks.is_empty() {
                settings.as_object_mut().map(|r| r.remove("hooks"));
            }
        }
        if let Some(allow) = settings.pointer_mut("/permissions/allow").and_then(Value::as_array_mut) {
            allow.retain(|r| r.as_str() != Some(&format!("mcp__{MCP_NAME}")));
        }
        write_json(&path, &settings)?;
        println!("  hooks removed from {}", path.display());
    }
    if let Ok(claude) = launch::find_program("claude", Config::load().commands.claude.as_deref()) {
        let _ = std::process::Command::new(claude)
            .args(["mcp", "remove", "--scope", "user", MCP_NAME])
            .output();
        println!("  MCP server `{MCP_NAME}` removed");
    }
    Ok(())
}

pub fn claude_connected() -> bool {
    read_json(&claude_settings_path())
        .ok()
        .and_then(|s| s.get("hooks").cloned())
        .and_then(|h| h.get("Stop").cloned())
        .and_then(|g| g.as_array().cloned())
        .is_some_and(|groups| groups.iter().any(is_liaison_hook))
}

// ── opencode ────────────────────────────────────────────────────────────────

fn opencode_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("opencode"),
        _ => paths::home_dir().join(".config").join("opencode"),
    }
}

fn opencode_config_path() -> PathBuf {
    let dir = opencode_dir();
    ["opencode.jsonc", "opencode.json"]
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.exists())
        .unwrap_or_else(|| dir.join("opencode.json"))
}

fn opencode_plugin_path() -> PathBuf {
    opencode_dir().join("plugins").join("liaison.js")
}

fn connect_opencode(exe: &Path) -> Result<()> {
    let path = opencode_config_path();
    let mut config = read_json(&path)?;
    backup(&path)?;
    let root = config.as_object_mut().context("opencode config is not a JSON object")?;
    root.entry("$schema").or_insert_with(|| json!("https://opencode.ai/config.json"));
    let mcp = root.entry("mcp").or_insert_with(|| json!({}));
    let mcp = mcp.as_object_mut().context("opencode `mcp` is not an object")?;
    mcp.insert(
        MCP_NAME.into(),
        json!({
            "type": "local",
            "command": [exe.to_string_lossy(), "mcp", "--harness", "opencode"],
            "enabled": true,
        }),
    );
    write_json(&path, &config)?;
    println!("  MCP server `{MCP_NAME}` → {}", path.display());

    let plugin = opencode_plugin_path();
    std::fs::create_dir_all(plugin.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(&plugin, OPENCODE_PLUGIN)?;
    println!("  plugin → {}", plugin.display());
    Ok(())
}

fn disconnect_opencode() -> Result<()> {
    let path = opencode_config_path();
    if path.exists() {
        let mut config = read_json(&path)?;
        if let Some(mcp) = config.get_mut("mcp").and_then(Value::as_object_mut)
            && mcp.remove(MCP_NAME).is_some() {
                backup(&path)?;
                write_json(&path, &config)?;
                println!("  MCP server removed from {}", path.display());
            }
    }
    let plugin = opencode_plugin_path();
    if plugin.exists() {
        std::fs::remove_file(&plugin)?;
        println!("  plugin removed");
    }
    Ok(())
}

pub fn opencode_connected() -> bool {
    opencode_plugin_path().exists()
}

// ── JSON helpers ────────────────────────────────────────────────────────────

fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(text) => {
            let clean = strip_jsonc(&text);
            serde_json::from_str(&clean).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    let tmp = path.with_extension("liaison-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Keeps one copy of the file as it was before Liaison first touched it.
fn backup(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".before-liaison");
    let target = path.with_file_name(name);
    if !target.exists() {
        std::fs::copy(path, &target)?;
        println!("  backup → {}", target.display());
    }
    Ok(())
}

/// Removes `//` and `/* */` comments and trailing commas so JSONC parses as JSON.
pub fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
        } else if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == ',' {
            // Drop the comma when the next significant character closes the container.
            let mut j = i + 1;
            loop {
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'/') {
                    while j < chars.len() && chars[j] != '\n' {
                        j += 1;
                    }
                } else if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'*') {
                    j += 2;
                    while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
                        j += 1;
                    }
                    j += 2;
                } else {
                    break;
                }
            }
            if !matches!(chars.get(j), Some('}') | Some(']')) {
                out.push(c);
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_jsonc;

    #[test]
    fn strips_comments_and_trailing_commas() {
        let text = r#"{
            // comment
            "a": "x // not a comment", /* block */
            "b": [1, 2, // last
            ],
        }"#;
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(v["a"], "x // not a comment");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }
}
