use std::path::PathBuf;

/// Bake in the ROM version this build expects, read from the test ROM's own header.
///
/// The app is built from the same tree as `multi64_test.z64`, so it can know which ROM it is for
/// rather than asking a tester to type a version string they have no way to know. The version
/// check is the one that stops a whole run being a test of some *other* ROM, so making it
/// automatic is worth a build script.
///
/// If the header cannot be read — someone vendoring this crate alone — the value is empty and the
/// app treats the check as unverifiable and skips it, rather than asserting something invented.
fn expected_rom_version() -> String {
    let header = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../n64/test-rom/test_proto.h")
        .canonicalize()
        .ok();
    let Some(header) = header else {
        return String::new();
    };
    println!("cargo:rerun-if-changed={}", header.display());
    let Ok(text) = std::fs::read_to_string(&header) else {
        return String::new();
    };
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("#define TEST_ROM_VERSION_STR") {
            if let Some(start) = rest.find('"') {
                if let Some(end) = rest[start + 1..].find('"') {
                    return rest[start + 1..start + 1 + end].to_string();
                }
            }
        }
    }
    String::new()
}

fn main() {
    println!(
        "cargo:rustc-env=MULTI64_EXPECTED_ROM={}",
        expected_rom_version()
    );
    tauri_build::build();
}
