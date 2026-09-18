//! Byte-identity against outputs that have run on a console. ROMs cannot be committed,
//! so these read paths from the environment and are ignored by default:
//!
//!   AP64_CV64_SEED=<seed.z64> AP64_CV64_EXPECTED=<spliced.z64> \
//!     cargo test -p ap64-core --test local_roms -- --ignored

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
