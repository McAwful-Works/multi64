//! Optional **TOML** config file and merge with **CLI** / **environment** variables.
//!
//! Precedence: explicit `--config` / `MULTI64D_CONFIG`, then `./multi64d.toml`, then OS config dir
//! (`multi64d/config.toml`). CLI flags and env vars override file values; see [`merge`] and **`docs/spec/daemon-api-v1.md`** §5.

use anyhow::Context;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Default, Clone)]
pub struct FileConfig {
    pub serial: Option<String>,
    pub baud: Option<u32>,
    pub listen: Option<String>,
    pub clear_serial: Option<bool>,
}

pub fn load_config_file(path: &Path) -> anyhow::Result<FileConfig> {
    let s =
        std::fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let c: FileConfig = toml::from_str(&s).with_context(|| format!("parse {}", path.display()))?;
    Ok(c)
}

/// Resolve config path: explicit `--config` / `MULTI64D_CONFIG`, else `./multi64d.toml`, else OS config dir.
pub fn resolve_config_path(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return if p.as_path().exists() { Some(p) } else { None };
    }
    let cwd = PathBuf::from("multi64d.toml");
    if cwd.exists() {
        return Some(cwd);
    }
    dirs::config_dir()
        .map(|d| d.join("multi64d").join("config.toml"))
        .filter(|p| p.exists())
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub serial: String,
    pub baud: u32,
    pub listen: String,
    pub clear_serial: bool,
    pub config_path: Option<PathBuf>,
}

pub fn merge(
    serial: Option<String>,
    baud: Option<u32>,
    listen: Option<String>,
    clear_cli: bool,
    file: FileConfig,
    config_path: Option<PathBuf>,
) -> anyhow::Result<Resolved> {
    let serial = serial
        .or(file.serial)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing serial: use --serial, environment MULTI64D_SERIAL, or `serial` in a config file"
            )
        })?;
    let baud = baud.or(file.baud).unwrap_or(115200);
    let listen = listen
        .or(file.listen)
        .unwrap_or_else(|| "127.0.0.1:38765".to_string());
    let clear_serial = clear_cli || file.clear_serial.unwrap_or(false);
    Ok(Resolved {
        serial,
        baud,
        listen,
        clear_serial,
        config_path,
    })
}
