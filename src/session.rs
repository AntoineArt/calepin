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
    /// First lines of the folded regions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folds: Vec<i32>,
    /// Column widths set by hand, per table (0: automatic).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub table_widths: Vec<Vec<i32>>,
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
    /// Middle click: fast scrolling (true) or paste.
    #[serde(default = "yes")]
    pub middle_scroll: bool,
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
            middle_scroll: true,
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
    let _ = std::fs::remove_file(history_path(id));
}

// ---------------------------------------------------------------------------
// Undo history, kept across restarts. Each state of a document is stored
// once under undo/<key>.md (written once, shared, never rewritten); a tab's
// small index lists its states and the current step.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct UndoIndex {
    pos: usize,
    /// (state key, caret offset), oldest first.
    steps: Vec<(String, i32)>,
}

fn undo_dir() -> PathBuf {
    dir().join("undo")
}

fn history_path(id: &str) -> PathBuf {
    tabs_dir().join(format!("{id}.undo.json"))
}

/// Content key of a state: its hash and length.
fn state_key(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}-{:x}", h.finish(), text.len())
}

pub fn save_history(id: &str, steps: &[(String, i32)], pos: usize) {
    // A single step: nothing to undo, no copy of the document to keep.
    if steps.len() < 2 {
        let _ = std::fs::remove_file(history_path(id));
        return;
    }
    let mut index = UndoIndex { pos, steps: Vec::with_capacity(steps.len()) };
    for (text, cursor) in steps {
        let key = state_key(text);
        let path = undo_dir().join(format!("{key}.md"));
        if !path.exists()
            && let Err(e) = write_atomic(&path, text.as_bytes())
        {
            eprintln!("calepin: cannot save undo history: {e}");
            return;
        }
        index.steps.push((key, *cursor));
    }
    if let Ok(json) = serde_json::to_vec(&index) {
        let _ = write_atomic(&history_path(id), &json);
    }
}

/// A tab's history: its states and current step. States whose file went
/// missing cut the history there (the older steps are dropped).
pub fn load_history(id: &str) -> Option<(Vec<(String, i32)>, usize)> {
    let bytes = std::fs::read(history_path(id)).ok()?;
    let index: UndoIndex = serde_json::from_slice(&bytes).ok()?;
    let mut steps = Vec::with_capacity(index.steps.len());
    let mut pos = index.pos;
    for (i, (key, cursor)) in index.steps.iter().enumerate() {
        match std::fs::read(undo_dir().join(format!("{key}.md"))).ok().and_then(|b| String::from_utf8(b).ok()) {
            Some(text) => steps.push((text, *cursor)),
            None if i <= index.pos => {
                steps.clear();
                pos = index.pos.saturating_sub(i + 1);
            }
            None => break,
        }
    }
    if steps.is_empty() {
        return None;
    }
    let pos = pos.min(steps.len() - 1);
    Some((steps, pos))
}

/// Removes content files that no tab or closed tab refers to.
pub fn gc(session: &Session) {
    let keep: std::collections::HashSet<String> = session
        .tabs
        .iter()
        .chain(session.closed.iter())
        .chain(session.detached.iter().flatten())
        .flat_map(|t| [format!("{}.md", t.id), format!("{}.undo.json", t.id)])
        .collect();
    let mut states = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(tabs_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !keep.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            } else if name.ends_with(".undo.json")
                && let Some(index) = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<UndoIndex>(&b).ok())
            {
                states.extend(index.steps.into_iter().map(|(k, _)| format!("{k}.md")));
            }
        }
    }
    // States no history refers to any more.
    if let Ok(entries) = std::fs::read_dir(undo_dir()) {
        for e in entries.flatten() {
            if !states.contains(&e.file_name().to_string_lossy().to_string()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub fn new_id() -> String {
    let now = glib::real_time();
    format!("{now:x}{:04x}", glib::random_int_range(0, 0xffff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_round_trip_and_gc() {
        let tmp = std::env::temp_dir().join(format!("calepin-test-{}", std::process::id()));
        // SAFETY: the only test touching the environment.
        unsafe { std::env::set_var("XDG_STATE_HOME", &tmp) };
        let steps = vec![("a".to_string(), 0), ("a b".to_string(), 3), ("a".to_string(), 1)];
        save_history("t1", &steps, 1);
        assert_eq!(load_history("t1"), Some((steps.clone(), 1)));
        // The repeated state is stored once.
        assert_eq!(std::fs::read_dir(undo_dir()).unwrap().count(), 2);
        // A lost older state drops the steps before it.
        let first = undo_dir().join(format!("{}.md", state_key("a b")));
        std::fs::remove_file(first).unwrap();
        assert_eq!(load_history("t1"), Some((vec![("a".to_string(), 1)], 0)));
        // A tab that no longer exists: its index and states go.
        save_history("t2", &[("x".to_string(), 0), ("xy".to_string(), 2)], 1);
        let session = Session { tabs: vec![TabState { id: "t2".into(), ..Default::default() }], ..load() };
        gc(&session);
        assert!(load_history("t1").is_none());
        assert!(load_history("t2").is_some());
        assert_eq!(std::fs::read_dir(undo_dir()).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
