//! Byte-identity against outputs that have run on a console. ROMs cannot be committed,
//! so these read paths from the environment and are ignored by default:
//!
//!   AP64_CV64_SEED=<seed.z64> AP64_CV64_EXPECTED=<spliced.z64> \
//!     cargo test -p ap64-core --test local_roms -- --ignored
//!
//! The DK64 pair (`AP64_DK64_SEED`, `AP64_DK64_EXPECTED`) is a dk64.apworld v1.5.8 seed.

use std::path::PathBuf;

fn env_path(key: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {key}"))
}

fn check(profile: &str, seed_key: &str, expected_key: &str) {
    let bundles = ap64_core::builtin().unwrap();
    let mut seed = std::fs::read(env_path(seed_key)).unwrap();
    let expected = std::fs::read(env_path(expected_key)).unwrap();
    let detection = ap64_core::detect(&bundles, &mut seed).unwrap();
    let chosen = detection.chosen().expect("one profile passes");
    assert_eq!(chosen.profile_id, profile);
    let bundle = bundles.iter().find(|b| b.profile.id == profile).unwrap();
    let out = ap64_core::apply(bundle, &seed).unwrap();
    assert_eq!(out.rom.len(), expected.len());
    if let Some(i) = (0..out.rom.len()).find(|&i| out.rom[i] != expected[i]) {
        panic!("first difference at 0x{i:X}");
    }
}

#[test]
#[ignore = "needs ROMs from the environment"]
fn cv64_matches_the_console_tested_splice() {
    check("cv64", "AP64_CV64_SEED", "AP64_CV64_EXPECTED");
}

#[test]
#[ignore = "needs ROMs from the environment"]
fn dk64_matches_the_console_tested_splice() {
    check("dk64", "AP64_DK64_SEED", "AP64_DK64_EXPECTED");
}

/// A stand-in for a randomizer release that moves DK64's main code: the same seed with 4 KB
/// inserted before it. The splice must follow the code and otherwise be the same splice.
#[test]
#[ignore = "needs ROMs from the environment"]
fn dk64_follows_its_main_code_when_it_moves() {
    const AT: usize = 0x200_0000; // past the boot segment, before the main code
    const SHIFT: usize = 0x1000;
    const AGENT: usize = 0x340_2000;
    let moved = |rom: &[u8], len: usize| {
        let mut out = rom[..AT].to_vec();
        out.resize(AT + SHIFT, 0);
        out.extend_from_slice(&rom[AT..]);
        out.truncate(len);
        out
    };
    let seed = std::fs::read(env_path("AP64_DK64_SEED")).unwrap();
    let expected = std::fs::read(env_path("AP64_DK64_EXPECTED")).unwrap();
    // The seed ends in padding, so keeping its length drops none of its data.
    let mut shifted = moved(&seed, seed.len());

    let bundles = ap64_core::builtin().unwrap();
    let detection = ap64_core::detect(&bundles, &mut shifted).unwrap();
    let chosen = detection.chosen().expect("one profile passes");
    assert_eq!(chosen.profile_id, "dk64");
    let bundle = bundles.iter().find(|b| b.profile.id == "dk64").unwrap();
    let out = ap64_core::apply(bundle, &shifted).unwrap().rom;

    let want = moved(&expected, seed.len());
    if let Some(i) = (0..seed.len()).find(|&i| out[i] != want[i]) {
        panic!("first difference at 0x{i:X}");
    }
    assert_eq!(
        out[AGENT..],
        expected[AGENT..],
        "the agent stays where it goes"
    );
}

/// A stand-in for a randomizer release whose own code starts somewhere else: the same seed
/// with another heap top. The splice writes the same top whatever the seed held, so the output
/// is the same ROM; a top below the agent's end is refused.
#[test]
#[ignore = "needs ROMs from the environment"]
fn dk64_accepts_any_heap_top_that_clears_the_agent() {
    const HI: usize = 0x204_EFD0; // lui t5 / ori t5 at 0x80610510 in v1.5.8
    let seed = std::fs::read(env_path("AP64_DK64_SEED")).unwrap();
    let expected = std::fs::read(env_path("AP64_DK64_EXPECTED")).unwrap();
    let bundles = ap64_core::builtin().unwrap();
    let bundle = bundles.iter().find(|b| b.profile.id == "dk64").unwrap();
    let with_top = |top: u32| {
        let mut rom = seed.clone();
        rom[HI..HI + 4].copy_from_slice(&(0x3C0D_0000 | top >> 16).to_be_bytes());
        rom[HI + 8..HI + 12].copy_from_slice(&(0x35AD_0000 | top & 0xFFFF).to_be_bytes());
        rom
    };

    let out = ap64_core::apply(bundle, &with_top(0x805D_0000))
        .unwrap()
        .rom;
    assert!(out == expected, "a higher top patches to the same ROM");

    match ap64_core::apply(bundle, &with_top(0x805C_0000)) {
        Err(ap64_core::ApplyError::Refused(r)) => {
            let failed: Vec<_> = r
                .checks
                .iter()
                .filter(|c| !c.ok)
                .map(|c| &c.label)
                .collect();
            assert_eq!(failed, ["Heap top"]);
        }
        other => panic!("a top inside the agent must be refused, got {other:?}"),
    }
}

/// A stand-in for a release whose ROM grew into where the agent usually goes: the same seed
/// with 8 KB of data past its end. The agent goes past that data instead, the stub is told
/// where, and nothing else in the splice changes.
#[test]
#[ignore = "needs ROMs from the environment"]
fn dk64_puts_its_agent_past_a_seed_that_grew_into_it() {
    const AGENT: usize = 0x340_2000;
    const MOVED: usize = 0x340_4000; // the first 4 KiB boundary past the grown data
    const PAIR: usize = 0xDDD4; // the stub's lui/addiu of the agent's ROM offset
    let mut seed = std::fs::read(env_path("AP64_DK64_SEED")).unwrap();
    let expected = std::fs::read(env_path("AP64_DK64_EXPECTED")).unwrap();
    let original = seed.len();
    seed.resize(MOVED - 0x800, 0x5A);

    let bundles = ap64_core::builtin().unwrap();
    let bundle = bundles.iter().find(|b| b.profile.id == "dk64").unwrap();
    let out = ap64_core::apply(bundle, &seed).unwrap().rom;

    let word = |at: usize| u32::from_be_bytes(out[at..at + 4].try_into().unwrap());
    assert_eq!((word(PAIR), word(PAIR + 4)), (0x3C04_0340, 0x2484_4000));
    assert_eq!(out[MOVED..], expected[AGENT..], "the same agent, moved");
    assert_eq!(
        out[original..MOVED - 0x800],
        seed[original..],
        "the grown data is kept"
    );
    // Everything else is the splice that ran on the console: the stub differs only in the pair.
    let diff: Vec<usize> = (0..original).filter(|&i| out[i] != expected[i]).collect();
    assert!(
        diff.iter()
            .all(|&i| (PAIR..PAIR + 8).contains(&i) || (0x10..0x18).contains(&i)),
        "differs outside the pair and the checksum: {:X?}",
        &diff[..diff.len().min(8)]
    );
    assert!(ap64_core::has_agent(bundle, &out));
}
