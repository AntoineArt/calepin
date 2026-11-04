//! Crash-safe session persistence: every tab's content lives under
//! `$XDG_STATE_HOME/calepin`, whether or not it was ever saved to a file.

use std::path::{Path, PathBuf};

use gtk::glib;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TabState {
    pub id: String,
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// true: rich Markdown editing; false: plain text / source view.
    #[serde(default = "yes")]
    pub markdown: bool,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub cursor: i32,
    /// Tables shown wide, by order in the document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wide_tables: Vec<usize>,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    #[serde(default)]
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub active: usize,
    /// Most recent last.
    #[serde(default)]
    pub closed: Vec<TabState>,
    #[serde(default = "yes")]
    pub minimap: bool,
    #[serde(default = "yes")]
    pub toolbar: bool,
    #[serde(default = "default_size")]
    pub font_size: f64,
    /// Tabs of detached windows, by window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detached: Vec<Vec<TabState>>,
}

fn default_size() -> f64 {
    12.0
}

impl Default for Session {
    fn default() -> Self {
        Session {
            tabs: Vec::new(),
            active: 0,
            closed: Vec::new(),
            minimap: true,
            toolbar: true,
            font_size: default_size(),
            detached: Vec::new(),
        }
    }
}

pub const MAX_CLOSED: usize = 30;

pub fn dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| glib::home_dir().join(".local/state"))
        .join("calepin")
}

fn tabs_dir() -> PathBuf {
    dir().join("tabs")
}

pub fn content_path(id: &str) -> PathBuf {
    tabs_dir().join(format!("{id}.md"))
}

pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, data)?;
    let f = std::fs::File::open(&tmp)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Moves the state of the former name (MyWriter) once.
pub fn migrate() {
    let new = dir();
    let old = new.with_file_name("mywriter");
    if !new.exists() && old.is_dir()
        && let Err(e) = std::fs::rename(&old, &new) {
            eprintln!("calepin: cannot migrate {}: {e}", old.display());
        }
}

pub fn load() -> Session {
    std::fs::read(dir().join("session.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(session: &Session) {
    if let Ok(json) = serde_json::to_vec_pretty(session)
        && let Err(e) = write_atomic(&dir().join("session.json"), &json)
    {
        eprintln!("calepin: cannot save session: {e}");
    }
}

pub fn save_content(id: &str, text: &str) {
    if let Err(e) = write_atomic(&content_path(id), text.as_bytes()) {
        eprintln!("calepin: cannot save tab {id}: {e}");
    }
}

pub fn load_content(id: &str) -> Option<String> {
    std::fs::read(content_path(id)).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub fn delete_content(id: &str) {
    let _ = std::fs::remove_file(content_path(id));
}

/// Removes content files that no tab or closed tab refers to.
pub fn gc(session: &Session) {
    let keep: std::collections::HashSet<String> = session
        .tabs
        .iter()
        .chain(session.closed.iter())
        .chain(session.detached.iter().flatten())
        .map(|t| format!("{}.md", t.id))
        .collect();
    if let Ok(entries) = std::fs::read_dir(tabs_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !keep.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub fn new_id() -> String {
    let now = glib::real_time();
    format!("{now:x}{:04x}", glib::random_int_range(0, 0xffff))
}
