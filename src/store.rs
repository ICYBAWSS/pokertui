//! Local config, identity and hand-history persistence.

use crate::crypto::Identity;
use crate::game::HandRecord;
use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn project() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "pokertui", "pokertui")
        .context("no home directory for config/history")
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = project()?.config_dir().to_path_buf();
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn data_dir() -> Result<PathBuf> {
    let dir = project()?.data_dir().to_path_buf();
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub name: Option<String>,
    pub broker: Option<String>,
}

impl Config {
    pub fn load() -> Config {
        let Ok(dir) = config_dir() else {
            return Config::default();
        };
        let path = dir.join("config.toml");
        let Ok(text) = fs::read_to_string(path) else {
            return Config::default();
        };
        toml_lite(&text)
    }
}

/// Tiny `key = "value"` reader so we do not need a TOML dependency.
fn toml_lite(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "name" if !v.is_empty() => cfg.name = Some(v),
            "broker" if !v.is_empty() => cfg.broker = Some(v),
            _ => {}
        }
    }
    cfg
}

pub fn save_config(cfg: &Config) -> Result<()> {
    let dir = config_dir()?;
    let mut out = String::from("# pokertui config\n");
    if let Some(name) = &cfg.name {
        out.push_str(&format!("name = \"{name}\"\n"));
    }
    if let Some(broker) = &cfg.broker {
        out.push_str(&format!("broker = \"{broker}\"\n"));
    }
    fs::write(dir.join("config.toml"), out)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    seed: String,
}

/// Load the persistent player identity, creating one on first run.
pub fn load_or_create_identity() -> Result<Identity> {
    let path = config_dir()?.join("identity.json");
    if let Ok(text) = fs::read_to_string(&path)
        && let Ok(file) = serde_json::from_str::<IdentityFile>(&text)
        && let Ok(id) = Identity::from_seed_b64(&file.seed)
    {
        return Ok(id);
    }
    let id = Identity::generate()?;
    let file = IdentityFile {
        seed: id.seed_b64(),
    };
    let text = serde_json::to_string_pretty(&file)?;
    fs::write(&path, text)?;
    restrict_permissions(&path)?;
    Ok(id)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn history_dir(code: &str) -> Result<PathBuf> {
    let dir = data_dir()?.join("history").join(code.replace('-', ""));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Append one finished hand as pretty JSON and return its path.
pub fn save_hand(code: &str, record: &HandRecord) -> Result<PathBuf> {
    let dir = history_dir(code)?;
    let path = dir.join(format!("hand-{:04}.json", record.hand_no));
    let text = serde_json::to_string_pretty(record)?;
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn load_hand(path: &Path) -> Result<HandRecord> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).context("not a pokertui hand-history file")
}

/// Write a small crash-recovery copy of the current table for the host.
pub fn save_session(code: &str, json: &str) -> Result<PathBuf> {
    let dir = data_dir()?.join("sessions");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", code.replace('-', "")));
    let mut f = fs::File::create(&path)?;
    f.write_all(json.as_bytes())?;
    Ok(path)
}

pub fn default_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "player".into())
}

pub fn require_hand_path(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    Ok(path.to_path_buf())
}
