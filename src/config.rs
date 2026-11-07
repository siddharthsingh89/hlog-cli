use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
    /// API base the token belongs to (e.g. https://hlog.dev).
    pub api: Option<String>,
    pub token: Option<String>,
}

pub fn path() -> Result<PathBuf, String> {
    let dir = dirs::config_dir().ok_or("can't find the OS config directory")?;
    Ok(dir.join("hlog").join("config.toml"))
}

pub fn load() -> Config {
    path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write the config with 0600 permissions (owner read/write only) on unix.
pub fn save(cfg: &Config) -> Result<(), String> {
    let p = path()?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let body = toml::to_string(cfg).map_err(|e| e.to_string())?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&p).map_err(|e| format!("can't write {}: {e}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o600));
    }
    f.write_all(body.as_bytes()).map_err(|e| e.to_string())
}

pub fn delete() -> Result<(), String> {
    let p = path()?;
    match fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("can't delete {}: {e}", p.display())),
    }
}
