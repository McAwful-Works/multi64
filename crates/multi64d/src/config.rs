//! Optional **TOML** config file and merge with **CLI** / **environment** variables.
//!
//! Precedence: explicit `--config` / `MULTI64D_CONFIG`, then `./multi64d.toml`, then OS config dir
//! (`multi64d/config.toml`). CLI flags and env vars override file values; see [`merge`] and **`docs/spec/daemon-api-v1.md`** §5.

use crate::CartKind;
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
    /// `"sc64"`, `"ed64"` or `"ed64pro"`; any other value is a parse error naming the accepted ones.
    pub cart: Option<CartKind>,
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
    pub cart: CartKind,
    pub config_path: Option<PathBuf>,
}

/// Values from the command line and environment. `None` (or an empty `allow_origin`) means "not
/// given", so the file value or the default applies instead.
#[derive(Debug, Default, Clone)]
pub struct CliConfig {
    pub serial: Option<String>,
    pub baud: Option<u32>,
    pub listen: Option<String>,
    pub clear_serial: Option<bool>,
    pub allow_origin: Vec<String>,
    pub cart: Option<CartKind>,
}

/// Apply CLI/env values over file values. Every option **overrides** its file counterpart rather
/// than combining with it — `clear_serial` in particular must be turn-off-able from the command
/// line, which is why it arrives as `Option<bool>` and not a bare flag.
pub fn merge(
    cli: CliConfig,
    file: FileConfig,
    config_path: Option<PathBuf>,
) -> anyhow::Result<Resolved> {
    let serial = cli
        .serial
        .or(file.serial)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing serial: use --serial, environment MULTI64D_SERIAL, or `serial` in a config file"
            )
        })?;
    let baud = cli.baud.or(file.baud).unwrap_or(115200);
    let listen = cli
        .listen
        .or(file.listen)
        .unwrap_or_else(|| "127.0.0.1:38765".to_string());
    let clear_serial = cli.clear_serial.or(file.clear_serial).unwrap_or(false);
    let allow_origin = if cli.allow_origin.is_empty() {
        file.allow_origin.unwrap_or_default()
    } else {
        cli.allow_origin
    };
    let cart = cli.cart.or(file.cart).unwrap_or_default();
    Ok(Resolved {
        serial,
        baud,
        listen,
        clear_serial,
        allow_origin,
        cart,
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
            cart: Some(CartKind::Ed64),
        }
    }

    fn serial_only() -> CliConfig {
        CliConfig {
            serial: Some("COM3".into()),
            ..CliConfig::default()
        }
    }

    #[test]
    fn file_values_apply_when_cli_is_absent() {
        let r = merge(CliConfig::default(), file_cfg(), None).unwrap();
        assert_eq!(r.serial, "COM9");
        assert_eq!(r.baud, 9600);
        assert_eq!(r.listen, "0.0.0.0:1");
        assert!(r.clear_serial);
        assert_eq!(r.allow_origin, ["https://from-file.example"]);
        assert_eq!(r.cart, CartKind::Ed64);
    }

    #[test]
    fn cli_false_turns_off_clear_serial_from_file() {
        // `MULTI64D_CLEAR_SERIAL=false` with `clear_serial = true` in the config file: the spec
        // (§5.1) says CLI and environment override file values, so this must resolve to `false`.
        let cli = CliConfig {
            clear_serial: Some(false),
            ..CliConfig::default()
        };
        let r = merge(cli, file_cfg(), None).unwrap();
        assert!(!r.clear_serial);
    }

    #[test]
    fn cli_overrides_every_file_value() {
        let cli = CliConfig {
            serial: Some("COM3".into()),
            baud: Some(115200),
            listen: Some("127.0.0.1:38765".into()),
            clear_serial: Some(true),
            allow_origin: vec!["https://from-cli.example".into()],
            cart: Some(CartKind::Sc64),
        };
        let r = merge(cli, file_cfg(), None).unwrap();
        assert_eq!(r.serial, "COM3");
        assert_eq!(r.baud, 115200);
        assert_eq!(r.listen, "127.0.0.1:38765");
        assert!(r.clear_serial);
        assert_eq!(r.allow_origin, ["https://from-cli.example"]);
        assert_eq!(
            r.cart,
            CartKind::Sc64,
            "CLI sc64 must override ed64 from the file"
        );
    }

    #[test]
    fn defaults_apply_with_no_file_and_only_serial() {
        let r = merge(serial_only(), FileConfig::default(), None).unwrap();
        assert_eq!(r.baud, 115200);
        assert_eq!(r.listen, "127.0.0.1:38765");
        assert!(!r.clear_serial);
        assert!(r.allow_origin.is_empty());
        assert_eq!(r.cart, CartKind::Sc64, "SummerCart64 stays the default");
    }

    #[test]
    fn missing_serial_is_an_error() {
        let err = merge(CliConfig::default(), FileConfig::default(), None).unwrap_err();
        assert!(err.to_string().contains("missing serial"));
    }

    #[test]
    fn cart_parses_from_toml() {
        let c: FileConfig = toml::from_str("serial = \"COM3\"\ncart = \"ed64\"\n").unwrap();
        assert_eq!(c.cart, Some(CartKind::Ed64));
        let c: FileConfig = toml::from_str("serial = \"COM3\"\ncart = \"ed64pro\"\n").unwrap();
        assert_eq!(c.cart, Some(CartKind::Ed64Pro));
        let c: FileConfig = toml::from_str("serial = \"COM3\"\n").unwrap();
        assert_eq!(c.cart, None);
    }

    #[test]
    fn unknown_cart_in_toml_is_rejected() {
        let err = toml::from_str::<FileConfig>("cart = \"everdrive\"\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("sc64") && msg.contains("ed64") && msg.contains("ed64pro"),
            "{msg}"
        );
    }
}
