//! Detect an Archipelago N64 seed and splice the M64P cart agent into it.
//!
//! A seed is recognised by its header and checked against a per-game [`profile`]: the
//! code the agent hooks into must be exactly what the profile was measured against, and
//! nothing of the seed may lie where the agent goes. Only then is anything written.

pub mod crc;
pub mod patch;
pub mod profile;
pub mod rom;
pub mod transform;

use std::collections::HashMap;

use serde::Serialize;

pub use patch::{apply, has_agent, verify, ApplyError, Bundle, Check, Patched, Report};
pub use profile::Profile;

macro_rules! builtin {
    ($dir:literal, [$($blob:literal),*]) => {{
        let text = include_str!(concat!("../profiles/", $dir, "/profile.toml"));
        let profile = Profile::parse(text).map_err(|e| format!("profiles/{}: {e}", $dir))?;
        let mut blobs = HashMap::new();
        $(blobs.insert(
            $blob.to_string(),
            include_bytes!(concat!("../profiles/", $dir, "/", $blob)).to_vec(),
        );)*
        Bundle::new(profile, blobs)?
    }};
}

/// The profiles this build offers: what the Play card lists and the patcher will accept.
pub fn builtin() -> Result<Vec<Bundle>, String> {
    Ok(vec![
        builtin!("cv64", ["agent.bin", "stub.bin"]),
        builtin!("pmr", ["agent.bin", "stub.bin"]),
        builtin!("oot", ["agent.bin", "stub.bin"]),
    ])
}

/// Profiles kept in the tree but not offered, because the game cannot be played through
/// for a reason outside AP64. Held here rather than deleted so they stay compiled, parsed
/// and checked against their blobs by the same tests as the rest; moving one into
/// [`builtin`] is all that is needed when its reason goes away.
///
/// - `cvlod` — Castlevania: Legacy of Darkness. The splice and the agent work on a cart,
///   but a seed freezes, in attract mode at a fixed point and again during play. The same
///   seed with no agent in it freezes at the same point in an emulator and the retail ROM
///   does not, so the fault is in Archipelago's CVLoD world (seen on v2.0.2), not here.
pub fn withheld() -> Result<Vec<Bundle>, String> {
    Ok(vec![builtin!("cvlod", ["agent.bin", "stub.bin"])])
}

/// A ROM file as loaded: normalised to big-endian, with what could be learned from it.
#[derive(Debug, Clone, Serialize)]
pub struct Detection {
    pub byte_order: String,
    pub header: Option<rom::Header>,
    /// The CIC its IPL3 belongs to, if a retail one. A seed's IPL3 may be patched.
    pub cic: Option<String>,
    /// Every profile for this header's game code, each verified against the seed.
    pub candidates: Vec<Report>,
}

impl Detection {
    /// The profile to use: the one candidate that passes every check.
    pub fn chosen(&self) -> Option<&Report> {
        let mut ok = self.candidates.iter().filter(|r| r.ok());
        match (ok.next(), ok.next()) {
            (Some(r), None) => Some(r),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum LoadError {
    NotARom,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotARom => f.write_str("not an N64 ROM (no z64, v64 or n64 header)"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Normalise `data` to big-endian in place and verify it against every matching profile.
pub fn detect(bundles: &[Bundle], data: &mut [u8]) -> Result<Detection, LoadError> {
    let order = rom::byte_order(data).ok_or(LoadError::NotARom)?;
    rom::to_big_endian(data, order);
    let header = rom::header(data);
    let candidates = bundles
        .iter()
        .filter(|b| {
            header
                .as_ref()
                .is_some_and(|h| h.game_code == b.profile.game_code)
        })
        .map(|b| verify(b, data))
        .collect();
    Ok(Detection {
        byte_order: order.to_string(),
        cic: crc::identify(data).map(|c| c.to_string()),
        header,
        candidates,
    })
}

/// The profile whose agent a patched ROM (big-endian) carries, if any.
pub fn patched_with<'a>(bundles: &'a [Bundle], rom: &[u8]) -> Option<&'a Bundle> {
    let h = rom::header(rom)?;
    bundles
        .iter()
        .filter(|b| b.profile.game_code == h.game_code && b.profile.version == h.version)
        .find(|b| has_agent(b, rom))
}

/// Where the output goes by default: beside the input, `<stem>-agent.z64`.
pub fn default_output(input: &std::path::Path) -> std::path::PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("seed");
    input.with_file_name(format!("{stem}-agent.z64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(text: &str, key: &str) -> u32 {
        let line = text
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("layout.env has no {key}"));
        let v = line.trim();
        match v.strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16).unwrap(),
            None => v.parse().unwrap(),
        }
    }

    /// Instructions of `stub.bin`, big-endian words in order.
    fn stub_words(bundle: &Bundle) -> Vec<u32> {
        let file = bundle
            .profile
            .write
            .iter()
            .find_map(|w| match w {
                profile::Write::Blob { file, .. } => Some(file),
                _ => None,
            })
            .expect("the profile writes a stub");
        bundle.blobs[file]
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect()
    }

    /// 1 if some `lui`/`lw` pair in `w` reads the word at `addr`, as %hi/%lo of it.
    fn loads_word_at(w: &[u32], addr: u32) -> bool {
        w.iter().enumerate().any(|(i, &lui)| {
            if lui >> 26 != 0x0F {
                return false;
            }
            let (reg, hi) = ((lui >> 16) & 0x1F, (lui & 0xFFFF) << 16);
            w[i + 1..].iter().any(|&lw| {
                lw >> 26 == 0x23
                    && (lw >> 21) & 0x1F == reg
                    && hi.wrapping_add((lw & 0xFFFF) as i16 as u32) == addr
            })
        })
    }

    /// 1 if some `lui`/`ori` pair in `w` builds `value` in one register.
    fn builds_word(w: &[u32], value: u32) -> bool {
        w.windows(2).any(|p| {
            p[0] >> 26 == 0x0F
                && p[0] & 0xFFFF == value >> 16
                && p[1] >> 26 == 0x0D
                && (p[1] >> 21) & 0x1F == (p[0] >> 16) & 0x1F
                && (p[1] >> 16) & 0x1F == (p[0] >> 16) & 0x1F
                && p[1] & 0xFFFF == value & 0xFFFF
        })
    }

    /// A stub must not run the agent without first seeing that the agent is there.
    ///
    /// Every integration keeps the agent in RAM the game is not expected to use, and
    /// nothing reserves it: if the game ever writes over that RAM, the retargeted jal runs
    /// whatever is there now, and the console is gone with no way back. The marker
    /// `gAgentSegmentMagic` is what says the image is intact
    /// (`n64/agent/templates/segment_magic.c`), so a stub reads it and compares it with
    /// 'M64P' before jumping in -- whether it loaded the agent itself or, as OoT's does,
    /// relies on the game's own loader to have done it at boot.
    ///
    /// This reads the linked instructions, so it holds for the blob AP64 actually writes.
    #[test]
    fn every_stub_reads_the_load_marker_before_running_the_agent() {
        const M64P: u32 = 0x4D36_3450;
        let mut bundles = builtin().unwrap();
        bundles.extend(withheld().unwrap());
        let layouts = [
            include_str!("../profiles/cv64/layout.env"),
            include_str!("../profiles/pmr/layout.env"),
            include_str!("../profiles/oot/layout.env"),
            include_str!("../profiles/cvlod/layout.env"),
        ];
        for (b, env) in bundles.iter().zip(layouts) {
            let id = &b.profile.id;
            let w = stub_words(b);
            let magic = layout(env, "AGENT_MAGIC_ADDR");
            assert!(
                loads_word_at(&w, magic),
                "{id}: the stub never reads the marker at 0x{magic:X}"
            );
            assert!(
                builds_word(&w, M64P),
                "{id}: the stub never builds 'M64P' to compare the marker with"
            );
        }
    }

    /// The profile must describe the blobs it carries: the numbers in profile.toml are
    /// typed by hand, layout.env is written by the build that linked the blobs.
    #[test]
    fn builtin_profiles_match_the_build_that_made_their_blobs() {
        let mut bundles = builtin().unwrap();
        bundles.extend(withheld().unwrap());
        let layouts = [
            ("cv64", include_str!("../profiles/cv64/layout.env")),
            ("pmr", include_str!("../profiles/pmr/layout.env")),
            ("oot", include_str!("../profiles/oot/layout.env")),
            ("cvlod", include_str!("../profiles/cvlod/layout.env")),
        ];
        assert_eq!(bundles.len(), layouts.len());
        for (b, (id, env)) in bundles.iter().zip(layouts) {
            let p = &b.profile;
            assert_eq!(p.id, id);
            assert_eq!(p.agent.rom, layout(env, "AGENT_ROM"), "{id} AGENT_ROM");
            assert_eq!(p.agent.vram, layout(env, "AGENT_VRAM"), "{id} AGENT_VRAM");
            assert_eq!(
                p.agent.min_ram,
                layout(env, "AGENT_MIN_RAM"),
                "{id} AGENT_MIN_RAM"
            );
            assert_eq!(
                b.blobs[&p.agent.image].len() as u32,
                layout(env, "AGENT_LOAD_SIZE"),
                "{id} agent size"
            );
            let stub_vram = layout(env, "STUB_VRAM");
            let stub_size = layout(env, "STUB_SIZE");
            let jal = p.write.iter().find_map(|w| match w {
                profile::Write::Jal { target, .. } => Some(*target),
                _ => None,
            });
            assert_eq!(jal, Some(stub_vram), "{id} jal target is the stub");
            let stub = p.write.iter().find_map(|w| match w {
                profile::Write::Blob { file, .. } => Some(b.blobs[file].len() as u32),
                _ => None,
            });
            assert_eq!(stub, Some(stub_size), "{id} stub size");
            if p.agent.bss > 0 {
                assert_eq!(
                    p.agent.bss,
                    layout(env, "AGENT_BSS_END") - layout(env, "AGENT_BSS_START"),
                    "{id} BSS the loader zeroes"
                );
            }
        }
    }
}
