//! Verify a seed against a profile, and splice the agent into it.
//!
//! `apply` re-runs `verify` and refuses on any failure, then diffs its output against the
//! input: a byte changed anywhere a profile write, the header checksum or the appended
//! agent does not account for is a bug, and the output is withheld.
//!
//! A profile with a transform is checked and patched in the decompressed image, then laid back
//! out over the seed (`transform::repack`), and withheld unless that decompresses to exactly
//! the image that was patched.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use serde::Serialize;
use sha1::{Digest, Sha1};

use crate::crc::{self, CRC_END, CRC_OFFSET};
use crate::profile::{parse_hex, Addr, ImmValue, Profile, Require, Transform, Write};
use crate::rom;

/// Where each of a profile's finds was located in a seed, by name. A find that was not
/// located, or was located more than once, is absent.
type Found = HashMap<String, u32>;

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

/// Locate each of the profile's finds in `rom`, with a check for each.
fn locate(p: &Profile, rom: &[u8]) -> (Vec<Check>, Found) {
    let mut checks = Vec::new();
    let mut found = Found::new();
    for f in &p.find {
        let len = f.len as usize;
        let word = f.word.to_be_bytes();
        let mut hits = Vec::new();
        for (i, w) in rom.chunks_exact(4).enumerate() {
            let at = i * 4;
            if w == word
                && rom
                    .get(at..at + len)
                    .is_some_and(|r| crc::hex(&Sha1::digest(r)) == f.sha1)
            {
                hits.push(at);
                if hits.len() == 2 {
                    break;
                }
            }
        }
        let detail = match hits.as_slice() {
            [] => "not found".to_string(),
            [at] => match f.vram {
                Some(vram) => format!("found at 0x{at:X}, which runs at 0x{vram:08X}"),
                None => format!("found at 0x{at:X}"),
            },
            [a, b, ..] => format!("found more than once (0x{a:X}, 0x{b:X})"),
        };
        if let [at] = hits.as_slice() {
            found.insert(f.name.clone(), *at as u32);
        }
        checks.push(check(&f.label, hits.len() == 1, detail, &f.hint));
    }
    (checks, found)
}

/// `at` as a ROM offset, if what it is relative to was found.
fn resolve(p: &Profile, found: &Found, at: &Addr) -> Option<u32> {
    let (base, offset) = p.place(at).ok()?;
    let base = match base {
        None => 0,
        Some(name) => i64::from(*found.get(name)?),
    };
    u32::try_from(base + offset).ok()
}

/// The label of the find `at` is relative to, for a check that could not be made.
fn missing(p: &Profile, at: &Addr) -> String {
    let name = p.place(at).ok().and_then(|(b, _)| b).unwrap_or_default();
    let label = p
        .find
        .iter()
        .find(|f| f.name == name)
        .map_or(name, |f| f.label.as_str());
    format!("not checked: it is placed relative to {label:?}, which was not located")
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
        Ok(prepared) => verify_prepared(bundle, &prepared).0,
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

fn verify_prepared(bundle: &Bundle, rom: &[u8]) -> (Report, Found) {
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
        return (report(p, checks), Found::new());
    }

    let (located, found) = locate(p, rom);
    checks.extend(located);

    for r in &p.require {
        let places = r.places();
        let Some(ats) = places
            .iter()
            .map(|(a, _)| resolve(p, &found, a))
            .collect::<Option<Vec<u32>>>()
        else {
            let (lost, _) = places
                .iter()
                .find(|(a, _)| resolve(p, &found, a).is_none())
                .expect("one place did not resolve");
            checks.push(check(r.label(), false, missing(p, lost), ""));
            continue;
        };
        let at = ats[0];
        checks.push(match r {
            Require::Word {
                label,
                equals,
                hint,
                ..
            } => {
                let found = rom::read_u32(rom, at as usize);
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
            Require::Sha1 {
                label,
                len,
                sha1,
                hint,
                ..
            } => {
                let found = region(rom, at, *len as usize).map(|b| crc::hex(&Sha1::digest(b)));
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
            Require::Imm {
                label,
                hi_word,
                lo_word,
                min,
                max,
                hint,
                ..
            } => {
                let lo = ats[1];
                let words = rom::read_u32(rom, at as usize).zip(rom::read_u32(rom, lo as usize));
                let (ok, detail) = match words {
                    None => (false, format!("0x{at:X} or 0x{lo:X} is past the end")),
                    Some((h, l)) if h & 0xFFFF_0000 != *hi_word || l & 0xFFFF_0000 != *lo_word => (
                        false,
                        format!(
                            "0x{at:X} holds 0x{h:08X} and 0x{lo:X} holds 0x{l:08X}, not the \
                             instructions expected"
                        ),
                    ),
                    Some((h, l)) => {
                        let v = crate::profile::imm_value(h, l);
                        let ok = (*min..=*max).contains(&v);
                        (
                            ok,
                            format!(
                                "0x{at:X} and 0x{lo:X} load 0x{v:08X} ({} 0x{min:08X} to \
                                 0x{max:08X})",
                                if ok { "within" } else { "outside" }
                            ),
                        )
                    }
                };
                check(label, ok, detail, hint)
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

    (report(p, checks), found)
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
    let (report, found) = verify_prepared(bundle, &input);
    if !report.ok() {
        return Err(ApplyError::Refused(report));
    }
    // Every find was located, or the report would have failed, so this only fails on a bug.
    let at = |a: &Addr| {
        resolve(p, &found, a).ok_or_else(|| ApplyError::Internal(format!("{a} was not placed")))
    };
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

    for w in &p.write {
        let (at, bytes) = match w {
            Write::Blob { at: a, file, .. } => (
                at(a)?,
                bundle.blob(file).map_err(ApplyError::Internal)?.to_vec(),
            ),
            Write::Jal { at: a, target, .. } => (at(a)?, jal(*target).to_be_bytes().to_vec()),
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
        "Agent: {} bytes at {} 0x{agent_rom:X}{}, runs at 0x{:08X}{}",
        agent.len(),
        if p.transform.is_some() { "vrom" } else { "ROM" },
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
            // The low instruction decides how the value splits. addiu sign-extends its
            // immediate, so the lui must carry it; ori zero-extends, so it must not. Splitting
            // an ori pair the addiu way loads a value 0x10000 too high whenever bit 15 is set.
            let hi = at(hi)?;
            let lo = lo.as_ref().map(at).transpose()?;
            let (hi16, lo16) = match lo {
                Some(l) => match rom::read_u32(&out, l as usize).map(|w| w >> 26) {
                    Some(0x09) => ((v.wrapping_add(0x8000) >> 16) & 0xFFFF, v & 0xFFFF),
                    Some(0x0D) => (v >> 16, v & 0xFFFF),
                    _ => {
                        return Err(ApplyError::Internal(format!(
                            "imm {label:?}: the word at 0x{l:X} is neither an addiu nor an ori"
                        )))
                    }
                },
                None if v & 0xFFFF == 0 => (v >> 16, 0),
                None => {
                    return Err(ApplyError::Internal(format!(
                        "imm {label:?}: 0x{v:X} needs an addiu or ori, and the profile gives none"
                    )))
                }
            };
            for (at, imm) in std::iter::once((hi, hi16)).chain(lo.map(|l| (l, lo16))) {
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

    let out = match &p.transform {
        None => out,
        Some(crate::profile::Transform::Yaz0Dmadata { table }) => {
            repacked(rom, &input, &out, *table, cic, &mut summary)?
        }
    };

    let sha1 = crc::hex(&Sha1::digest(&out)).to_uppercase();
    Ok(Patched {
        rom: out,
        sha1,
        summary,
    })
}

/// Only what the patch changed, laid out over `seed`: see `transform::repack`. `patched` is the
/// finished decompressed image, checksum and all.
fn repacked(
    seed: &[u8],
    original: &[u8],
    patched: &[u8],
    table: u32,
    cic: crc::Cic,
    summary: &mut Vec<String>,
) -> Result<Vec<u8>, ApplyError> {
    let r =
        crate::transform::repack(seed, original, patched, table).map_err(ApplyError::Internal)?;
    let mut out = r.rom;
    let (a, b) = crc::fix(&mut out, cic);

    // What the game reads must be exactly what was patched. The checksum is the one exception:
    // it covers different bytes in the two layouts.
    let mut seen = crate::transform::yaz0_dmadata(&out, table).map_err(ApplyError::Internal)?;
    if seen.len() >= CRC_OFFSET + 8 {
        seen[CRC_OFFSET..CRC_OFFSET + 8].copy_from_slice(&patched[CRC_OFFSET..CRC_OFFSET + 8]);
    }
    if seen != patched {
        return Err(ApplyError::Internal(
            "the repacked ROM does not decompress to the patched image".to_string(),
        ));
    }
    if crc::identify(&out) != Some(cic) || crc::stored(&out) != Some(crc::compute(&out, cic)) {
        return Err(ApplyError::Internal(
            "repacked output does not boot-check".to_string(),
        ));
    }

    // The decompressed image's checksum line is replaced by the one the output carries.
    summary.retain(|l| !l.starts_with("Checksum:"));
    summary.push(format!(
        "Files: {} kept exactly as the seed stores them",
        r.kept
    ));
    for (vs, ve, at) in &r.moved {
        summary.push(format!(
            "File 0x{vs:X}-0x{ve:X}: stored uncompressed at ROM 0x{at:X}"
        ));
    }
    summary.push(format!(
        "Size: {} bytes (seed {}, decompressed image {})",
        out.len(),
        seed.len(),
        patched.len()
    ));
    summary.push(format!("Checksum: {cic} 0x{a:08X} 0x{b:08X}"));
    Ok(out)
}

/// Whether `rom` (big-endian) already carries this profile's agent: every hook points
/// at its target, every blob is in place, and the agent image sits at its offset. For a
/// profile with a transform, all of that is read in the decompressed image, since the patched
/// files are not where the seed kept them.
pub fn has_agent(bundle: &Bundle, rom: &[u8]) -> bool {
    let rom: &[u8] = &match prepare(bundle, rom) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let p = &bundle.profile;
    let agent = match bundle.blobs.get(&p.agent.image) {
        Some(a) => a,
        None => return false,
    };
    let (_, found) = locate(p, rom);
    let at = |a: &Addr| resolve(p, &found, a);
    let writes_hold = p.write.iter().all(|w| match w {
        Write::Jal { at: a, target, .. } => {
            at(a).and_then(|a| rom::read_u32(rom, a as usize)) == Some(jal(*target))
        }
        Write::Blob { at: a, file, .. } => bundle
            .blobs
            .get(file)
            .is_some_and(|b| at(a).and_then(|a| region(rom, a, b.len())) == Some(b.as_slice())),
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
        bundle_with(stub_len, rom, "")
    }

    /// `bundle`, with `extra` appended to the profile text.
    fn bundle_with(stub_len: usize, rom: &[u8], extra: &str) -> Bundle {
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
{extra}"#
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

    /// A lui/ori pair splits a value differently from lui/addiu, and bit 15 set is where
    /// it shows: 0x805B9040 is lui 0x805B + ori 0x9040, but lui 0x805C + addiu -0x6FC0.
    #[test]
    fn an_imm_write_splits_by_the_low_instruction() {
        const ORI: usize = 0x4000;
        const ADDIU: usize = 0x4010;
        let mut rom = seed();
        rom::write_u32(&mut rom, ORI, 0x3C0D_805C); // lui t5, 0x805C
        rom::write_u32(&mut rom, ORI + 4, 0x35AD_1040); // ori t5, t5, 0x1040
        rom::write_u32(&mut rom, ADDIU, 0x3C0D_805C); // lui t5, 0x805C
        rom::write_u32(&mut rom, ADDIU + 4, 0x25AD_1040); // addiu t5, t5, 0x1040
        let pins: String = [ORI, ORI + 4, ADDIU, ADDIU + 4]
            .iter()
            .map(|&at| {
                format!(
                    "[[require]]\nkind = \"word\"\nlabel = \"w{at:X}\"\nat = {at}\nequals = {}\n",
                    rom::read_u32(&rom, at).unwrap()
                )
            })
            .collect();
        let writes = format!(
            "[[write]]\nkind = \"imm\"\nlabel = \"ori pair\"\nhi = {ORI}\nlo = {}\nvalue = 0x805B9040\n\
             [[write]]\nkind = \"imm\"\nlabel = \"addiu pair\"\nhi = {ADDIU}\nlo = {}\nvalue = 0x805B9040\n",
            ORI + 4,
            ADDIU + 4
        );
        let b = bundle_with(0xCC, &rom, &format!("{pins}{writes}"));
        let out = apply(&b, &rom).unwrap().rom;
        assert_eq!(rom::read_u32(&out, ORI), Some(0x3C0D_805B));
        assert_eq!(rom::read_u32(&out, ORI + 4), Some(0x35AD_9040));
        assert_eq!(rom::read_u32(&out, ADDIU), Some(0x3C0D_805C));
        assert_eq!(rom::read_u32(&out, ADDIU + 4), Some(0x25AD_9040));
    }

    /// Where `seed_with_code` puts the found region by default. Its bytes are 0xFE, which
    /// `seed`'s filler never produces, so the region is unique unless a test copies it.
    const CODE: usize = 0x5000;

    /// `seed`, with a 0x40-byte region at `at` and a hook word 0x100 past its start: the
    /// shape of code a randomizer moves as a whole between releases.
    fn seed_with_code(at: usize) -> Vec<u8> {
        let mut rom = seed();
        rom[at..at + 0x40].fill(0xFE);
        rom::write_u32(&mut rom, at + 0x100, 0x0C00_1234);
        rom
    }

    /// A profile that finds the region and gives the hook by the RAM address it runs at.
    fn found_bundle(rom: &[u8]) -> Bundle {
        let sha = crc::hex(&Sha1::digest([0xFE; 0x40]));
        let extra = format!(
            r#"
[[find]]
name = "code"
label = "Code"
word = 0xFEFEFEFE
len = 0x40
sha1 = "{sha}"
vram = 0x80100000
[[require]]
kind = "word"
label = "Found hook"
at = "code@0x80100100"
equals = 0x0C001234
[[write]]
kind = "jal"
label = "Found jal"
at = "code@0x80100100"
target = 0x80019C00
"#
        );
        bundle_with(4, rom, &extra)
    }

    #[test]
    fn a_find_places_checks_and_writes_wherever_the_seed_has_it() {
        for at in [CODE, CODE + 0x800] {
            let rom = seed_with_code(at);
            let b = found_bundle(&rom);
            let out = apply(&b, &rom).unwrap().rom;
            assert_eq!(
                rom::read_u32(&out, at + 0x100),
                Some(0x0C00_6700),
                "at 0x{at:X}"
            );
            assert!(has_agent(&b, &out) && !has_agent(&b, &rom), "at 0x{at:X}");
        }
    }

    #[test]
    fn a_find_not_located_or_located_twice_is_refused() {
        let rom = seed_with_code(CODE);
        let b = found_bundle(&rom);

        let mut gone = rom.clone();
        gone[CODE] = 0;
        assert_eq!(refused(&b, &gone), ["Code", "Found hook"]);

        let mut twice = rom.clone();
        twice.copy_within(CODE..CODE + 0x40, 0x9000);
        assert_eq!(refused(&b, &twice), ["Code", "Found hook"]);
        let report = verify(&b, &twice);
        assert!(report.checks[2].detail.contains("more than once"));
    }

    #[test]
    fn a_found_place_the_profile_cannot_pin_is_a_profile_error() {
        let text = |extra: &str| {
            let sha = crc::hex(&Sha1::digest([0xFE; 0x40]));
            format!(
                r#"
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
[[find]]
name = "code"
label = "Code"
word = 0xFEFEFEFE
len = 0x40
sha1 = "{sha}"
{extra}"#
            )
        };
        let err = |extra: &str| Profile::parse(&text(extra)).unwrap_err();
        // Writing over the region would leave a patched ROM with nothing to find.
        assert!(err(
            "[[require]]\nkind = \"word\"\nlabel = \"w\"\nat = \"code+0x3C\"\nequals = 0\n\
             [[write]]\nkind = \"jal\"\nlabel = \"j\"\nat = \"code+0x3C\"\ntarget = 0\n"
        )
        .contains("inside find"));
        // A RAM address means nothing without the find saying where its region runs.
        assert!(err(
            "[[require]]\nkind = \"word\"\nlabel = \"w\"\nat = \"code@0x80100100\"\nequals = 0\n"
        )
        .contains("gives no vram"));
        assert!(
            err("[[require]]\nkind = \"word\"\nlabel = \"w\"\nat = \"other+4\"\nequals = 0\n")
                .contains("names no find")
        );
        // A pin relative to one place does not cover a write at the same number elsewhere.
        assert!(err(
            "[[require]]\nkind = \"word\"\nlabel = \"w\"\nat = \"code+0x100\"\nequals = 0\n\
             [[write]]\nkind = \"jal\"\nlabel = \"j\"\nat = 0x100\ntarget = 0\n"
        )
        .contains("not covered"));
    }

    /// `seed` with a `lui t5`/`ori t5` pair at 0x4000 loading `value`, and a profile that
    /// accepts 0x805C0000..=0x805D0000 there and writes 0x805B9040 over it.
    fn ranged(value: u32) -> (Vec<u8>, Bundle) {
        let mut rom = seed();
        rom::write_u32(&mut rom, 0x4000, 0x3C0D_0000 | value >> 16);
        rom::write_u32(&mut rom, 0x4004, 0x35AD_0000 | value & 0xFFFF);
        let extra = "[[require]]\nkind = \"imm\"\nlabel = \"Top\"\nhi = 0x4000\nlo = 0x4004\n\
                     hi_word = 0x3C0D0000\nlo_word = 0x35AD0000\nmin = 0x805C0000\nmax = 0x805D0000\n\
                     [[write]]\nkind = \"imm\"\nlabel = \"Lowered\"\nhi = 0x4000\nlo = 0x4004\n\
                     value = 0x805B9040\n";
        let b = bundle_with(4, &rom, extra);
        (rom, b)
    }

    #[test]
    fn an_imm_check_accepts_any_value_in_its_range() {
        for top in [0x805C_0000, 0x805C_1040, 0x805D_0000] {
            let (rom, b) = ranged(top);
            let out = apply(&b, &rom).unwrap().rom;
            assert_eq!(
                rom::read_u32(&out, 0x4000),
                Some(0x3C0D_805B),
                "top 0x{top:X}"
            );
            assert_eq!(
                rom::read_u32(&out, 0x4004),
                Some(0x35AD_9040),
                "top 0x{top:X}"
            );
        }
    }

    #[test]
    fn an_imm_check_refuses_a_value_outside_its_range_or_other_instructions() {
        for top in [0x805B_FFF0, 0x805D_0010] {
            let (rom, b) = ranged(top);
            assert_eq!(refused(&b, &rom), ["Top"], "top 0x{top:X}");
        }
        // The same value, built in another register.
        let (mut rom, b) = ranged(0x805C_1040);
        rom::write_u32(&mut rom, 0x4004, 0x35AC_1040);
        assert_eq!(refused(&b, &rom), ["Top"]);
    }

    #[test]
    fn an_addiu_pair_loads_its_low_half_sign_extended() {
        use crate::profile::imm_value;
        assert_eq!(imm_value(0x3C0D_805C, 0x25AD_9040), 0x805B_9040);
        assert_eq!(imm_value(0x3C0D_805B, 0x35AD_9040), 0x805B_9040);
    }

    #[test]
    fn has_agent_tells_a_patched_rom_from_its_seed() {
        let rom = seed();
        let b = bundle(0xCC, &rom);
        assert!(!has_agent(&b, &rom));
        let out = apply(&b, &rom).unwrap().rom;
        assert!(has_agent(&b, &out));
    }

    /// An Ocarina-of-Time-shaped seed: a file table at 0x1000, the hook inside a compressed
    /// `code` file, and a second compressed file the patch never touches.
    mod compressed {
        use super::*;
        use crate::transform::{dma_entries, yaz0_dmadata, yaz0_literal};

        const TABLE: usize = 0x1000;
        const CODE: u32 = 0x20_0000;
        const CODE_HOOK: u32 = CODE + 0x100;
        const OTHER: u32 = 0x20_2000;
        const REGION: u32 = 0x30_0000;

        fn entry(rom: &mut [u8], i: usize, w: [u32; 4]) {
            for (k, v) in w.iter().enumerate() {
                rom::write_u32(rom, TABLE + i * 16 + 4 * k, *v);
            }
        }

        fn seed_rom() -> Vec<u8> {
            let mut rom = seed();
            rom[TABLE..TABLE + 0x100].fill(0);
            let mut code = vec![0u8; 0x1000];
            code[0x100..0x104].copy_from_slice(&0x0C00_08CDu32.to_be_bytes());
            let other: Vec<u8> = (0..0x800u32).map(|i| (i * 11) as u8).collect();
            let (yc, yo) = (yaz0_literal(&code), yaz0_literal(&other));
            let (rc, ro) = (0x10_2000u32, 0x10_4000u32);
            entry(&mut rom, 0, [0x1000, 0x1100, 0x1000, 0]);
            entry(&mut rom, 1, [CODE, CODE + 0x1000, rc, rc + yc.len() as u32]);
            entry(
                &mut rom,
                2,
                [OTHER, OTHER + 0x800, ro, ro + yo.len() as u32],
            );
            rom[rc as usize..rc as usize + yc.len()].copy_from_slice(&yc);
            rom[ro as usize..ro as usize + yo.len()].copy_from_slice(&yo);
            rom
        }

        fn bundle() -> Bundle {
            let free = crc::hex(&Sha1::digest([0u8; 0x20]));
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
[transform]
kind = "yaz0_dmadata"
table = {TABLE}
[agent]
image = "agent.bin"
region = {REGION}
rom = {REGION}
bss = 0x100
dma_slot = 0x1030
vram = 0x80480000
min_ram = 0
[[require]]
kind = "word"
label = "Hook"
at = {CODE_HOOK}
equals = 0x0C0008CD
[[require]]
kind = "sha1"
label = "Free file table entries"
at = 0x1030
len = 0x20
sha1 = "{free}"
[[write]]
kind = "restore"
label = "IPL3"
at = 0x66C
bytes = "a5a5a5a5"
accept = ["00000000"]
[[write]]
kind = "jal"
label = "Jal"
at = {CODE_HOOK}
target = 0x80019C00
"#
            );
            let blobs = HashMap::from([("agent.bin".to_string(), vec![0x11; 0x1103])]);
            Bundle::new(Profile::parse(&text).unwrap(), blobs).unwrap()
        }

        #[test]
        fn only_the_changed_file_is_stored_uncompressed() {
            let rom = seed_rom();
            let b = bundle();
            let decompressed = yaz0_dmadata(&rom, TABLE as u32).unwrap().len();
            let out = apply(&b, &rom).unwrap().rom;
            assert!(
                out.len() < decompressed,
                "{} bytes out, the decompressed image is {decompressed}",
                out.len()
            );

            // The untouched file is still the seed's compressed bytes, under its seed entry.
            let (s, e) = (
                dma_entries(&rom, TABLE).unwrap(),
                dma_entries(&out, TABLE).unwrap(),
            );
            assert_eq!(e[2], s[2]);
            let (rs, re_) = (s[2].rom_start as usize, s[2].rom_end as usize);
            assert_eq!(out[rs..re_], rom[rs..re_]);
            // `code` is moved and plain; the new file has its entry.
            assert_eq!(e[1].rom_end, 0);
            assert_eq!((e[3].vrom_start, e[3].rom_end), (REGION, 0));
        }

        #[test]
        fn the_game_sees_the_hook_and_the_agent() {
            let rom = seed_rom();
            let b = bundle();
            let out = apply(&b, &rom).unwrap().rom;
            let seen = yaz0_dmadata(&out, TABLE as u32).unwrap();
            assert_eq!(rom::read_u32(&seen, CODE_HOOK as usize), Some(0x0C00_6700));
            assert_eq!(
                seen[REGION as usize..REGION as usize + 0x1103],
                [0x11; 0x1103]
            );
            assert_eq!(
                crc::stored(&out),
                Some(crc::compute(&out, crc::Cic::Cic6102))
            );
            assert!(has_agent(&b, &out) && !has_agent(&b, &rom));
        }
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
