//! Shared workspace persistence, independent of any presentation layer.
use crate::config::global_dir;
use std::path::{Path, PathBuf};

pub fn is_system_dir(p: &Path) -> bool {
    let raw = p.to_string_lossy().to_lowercase();
    let win = raw.replace('/', "\\");
    if win.starts_with("c:\\windows") || win.starts_with("c:\\program files") || win == "c:\\" {
        return true;
    }
    let unix = raw.replace('\\', "/");
    matches!(
        unix.as_str(),
        "/" | "/applications" | "/system" | "/library" | "/usr" | "/bin" | "/sbin"
    ) || unix.starts_with("/applications/")
        || unix.starts_with("/system/")
}

pub fn load_recent_projects() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_to_string(global_dir().join("recent_projects.json"))
        .ok().and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default().into_iter().map(PathBuf::from)
        .filter(|p| p.is_dir() && !is_system_dir(p)).collect();
    if out.len() <= 1 {
        if let Some(home) = dirs::home_dir() {
            if let Ok(text) = std::fs::read_to_string(home.join(".codex/config.toml")) {
                if let Ok(v) = toml::from_str::<toml::Value>(&text) {
                    if let Some(projects) = v.get("projects").and_then(|v| v.as_table()) {
                        for path in projects.keys() {
                            let p = PathBuf::from(path);
                            if p.is_dir() && !is_system_dir(&p) && !out.contains(&p) {
                                out.push(p);
                            }
                        }
                    }
                }
            }
        }
    }
    out.truncate(12);
    out
}

pub fn record_recent_project(cwd: &Path) {
    if !cwd.is_dir() || is_system_dir(cwd) { return; }
    let mut list = load_recent_projects();
    list.retain(|p| p != cwd);
    list.insert(0, cwd.to_path_buf());
    list.truncate(12);
    let _ = std::fs::create_dir_all(global_dir());
    if let Ok(text) = serde_json::to_string_pretty(&list) {
        let _ = std::fs::write(global_dir().join("recent_projects.json"), text);
    }
}
