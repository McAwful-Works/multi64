//! Finishing a seed that its world only finishes after the patch has been applied.
//!
//! Bomberman 64: The Second Attack's world does not finish a seed when its patch is applied.
//! BizHawk Client runs the world's ROM adjuster (`adjust.py`) the first time it connects: it
//! unpacks the ROM with the world's own `pack.exe`, copies the chosen character and Guardian
//! Armor models over the stock ones, rewrites the door data in 118 stage maps, replaces three
//! power-up files, repacks the ROM and sets two flags. On a console the client can only offer
//! to adjust a file on the PC, too late for the ROM already running, and adjusting a ROM AP64
//! has patched cuts it back to 16 MB and leaves its checksum wrong. So AP64 runs the same
//! steps itself, before the checks, with the same `pack.exe`, taken from the world installed
//! here. Issue #331.
//!
//! Every file this takes from the apworld is pinned by SHA-1 to world 1.0.2's: the adjuster's
//! own source, whose door table is copied below, and the program it runs. A world whose
//! adjuster differs is refused rather than approximated, and only the `pack.exe` pinned here
//! is ever run.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha1::{Digest, Sha1};

use crate::installed;
use crate::profile::{Adjust, Profile};

/// A seed adjusted: its bytes, and what to tell the player about it.
#[derive(Debug, Clone)]
pub struct Adjusted {
    pub rom: Vec<u8>,
    pub note: String,
}

/// Whether this seed still needs its profile's adjuster. `rom` is big-endian.
pub fn pending(profile: &Profile, rom: &[u8]) -> bool {
    match profile.adjust {
        Some(Adjust::BmtsaRomAdjuster) => rom.get(BMTSA_FLAG).is_some_and(|f| f & 1 == 0),
        None => false,
    }
}

/// Adjust a copy of `rom` (big-endian) the way its world's client would.
pub fn run(profile: &Profile, rom: &[u8]) -> Result<Adjusted, String> {
    match profile.adjust {
        Some(Adjust::BmtsaRomAdjuster) => bmtsa(rom),
        None => Err(format!("{} has no adjuster", profile.name)),
    }
}

const BMTSA_APWORLD: &str = "bomberman_tsa.apworld";
/// `ADJUST_OFFSET` in the world's `gamemaps.py`: the flag byte, then at +0x10 five 8-byte
/// file names (the character, then the Guardian helmet, vest, arms and legs).
const BMTSA_FLAG: usize = 0xF8000;
/// The option byte `compress_rom` also sets (the client reads it as "already adjusted").
const BMTSA_OPTION_FLAG: usize = 0x99FDA;

/// The files taken from the apworld, by path under its top directory, with their SHA-1 in
/// world 1.0.2. `adjust.py` is pinned because the steps and the door table below are its.
const BMTSA_FILES: [(&str, &str); 6] = [
    ("adjust.py", "23a475af150d6d863802cf1220953d10be20a1dd"),
    ("data/pack.exe", "28a23327e54a1a57437dd3614368a0925c7b6a60"),
    (
        "data/baku2.us.bin",
        "4e1a5fb2caa8ee67624d852c09e577cd50a6fc74",
    ),
    ("data/736.bin", "b7f70619fc38389439426fc7466a3ecc0f1699c3"),
    ("data/737.bin", "9138c0bccbe10bb8a051827c729e95155dc9f3a8"),
    ("data/738.bin", "612633702a0e0d76a56fb10df54cc3fba4c3d19a"),
];

/// `process_doors`' `door_map`: each stage map, and the event flag its doors wait on.
/// `86a.bin` (Starlight's casino entrance) and `88c.bin` (Thantos' battery fight) are left out,
/// as the world leaves them out.
const BMTSA_DOORS: [(&str, u16); 118] = [
    ("822.bin", 0x8C),
    ("823.bin", 0x8C),
    ("824.bin", 0x8C),
    ("825.bin", 0x8C),
    ("826.bin", 0x8C),
    ("827.bin", 0x8C),
    ("828.bin", 0x8C),
    ("829.bin", 0x8C),
    ("82a.bin", 0x8C),
    ("82b.bin", 0x8C),
    ("82c.bin", 0xB0),
    ("82d.bin", 0xB0),
    ("82e.bin", 0xB0),
    ("82f.bin", 0xB0),
    ("830.bin", 0xB0),
    ("831.bin", 0xB0),
    ("832.bin", 0xB0),
    ("833.bin", 0xB0),
    ("834.bin", 0xB0),
    ("835.bin", 0x89),
    ("836.bin", 0x89),
    ("837.bin", 0x89),
    ("838.bin", 0x89),
    ("839.bin", 0x89),
    ("83a.bin", 0x89),
    ("83b.bin", 0x89),
    ("83c.bin", 0x89),
    ("83d.bin", 0x89),
    ("83e.bin", 0x89),
    ("83f.bin", 0x89),
    ("840.bin", 0x89),
    ("841.bin", 0x89),
    ("842.bin", 0xAD),
    ("843.bin", 0xAD),
    ("844.bin", 0xAD),
    ("845.bin", 0xAD),
    ("846.bin", 0xAD),
    ("847.bin", 0xAD),
    ("848.bin", 0x88),
    ("849.bin", 0x88),
    ("84a.bin", 0x88),
    ("84b.bin", 0x88),
    ("84c.bin", 0x88),
    ("84d.bin", 0x88),
    ("84e.bin", 0xAC),
    ("84f.bin", 0xAC),
    ("850.bin", 0xAC),
    ("851.bin", 0xAC),
    ("852.bin", 0xAC),
    ("853.bin", 0xAC),
    ("854.bin", 0x8A),
    ("855.bin", 0x8A),
    ("856.bin", 0x8A),
    ("857.bin", 0x8A),
    ("858.bin", 0x8A),
    ("859.bin", 0x8A),
    ("85a.bin", 0x8A),
    ("85b.bin", 0xAE),
    ("85c.bin", 0xAE),
    ("85d.bin", 0xAE),
    ("85e.bin", 0xAE),
    ("85f.bin", 0xAE),
    ("860.bin", 0xAE),
    ("861.bin", 0xAE),
    ("862.bin", 0xAE),
    ("863.bin", 0xAE),
    ("864.bin", 0xAE),
    ("865.bin", 0x8B),
    ("866.bin", 0x8B),
    ("867.bin", 0x8B),
    ("868.bin", 0x8B),
    ("869.bin", 0x8B),
    ("86b.bin", 0x8B),
    ("86c.bin", 0x8B),
    ("86d.bin", 0x8B),
    ("86e.bin", 0x8B),
    ("86f.bin", 0x8B),
    ("870.bin", 0xAF),
    ("871.bin", 0xAF),
    ("872.bin", 0xAF),
    ("873.bin", 0x8D),
    ("874.bin", 0x8D),
    ("875.bin", 0x8D),
    ("876.bin", 0x8D),
    ("877.bin", 0x8D),
    ("878.bin", 0x8D),
    ("879.bin", 0x8D),
    ("87a.bin", 0x8D),
    ("87b.bin", 0x8D),
    ("87c.bin", 0x8D),
    ("87d.bin", 0x8D),
    ("87e.bin", 0x8D),
    ("87f.bin", 0x8D),
    ("880.bin", 0xB1),
    ("881.bin", 0xB1),
    ("882.bin", 0x8D),
    ("883.bin", 0x8D),
    ("884.bin", 0x8D),
    ("885.bin", 0x8D),
    ("886.bin", 0xB1),
    ("887.bin", 0x8E),
    ("888.bin", 0x8E),
    ("889.bin", 0x8E),
    ("88a.bin", 0x8E),
    ("88b.bin", 0x8E),
    ("88d.bin", 0x8E),
    ("88e.bin", 0x8E),
    ("88f.bin", 0x8E),
    ("890.bin", 0x8E),
    ("891.bin", 0x8E),
    ("892.bin", 0x8E),
    ("893.bin", 0x8E),
    ("894.bin", 0x8E),
    ("895.bin", 0x8E),
    ("896.bin", 0x8E),
    ("897.bin", 0x8E),
    ("898.bin", 0x8E),
    ("899.bin", 0x8E),
];

fn bmtsa(rom: &[u8]) -> Result<Adjusted, String> {
    let names = bmtsa_names(rom)?;
    let path = installed::installed_apworld(BMTSA_APWORLD).ok_or_else(|| {
        format!("{BMTSA_APWORLD} is not installed here, and AP64 runs the adjuster from it")
    })?;
    let apworld = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let version = installed::apworld_version(&apworld).unwrap_or_else(|| "?".into());
    let files = bmtsa_files(&apworld, &version)?;
    let file = |name: &str| &files[BMTSA_FILES.iter().position(|(n, _)| *n == name).unwrap()];

    let work = WorkDir::new()?;
    let exe = work.0.join("pack.exe");
    let table = work.0.join("baku2.us.bin");
    let image = work.0.join("rom.z64");
    let unpacked = work.0.join("unpacked");
    let write =
        |p: &Path, b: &[u8]| std::fs::write(p, b).map_err(|e| format!("{}: {e}", p.display()));
    write(&exe, file("data/pack.exe"))?;
    write(&table, file("data/baku2.us.bin"))?;
    write(&image, rom)?;

    // decompress_rom passes the directory with a trailing separator, compress_rom without.
    let mut into = unpacked.clone().into_os_string();
    into.push(std::path::MAIN_SEPARATOR_STR);
    pack(&exe, &work.0, "d", &table, &image, Path::new(&into))?;
    let assets = unpacked.join("assets");
    bmtsa_models(&assets, &names)?;
    for (map, flag) in BMTSA_DOORS {
        let p = assets.join(map);
        let mut bytes = std::fs::read(&p).map_err(|e| format!("{map}: {e}"))?;
        doors(&mut bytes, flag);
        write(&p, &bytes)?;
    }
    for name in ["736.bin", "737.bin", "738.bin"] {
        write(&assets.join(name), file(&format!("data/{name}")))?;
    }
    pack(&exe, &work.0, "e", &table, &image, &unpacked)?;

    let mut out = std::fs::read(&image).map_err(|e| format!("{}: {e}", image.display()))?;
    for at in [BMTSA_OPTION_FLAG, BMTSA_FLAG] {
        *out.get_mut(at)
            .ok_or("pack.exe wrote a ROM too short to adjust")? = 1;
    }
    Ok(Adjusted {
        rom: out,
        note: format!(
            "AP64 ran the world's ROM adjuster on this seed, from {BMTSA_APWORLD} {version}: its \
             doors, power-ups and models are set up as BizHawk Client would."
        ),
    })
}

/// The model file names the seed asks for, as `adjust_rom` reads them: five 8-byte fields at
/// +0x10, keeping only printable ASCII. Each must be an asset name or "None".
fn bmtsa_names(rom: &[u8]) -> Result<[String; 5], String> {
    let data = rom
        .get(BMTSA_FLAG..BMTSA_FLAG + 0x38)
        .ok_or("the seed is too short to hold the adjuster's settings")?;
    let mut names: [String; 5] = Default::default();
    for (i, name) in names.iter_mut().enumerate() {
        let field = &data[0x10 + i * 8..0x18 + i * 8];
        *name = field
            .iter()
            .filter(|b| (32..128).contains(*b))
            .map(|&b| b as char)
            .collect();
        let asset = name
            .strip_suffix(".bin")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()));
        if !(asset || (i > 0 && name == "None")) {
            return Err(format!(
                "the seed's adjuster settings name {name:?}, which is not a model file"
            ));
        }
    }
    Ok(names)
}

/// Read the pinned files out of the apworld, refusing any that differ from world 1.0.2's.
fn bmtsa_files(apworld: &[u8], version: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(apworld))
        .map_err(|e| format!("{BMTSA_APWORLD} is not a readable apworld: {e}"))?;
    let top = z
        .file_names()
        .find(|n| n.ends_with("/adjust.py") && n.matches('/').count() == 1)
        .map(|n| n.trim_end_matches("adjust.py").to_string())
        .ok_or_else(|| format!("{BMTSA_APWORLD} {version} has no ROM adjuster (adjust.py)"))?;
    let mut out = Vec::new();
    for (name, sha1) in BMTSA_FILES {
        let mut bytes = Vec::new();
        z.by_name(&format!("{top}{name}"))
            .map_err(|_| format!("{BMTSA_APWORLD} {version} has no {name}"))?
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{BMTSA_APWORLD}: {name}: {e}"))?;
        if hex(&Sha1::digest(&bytes)) != sha1 {
            return Err(format!(
                "the ROM adjuster in {BMTSA_APWORLD} {version} is not the one AP64 knows (its \
                 {name} differs from world 1.0.2's), so AP64 will not run it"
            ));
        }
        out.push(bytes);
    }
    Ok(out)
}

/// `replace_files`: the character model over `17e.bin`, then each Guardian part over its own
/// file, from its named model or, for "None", from the character's. In this order, because a
/// later copy can read a file an earlier one replaced, exactly as the world's does.
fn bmtsa_models(assets: &Path, names: &[String; 5]) -> Result<(), String> {
    let chara = &names[0];
    let mut copies = vec![(chara.as_str(), "17e.bin")];
    for (part, target) in names[1..]
        .iter()
        .zip(["3e7.bin", "3e8.bin", "3e9.bin", "3ea.bin"])
    {
        copies.push((if part == "None" { chara } else { part }, target));
    }
    for (from, to) in copies {
        if from != to {
            std::fs::copy(assets.join(from), assets.join(to))
                .map_err(|e| format!("copying model {from} over {to}: {e}"))?;
        }
    }
    Ok(())
}

/// `process_map_file`: in one stage map, every object whose trigger byte (+0x13) is 1 gets
/// despawn type 2 (+0x1B), the door's event flag (+0x1E, big-endian) and no clear flag
/// (+0x20, four 0xFF). The header's byte 0xB counts 0x20-byte sets before the objects, and
/// byte 0xF counts the 0x4C-byte objects. Triggers are read from the map as it was, and a
/// field past the end extends the file with zeros, as a Python seek-and-write does.
fn doors(map: &mut Vec<u8>, flag: u16) {
    let orig = map.clone();
    let byte = |at: usize| orig.get(at).copied().unwrap_or(0) as usize;
    let base = byte(0xB) * 0x20 + 0x10;
    for i in 0..byte(0xF) {
        let obj = base + i * 0x4C;
        if orig.get(obj + 0x13) != Some(&1) {
            continue;
        }
        let [hi, lo] = flag.to_be_bytes();
        for (at, b) in [
            (0x1B, 2),
            (0x1E, hi),
            (0x1F, lo),
            (0x20, 0xFF),
            (0x21, 0xFF),
            (0x22, 0xFF),
            (0x23, 0xFF),
        ] {
            if map.len() <= obj + at {
                map.resize(obj + at + 1, 0);
            }
            map[obj + at] = b;
        }
    }
}

/// Run the world's `pack.exe`: `d` unpacks `image` into `dir`, `e` packs `dir` back into it.
fn pack(
    exe: &Path,
    cwd: &Path,
    verb: &str,
    table: &Path,
    image: &Path,
    dir: &Path,
) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("the world's ROM adjuster is a Windows program".into());
    }
    let mut cmd = Command::new(exe);
    cmd.current_dir(cwd)
        .arg(verb)
        .arg(table)
        .arg(image)
        .arg(dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run the world's pack.exe: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the world's pack.exe failed to {} the ROM ({})",
            if verb == "d" { "unpack" } else { "repack" },
            out.status
        ));
    }
    Ok(())
}

/// A directory of AP64's own under the system temp folder, removed when dropped. Created
/// fresh, never reused, so removing it can only remove what this run put there.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("ap64-adjust-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(WorkDir(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map with one set before the objects and three objects, the middle one a door.
    fn map() -> Vec<u8> {
        let mut m = vec![0u8; 0x10 + 0x20 + 3 * 0x4C];
        m[0xB] = 1;
        m[0xF] = 3;
        m[0x30 + 0x4C + 0x13] = 1;
        m
    }

    #[test]
    fn a_door_gets_the_flag_it_waits_on_and_nothing_else_changes() {
        let before = map();
        let mut after = before.clone();
        doors(&mut after, 0x8C);
        let door = 0x30 + 0x4C;
        assert_eq!(after[door + 0x1B], 2);
        assert_eq!(
            &after[door + 0x1E..door + 0x24],
            &[0x00, 0x8C, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        let changed: Vec<usize> = (0..before.len())
            .filter(|&i| before[i] != after[i])
            .collect();
        assert_eq!(
            changed,
            vec![
                door + 0x1B,
                door + 0x1F,
                door + 0x20,
                door + 0x21,
                door + 0x22,
                door + 0x23
            ]
        );
    }

    #[test]
    fn a_door_at_the_very_end_extends_the_map_as_the_world_does() {
        let mut m = vec![0u8; 0x10 + 0x14];
        m[0xF] = 1;
        m[0x10 + 0x13] = 1;
        doors(&mut m, 0xB1);
        assert_eq!(m.len(), 0x10 + 0x24);
        assert_eq!(&m[0x10 + 0x1E..], &[0x00, 0xB1, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn a_map_without_doors_is_left_alone() {
        let mut m = map();
        m[0x30 + 0x4C + 0x13] = 2;
        let before = m.clone();
        doors(&mut m, 0x8C);
        assert_eq!(m, before);
    }

    fn settings(names: [&str; 5]) -> Vec<u8> {
        let mut rom = vec![0u8; BMTSA_FLAG + 0x40];
        for (i, n) in names.iter().enumerate() {
            let at = BMTSA_FLAG + 0x10 + i * 8;
            rom[at..at + n.len()].copy_from_slice(n.as_bytes());
        }
        rom
    }

    #[test]
    fn the_default_models_are_read_back() {
        let rom = settings(["17e.bin", "3e7.bin", "3e8.bin", "3e9.bin", "3ea.bin"]);
        assert_eq!(
            bmtsa_names(&rom).unwrap(),
            ["17e.bin", "3e7.bin", "3e8.bin", "3e9.bin", "3ea.bin"]
        );
    }

    #[test]
    fn a_guardian_part_may_be_none_but_the_character_may_not() {
        assert!(bmtsa_names(&settings(["17e.bin", "None", "3e8.bin", "None", "3ea.bin"])).is_ok());
        assert!(bmtsa_names(&settings([
            "None", "3e7.bin", "3e8.bin", "3e9.bin", "3ea.bin"
        ]))
        .is_err());
    }

    #[test]
    fn a_name_that_is_not_a_model_file_is_refused() {
        for bad in ["../x.bin", "17e.exe", ".bin", "17g.bin"] {
            let rom = settings(["17e.bin", bad, "3e8.bin", "3e9.bin", "3ea.bin"]);
            assert!(bmtsa_names(&rom).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn guardian_parts_left_as_none_take_the_character_model() {
        let dir = WorkDir::new().unwrap();
        for (name, body) in [
            ("17e.bin", "stock"),
            ("1a0.bin", "other"),
            ("3e7.bin", "helmet"),
            ("3e8.bin", "vest"),
            ("3e9.bin", "arms"),
            ("3ea.bin", "legs"),
        ] {
            std::fs::write(dir.0.join(name), body).unwrap();
        }
        let names = ["1a0.bin", "None", "3e8.bin", "None", "3ea.bin"].map(String::from);
        bmtsa_models(&dir.0, &names).unwrap();
        let read = |n: &str| std::fs::read_to_string(dir.0.join(n)).unwrap();
        assert_eq!(
            [
                read("17e.bin"),
                read("3e7.bin"),
                read("3e8.bin"),
                read("3e9.bin"),
                read("3ea.bin")
            ],
            ["other", "other", "vest", "other", "legs"]
        );
    }

    #[test]
    fn the_door_table_is_the_worlds_less_its_two_broken_maps() {
        assert_eq!(BMTSA_DOORS.len(), 118);
        let names: std::collections::HashSet<_> = BMTSA_DOORS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), 118, "a map is listed twice");
        assert!(!names.contains("86a.bin") && !names.contains("88c.bin"));
    }

    /// Against the world's own adjuster, byte for byte. Needs Windows, the apworld installed,
    /// and two files made by hand: set AP64_TSA_SEED to an unadjusted seed and
    /// AP64_TSA_ADJUSTED to the same seed after the world's adjust.py.
    #[test]
    #[ignore]
    fn matches_the_worlds_adjuster() {
        let seed = std::fs::read(std::env::var("AP64_TSA_SEED").unwrap()).unwrap();
        let want = std::fs::read(std::env::var("AP64_TSA_ADJUSTED").unwrap()).unwrap();
        let got = bmtsa(&seed).unwrap();
        assert!(got.rom == want, "differs from the world's adjuster");
    }
}
