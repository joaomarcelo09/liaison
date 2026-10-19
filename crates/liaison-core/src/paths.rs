//! Filesystem locations. Every path can be overridden with an environment variable,
//! which keeps tests isolated from the real installation.

use std::{os::unix::fs::MetadataExt, path::PathBuf};

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "liaison")
}

/// `$XDG_RUNTIME_DIR/liaison` (or `/tmp/liaison-<uid>`).
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LIAISON_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("liaison"),
        _ => {
            let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
            PathBuf::from(format!("/tmp/liaison-{uid}"))
        }
    }
}

/// The hub's unix socket.
pub fn socket_path() -> PathBuf {
    match std::env::var("LIAISON_SOCKET") {
        Ok(path) => PathBuf::from(path),
        Err(_) => runtime_dir().join("hub.sock"),
    }
}

/// `~/.local/share/liaison`
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LIAISON_DATA_DIR") {
        return PathBuf::from(dir);
    }
    project_dirs()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".liaison"))
}

/// `~/.config/liaison`
pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LIAISON_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    project_dirs()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".liaison"))
}

pub fn db_path() -> PathBuf {
    data_dir().join("liaison.db")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())))
}
