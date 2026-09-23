//! A game profile: how to recognize a seed of one game, what must still hold in it, and
//! what to write. Profiles are TOML beside the blobs they reference (`profiles/<id>/`).
//!
//! Profiles carry no retail bytes beyond single instruction words: regions are checked by
//! hash, and code comes from our own blobs.

use serde::Deserialize;

use crate::crc::Cic;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Stable identifier, also the directory name.
    pub id: String,
    /// Shown to the user.
    pub name: String,
    /// Which release, e.g. "US 1.0".
    pub release: String,
    /// Header game code (0x3B..0x3F).
    pub game_code: String,
    /// Header version byte (0x3F).
    pub version: u8,
    /// The CIC whose IPL3 the output must carry, and whose checksum it gets.
    pub cic: String,
    /// The randomizer this profile was measured against, shown to the user.
    pub randomizer: String,
    /// Which connector script plays this game (`connectors/<id>/`).
    pub connector: String,
    /// Applied to the seed before any check or write.
    #[serde(default)]
    pub transform: Option<Transform>,
    pub agent: Agent,
    #[serde(default)]
    pub require: Vec<Require>,
    #[serde(default)]
    pub write: Vec<Write>,
}

/// A whole-ROM transform (see `transform.rs`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum Transform {
    /// Decompress every Yaz0 file listed in the file table at `table`.
    Yaz0Dmadata { table: u32 },
}

/// The agent image, appended to the ROM and copied to RAM at boot.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// Blob file name in the profile directory.
    pub image: String,
    /// ROM offset the image is written at.
    pub rom: u32,
    /// Start of the ROM this profile appends (default `rom`). Nothing of the seed may lie at
    /// or past it, and writes there need no check: the bytes are all ours.
    #[serde(default)]
    pub region: Option<u32>,
    /// Zero bytes after the image, for an agent loaded by a loader that does not clear BSS.
    #[serde(default)]
    pub bss: u32,
    /// Make the appended region one file in the file table, with its entry at this offset.
    #[serde(default)]
    pub dma_slot: Option<u32>,
    /// RAM address it is linked at (informational; the stub carries it).
    pub vram: u32,
    /// Smallest `osMemSize` the stub loads it with (0 = always).
    pub min_ram: u32,
}

impl Agent {
    pub fn region(&self) -> u32 {
        self.region.unwrap_or(self.rom)
    }
}

/// The value an `imm` write puts in a `lui`/`addiu` pair.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImmValue {
    Const(u32),
    /// `"region_start"` (the appended file's vrom) or `"region_size"` (its length).
    Named(String),
}

/// Something the seed must satisfy before anything is written.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum Require {
    /// A 32-bit word equals a constant (typically the call being retargeted).
    Word {
        label: String,
        at: u32,
        equals: u32,
        #[serde(default)]
        hint: String,
    },
    /// A region hashes to a known SHA-1 (typically where the stub goes, or code the stub calls).
    Sha1 {
        label: String,
        at: u32,
        len: u32,
        sha1: String,
        #[serde(default)]
        hint: String,
    },
}

/// Something written into the seed.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum Write {
    /// A blob from the profile directory, at a fixed offset, no longer than `max_len`.
    Blob {
        label: String,
        at: u32,
        file: String,
        max_len: u32,
    },
    /// A `jal` to `target`, replacing the word at `at`.
    Jal { label: String, at: u32, target: u32 },
    /// Copy `len` bytes of the seed, from `from`, into the appended region at `at`.
    Copy {
        label: String,
        from: u32,
        len: u32,
        at: u32,
    },
    /// Set the immediate of the `lui` at `hi`, and of the `addiu` at `lo` if given, so the
    /// pair loads `value`. Opcode and registers are kept.
    Imm {
        label: String,
        hi: u32,
        #[serde(default)]
        lo: Option<u32>,
        value: ImmValue,
    },
    /// Put back bytes a patch changed. The seed must hold either `bytes` already or one of
    /// `accept`, so this can only undo a known change, never overwrite an unknown one.
    Restore {
        label: String,
        at: u32,
        bytes: String,
        accept: Vec<String>,
    },
}

impl Write {
    pub fn label(&self) -> &str {
        match self {
            Write::Blob { label, .. }
            | Write::Jal { label, .. }
            | Write::Restore { label, .. }
            | Write::Copy { label, .. }
            | Write::Imm { label, .. } => label,
        }
    }
}

impl Profile {
    pub fn parse(text: &str) -> Result<Self, String> {
        let profile: Profile = toml::from_str(text).map_err(|e| e.to_string())?;
        profile.cic()?;
        if profile.game_code.len() != 4 {
            return Err(format!(
                "game_code {:?} is not 4 characters",
                profile.game_code
            ));
        }
        // Every write must land on bytes a check has pinned down, so a seed that changed
        // them is refused instead of silently overwritten. The appended region is ours.
        let region = profile.agent.region();
        if profile.agent.rom < region {
            return Err("agent.rom lies before agent.region".into());
        }
        let pinned = |at: u32, len: u32| {
            at >= region
                || profile.require.iter().any(|r| match r {
                    Require::Word { at: w, .. } => *w == at && len == 4,
                    Require::Sha1 { at: s, len: n, .. } => *s <= at && at + len <= s + n,
                })
        };
        if let Some(slot) = profile.agent.dma_slot {
            if !pinned(slot, 32) {
                return Err(format!(
                    "dma_slot 0x{slot:X} and the entry after it are not covered by a sha1 check"
                ));
            }
        }
        for w in &profile.write {
            match w {
                Write::Restore { bytes, accept, .. } => {
                    let len = parse_hex(bytes)?.len();
                    for a in accept {
                        if parse_hex(a)?.len() != len {
                            return Err(format!(
                                "restore {:?}: accept {a:?} is not {len} bytes",
                                w.label()
                            ));
                        }
                    }
                }
                Write::Blob { at, max_len, .. } if !pinned(*at, *max_len) => {
                    return Err(format!(
                        "blob {:?} at 0x{at:X}+0x{max_len:X} is not covered by a sha1 check",
                        w.label()
                    ));
                }
                Write::Jal { at, .. } if !pinned(*at, 4) => {
                    return Err(format!(
                        "jal {:?} at 0x{at:X} is not covered by a word check",
                        w.label()
                    ));
                }
                Write::Copy { at, len, .. } if *at < region || at + len > profile.agent.rom => {
                    return Err(format!(
                        "copy {:?} must land in the appended region, before the agent",
                        w.label()
                    ));
                }
                Write::Imm { hi, lo, value, .. } => {
                    for at in std::iter::once(*hi).chain(*lo) {
                        if !pinned(at, 4) {
                            return Err(format!(
                                "imm {:?} at 0x{at:X} is not covered by a word check",
                                w.label()
                            ));
                        }
                    }
                    if let ImmValue::Named(n) = value {
                        if n != "region_start" && n != "region_size" {
                            return Err(format!("imm {:?}: unknown value {n:?}", w.label()));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(profile)
    }

    pub fn cic(&self) -> Result<Cic, String> {
        self.cic.parse()
    }
}

pub(crate) fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 {
        return Err(format!("hex {s:?} has an odd number of digits"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("bad hex {s:?}")))
        .collect()
}
