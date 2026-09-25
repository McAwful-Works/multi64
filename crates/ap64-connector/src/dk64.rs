//! Donkey Kong 64: how its client is answered, and the one fix that client needs first.
//!
//! The DK64 randomizer's Archipelago client (`DK64Client.py`) uses no connector script. It
//! reads emulator memory through the EmuLoader library, and when it finds no emulator it
//! falls back to RetroArch's Network Commands, which is what AP64 answers
//! ([`crate::retroarch`]). Measurements and the profile: `profiles/dk64`, issue #309.
//!
//! # The client fix
//!
//! EmuLoader v0.1.4, which `dk64.apworld` vendors, leaves `read_bytestring` and
//! `write_bytestring` off its RetroArch backend, and the client calls `read_bytestring` on
//! every loop to read the player name. So the fallback dies before it does anything, for
//! every DK64 player, whether the other end is AP64 or RetroArch itself. [`client_state`]
//! reports whether an installed `dk64.apworld` has the two methods, and [`fix_client`] adds
//! them, exactly as EmuLoader's process-memory backend has them. The apworld replaces itself
//! when the randomizer releases a new version, so the check is made every time.

use std::io::{self, Cursor, Read, Write as _};
use std::ops::Range;
use std::path::PathBuf;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::retroarch::Options;
use crate::ClientState;

/// DK64Client's flag bitfield (`DK64MemoryMap.EEPROM`, 0x807ECEA8): flags 0 to 1061. Its
/// `setFlag` reads a byte, ORs a bit in and writes the byte back.
pub const FLAGS: Range<u32> = 0x7E_CEA8..0x7E_CEA8 + 134;

/// RetroArch's own port, which is where EmuLoader looks.
pub const OPTIONS: Options = Options {
    port: 55355,
    bitwise: &[FLAGS],
};

/// The installed world's file name.
pub const APWORLD: &str = "dk64.apworld";

const MODULE: &str = "emu_loader/retroarch_udp.py";
const BACKEND: &str = "class RetroArchNetworkInfo";
const MARKER: &str = "def read_bytestring";

/// Appended to `RetroArchNetworkInfo`, the module's last class: EmuLoader's own
/// `EmulatorInfo.read_bytestring` and `write_bytestring`, over this backend's `read_u8` and
/// `write_u8`.
const METHODS: &str = r#"
    # Added by AP64: EmuLoader's EmulatorInfo has these and this backend did not, so
    # DK64Client's reset_auth failed here on every loop. Same bodies as EmulatorInfo's.
    def read_bytestring(self, address: int, length: int) -> str:
        """Read a string of at most length bytes, stopping at NUL."""
        result = ""
        for i in range(length):
            byte_val = self.read_u8(address + i)
            if byte_val == 0:
                break
            result += chr(byte_val)
        return result

    def write_bytestring(self, address: int, data: str):
        """Write a NUL-terminated string."""
        from .utils import sanitize_and_trim

        sanitized_data = sanitize_and_trim(data)
        for i, char in enumerate(sanitized_data):
            self.write_u8(address + i, ord(char))
        self.write_u8(address + len(sanitized_data), 0)
"#;

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// The vendored-dependency zips inside an apworld: `<world>/vendor/*.zip`.
fn is_vendor_zip(name: &str) -> bool {
    let mut parts = name.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(_), Some("vendor"), Some(file), None) if file.ends_with(".zip")
    )
}

/// The EmuLoader module in one vendored zip, if it has one.
fn module_in(zip_bytes: &[u8]) -> io::Result<Option<String>> {
    let mut z = ZipArchive::new(Cursor::new(zip_bytes)).map_err(bad)?;
    let Ok(mut f) = z.by_name(MODULE) else {
        return Ok(None);
    };
    let mut text = String::new();
    f.read_to_string(&mut text)?;
    Ok(Some(text))
}

/// Every vendored zip that carries EmuLoader: (entry name, zip bytes, module source).
fn vendored(apworld: &[u8]) -> io::Result<Vec<(String, Vec<u8>, String)>> {
    let mut outer = ZipArchive::new(Cursor::new(apworld)).map_err(bad)?;
    let mut out = Vec::new();
    for i in 0..outer.len() {
        let mut f = outer.by_index(i).map_err(bad)?;
        if !is_vendor_zip(f.name()) {
            continue;
        }
        let name = f.name().to_string();
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        if let Some(text) = module_in(&bytes)? {
            out.push((name, bytes, text));
        }
    }
    Ok(out)
}

/// Whether the client in `apworld` (the bytes of an installed `dk64.apworld`) can reach AP64:
/// [`ClientState::Ready`] when every vendored EmuLoader has the methods.
pub fn client_state(apworld: &[u8]) -> ClientState {
    let found = match vendored(apworld) {
        Ok(f) => f,
        Err(e) => return ClientState::Unrecognized(format!("{APWORLD} could not be read: {e}")),
    };
    if found.is_empty() {
        return ClientState::Unrecognized(format!(
            "{APWORLD} carries no vendored EmuLoader; this version of the world is not one AP64 knows"
        ));
    }
    if let Some((name, ..)) = found.iter().find(|(_, _, t)| !t.contains(BACKEND)) {
        return ClientState::Unrecognized(format!(
            "the EmuLoader in {name} has no RetroArch backend"
        ));
    }
    if found.iter().all(|(_, _, t)| t.contains(MARKER)) {
        ClientState::Ready
    } else {
        ClientState::NeedsFix
    }
}

/// One vendored zip with the methods added to its module and the module's stale bytecode
/// dropped. Every other entry is copied as stored, byte for byte.
fn fix_vendored(zip_bytes: &[u8], text: &str) -> io::Result<Vec<u8>> {
    let mut src = ZipArchive::new(Cursor::new(zip_bytes)).map_err(bad)?;
    let mut dst = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let f = src.by_index_raw(i).map_err(bad)?;
        let name = f.name().to_string();
        // Compiled before the change; left in place, Python could keep running the old code.
        if name.starts_with("emu_loader/__pycache__/retroarch_udp.") && name.ends_with(".pyc") {
            continue;
        }
        if name == MODULE {
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            drop(f);
            dst.start_file(name, opts).map_err(bad)?;
            dst.write_all(text.trim_end_matches('\n').as_bytes())?;
            dst.write_all(b"\n")?;
            dst.write_all(METHODS.as_bytes())?;
        } else {
            dst.raw_copy_file(f).map_err(bad)?;
        }
    }
    Ok(dst.finish().map_err(bad)?.into_inner())
}

/// `apworld` with the fix applied to every vendored EmuLoader that lacks it. Refuses a world
/// [`client_state`] does not report as [`ClientState::NeedsFix`], so it is never applied
/// twice or to something unrecognized.
pub fn fix_client(apworld: &[u8]) -> Result<Vec<u8>, String> {
    match client_state(apworld) {
        ClientState::NeedsFix => {}
        ClientState::Ready => return Err(format!("{APWORLD} already has the fix")),
        ClientState::Unrecognized(why) => return Err(why),
    }
    let io = |e: io::Error| e.to_string();
    let fixes: Vec<(String, Vec<u8>)> = vendored(apworld)
        .map_err(io)?
        .into_iter()
        .filter(|(_, _, t)| !t.contains(MARKER))
        .map(|(name, bytes, text)| fix_vendored(&bytes, &text).map(|b| (name, b)))
        .collect::<io::Result<_>>()
        .map_err(io)?;
    let mut src = ZipArchive::new(Cursor::new(apworld)).map_err(|e| e.to_string())?;
    let mut dst = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let f = src.by_index_raw(i).map_err(|e| e.to_string())?;
        match fixes.iter().find(|(n, _)| n == f.name()) {
            Some((name, bytes)) => {
                drop(f);
                // A zip of already-compressed entries: storing it costs little.
                let opts =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
                dst.start_file(name.as_str(), opts)
                    .map_err(|e| e.to_string())?;
                dst.write_all(bytes).map_err(io)?;
            }
            None => dst.raw_copy_file(f).map_err(|e| e.to_string())?,
        }
    }
    let out = dst.finish().map_err(|e| e.to_string())?.into_inner();
    match client_state(&out) {
        ClientState::Ready => Ok(out),
        other => Err(format!("the fixed {APWORLD} did not check out: {other:?}")),
    }
}

/// Where the installed `dk64.apworld` is ([`crate::installed_apworld`]).
pub fn installed_apworld() -> Option<PathBuf> {
    crate::installed_apworld(APWORLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNFIXED: &str =
        "class RetroArchNetworkInfo:\n    def read_u8(self, address):\n        return 0\n";

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

    fn vendor(module: &str) -> Vec<u8> {
        zip_of(&[
            ("emu_loader/__init__.py", b"# init\n"),
            ("emu_loader/retroarch_udp.py", module.as_bytes()),
            (
                "emu_loader/__pycache__/retroarch_udp.cpython-313.pyc",
                b"stale",
            ),
            ("pyxdelta.cp313-win_amd64.pyd", b"\x00binary"),
        ])
    }

    fn apworld(vendor_zip: &[u8]) -> Vec<u8> {
        zip_of(&[
            ("dk64/__init__.py", b"# world\n"),
            ("dk64/vendor/windows.zip", vendor_zip),
            ("dk64/static/big.bin", &[7u8; 4096]),
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
    fn the_released_layout_needs_the_fix_and_the_fix_makes_it_ready() {
        let world = apworld(&vendor(UNFIXED));
        assert_eq!(client_state(&world), ClientState::NeedsFix);
        let fixed = fix_client(&world).unwrap();
        assert_eq!(client_state(&fixed), ClientState::Ready);

        let inner = read(&fixed, "dk64/vendor/windows.zip").unwrap();
        let module = String::from_utf8(read(&inner, MODULE).unwrap()).unwrap();
        assert!(module.starts_with(UNFIXED.trim_end()));
        assert!(module.contains("def read_bytestring(self, address: int, length: int)"));
        assert!(module.contains("def write_bytestring(self, address: int, data: str)"));
        // Stale bytecode would keep running the old class.
        assert!(read(
            &inner,
            "emu_loader/__pycache__/retroarch_udp.cpython-313.pyc"
        )
        .is_none());
    }

    #[test]
    fn everything_else_is_carried_over_unchanged() {
        let world = apworld(&vendor(UNFIXED));
        let fixed = fix_client(&world).unwrap();
        for name in ["dk64/__init__.py", "dk64/static/big.bin"] {
            assert_eq!(read(&fixed, name), read(&world, name), "{name}");
        }
        let (a, b) = (
            read(&world, "dk64/vendor/windows.zip").unwrap(),
            read(&fixed, "dk64/vendor/windows.zip").unwrap(),
        );
        for name in ["emu_loader/__init__.py", "pyxdelta.cp313-win_amd64.pyd"] {
            assert_eq!(read(&a, name), read(&b, name), "{name}");
        }
    }

    #[test]
    fn a_fixed_world_is_not_fixed_twice() {
        let fixed = fix_client(&apworld(&vendor(UNFIXED))).unwrap();
        assert!(fix_client(&fixed).unwrap_err().contains("already"));
    }

    #[test]
    fn a_world_without_emuloader_is_left_alone() {
        let world = zip_of(&[("dk64/__init__.py", b"# world\n")]);
        assert!(matches!(client_state(&world), ClientState::Unrecognized(_)));
        assert!(fix_client(&world).is_err());
    }

    #[test]
    fn not_a_zip_is_unrecognized_not_a_panic() {
        assert!(matches!(
            client_state(b"not a zip"),
            ClientState::Unrecognized(_)
        ));
    }

    /// The released world, when one is supplied: `DK64_APWORLD=<path> cargo test -- --ignored`.
    /// Checked against v1.5.8, the version the profile was measured with.
    #[test]
    #[ignore = "needs dk64.apworld from the environment"]
    fn the_released_world_needs_the_fix_and_takes_it() {
        let path = std::env::var("DK64_APWORLD").expect("DK64_APWORLD names a dk64.apworld");
        let world = std::fs::read(path).unwrap();
        assert_eq!(client_state(&world), ClientState::NeedsFix);
        let fixed = fix_client(&world).unwrap();
        assert_eq!(client_state(&fixed), ClientState::Ready);
        let before = ZipArchive::new(Cursor::new(&world[..])).unwrap().len();
        let after = ZipArchive::new(Cursor::new(&fixed[..])).unwrap().len();
        assert_eq!(
            before, after,
            "the fix adds and drops no entry of the apworld itself"
        );
    }

    #[test]
    fn only_top_level_vendor_zips_count() {
        assert!(is_vendor_zip("dk64/vendor/windows.zip"));
        assert!(!is_vendor_zip("dk64/vendor/nested/x.zip"));
        assert!(!is_vendor_zip("dk64/static/windows.zip"));
    }
}
