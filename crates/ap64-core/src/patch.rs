//! Verify a seed against a profile, and splice the agent into it.
//!
//! `apply` re-runs `verify` and refuses on any failure, then diffs its output against the
//! input: a byte changed anywhere a profile write, the header checksum or the appended
//! agent does not account for is a bug, and the output is withheld.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use serde::Serialize;
use sha1::{Digest, Sha1};

use crate::crc::{self, CRC_END, CRC_OFFSET};
use crate::profile::{parse_hex, ImmValue, Profile, Transform, Write};
use crate::rom;

/// A profile together with the blobs its writes reference.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub profile: Profile,
    pub blobs: HashMap<String, Vec<u8>>,
}

impl Bundle {
    pub fn new(profile: Profile, blobs: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let bundle = Bundle { profile, blobs };
        bundle.blob(&bundle.profile.agent.image)?;
        for w in &bundle.profile.write {
            if let Write::Blob { file, .. } = w {
                bundle.blob(file)?;
            }
        }
        Ok(bundle)
    }

    fn blob(&self, name: &str) -> Result<&[u8], String> {
        self.blobs.get(name).map(Vec::as_slice).ok_or_else(|| {
            format!(
                "profile {} names {name:?}, which it does not carry",
                self.profile.id
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Check {
    pub label: String,
    pub ok: bool,
    /// What was found, for the user.
    pub detail: String,
    /// What a failure usually means (e.g. a randomizer option that moves code).
    pub hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub profile_id: String,
    pub game: String,
    pub release: String,
    pub randomizer: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Patched {
    #[serde(skip)]
    pub rom: Vec<u8>,
    pub sha1: String,
    /// What was written, one line each.
    pub summary: Vec<String>,
}

fn check(label: &str, ok: bool, detail: String, hint: &str) -> Check {
    Check {
        label: label.to_string(),
        ok,
        detail,
        hint: if ok { String::new() } else { hint.to_string() },
    }
}

fn region(rom: &[u8], at: u32, len: usize) -> Option<&[u8]> {
    rom.get(at as usize..at as usize + len)
}

/// The seed as the profile's checks and writes see it: after its transform, if it has one.
pub fn prepare<'a>(bundle: &Bundle, rom: &'a [u8]) -> Result<Cow<'a, [u8]>, String> {
    match &bundle.profile.transform {
        None => Ok(Cow::Borrowed(rom)),
        Some(Transform::Yaz0Dmadata { table }) => {
            crate::transform::yaz0_dmadata(rom, *table).map(Cow::Owned)
        }
    }
}

/// `rom` must already be big-endian.
pub fn verify(bundle: &Bundle, rom: &[u8]) -> Report {
    match prepare(bundle, rom) {
        Ok(prepared) => verify_prepared(bundle, &prepared),
        Err(e) => report(
            &bundle.profile,
            vec![check(
                "Decompress",
                false,
                e,
                "this does not look like a seed of this game",
            )],
        ),
    }
}

fn verify_prepared(bundle: &Bundle, rom: &[u8]) -> Report {
    let p = &bundle.profile;
    let mut checks = Vec::new();

    let header = rom::header(rom);
    let (code, version) = header
        .as_ref()
        .map_or((String::from("?"), 0), |h| (h.game_code.clone(), h.version));
    checks.push(check(
        "Game",
        code == p.game_code && version == p.version,
        format!("header says {code} v{version}"),
        &format!(
            "this profile is for {} {} ({} v{})",
            p.name, p.release, p.game_code, p.version
        ),
    ));
    checks.push(check(
        "Size",
        rom.len() >= CRC_END,
        format!("{} bytes", rom.len()),
        "too small to be an N64 ROM",
    ));
    if rom.len() < CRC_END {
        return report(p, checks);
    }

    for r in &p.require {
        checks.push(match r {
            crate::profile::Require::Word {
                label,
                at,
                equals,
                hint,
            } => {
                let found = rom::read_u32(rom, *at as usize);
                check(
                    label,
                    found == Some(*equals),
                    match found {
                        Some(w) => format!("0x{at:X} holds 0x{w:08X} (expected 0x{equals:08X})"),
                        None => format!("0x{at:X} is past the end"),
                    },
                    hint,
                )
            }
            crate::profile::Require::Sha1 {
                label,
                at,
                len,
                sha1,
                hint,
            } => {
                let found = region(rom, *at, *len as usize).map(|b| crc::hex(&Sha1::digest(b)));
                check(
                    label,
                    found.as_deref() == Some(sha1.as_str()),
                    match found {
                        Some(h) if h == *sha1 => format!("0x{at:X}+0x{len:X} unchanged"),
                        Some(_) => format!("0x{at:X}+0x{len:X} differs from the known bytes"),
                        None => format!("0x{at:X}+0x{len:X} is past the end"),
                    },
                    hint,
                )
            }
        });
    }

    // Restores apply to a scratch copy, so IPL3 can be identified as it will be written.
    let mut restored = rom.to_vec();
    for w in &p.write {
        match w {
            Write::Restore {
                label,
                at,
                bytes,
                accept,
            } => {
                let want = parse_hex(bytes).unwrap_or_default();
                let found = region(rom, *at, want.len()).map(<[u8]>::to_vec);
                let accepted = found.as_ref().is_some_and(|f| {
                    *f == want || accept.iter().any(|a| parse_hex(a).ok().as_ref() == Some(f))
                });
                if accepted {
                    restored[*at as usize..*at as usize + want.len()].copy_from_slice(&want);
                }
                checks.push(check(
                    label,
                    accepted,
                    match found {
                        Some(f) if f == want => {
                            format!("0x{at:X} already holds the original bytes")
                        }
                        Some(f) if accepted => {
                            format!("0x{at:X} holds {}; restoring {bytes}", crc::hex(&f))
                        }
                        Some(f) => format!("0x{at:X} holds {}", crc::hex(&f)),
                        None => format!("0x{at:X} is past the end"),
                    },
                    "the patch changed these bytes in a way this profile does not know how to undo",
                ));
            }
            Write::Blob {
                label,
                file,
                max_len,
                ..
            } => {
                let len = bundle.blobs.get(file).map_or(0, Vec::len);
                checks.push(check(
                    label,
                    len as u32 <= *max_len,
                    format!("{len} bytes (room for {max_len})"),
                    "the profile's blob does not fit where it goes",
                ));
            }
            Write::Copy {
                label, from, len, ..
            } => {
                checks.push(check(
                    label,
                    region(rom, *from, *len as usize).is_some(),
                    format!("0x{from:X}+0x{len:X}"),
                    "the seed is too short to hold what this copies",
                ));
            }
            Write::Jal { .. } | Write::Imm { .. } => {}
        }
    }

    let expected_cic = p.cic().ok();
    let found_cic = crc::identify(&restored);
    checks.push(check(
        "Boot code (IPL3)",
        found_cic.is_some() && found_cic == expected_cic,
        match found_cic {
            Some(c) => format!("retail {c} boot code"),
            None => "not a known retail boot code".to_string(),
        },
        "the output would carry boot code whose checksum this tool cannot produce",
    ));

    let agent_rom = p.agent.region() as usize;
    let tail = rom.get(agent_rom..).unwrap_or(&[]);
    let last_data = rom
        .iter()
        .rposition(|&b| b != 0x00 && b != 0xFF)
        .map_or(0, |i| i + 1);
    checks.push(check(
        "Room for the agent",
        tail.iter().all(|&b| b == 0x00 || b == 0xFF),
        format!("seed data ends at 0x{last_data:X}; the agent goes at 0x{agent_rom:X}"),
        "the randomizer's data reaches where the agent goes",
    ));

    report(p, checks)
}

fn report(p: &Profile, checks: Vec<Check>) -> Report {
    Report {
        profile_id: p.id.clone(),
        game: p.name.clone(),
        release: p.release.clone(),
        randomizer: p.randomizer.clone(),
        checks,
    }
}

#[derive(Debug)]
pub enum ApplyError {
    /// A check failed; nothing was written.
    Refused(Report),
    /// The output changed something no write accounts for. A bug; nothing was written.
    Internal(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Refused(r) => {
                let failed: Vec<_> = r
                    .checks
                    .iter()
                    .filter(|c| !c.ok)
                    .map(|c| c.label.as_str())
                    .collect();
                write!(f, "refused: {}", failed.join(", "))
            }
            ApplyError::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for ApplyError {}

fn jal(target: u32) -> u32 {
    0x0C00_0000 | ((target >> 2) & 0x03FF_FFFF)
}

fn put(out: &mut Vec<u8>, allowed: &mut Vec<Range<usize>>, at: usize, bytes: &[u8]) {
    if out.len() < at + bytes.len() {
        out.resize(at + bytes.len(), 0);
    }
    out[at..at + bytes.len()].copy_from_slice(bytes);
    allowed.push(at..at + bytes.len());
}

/// `rom` must already be big-endian.
pub fn apply(bundle: &Bundle, rom: &[u8]) -> Result<Patched, ApplyError> {
    let p = &bundle.profile;
    let input = prepare(bundle, rom).map_err(|_| ApplyError::Refused(verify(bundle, rom)))?;
    let report = verify_prepared(bundle, &input);
    if !report.ok() {
        return Err(ApplyError::Refused(report));
    }
    let cic = p.cic().map_err(ApplyError::Internal)?;
    let agent = bundle.blob(&p.agent.image).map_err(ApplyError::Internal)?;
    let region_start = p.agent.region() as usize;
    let mut out = input.to_vec();
    out.truncate(region_start);
    out.resize(region_start, 0);
    let mut allowed: Vec<Range<usize>> = Vec::new();
    allowed.push(CRC_OFFSET..CRC_OFFSET + 8);
    allowed.push(region_start..usize::MAX);
    let mut summary = Vec::new();
    if p.transform.is_some() {
        summary.push(format!("Decompressed: {} bytes", input.len()));
    }

    for w in &p.write {
        let (at, bytes) = match w {
            Write::Blob { at, file, .. } => (
                *at,
                bundle.blob(file).map_err(ApplyError::Internal)?.to_vec(),
            ),
            Write::Jal { at, target, .. } => (*at, jal(*target).to_be_bytes().to_vec()),
            Write::Restore { at, bytes, .. } => {
                (*at, parse_hex(bytes).map_err(ApplyError::Internal)?)
            }
            Write::Copy { at, from, len, .. } => (
                *at,
                region(&input, *from, *len as usize)
                    .ok_or_else(|| ApplyError::Internal("copy source past the end".into()))?
                    .to_vec(),
            ),
            Write::Imm { .. } => continue,
        };
        put(&mut out, &mut allowed, at as usize, &bytes);
        summary.push(match w {
            Write::Jal { label, target, .. } => {
                format!("{label}: jal at 0x{at:X} -> 0x{target:08X}")
            }
            Write::Copy { label, from, .. } => {
                format!("{label}: {} bytes from 0x{from:X} to 0x{at:X}", bytes.len())
            }
            _ => format!("{}: {} bytes at 0x{at:X}", w.label(), bytes.len()),
        });
    }

    let agent_rom = p.agent.rom as usize;
    if out.len() > agent_rom {
        return Err(ApplyError::Internal(
            "a write lands where the agent goes".into(),
        ));
    }
    out.resize(agent_rom, 0);
    out.extend_from_slice(agent);
    out.resize(out.len() + p.agent.bss as usize, 0);
    let align = if p.agent.dma_slot.is_some() { 16 } else { 4 };
    out.resize(out.len().next_multiple_of(align), 0);
    summary.push(format!(
        "Agent: {} bytes at ROM 0x{agent_rom:X}{}, runs at 0x{:08X}{}",
        agent.len(),
        if p.agent.bss > 0 {
            format!(" with {} bytes of BSS", p.agent.bss)
        } else {
            String::new()
        },
        p.agent.vram,
        if p.agent.min_ram > 0 {
            format!(
                ", loaded only with {} MiB of RAM or more",
                p.agent.min_ram >> 20
            )
        } else {
            String::new()
        }
    ));

    let region_size = (out.len() - region_start) as u32;
    if let Some(slot) = p.agent.dma_slot {
        let start = region_start as u32;
        let entry: Vec<u8> = [start, start + region_size, start, 0]
            .iter()
            .flat_map(|w| w.to_be_bytes())
            .collect();
        put(&mut out, &mut allowed, slot as usize, &entry);
        summary.push(format!(
            "File table: new file 0x{start:X}-0x{:X} (entry at 0x{slot:X})",
            start + region_size
        ));
    }

    for w in &p.write {
        if let Write::Imm {
            label,
            hi,
            lo,
            value,
        } = w
        {
            let v = match value {
                ImmValue::Const(v) => *v,
                ImmValue::Named(n) if n == "region_start" => region_start as u32,
                ImmValue::Named(_) => region_size,
            };
            let (hi16, lo16) = match lo {
                Some(_) => ((v.wrapping_add(0x8000) >> 16) & 0xFFFF, v & 0xFFFF),
                None if v & 0xFFFF == 0 => (v >> 16, 0),
                None => {
                    return Err(ApplyError::Internal(format!(
                        "imm {label:?}: 0x{v:X} needs an addiu, and the profile gives none"
                    )))
                }
            };
            for (at, imm) in std::iter::once((*hi, hi16)).chain(lo.map(|l| (l, lo16))) {
                let old = rom::read_u32(&out, at as usize)
                    .ok_or_else(|| ApplyError::Internal(format!("imm {label:?} past the end")))?;
                let word = (old & 0xFFFF_0000) | imm;
                put(&mut out, &mut allowed, at as usize, &word.to_be_bytes());
            }
            summary.push(format!("{label}: 0x{v:X} into the lui at 0x{hi:X}"));
        }
    }

    let (a, b) = crc::fix(&mut out, cic);
    summary.push(format!("Checksum: {cic} 0x{a:08X} 0x{b:08X}"));

    if let Some(i) = (0..input.len().min(out.len()))
        .find(|&i| input[i] != out[i] && !allowed.iter().any(|r| r.contains(&i)))
    {
        return Err(ApplyError::Internal(format!(
            "unexpected change at 0x{i:X}"
        )));
    }
    if crc::identify(&out) != Some(cic) || crc::stored(&out) != Some(crc::compute(&out, cic)) {
        return Err(ApplyError::Internal(
            "output does not boot-check".to_string(),
        ));
    }

    let sha1 = crc::hex(&Sha1::digest(&out)).to_uppercase();
    Ok(Patched {
        rom: out,
        sha1,
        summary,
    })
}

/// Whether `rom` (big-endian) already carries this profile's agent: every hook points
/// at its target, every blob is in place, and the agent image sits at its offset.
pub fn has_agent(bundle: &Bundle, rom: &[u8]) -> bool {
    let p = &bundle.profile;
    let agent = match bundle.blobs.get(&p.agent.image) {
        Some(a) => a,
        None => return false,
    };
    let writes_hold = p.write.iter().all(|w| match w {
        Write::Jal { at, target, .. } => rom::read_u32(rom, *at as usize) == Some(jal(*target)),
        Write::Blob { at, file, .. } => bundle
            .blobs
            .get(file)
            .is_some_and(|b| region(rom, *at, b.len()) == Some(b.as_slice())),
        Write::Restore { .. } | Write::Copy { .. } | Write::Imm { .. } => true,
    });
    writes_hold && region(rom, p.agent.rom, agent.len()) == Some(agent.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::test_ipl3;

    const HOOK: usize = 0x2000;
    const STUB: usize = 0x3000;
    const AGENT: usize = 0x18_0000;

    /// 2 MiB with a header, the test IPL3 (its word at 0x66C NOPped, as a patch would),
    /// a hook word, a stub region and data up to 0x100000.
    fn seed() -> Vec<u8> {
        let mut rom = vec![0xFF; 0x20_0000];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        rom[0x20..0x34].copy_from_slice(b"TEST GAME           ");
        rom[0x3B..0x3F].copy_from_slice(b"NTSE");
        rom[0x3F] = 0;
        rom[crc::IPL3].fill(test_ipl3::FILL);
        rom[0x66C..0x670].fill(0);
        for (i, b) in rom[0x1000..0x10_0000].iter_mut().enumerate() {
            *b = (i * 7 % 251) as u8;
        }
        rom::write_u32(&mut rom, HOOK, 0x0C00_08CD);
        rom
    }

    fn bundle(stub_len: usize, rom: &[u8]) -> Bundle {
        let sha = crc::hex(&Sha1::digest(&rom[STUB..STUB + 0x100]));
        let text = format!(
            r#"
id = "test"
name = "Test"
release = "US"
game_code = "NTSE"
version = 0
cic = "6102"
randomizer = "none"
connector = "generic"
[agent]
image = "agent.bin"
rom = {AGENT}
vram = 0x80480000
min_ram = 0x800000
[[require]]
kind = "word"
label = "Hook"
at = {HOOK}
equals = 0x0C0008CD
[[require]]
kind = "sha1"
label = "Stub space"
at = {STUB}
len = 0x100
sha1 = "{sha}"
[[write]]
kind = "restore"
label = "IPL3"
at = 0x66C
bytes = "a5a5a5a5"
accept = ["00000000"]
[[write]]
kind = "blob"
label = "Stub"
at = {STUB}
file = "stub.bin"
max_len = 0x100
[[write]]
kind = "jal"
label = "Jal"
at = {HOOK}
target = 0x80019C00
"#
        );
        let blobs = HashMap::from([
            ("agent.bin".to_string(), vec![0x11; 0x1103]),
            ("stub.bin".to_string(), vec![0x22; stub_len]),
        ]);
        Bundle::new(Profile::parse(&text).unwrap(), blobs).unwrap()
    }

    fn failed(r: &Report) -> Vec<&str> {
        r.checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.label.as_str())
            .collect()
    }

    fn refused(b: &Bundle, rom: &[u8]) -> Vec<String> {
        match apply(b, rom) {
            Err(ApplyError::Refused(r)) => failed(&r).into_iter().map(String::from).collect(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_clean_seed_is_spliced_and_checksummed() {
        let rom = seed();
        let b = bundle(0xCC, &rom);
        let out = apply(&b, &rom).unwrap().rom;
        assert_eq!(rom::read_u32(&out, HOOK), Some(0x0C00_6700));
        assert_eq!(out[STUB..STUB + 0xCC], [0x22; 0xCC]);
        assert_eq!(
            out[STUB + 0xCC..STUB + 0x100],
            rom[STUB + 0xCC..STUB + 0x100]
        );
        assert_eq!(out[0x66C..0x670], [0xA5; 4]);
        assert_eq!(out[AGENT..AGENT + 0x1103], [0x11; 0x1103]);
        assert_eq!(out.len(), AGENT + 0x1104, "padded to a whole word");
        assert_eq!(
            crc::stored(&out),
            Some(crc::compute(&out, crc::Cic::Cic6102))
        );
        assert_eq!(out[0x1000..0x2000], rom[0x1000..0x2000]);
    }

    #[test]
    fn has_agent_tells_a_patched_rom_from_its_seed() {
        let rom = seed();
        let b = bundle(0xCC, &rom);
        assert!(!has_agent(&b, &rom));
        let out = apply(&b, &rom).unwrap().rom;
        assert!(has_agent(&b, &out));
    }

    #[test]
    fn a_seed_ending_short_of_the_agent_is_padded_up_to_it() {
        let rom = seed()[..0x10_1000].to_vec();
        let out = apply(&bundle(4, &rom), &rom).unwrap().rom;
        assert!(out[0x10_1000..AGENT].iter().all(|&b| b == 0));
        assert_eq!(out.len(), AGENT + 0x1104);
    }

    #[test]
    fn a_changed_hook_site_is_refused() {
        let mut rom = seed();
        let b = bundle(4, &rom);
        rom::write_u32(&mut rom, HOOK, 0x0C00_0000);
        assert_eq!(refused(&b, &rom), ["Hook"]);
    }

    #[test]
    fn a_changed_stub_region_is_refused() {
        let mut rom = seed();
        let b = bundle(4, &rom);
        rom[STUB + 0xFF] ^= 1;
        assert_eq!(refused(&b, &rom), ["Stub space"]);
    }

    #[test]
    fn data_where_the_agent_goes_is_refused() {
        let mut rom = seed();
        let b = bundle(4, &rom);
        rom[AGENT + 0x1_0000] = 0x42;
        assert_eq!(refused(&b, &rom), ["Room for the agent"]);
    }

    #[test]
    fn an_unknown_change_to_restored_bytes_is_refused() {
        let mut rom = seed();
        let b = bundle(4, &rom);
        rom[0x66D] = 0x12;
        // Unrestorable, so the boot code also stays unknown.
        assert_eq!(refused(&b, &rom), ["IPL3", "Boot code (IPL3)"]);
    }

    #[test]
    fn any_other_boot_code_change_is_refused() {
        let mut rom = seed();
        let b = bundle(4, &rom);
        rom[0x800] = 0;
        assert_eq!(refused(&b, &rom), ["Boot code (IPL3)"]);
    }

    #[test]
    fn a_stub_too_large_is_refused() {
        let rom = seed();
        assert_eq!(refused(&bundle(0x104, &rom), &rom), ["Stub"]);
    }

    #[test]
    fn another_game_is_refused() {
        let mut rom = seed();
        let b = bundle(4, &rom);
        rom[0x3E] = b'J';
        assert_eq!(refused(&b, &rom), ["Game"]);
    }

    #[test]
    fn a_write_no_check_pins_down_is_a_profile_error() {
        let unpinned = r#"
id = "t"
name = "T"
release = "US"
game_code = "NTSE"
version = 0
cic = "6102"
randomizer = "none"
connector = "generic"
[agent]
image = "a"
rom = 0x100000
vram = 0
min_ram = 0
[[write]]
kind = "jal"
label = "Jal"
at = 0x2000
target = 0x80000400
"#;
        assert!(Profile::parse(unpinned)
            .unwrap_err()
            .contains("not covered"));
    }

    #[test]
    fn detect_accepts_a_byte_swapped_file() {
        let rom = seed();
        let b = bundle(4, &rom);
        let mut v64 = rom.clone();
        v64.chunks_exact_mut(2).for_each(|c| c.swap(0, 1));
        let d = crate::detect(std::slice::from_ref(&b), &mut v64).unwrap();
        assert_eq!(v64, rom);
        assert_eq!(d.byte_order, "v64 (byte-swapped)");
        assert_eq!(d.chosen().map(|r| r.profile_id.as_str()), Some("test"));
    }
}
