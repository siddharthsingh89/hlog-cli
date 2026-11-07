//! Local queue for lines that couldn't be sent (offline, server trouble, rate limits, logged out).
//! Stored as JSON lines next to config.toml. Nothing typed into `hlog` is lost.
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Queued {
    /// API base the line is meant for (lines are only ever sent to that server).
    pub api: String,
    pub text: String,
    pub level: Option<String>,
    /// When it was typed, local time (shown to the user; the server still assigns the line's timestamp).
    pub queued_at: String,
}

pub fn path() -> Result<PathBuf, String> {
    Ok(crate::config::path()?.with_file_name("queue.jsonl"))
}

pub fn load_from(p: &PathBuf) -> Vec<Queued> {
    fs::read_to_string(p)
        .map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

pub fn load() -> Vec<Queued> {
    path().map(|p| load_from(&p)).unwrap_or_default()
}

fn open_private(p: &PathBuf, append: bool) -> Result<fs::File, String> {
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let mut o = fs::OpenOptions::new();
    o.create(true).write(true);
    if append {
        o.append(true);
    } else {
        o.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(p).map_err(|e| format!("can't write {}: {e}", p.display()))
}

pub fn push_to(p: &PathBuf, q: &Queued) -> Result<(), String> {
    let mut f = open_private(p, true)?;
    let line = serde_json::to_string(q).map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())
}

pub fn push(q: &Queued) -> Result<(), String> {
    push_to(&path()?, q)
}

/// Replace the queue with `items` (deletes the file when empty).
pub fn save_to(p: &PathBuf, items: &[Queued]) -> Result<(), String> {
    if items.is_empty() {
        return match fs::remove_file(p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let mut f = open_private(p, false)?;
    for q in items {
        writeln!(f, "{}", serde_json::to_string(q).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn save(items: &[Queued]) -> Result<(), String> {
    save_to(&path()?, items)
}

pub fn now_local() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_load_save_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hlog-queue-test-{}", std::process::id()));
        let p = dir.join("queue.jsonl");
        let _ = fs::remove_file(&p);
        let a = Queued { api: "https://hlog.dev".into(), text: "one".into(), level: None, queued_at: "t1".into() };
        let b = Queued { api: "https://hlog.dev".into(), text: "two \"quoted\"\nnewline".into(), level: Some("LEARN".into()), queued_at: "t2".into() };
        push_to(&p, &a).unwrap();
        push_to(&p, &b).unwrap();
        assert_eq!(load_from(&p), vec![a.clone(), b.clone()]);
        save_to(&p, &[b.clone()]).unwrap();
        assert_eq!(load_from(&p), vec![b]);
        save_to(&p, &[]).unwrap();
        assert!(!p.exists());
        assert!(load_from(&p).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
