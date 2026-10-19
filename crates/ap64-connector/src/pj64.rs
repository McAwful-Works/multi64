//! Where a client that talks to a Project64 script looks for it.
//!
//! Pokémon Snap's client (nielrenned/PokemonSnap_Archipelago, `update_pj64_config.py`)
//! connects to `127.0.0.1` on a port it keeps in Project64's own settings file: on its first
//! run it picks one at random, from 40000 to 49999, and writes it as `psnap_ap_port` under
//! `[Debugger]` in `Config\Project64.cfg`, beside the Project64 named by
//! `pokemon_snap_options.emulator_settings.path` in Archipelago's `host.yaml`. It reads it back
//! from there every time it starts, and stops if that file is missing. So AP64 reads the port
//! from the same place, and listens on it in place of the script ([`crate::PJ64`]).
//!
//! The client writes that file; AP64 only reads it. A player who has never opened the client
//! has no port yet, and is told to open it once.

use std::path::{Path, PathBuf};

/// The client's key for its port in `Project64.cfg` (`PJ64_PORT_KEY_NAME`).
const PORT_KEY: &str = "psnap_ap_port";

/// The port Pokémon Snap's client will connect to, from the first Archipelago folder whose
/// `host.yaml` names a Project64 for it.
pub fn snap_ports() -> Result<Vec<u16>, String> {
    let mut tried = Vec::new();
    for dir in ap64_core::installed::archipelago_dirs() {
        let yaml = dir.join("host.yaml");
        let Ok(text) = std::fs::read_to_string(&yaml) else {
            continue;
        };
        let Some(exe) = yaml_value(
            &text,
            &["pokemon_snap_options", "emulator_settings", "path"],
        ) else {
            tried.push(format!(
                "{} sets no Project64 for Pokémon Snap Client",
                yaml.display()
            ));
            continue;
        };
        let cfg = config_path(&dir, &exe);
        return match std::fs::read(&cfg) {
            Ok(bytes) => port_in_config(&bytes).map(|p| vec![p]).ok_or_else(|| {
                format!(
                    "{} has no {PORT_KEY} yet: open Pokémon Snap Client once, so it picks the \
                     port it will connect to, then press Start again",
                    cfg.display()
                )
            }),
            Err(e) => Err(format!(
                "Pokémon Snap Client keeps its port in {}, which could not be read ({e}): set \
                 pokemon_snap_options.emulator_settings.path in host.yaml to your \
                 Project64.exe, and open the client once",
                cfg.display()
            )),
        };
    }
    if tried.is_empty() {
        tried.push("no host.yaml found in any Archipelago folder".into());
    }
    Err(format!(
        "Pokémon Snap Client needs Project64's settings file to find AP64, and {}: set \
         pokemon_snap_options.emulator_settings.path in host.yaml to your Project64.exe, \
         and open the client once",
        tried.join("; ")
    ))
}

/// `Config\Project64.cfg` beside the Project64 the client was given, which it resolves as
/// Archipelago's `user_path` does when the path is not there as given: against the Archipelago
/// folder. The executable itself need not exist; only the folder's settings file is read.
fn config_path(archipelago: &Path, exe: &str) -> PathBuf {
    let mut exe = PathBuf::from(exe);
    if !exe.exists() && exe.is_relative() {
        exe = archipelago.join(exe);
    }
    exe.parent()
        .unwrap_or(Path::new(""))
        .join("Config")
        .join("Project64.cfg")
}

/// `psnap_ap_port` under `[Debugger]` in a Project64.cfg, as Python's ConfigParser reads it:
/// section and key names exact, `=` or `:` as the delimiter, surrounding space dropped.
fn port_in_config(bytes: &[u8]) -> Option<u16> {
    // The client reads it as cp1252, but every byte that matters here is ASCII.
    let text = String::from_utf8_lossy(bytes);
    let mut in_debugger = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_debugger = name == "Debugger";
            continue;
        }
        if !in_debugger {
            continue;
        }
        let Some(i) = line.find(['=', ':']) else {
            continue;
        };
        if line[..i].trim() == PORT_KEY {
            return line[i + 1..].trim().parse().ok();
        }
    }
    None
}

/// A scalar at `path` in a block-style YAML document of the shape Archipelago writes
/// `host.yaml` in: one key per line, nested by indentation. `None` when it is missing, empty
/// or null. Double-quoted values have their escapes undone, single-quoted ones their `''`.
fn yaml_value(text: &str, path: &[&str]) -> Option<String> {
    // The indentation of each block entered on the way down, and of the current block's own
    // keys, which its first line sets: only those are its children, not anything deeper.
    let mut entered: Vec<usize> = Vec::new();
    let mut keys_at: Option<usize> = Some(0);
    for line in text.lines() {
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let indent = line.len() - body.len();
        if entered.last().is_some_and(|&i| indent <= i) {
            // Out of the block the path went into, without finding the rest of it.
            return None;
        }
        if indent != *keys_at.get_or_insert(indent) {
            continue;
        }
        let Some((key, rest)) = body.split_once(':') else {
            continue;
        };
        if key.trim() != path[entered.len()] {
            continue;
        }
        if entered.len() + 1 == path.len() {
            return scalar(rest.trim());
        }
        entered.push(indent);
        keys_at = None;
    }
    None
}

fn scalar(v: &str) -> Option<String> {
    let v = if let Some(q) = v.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = q.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        out
    } else if let Some(q) = v.strip_prefix('\'') {
        let mut out = String::new();
        let mut rest = q;
        loop {
            match rest.find('\'') {
                Some(i) if rest[i + 1..].starts_with('\'') => {
                    out.push_str(&rest[..=i]);
                    rest = &rest[i + 2..];
                }
                Some(i) => {
                    out.push_str(&rest[..i]);
                    break;
                }
                None => {
                    out.push_str(rest);
                    break;
                }
            }
        }
        out
    } else {
        // A plain scalar ends at a comment.
        let v = v.split(" #").next().unwrap_or("").trim();
        if matches!(v, "" | "~" | "null" | "Null" | "NULL") {
            return None;
        }
        v.to_string()
    };
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = r#"general_options:
  # File path of the Banjo-Tooie (USA) ROM.
  output_path: "output"
banjo_tooie_options:
  rom_path: "D:\\ROMs\\Banjo.z64"
pokemon_snap_options:
  rom_path: "D:\\ROMs\\N64\\USA M - R\\Pokemon Snap (USA).z64"
  use_pj64: true
  emulator_settings:
    # Project64 executable path
    path: "E:\\Program Files (x86)\\Project64 3.0\\Project64.exe"
    auto_start: false
zillion_options:
  path: "nope"
"#;

    #[test]
    fn finds_the_project64_the_client_was_given() {
        assert_eq!(
            yaml_value(HOST, &["pokemon_snap_options", "emulator_settings", "path"]).as_deref(),
            Some(r"E:\Program Files (x86)\Project64 3.0\Project64.exe")
        );
        assert_eq!(
            yaml_value(HOST, &["pokemon_snap_options", "rom_path"]).as_deref(),
            Some(r"D:\ROMs\N64\USA M - R\Pokemon Snap (USA).z64")
        );
    }

    #[test]
    fn a_key_outside_its_block_is_not_taken_for_it() {
        // `path` exists, but under another game, and the client was given none.
        let yaml = "pokemon_snap_options:\n  emulator_settings:\n    auto_start: true\nzillion_options:\n  path: x\n";
        assert_eq!(
            yaml_value(yaml, &["pokemon_snap_options", "emulator_settings", "path"]),
            None
        );
        let yaml = "pokemon_snap_options:\n  rom_path: a\nemulator_settings:\n  path: x\n";
        assert_eq!(
            yaml_value(yaml, &["pokemon_snap_options", "emulator_settings", "path"]),
            None
        );
        // A same-named block nested deeper is not the one the path names.
        let yaml = "pokemon_snap_options:\n  other:\n    emulator_settings:\n      path: x\n  emulator_settings:\n    path: y\n";
        assert_eq!(
            yaml_value(yaml, &["pokemon_snap_options", "emulator_settings", "path"]).as_deref(),
            Some("y")
        );
    }

    #[test]
    fn an_unset_path_is_none() {
        for v in ["null", "~", "", "\"\"", "''"] {
            let yaml = format!("pokemon_snap_options:\n  emulator_settings:\n    path: {v}\n");
            assert_eq!(
                yaml_value(
                    &yaml,
                    &["pokemon_snap_options", "emulator_settings", "path"]
                ),
                None,
                "{v}"
            );
        }
        let yaml =
            "pokemon_snap_options:\n  emulator_settings:\n    path: 'C:\\it''s\\Project64.exe'\n";
        assert_eq!(
            yaml_value(yaml, &["pokemon_snap_options", "emulator_settings", "path"]).as_deref(),
            Some(r"C:\it's\Project64.exe")
        );
    }

    #[test]
    fn reads_the_port_the_client_wrote() {
        let cfg = b"[Settings]\r\nBasic Mode=0\r\n\r\n[Debugger]\r\nDebugger=1\r\nAutorun Scripts=ap_psnap_pj64_adapter_30.js\r\npsnap_ap_port=43817\r\n";
        assert_eq!(port_in_config(cfg), Some(43817));
        // Only under [Debugger], and only that key.
        assert_eq!(
            port_in_config(b"[Settings]\npsnap_ap_port=1\n[Debugger]\nDebugger=1\n"),
            None
        );
        assert_eq!(port_in_config(b"[Debugger]\npsnap_ap_port_old=1\n"), None);
        assert_eq!(
            port_in_config(b"[Debugger]\npsnap_ap_port : 40001\n"),
            Some(40001)
        );
    }

    #[test]
    fn the_settings_file_is_beside_the_executable() {
        // Paths absolute on whatever runs the test: a Windows drive path is relative on Linux.
        let base = std::env::temp_dir().join("ap64-pj64-test-missing");
        let dir = base.join("Archipelago");
        let exe = base.join("PJ64").join("Project64.exe");
        assert_eq!(
            config_path(&dir, exe.to_str().unwrap()),
            base.join("PJ64").join("Config").join("Project64.cfg")
        );
        // A relative path the client resolves against the Archipelago folder.
        let rel = Path::new("pj64").join("Project64.exe");
        assert_eq!(
            config_path(&dir, rel.to_str().unwrap()),
            dir.join("pj64").join("Config").join("Project64.cfg")
        );
    }
}
