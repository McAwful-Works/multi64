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
