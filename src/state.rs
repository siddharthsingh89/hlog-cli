//! Small local state next to config.toml (not secret): where you last read the feed, and the
//! once-a-day update check.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct State {
    /// API base -> timestamp (ms) of the newest feed line you've seen.
    #[serde(default)]
    pub feed_seen: BTreeMap<String, i64>,
    #[serde(default)]
    pub update_checked_ms: Option<i64>,
    #[serde(default)]
    pub latest_version: Option<String>,
}

pub fn path() -> Result<PathBuf, String> {
    Ok(crate::config::path()?.with_file_name("state.json"))
}

pub fn load() -> State {
    path().ok().and_then(|p| fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(s: &State) {
    if let Ok(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(body) = serde_json::to_string_pretty(s) {
            let _ = fs::write(p, body);
        }
    }
}

/// "0.10.2" > "0.9.9", ignoring a leading "v".
pub fn newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(latest) > parse(current)
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("0.3.0", "0.2.0"));
        assert!(!newer("v0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
    }
}
