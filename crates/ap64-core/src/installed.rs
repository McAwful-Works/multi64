//! What Archipelago has installed on this PC: its own version, and the version of a world.
//!
//! A profile is a measurement of one randomizer release (`profile.measured`). When the world
//! installed here is a different release, the seed it makes may not be what the profile was
//! measured against. The pins refuse the changes they can see, and the stub stands the agent
//! down if its RAM is overwritten, but anything else would only show on the console. So
//! [`release_notes`] says so up front, as a note rather than a refusal: most releases change
//! nothing AP64 depends on. Issue #292.

use std::io::Read as _;
use std::path::PathBuf;

use crate::profile::{Measured, Profile};

/// The name `[measured] world` uses for a world that ships with Archipelago itself.
pub const ARCHIPELAGO: &str = "Archipelago";

/// Archipelago folders to look in: `AP64_ARCHIPELAGO_DIR` first, then the Windows
/// installer's default and the per-user one.
fn roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("AP64_ARCHIPELAGO_DIR") {
        roots.push(PathBuf::from(dir));
    }
    for var in ["ProgramData", "LOCALAPPDATA"] {
        if let Some(dir) = std::env::var_os(var) {
            roots.push(PathBuf::from(dir).join("Archipelago"));
        }
    }
    roots
}

/// Where Archipelago keeps an installed world, by its file name (`dk64.apworld`).
pub fn installed_apworld(file: &str) -> Option<PathBuf> {
    roots()
        .into_iter()
        .flat_map(|r| [r.join("custom_worlds"), r.join("lib").join("worlds")])
        .map(|d| d.join(file))
        .find(|p| p.is_file())
}

/// The `world_version` an apworld declares in its `archipelago.json`, if it declares one.
pub fn apworld_version(apworld: &[u8]) -> Option<String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(apworld)).ok()?;
    let name = z
        .file_names()
        .find(|n| n.ends_with("archipelago.json") && n.matches('/').count() <= 1)?
        .to_string();
    let mut text = String::new();
    z.by_name(&name).ok()?.read_to_string(&mut text).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("world_version")?.as_str().map(str::to_string)
}

/// Archipelago's own version, from the `manifest.json` its installer writes (`[0, 6, 7]`).
pub fn archipelago_version() -> Option<String> {
    roots().into_iter().find_map(|r| {
        let text = std::fs::read_to_string(r.join("manifest.json")).ok()?;
        manifest_version(&text)
    })
}

fn manifest_version(text: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let parts: Option<Vec<String>> = json
        .get("version")?
        .as_array()?
        .iter()
        .map(|v| v.as_u64().map(|n| n.to_string()))
        .collect();
    Some(parts?.join("."))
}

/// The version of `world` installed here: Archipelago's own for [`ARCHIPELAGO`], else the
/// named apworld's. `None` when it is not installed or declares no version.
pub fn installed_version(world: &str) -> Option<String> {
    if world == ARCHIPELAGO {
        return archipelago_version();
    }
    let bytes = std::fs::read(installed_apworld(world)?).ok()?;
    apworld_version(&bytes)
}

/// Notes on how this seed and this PC differ from what `profile` was measured against, for
/// the user. Empty when nothing differs or nothing can be compared. `header_name` is the
/// seed's header name, trimmed.
pub fn release_notes(profile: &Profile, header_name: &str) -> Vec<String> {
    notes_with(profile, header_name, installed_version)
}

fn notes_with(
    profile: &Profile,
    header_name: &str,
    installed: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    let Some(m) = &profile.measured else {
        return Vec::new();
    };
    let mut notes = Vec::new();
    let tail = "AP64's checks still refuse the changes they can see, and the agent stands \
                down if its code in RAM is overwritten, but anything else would only show on \
                the console.";
    if let Some(want) = &m.header_name {
        if header_name != want {
            notes.push(format!(
                "this seed's header says \"{header_name}\", and AP64 was measured against \
                 \"{want}\": it may be a randomizer release AP64 has not been tested with. {tail}"
            ));
        }
    }
    if let Measured {
        world: Some(world),
        version: Some(want),
        ..
    } = m
    {
        if let Some(have) = installed(world) {
            if &have != want {
                let what = if world == ARCHIPELAGO {
                    format!("Archipelago here is {have}")
                } else {
                    format!("{world} here is version {have}")
                };
                notes.push(format!(
                    "{what}, and AP64 was measured against {want}: a seed it makes may differ \
                     from what AP64 has been tested with. {tail}"
                ));
            }
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(measured: &str) -> Profile {
        let text = format!(
            "id = \"t\"\nname = \"T\"\nrelease = \"US\"\ngame_code = \"NTTE\"\nversion = 0\n\
             cic = \"6102\"\nrandomizer = \"r\"\nconnector = \"generic\"\n{measured}\n\
             [agent]\nimage = \"agent.bin\"\nrom = 0x1000000\nvram = 0x80480000\n\
             min_ram = 0x800000\n"
        );
        Profile::parse(&text).unwrap()
    }

    #[test]
    fn a_different_installed_version_is_a_note_and_the_same_one_is_not() {
        let p = profile("[measured]\nworld = \"t.apworld\"\nversion = \"1.5.8\"\n");
        assert!(notes_with(&p, "T", |_| Some("1.5.8".into())).is_empty());
        let notes = notes_with(&p, "T", |w| (w == "t.apworld").then(|| "1.6.0".into()));
        assert_eq!(notes.len(), 1);
        assert!(notes[0]
            .starts_with("t.apworld here is version 1.6.0, and AP64 was measured against 1.5.8"));
        assert!(!notes[0].contains("  "), "{}", notes[0]);
    }

    #[test]
    fn a_world_that_is_not_installed_says_nothing() {
        let p = profile("[measured]\nworld = \"t.apworld\"\nversion = \"1.5.8\"\n");
        assert!(notes_with(&p, "T", |_| None).is_empty());
    }

    #[test]
    fn archipelago_itself_is_named_as_such() {
        let p = profile("[measured]\nworld = \"Archipelago\"\nversion = \"0.6.7\"\n");
        let notes = notes_with(&p, "T", |_| Some("0.7.0".into()));
        assert!(
            notes[0].starts_with("Archipelago here is 0.7.0"),
            "{}",
            notes[0]
        );
    }

    #[test]
    fn a_header_that_names_another_release_is_a_note() {
        let p = profile("[measured]\nheader_name = \"MK64 ARCHIPELAGO 0.2\"\n");
        assert!(notes_with(&p, "MK64 ARCHIPELAGO 0.2", |_| None).is_empty());
        let notes = notes_with(&p, "MK64 ARCHIPELAGO 0.3", |_| None);
        assert!(
            notes[0].contains("\"MK64 ARCHIPELAGO 0.3\""),
            "{}",
            notes[0]
        );
        assert!(!notes[0].contains("  "), "{}", notes[0]);
    }

    #[test]
    fn a_profile_that_records_nothing_has_no_notes() {
        assert!(notes_with(&profile(""), "T", |_| Some("9".into())).is_empty());
    }

    #[test]
    fn the_manifest_version_is_dotted() {
        let text = r#"{"buildtime": "x", "hashes": {}, "version": [0, 6, 7]}"#;
        assert_eq!(manifest_version(text).as_deref(), Some("0.6.7"));
        assert_eq!(manifest_version("not json"), None);
    }
}
