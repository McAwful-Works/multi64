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
//!
//! The methods go at the end of the backend class's body, wherever in the module that ends, and
//! the fix then checks they are inside it. The module is followed line by line, through its
//! strings, brackets and comments, far enough to tell where the class ends; a module it cannot
//! follow, or whose class is not a single top-level one indented by four spaces, is reported
//! as unrecognized and left alone, as is one that defines `read_bytestring` anywhere else.

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
const BACKEND: &str = "RetroArchNetworkInfo";
const MARKER: &str = "def read_bytestring";
const WRITE_MARKER: &str = "def write_bytestring";

/// The indentation of a method in [`METHODS`], which the class's own must match.
const INDENT: &str = "    ";

/// Added at the end of `RetroArchNetworkInfo`'s body, wherever in the module that ends:
/// EmuLoader's own `EmulatorInfo.read_bytestring` and `write_bytestring`, over this backend's
/// `read_u8` and `write_u8`.
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
        let name = f.name().map_err(bad)?.into_owned();
        if !is_vendor_zip(&name) {
            continue;
        }
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        if let Some(text) = module_in(&bytes)? {
            out.push((name, bytes, text));
        }
    }
    Ok(out)
}

/// What a line of Python source is, to the extent the fix needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Nothing but whitespace.
    Blank,
    /// Only a comment.
    Comment,
    /// The start of a statement, indented by this many bytes of the line.
    Statement(usize),
    /// Inside a bracket, a multi-line string or a backslash continuation.
    Continuation,
}

/// Each line of `src` (as `split_inclusive('\n')` gives them) and what it is. Brackets,
/// strings, comments and backslash continuations are followed, so a line inside a multi-line
/// string or call is never taken for a statement. `None` when the source is not something
/// this can follow: an unclosed string or bracket, or a closing bracket with no opening one.
fn scan(src: &str) -> Option<Vec<Kind>> {
    let mut kinds = Vec::new();
    let mut depth = 0usize;
    let mut triple: Option<u8> = None;
    let mut continued = false;
    for line in src.split_inclusive('\n') {
        let b = line.as_bytes();
        let indent = b.iter().take_while(|c| matches!(c, b' ' | b'\t')).count();
        let rest = line[indent..].trim_end();
        kinds.push(if triple.is_some() || depth > 0 || continued {
            Kind::Continuation
        } else if rest.is_empty() {
            Kind::Blank
        } else if rest.starts_with('#') {
            Kind::Comment
        } else {
            Kind::Statement(indent)
        });
        continued = false;
        let mut i = 0;
        while i < b.len() {
            if let Some(q) = triple {
                if b[i] == b'\\' {
                    i += 2;
                } else if b[i..].starts_with(&[q; 3]) {
                    triple = None;
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
            match b[i] {
                b'#' => break,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.checked_sub(1)?,
                b'\\' if line[i + 1..].trim().is_empty() => continued = true,
                q @ (b'"' | b'\'') if b[i..].starts_with(&[q; 3]) => {
                    triple = Some(q);
                    i += 2;
                }
                q @ (b'"' | b'\'') => loop {
                    i += 1;
                    match b.get(i) {
                        Some(b'\\') => i += 1,
                        Some(&c) if c == q => break,
                        Some(b'\n') | None => return None,
                        Some(_) => {}
                    }
                },
                _ => {}
            }
            i += 1;
        }
    }
    (depth == 0 && triple.is_none() && !continued).then_some(kinds)
}

/// `RetroArchNetworkInfo` as it stands in one copy of the module.
#[derive(Debug, PartialEq, Eq)]
struct Backend {
    /// Whether its body defines `read_bytestring`, and `write_bytestring`, as methods.
    read: bool,
    write: bool,
    /// The byte offset just past the last line of its body: where methods go.
    end: usize,
}

/// Find `RetroArchNetworkInfo` in `module`: a top-level class, defined once, whose body is
/// indented as [`METHODS`] is. Its body runs to the next top-level statement, or the end of
/// the module, whatever follows it. An error says what was not as the fix expects.
fn backend(module: &str) -> Result<Backend, String> {
    let kinds = scan(module).ok_or("its source could not be followed")?;
    let lines: Vec<(usize, &str)> = module
        .split_inclusive('\n')
        .scan(0, |at, l| {
            let start = *at;
            *at += l.len();
            Some((start, l))
        })
        .collect();
    let is_header = |l: &str| {
        l.strip_prefix("class ")
            .and_then(|r| r.strip_prefix(BACKEND))
            .is_some_and(|r| r.trim_start().starts_with(['(', ':']))
    };
    let headers: Vec<usize> = (0..lines.len())
        .filter(|&i| kinds[i] == Kind::Statement(0) && is_header(lines[i].1))
        .collect();
    let &[h] = headers.as_slice() else {
        return Err(if headers.is_empty() {
            "it has no RetroArch backend".into()
        } else {
            format!("it defines {BACKEND} more than once")
        });
    };
    let next = (h + 1..lines.len())
        .find(|&i| kinds[i] == Kind::Statement(0))
        .unwrap_or(lines.len());
    let body = h + 1..next;
    // Python takes the body's indentation from its first statement.
    let first = body.clone().find_map(|i| match kinds[i] {
        Kind::Statement(n) => Some(&lines[i].1[..n]),
        _ => None,
    });
    if first != Some(INDENT) {
        return Err(format!("{BACKEND}'s body is not indented by four spaces"));
    }
    let method = |name: &str| {
        body.clone().any(|i| {
            kinds[i] == Kind::Statement(INDENT.len())
                && lines[i].1[INDENT.len()..].starts_with(name)
        })
    };
    let last = body
        .clone()
        .rev()
        .find(|&i| matches!(kinds[i], Kind::Statement(_) | Kind::Continuation))
        .ok_or_else(|| format!("{BACKEND} has no body"))?;
    Ok(Backend {
        read: method(&format!("{MARKER}(")),
        write: method(&format!("{WRITE_MARKER}(")),
        end: lines[last].0 + lines[last].1.len(),
    })
}

/// Whether one copy of the module has the methods, needs them, or is not one the fix knows.
fn module_state(module: &str) -> ClientState {
    match backend(module) {
        Err(why) => ClientState::Unrecognized(why),
        Ok(Backend { read: true, .. }) => ClientState::Ready,
        // Somewhere other than the backend, it is not the change this fix makes.
        Ok(_) if module.contains(MARKER) => {
            ClientState::Unrecognized(format!("it defines read_bytestring, but not in {BACKEND}"))
        }
        Ok(_) => ClientState::NeedsFix,
    }
}

/// Whether the client in `apworld` (the bytes of an installed `dk64.apworld`) can reach AP64:
/// [`ClientState::Ready`] when every vendored EmuLoader's `RetroArchNetworkInfo` has the
/// methods.
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
    let mut state = ClientState::Ready;
    for (name, _, text) in &found {
        match module_state(text) {
            ClientState::Unrecognized(why) => {
                return ClientState::Unrecognized(format!("the EmuLoader in {name}: {why}"))
            }
            ClientState::NeedsFix => state = ClientState::NeedsFix,
            ClientState::Ready => {}
        }
    }
    state
}

/// `module` with [`METHODS`] at the end of `RetroArchNetworkInfo`'s body, and everything
/// else as it was. Checked: in the result, the backend has both methods.
fn add_methods(module: &str) -> Result<String, String> {
    let b = backend(module)?;
    let (head, tail) = module.split_at(b.end);
    let mut out = String::with_capacity(module.len() + METHODS.len() + 2);
    out.push_str(head);
    if !head.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(METHODS);
    // Something after the class: a blank line between, as there was before.
    if !tail.is_empty() && !tail.starts_with(['\n', '\r']) {
        out.push('\n');
    }
    out.push_str(tail);
    match backend(&out) {
        Ok(Backend {
            read: true,
            write: true,
            ..
        }) => Ok(out),
        other => Err(format!(
            "the methods did not land inside {BACKEND}: {other:?}"
        )),
    }
}

/// One vendored zip with the methods added to its module and the module's stale bytecode
/// dropped. Every other entry is copied as stored, byte for byte.
fn fix_vendored(zip_bytes: &[u8], text: &str) -> io::Result<Vec<u8>> {
    let fixed = add_methods(text).map_err(bad)?;
    let mut src = ZipArchive::new(Cursor::new(zip_bytes)).map_err(bad)?;
    let mut dst = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let f = src.by_index_raw(i).map_err(bad)?;
        let name = f.name().map_err(bad)?.into_owned();
        // Compiled before the change; left in place, Python could keep running the old code.
        if name.starts_with("emu_loader/__pycache__/retroarch_udp.") && name.ends_with(".pyc") {
            continue;
        }
        if name == MODULE {
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            drop(f);
            dst.start_file(name, opts).map_err(bad)?;
            dst.write_all(fixed.as_bytes())?;
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
        .filter(|(_, _, t)| module_state(t) == ClientState::NeedsFix)
        .map(|(name, bytes, text)| fix_vendored(&bytes, &text).map(|b| (name, b)))
        .collect::<io::Result<_>>()
        .map_err(io)?;
    let mut src = ZipArchive::new(Cursor::new(apworld)).map_err(|e| e.to_string())?;
    let mut dst = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let f = src.by_index_raw(i).map_err(|e| e.to_string())?;
        let entry = f.name().map_err(|e| e.to_string())?.into_owned();
        match fixes.iter().find(|(n, _)| *n == entry) {
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

/// Every installed copy of `dk64.apworld` ([`crate::installed_apworlds`]).
pub fn installed_apworlds() -> Vec<PathBuf> {
    crate::installed_apworlds(APWORLD)
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

    /// A module where the backend is not the last thing: a function and a constant follow it,
    /// and its body has a docstring with a line at column 0 and a call spread over lines.
    const NOT_LAST: &str = r#"import socket

class RetroArchNetworkInfo:
    """RetroArch's network commands.

Not indented: still the docstring.
    """

    def read_u8(self, address):
        return self.call(
'READ_CORE_MEMORY',
            address,
        )

# A comment at column 0, which does not end the class.
    def write_u8(self, address, value):
        pass


def connect():
    return RetroArchNetworkInfo()

TIMEOUT = 1
"#;

    fn fixed_module(module: &str) -> String {
        let fixed = fix_client(&apworld(&vendor(module))).unwrap();
        let inner = read(&fixed, "dk64/vendor/windows.zip").unwrap();
        String::from_utf8(read(&inner, MODULE).unwrap()).unwrap()
    }

    #[test]
    fn the_methods_go_inside_the_backend_whatever_follows_it() {
        assert_eq!(
            client_state(&apworld(&vendor(NOT_LAST))),
            ClientState::NeedsFix
        );
        let module = fixed_module(NOT_LAST);
        let at = |s: &str| module.find(s).unwrap_or_else(|| panic!("{s}"));
        // In the class, after its last method and before what follows it.
        assert!(at("        pass\n") < at(MARKER));
        assert!(at(WRITE_MARKER) < at("def connect():"));
        assert!(module.contains("\n    def read_bytestring(self, address: int, length: int)"));
        assert!(
            module.ends_with("def connect():\n    return RetroArchNetworkInfo()\n\nTIMEOUT = 1\n")
        );
        // Nothing else changed: the module is the original with the methods spliced in.
        let b = backend(NOT_LAST).unwrap();
        assert!(module.starts_with(&NOT_LAST[..b.end]));
        assert!(module.ends_with(&NOT_LAST[b.end..]));
        assert_eq!(
            backend(&module).map(|b| (b.read, b.write)),
            Ok((true, true))
        );
    }

    #[test]
    fn a_backend_the_fix_does_not_understand_is_left_alone() {
        let tabs = "class RetroArchNetworkInfo:\n\tdef read_u8(self, address):\n\t\treturn 0\n";
        let twice = format!("{UNFIXED}\n{UNFIXED}");
        let elsewhere =
            format!("{UNFIXED}\n\ndef read_bytestring(info, address, length):\n    return ''\n");
        let unclosed =
            "class RetroArchNetworkInfo:\n    def read_u8(self, address):\n        return (0\n";
        let empty = "class RetroArchNetworkInfo: pass\n";
        for module in [tabs, &twice, &elsewhere, unclosed, empty, "x = 1\n"] {
            let world = apworld(&vendor(module));
            assert!(
                matches!(client_state(&world), ClientState::Unrecognized(_)),
                "{module}"
            );
            assert!(fix_client(&world).is_err(), "{module}");
        }
    }

    #[test]
    fn a_line_in_a_string_or_bracket_is_not_a_statement() {
        let kinds = scan("x = (\n1)\ns = '''\ny = 2\n'''\n# c\n\nz = 'a#b' \\\n  + 'c'\n").unwrap();
        use Kind::*;
        assert_eq!(
            kinds,
            [
                Statement(0),
                Continuation,
                Statement(0),
                Continuation,
                Continuation,
                Comment,
                Blank,
                Statement(0),
                Continuation
            ]
        );
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
