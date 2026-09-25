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
//! or RetroArch itself.
//!
//! The fix is made where the attribute is read, not in EmuLoader, because the client may not be
//! running its own EmuLoader: it imports a global `emu_loader` package first, and DK64's world
//! puts its vendored copy on `sys.path` when the Launcher loads it. On a console with both worlds
//! installed, a fix to Banjo-Tooie's copy changed nothing. [`client_state`] reports whether an
//! installed `banjo_tooie.apworld` still reads the attribute unguarded, and [`fix_client`] reads
//! it with a fallback, whichever EmuLoader answers.
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

const MODULE: &str = "banjo_tooie/BTClient.py";
const PYCACHE: &str = "banjo_tooie/__pycache__/BTClient.";
/// The read, as 4.13.1 has it.
const READ: &str = ".emulator_info.id";
const UNFIXED: &str = "emu_name = ctx.emu_loader.emulator_info.id";
/// The same read with a fallback: `RetroArch` is EmuLoader's own name for RetroArch's process
/// backend, in its `emulators.json`.
const FIXED: &str = "emu_name = getattr(ctx.emu_loader.emulator_info, \"id\", \"RetroArch\")  # AP64: EmuLoader's RetroArch backend has no id";

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn module(apworld: &[u8]) -> io::Result<String> {
    let mut z = ZipArchive::new(Cursor::new(apworld)).map_err(bad)?;
    let mut f = z.by_name(MODULE).map_err(bad)?;
    let mut text = String::new();
    f.read_to_string(&mut text)?;
    Ok(text)
}

/// Whether the client in `apworld` (the bytes of an installed `banjo_tooie.apworld`) can reach
/// AP64: [`ClientState::Ready`] when nothing in `BTClient.py` reads `emulator_info.id` bare.
pub fn client_state(apworld: &[u8]) -> ClientState {
    let text = match module(apworld) {
        Ok(t) => t,
        Err(e) => {
            return ClientState::Unrecognized(format!(
                "{APWORLD} has no readable {MODULE}; this version of the world is not one AP64 knows ({e})"
            ))
        }
    };
    match (text.matches(READ).count(), text.matches(UNFIXED).count()) {
        (0, _) => ClientState::Ready,
        (1, 1) => ClientState::NeedsFix,
        _ => ClientState::Unrecognized(format!(
            "{MODULE} reads emulator_info.id somewhere AP64 does not know to fix"
        )),
    }
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
    let fixed = text.replacen(UNFIXED, FIXED, 1);
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

    /// The released monitor loop's head, as 4.13.1 ships it: CRLF, like the file itself.
    fn released() -> String {
        [
            "    while not ctx.exit_event.is_set():",
            "      await ctx.emu_loader.wait_for_emulator()",
            "",
            "      emu_name = ctx.emu_loader.emulator_info.id",
            "      logger.info(f\"Connected to {emu_name}.\")",
            "",
        ]
        .join("\r\n")
    }

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

    fn apworld(client: &str) -> Vec<u8> {
        zip_of(&[
            ("banjo_tooie/__init__.py", b"# world\n"),
            ("banjo_tooie/BTClient.py", client.as_bytes()),
            ("banjo_tooie/__pycache__/BTClient.cpython-313.pyc", b"stale"),
            ("banjo_tooie/emu_loader/retroarch_udp.py", b"# backend\n"),
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
    fn the_released_client_needs_the_fix_and_the_fix_makes_it_ready() {
        let world = apworld(&released());
        assert_eq!(client_state(&world), ClientState::NeedsFix);
        let fixed = fix_client(&world).unwrap();
        assert_eq!(client_state(&fixed), ClientState::Ready);

        let client = String::from_utf8(read(&fixed, MODULE).unwrap()).unwrap();
        assert_eq!(client, released().replace(UNFIXED, FIXED));
        assert!(client.contains(
            "\r\n      emu_name = getattr(ctx.emu_loader.emulator_info, \"id\", \"RetroArch\")"
        ));
        // Stale bytecode would keep running the old loop.
        assert!(read(&fixed, "banjo_tooie/__pycache__/BTClient.cpython-313.pyc").is_none());
        for name in [
            "banjo_tooie/__init__.py",
            "banjo_tooie/emu_loader/retroarch_udp.py",
            "banjo_tooie/assets/big.bin",
        ] {
            assert_eq!(read(&fixed, name), read(&world, name), "{name}");
        }
    }

    #[test]
    fn a_fixed_world_is_not_fixed_twice() {
        let fixed = fix_client(&apworld(&released())).unwrap();
        assert!(fix_client(&fixed).unwrap_err().contains("already"));
    }

    #[test]
    fn a_client_that_no_longer_reads_the_id_is_ready() {
        let upstream_fixed = released().replace(UNFIXED, "emu_name = \"emulator\"");
        assert_eq!(client_state(&apworld(&upstream_fixed)), ClientState::Ready);
    }

    #[test]
    fn a_read_somewhere_else_is_not_guessed_at() {
        let moved = released().replace(UNFIXED, "name = ctx.emu_loader.emulator_info.id");
        assert!(matches!(
            client_state(&apworld(&moved)),
            ClientState::Unrecognized(_)
        ));
        let twice = format!("{}\r\n  x = info.emulator_info.id\r\n", released());
        assert!(matches!(
            client_state(&apworld(&twice)),
            ClientState::Unrecognized(_)
        ));
        assert!(fix_client(&apworld(&twice)).is_err());
    }

    #[test]
    fn an_unrecognized_world_is_left_alone() {
        let no_client = zip_of(&[("banjo_tooie/__init__.py", b"# world\n")]);
        assert!(matches!(
            client_state(&no_client),
            ClientState::Unrecognized(_)
        ));
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
        if let Ok(out) = std::env::var("BT_APWORLD_FIXED") {
            std::fs::write(out, &fixed).unwrap();
        }
    }
}
