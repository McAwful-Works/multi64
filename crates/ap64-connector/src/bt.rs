//! Banjo-Tooie: how its client is answered, and the one fix that client needs first.
//!
//! Banjo-Tooie Client (`BTClient.py`) keeps the game logic in its own Python
//! (`client/game.py`, `client/state.py`) and reaches the game through the same EmuLoader
//! library DK64 Client uses ([`crate::dk64`]): v0.1.4, byte for byte, unpacked in the apworld
//! rather than vendored in a zip. With no emulator running it falls back to RetroArch's
//! Network Commands, which is what AP64 answers ([`crate::retroarch`]). The client's other
//! transport, a socket on port 21221 for upstream's EverDrive program, idles once EmuLoader is
//! attached. Issue #319.
//!
//! # The client fix
//!
//! Once attached, the client's monitor loop logs which emulator it found, by
//! `emulator_info.id`. EmuLoader's process backends have an `id`; its RetroArch backend,
//! `RetroArchNetworkInfo`, does not. So on the RetroArch path the loop raises
//! `AttributeError` on its first line, outside its own `try`, and the task ends without a word
//! logged: the client says "Emulator connected and ready!", asks nothing more, and never
//! reconnects. That is every Banjo-Tooie player on the fallback, whether the other end is AP64
//! or RetroArch itself. [`client_state`] reports whether an installed `banjo_tooie.apworld`'s
//! backend has an `id`, and [`fix_client`] gives it one.
//!
//! DK64's gap, a missing `read_bytestring`, is here too, but nothing on Banjo-Tooie Client's
//! path calls it: it reads and writes only `u8`, `u16` and `u32`. Its signature check
//! (`validate_bt_signature`) is applied to emulator processes only, never to the fallback.
//!
//! # No bitwise ranges
//!
//! The client reads the game's flag bitmaps (the randomizer's `n64_saves_*`) and never writes
//! them. Every write it makes is a whole byte, `u16` or `u32`: settings, item and trap counts,
//! the exit map, the message buffer, and the death link, tag link and text-queue counters.
//! [`crate::retroarch`] already writes only the bytes the client changed, so no range has to be
//! merged a bit at a time.
//!
//! # The settings
//!
//! By the client's own account, the ROM refuses to boot until the randomizer's settings are in
//! RAM. The client writes them (`write_slot_settings`) whenever the seed number in RAM differs
//! from its slot's, and they go out in the order it made them, like any other writes. This is
//! the work the forked connector script used to do in `process_slot()`.

use std::io::{self, Cursor, Read, Write as _};
use std::path::PathBuf;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::retroarch::Options;
use crate::ClientState;

/// RetroArch's own port, which is where EmuLoader looks.
pub const OPTIONS: Options = Options {
    port: 55355,
    bitwise: &[],
};

/// The installed world's file name.
pub const APWORLD: &str = "banjo_tooie.apworld";

const MODULE: &str = "banjo_tooie/emu_loader/retroarch_udp.py";
const PYCACHE: &str = "banjo_tooie/emu_loader/__pycache__/retroarch_udp.";
const BACKEND: &str = "class RetroArchNetworkInfo";
/// The line the `id` goes after: the backend's other name, its first class attribute.
const ANCHOR: &str = "    readable_emulator_name = \"RetroArch Network Commands\"";
/// The same name EmuLoader's `emulators.json` gives RetroArch's process backend.
const ADDED: &str = "    # Added by AP64: EmuLoader's process backends have an id and this one did not, so\n    # Banjo-Tooie Client's monitor loop failed on it as soon as it attached.\n    id = \"RetroArch\"";

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// The backend's class body: from its `class` line to the next top-level statement.
fn backend_class(module: &str) -> Option<&str> {
    let start = module.find(BACKEND)?;
    let body = &module[start..];
    let end = body[1..]
        .find("\nclass ")
        .or_else(|| body[1..].find("\ndef "))
        .map_or(body.len(), |i| i + 1);
    Some(&body[..end])
}

fn has_id(class: &str) -> bool {
    class
        .lines()
        .any(|l| l.starts_with("    id =") || l.starts_with("    id:"))
}

fn module(apworld: &[u8]) -> io::Result<String> {
    let mut z = ZipArchive::new(Cursor::new(apworld)).map_err(bad)?;
    let mut f = z.by_name(MODULE).map_err(bad)?;
    let mut text = String::new();
    f.read_to_string(&mut text)?;
    Ok(text)
}

/// Whether the client in `apworld` (the bytes of an installed `banjo_tooie.apworld`) can reach
/// AP64: [`ClientState::Ready`] when its RetroArch backend has an `id`.
pub fn client_state(apworld: &[u8]) -> ClientState {
    let text = match module(apworld) {
        Ok(t) => t,
        Err(e) => {
            return ClientState::Unrecognized(format!(
                "{APWORLD} has no readable {MODULE}; this version of the world is not one AP64 knows ({e})"
            ))
        }
    };
    let Some(class) = backend_class(&text) else {
        return ClientState::Unrecognized(format!("{MODULE} has no RetroArch backend"));
    };
    if has_id(class) {
        ClientState::Ready
    } else if class.contains(ANCHOR) {
        ClientState::NeedsFix
    } else {
        ClientState::Unrecognized(format!(
            "the RetroArch backend in {MODULE} is not laid out as AP64 expects"
        ))
    }
}

/// The module with the `id` added after [`ANCHOR`], keeping the file's own line endings.
fn add_id(text: &str) -> Option<String> {
    let class_at = text.find(BACKEND)?;
    let at = class_at + text[class_at..].find(ANCHOR)? + ANCHOR.len();
    let eol = if text[at..].starts_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let added = ADDED.replace('\n', eol);
    Some(format!("{}{eol}{added}{}", &text[..at], &text[at..]))
}

/// `apworld` with the fix applied. Refuses a world [`client_state`] does not report as
/// [`ClientState::NeedsFix`], so it is never applied twice or to something unrecognized. Every
/// other entry is copied as stored, byte for byte, and the module's stale bytecode is dropped.
pub fn fix_client(apworld: &[u8]) -> Result<Vec<u8>, String> {
    match client_state(apworld) {
        ClientState::NeedsFix => {}
        ClientState::Ready => return Err(format!("{APWORLD} already has the fix")),
        ClientState::Unrecognized(why) => return Err(why),
    }
    let e = |e: io::Error| e.to_string();
    let text = module(apworld).map_err(e)?;
    let fixed = add_id(&text).ok_or("the RetroArch backend could not be changed")?;
    let mut src = ZipArchive::new(Cursor::new(apworld)).map_err(|e| e.to_string())?;
    let mut dst = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let f = src.by_index_raw(i).map_err(|e| e.to_string())?;
        let name = f.name().to_string();
        // Compiled before the change; left in place, Python could keep running the old code.
        if name.starts_with(PYCACHE) && name.ends_with(".pyc") {
            continue;
        }
        if name == MODULE {
            drop(f);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            dst.start_file(name, opts).map_err(|e| e.to_string())?;
            dst.write_all(fixed.as_bytes()).map_err(e)?;
        } else {
            dst.raw_copy_file(f).map_err(|e| e.to_string())?;
        }
    }
    let out = dst.finish().map_err(|e| e.to_string())?.into_inner();
    match client_state(&out) {
        ClientState::Ready => Ok(out),
        other => Err(format!("the fixed {APWORLD} did not check out: {other:?}")),
    }
}

/// Where the installed `banjo_tooie.apworld` is ([`crate::installed_apworld`]).
pub fn installed_apworld() -> Option<PathBuf> {
    crate::installed_apworld(APWORLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The released backend's head, as 4.13.1 ships it.
    const UNFIXED: &str = "\"\"\"RetroArch Network Commands memory backend for EmuLoader.\"\"\"\n\nimport socket\n\n\nclass RetroArchNetworkInfo:\n    \"\"\"RetroArch Network Commands memory backend.\n    \"\"\"\n\n    readable_emulator_name = \"RetroArch Network Commands\"\n\n    def __init__(self):\n        self.socket = None\n";

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            w.start_file(
                *name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn apworld(module: &str) -> Vec<u8> {
        zip_of(&[
            ("banjo_tooie/__init__.py", b"# world\n"),
            ("banjo_tooie/emu_loader/client.py", b"# client\n"),
            ("banjo_tooie/emu_loader/retroarch_udp.py", module.as_bytes()),
            (
                "banjo_tooie/emu_loader/__pycache__/retroarch_udp.cpython-313.pyc",
                b"stale",
            ),
            ("banjo_tooie/assets/big.bin", &[7u8; 4096]),
        ])
    }

    fn read(zip: &[u8], name: &str) -> Option<Vec<u8>> {
        let mut z = ZipArchive::new(Cursor::new(zip)).unwrap();
        let mut f = z.by_name(name).ok()?;
        let mut v = Vec::new();
        f.read_to_end(&mut v).unwrap();
        Some(v)
    }

    #[test]
    fn the_released_backend_needs_the_fix_and_the_fix_makes_it_ready() {
        let world = apworld(UNFIXED);
        assert_eq!(client_state(&world), ClientState::NeedsFix);
        let fixed = fix_client(&world).unwrap();
        assert_eq!(client_state(&fixed), ClientState::Ready);

        let module = String::from_utf8(read(&fixed, MODULE).unwrap()).unwrap();
        assert!(module.contains(&format!("{ANCHOR}\n{ADDED}\n\n    def __init__")));
        assert!(module.contains("\n    id = \"RetroArch\"\n"));
        // Stale bytecode would keep running the old class.
        assert!(read(
            &fixed,
            "banjo_tooie/emu_loader/__pycache__/retroarch_udp.cpython-313.pyc"
        )
        .is_none());
        for name in [
            "banjo_tooie/__init__.py",
            "banjo_tooie/emu_loader/client.py",
            "banjo_tooie/assets/big.bin",
        ] {
            assert_eq!(read(&fixed, name), read(&world, name), "{name}");
        }
    }

    #[test]
    fn crlf_line_endings_are_kept() {
        let crlf = UNFIXED.replace('\n', "\r\n");
        let fixed = fix_client(&apworld(&crlf)).unwrap();
        let module = String::from_utf8(read(&fixed, MODULE).unwrap()).unwrap();
        assert!(module.contains("\r\n    id = \"RetroArch\"\r\n"));
        assert!(!module.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn a_fixed_world_is_not_fixed_twice() {
        let fixed = fix_client(&apworld(UNFIXED)).unwrap();
        assert!(fix_client(&fixed).unwrap_err().contains("already"));
    }

    #[test]
    fn a_backend_that_already_has_an_id_is_ready() {
        let upstream_fixed =
            UNFIXED.replace(ANCHOR, &format!("{ANCHOR}\n    id = \"RetroArchNetwork\""));
        assert_eq!(client_state(&apworld(&upstream_fixed)), ClientState::Ready);
    }

    #[test]
    fn an_id_elsewhere_in_the_module_does_not_count() {
        let other = format!("{UNFIXED}\n\nclass Other:\n    id = \"x\"\n");
        assert_eq!(client_state(&apworld(&other)), ClientState::NeedsFix);
    }

    #[test]
    fn an_unrecognized_world_is_left_alone() {
        let no_module = zip_of(&[("banjo_tooie/__init__.py", b"# world\n")]);
        assert!(matches!(
            client_state(&no_module),
            ClientState::Unrecognized(_)
        ));
        let moved = apworld(&UNFIXED.replace(ANCHOR, "    name = \"x\""));
        assert!(matches!(client_state(&moved), ClientState::Unrecognized(_)));
        assert!(fix_client(&moved).is_err());
        assert!(matches!(
            client_state(b"not a zip"),
            ClientState::Unrecognized(_)
        ));
    }

    /// The released world, when one is supplied: `BT_APWORLD=<path> cargo test -- --ignored`.
    /// Checked against 4.13.1.
    #[test]
    #[ignore = "needs banjo_tooie.apworld from the environment"]
    fn the_released_world_needs_the_fix_and_takes_it() {
        let path = std::env::var("BT_APWORLD").expect("BT_APWORLD names a banjo_tooie.apworld");
        let world = std::fs::read(path).unwrap();
        assert_eq!(client_state(&world), ClientState::NeedsFix);
        let fixed = fix_client(&world).unwrap();
        assert_eq!(client_state(&fixed), ClientState::Ready);
        let before = ZipArchive::new(Cursor::new(&world[..])).unwrap().len();
        let after = ZipArchive::new(Cursor::new(&fixed[..])).unwrap().len();
        assert_eq!(
            before, after,
            "the released world carries no bytecode to drop"
        );
    }
}
