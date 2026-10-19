//! `~/.config/liaison/config.toml`, shared by the hub, the CLI and the app.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub notifications: Notifications,
    pub timeouts: Timeouts,
    pub permissions: Permissions,
    pub commands: Commands,
    pub desktop: Desktop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Desktop {
    /// `system`, `light` or `dark`.
    pub theme: String,
    /// UI font family (a static, non-variable font); Noto Sans, DejaVu Sans or Liberation Sans when unset.
    pub font: Option<String>,
    /// Scales the whole window (1.0 = 100%).
    pub zoom: f32,
    /// Size of chat text in pixels; the rest of the text scales with it.
    pub text_size: u16,
    /// Show the Liaison icon in the system tray (StatusNotifierItem), with a dot for unread messages.
    pub tray: bool,
}

impl Desktop {
    pub const ZOOM_RANGE: (f32, f32) = (0.8, 2.0);
    pub const TEXT_SIZE_RANGE: (u16, u16) = (12, 24);
    pub const DEFAULT_TEXT_SIZE: u16 = 16;
}

impl Default for Desktop {
    fn default() -> Self {
        Self { theme: "system".into(), font: None, zoom: 1.0, text_size: Self::DEFAULT_TEXT_SIZE, tray: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Permissions {
    /// When agents' permission prompts go to the app:
    /// `auto` — when nobody is at the agent's terminal (background session or listen mode on),
    /// `always`, or `never` (the terminal prompt only).
    pub route: String,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { route: "auto".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    pub enabled: bool,
    /// Notify for plain chat messages too, not only notifications/questions/permissions.
    pub messages: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self { enabled: true, messages: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Timeouts {
    /// How long a permission request waits for you before falling back to the terminal prompt.
    pub permission_secs: u64,
    /// Default wait for `chat_ask`.
    pub question_secs: u64,
    /// How long a session in listen mode stays parked when its turn ends.
    pub listen_secs: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { permission_secs: 120, question_secs: 900, listen_secs: 3000 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Commands {
    /// Absolute path to `claude`; recorded by `liaison connect claude-code`.
    pub claude: Option<String>,
    /// Absolute path to `opencode`; recorded by `liaison connect opencode`.
    pub opencode: Option<String>,
    /// Terminal used to reopen sessions. `{cwd}` is replaced by the session folder and the
    /// agent command is appended, e.g. `["kitty", "--directory", "{cwd}", "--"]`.
    pub terminal: Option<Vec<String>>,
}

impl Config {
    pub fn load() -> Config {
        Self::load_from(&crate::paths::config_path())
    }

    pub fn load_from(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("liaison: ignoring invalid {}: {e}", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = crate::paths::config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}
