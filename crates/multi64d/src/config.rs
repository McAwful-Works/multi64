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
    pub allow_origin: Option<Vec<String>>,
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
    pub allow_origin: Vec<String>,
    pub config_path: Option<PathBuf>,
}

/// Apply CLI/env values over file values. Every option **overrides** its file counterpart rather
/// than combining with it — `clear_serial` in particular must be turn-off-able from the command
/// line, which is why it arrives as `Option<bool>` and not a bare flag.
pub fn merge(
    serial: Option<String>,
    baud: Option<u32>,
    listen: Option<String>,
    clear_cli: Option<bool>,
    allow_origin_cli: Vec<String>,
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
    let clear_serial = clear_cli.or(file.clear_serial).unwrap_or(false);
    let allow_origin = if allow_origin_cli.is_empty() {
        file.allow_origin.unwrap_or_default()
    } else {
        allow_origin_cli
    };
    Ok(Resolved {
        serial,
        baud,
        listen,
        clear_serial,
        allow_origin,
        config_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_cfg() -> FileConfig {
        FileConfig {
            serial: Some("COM9".into()),
            baud: Some(9600),
            listen: Some("0.0.0.0:1".into()),
            clear_serial: Some(true),
            allow_origin: Some(vec!["https://from-file.example".into()]),
        }
    }

    #[test]
    fn file_values_apply_when_cli_is_absent() {
        let r = merge(None, None, None, None, vec![], file_cfg(), None).unwrap();
        assert_eq!(r.serial, "COM9");
        assert_eq!(r.baud, 9600);
        assert_eq!(r.listen, "0.0.0.0:1");
        assert!(r.clear_serial);
        assert_eq!(r.allow_origin, ["https://from-file.example"]);
    }

    #[test]
    fn cli_false_turns_off_clear_serial_from_file() {
        // `MULTI64D_CLEAR_SERIAL=false` with `clear_serial = true` in the config file: the spec
        // (§5.1) says CLI and environment override file values, so this must resolve to `false`.
        let r = merge(None, None, None, Some(false), vec![], file_cfg(), None).unwrap();
        assert!(!r.clear_serial);
    }

    #[test]
    fn cli_overrides_every_file_value() {
        let r = merge(
            Some("COM3".into()),
            Some(115200),
            Some("127.0.0.1:38765".into()),
            Some(true),
            vec!["https://from-cli.example".into()],
            file_cfg(),
            None,
        )
        .unwrap();
        assert_eq!(r.serial, "COM3");
        assert_eq!(r.baud, 115200);
        assert_eq!(r.listen, "127.0.0.1:38765");
        assert!(r.clear_serial);
        assert_eq!(r.allow_origin, ["https://from-cli.example"]);
    }

    #[test]
    fn defaults_apply_with_no_file_and_only_serial() {
        let r = merge(
            Some("COM3".into()),
            None,
            None,
            None,
            vec![],
            FileConfig::default(),
            None,
        )
        .unwrap();
        assert_eq!(r.baud, 115200);
        assert_eq!(r.listen, "127.0.0.1:38765");
        assert!(!r.clear_serial);
        assert!(r.allow_origin.is_empty());
    }

    #[test]
    fn missing_serial_is_an_error() {
        let err = merge(None, None, None, None, vec![], FileConfig::default(), None).unwrap_err();
        assert!(err.to_string().contains("missing serial"));
    }
}
