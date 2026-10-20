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
/// `host.yaml` names a Project64 with the port in its settings file.
pub fn snap_ports() -> Result<Vec<u16>, String> {
    ports_in(&ap64_core::installed::archipelago_dirs())
}

/// [`snap_ports`], looking in `dirs` in order.
///
/// A folder that cannot say is passed over for the next, rather than ending the search: a
/// player with two Archipelago installs may have a `host.yaml` left in one that names a
/// Project64 since removed, and run the other. Only when none can say is the player told, with
/// what each one lacked, named by its `host.yaml`, since they may be fixing the wrong one.
fn ports_in(dirs: &[PathBuf]) -> Result<Vec<u16>, String> {
    let mut tried = Vec::new();
    for dir in dirs {
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
        let cfg = config_path(dir, &exe);
        match std::fs::read(&cfg) {
            Ok(bytes) => match port_in_config(&bytes) {
                Some(port) => return Ok(vec![port]),
                None => tried.push(format!(
                    "{} has no {PORT_KEY} yet (open Pokémon Snap Client once, so it picks the \
                     port it will connect to)",
                    cfg.display()
                )),
            },
            Err(e) => tried.push(format!(
                "{} names a Project64 whose {} could not be read ({e})",
                yaml.display(),
                cfg.display()
            )),
        }
    }
    if tried.is_empty() {
        tried.push("no host.yaml found in any Archipelago folder".into());
    }
    Err(format!(
        "Pokémon Snap Client needs Project64's settings file to find AP64, and {}: set \
         pokemon_snap_options.emulator_settings.path in host.yaml to your Project64.exe, \
         open the client once, then press Start again",
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
                // YAML's escapes. Archipelago saves `host.yaml` ASCII-only, so a path with any
                // other letter in it comes back as `\xNN`, `\uNNNN` or `\UNNNNNNNN`.
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' | '\t' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    'a' => out.push('\x07'),
                    'b' => out.push('\x08'),
                    'v' => out.push('\x0B'),
                    'f' => out.push('\x0C'),
                    'e' => out.push('\x1B'),
                    'N' => out.push('\u{85}'),
                    '_' => out.push('\u{A0}'),
                    'L' => out.push('\u{2028}'),
                    'P' => out.push('\u{2029}'),
                    x @ ('x' | 'u' | 'U') => {
                        let digits = match x {
                            'x' => 2,
                            'u' => 4,
                            _ => 8,
                        };
                        let hex: String = chars.by_ref().take(digits).collect();
                        if hex.len() != digits {
                            return None;
                        }
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
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

    /// Archipelago writes `host.yaml` double-quoted and ASCII-only, so a path with a letter
    /// outside ASCII in it is saved escaped, the way `_persistent_storage.yaml` holds
    /// `Pok\xE9mon`.
    #[test]
    fn a_path_saved_with_escapes_reads_as_the_path() {
        for (saved, path) in [
            (
                r#""E:\\Users\\J\xF6rg\\Project64 3.0\\Project64.exe""#,
                "E:\\Users\\Jörg\\Project64 3.0\\Project64.exe",
            ),
            (
                r#""E:\\Spi\u00E9le\\Project64.exe""#,
                "E:\\Spiéle\\Project64.exe",
            ),
            (
                r#""E:\\\U0001F3AE\\Project64.exe""#,
                "E:\\🎮\\Project64.exe",
            ),
            (
                r#""E:\\a\tb\\\"q\"\\Project64.exe""#,
                "E:\\a\tb\\\"q\"\\Project64.exe",
            ),
        ] {
            let yaml = format!("pokemon_snap_options:\n  emulator_settings:\n    path: {saved}\n");
            assert_eq!(
                yaml_value(
                    &yaml,
                    &["pokemon_snap_options", "emulator_settings", "path"]
                )
                .as_deref(),
                Some(path),
                "{saved}"
            );
        }
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

    /// Two Archipelago folders, each with a `host.yaml` naming a Project64 (`None` for none),
    /// and the second's Project64 with the port in its settings. Removed when dropped.
    struct Installs(PathBuf);

    impl Drop for Installs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn installs(name: &str, first: Option<&str>) -> (Installs, Vec<PathBuf>) {
        let base = std::env::temp_dir().join(format!("ap64-pj64-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b) = (base.join("ProgramData"), base.join("LocalAppData"));
        let pj64 = base.join("PJ64");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(pj64.join("Config")).unwrap();
        std::fs::write(
            pj64.join("Config").join("Project64.cfg"),
            "[Debugger]\npsnap_ap_port=43817\n",
        )
        .unwrap();
        let yaml =
            |exe: &str| format!("pokemon_snap_options:\n  emulator_settings:\n    path: '{exe}'\n");
        if let Some(exe) = first {
            std::fs::write(a.join("host.yaml"), yaml(exe)).unwrap();
        }
        let exe = pj64.join("Project64.exe");
        std::fs::write(b.join("host.yaml"), yaml(exe.to_str().unwrap())).unwrap();
        (Installs(base), vec![a, b])
    }

    /// A `host.yaml` left in one install naming a Project64 since removed does not hide the one
    /// the player runs.
    #[test]
    fn an_install_that_cannot_say_is_passed_over_for_the_next() {
        let gone = std::env::temp_dir()
            .join("ap64-pj64-gone")
            .join("Project64.exe");
        let (_tmp, dirs) = installs("passed-over", Some(gone.to_str().unwrap()));
        assert_eq!(ports_in(&dirs), Ok(vec![43817]));
    }

    /// When none can say, each is named, with what it lacked.
    #[test]
    fn when_no_install_can_say_each_is_named() {
        let gone = std::env::temp_dir()
            .join("ap64-pj64-gone")
            .join("Project64.exe");
        let (_tmp, dirs) = installs("none", Some(gone.to_str().unwrap()));
        std::fs::write(dirs[1].join("host.yaml"), "general_options:\n  x: 1\n").unwrap();
        let e = ports_in(&dirs).unwrap_err();
        for d in &dirs {
            assert!(
                e.contains(&d.join("host.yaml").display().to_string()),
                "{e}"
            );
        }
        assert!(
            e.contains("could not be read") && e.contains("sets no Project64"),
            "{e}"
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
