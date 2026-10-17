//! What Archipelago has installed on this PC: its own version, and the version of each
//! installed copy of a world.
//!
//! A profile is a measurement of one randomizer release (`profile.measured`). When a copy of
//! the world installed here is a different release, the seed it makes may not be what the profile was
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

/// Where Archipelago keeps an installed world, by its file name (`dk64.apworld`): the first
/// of [`installed_apworlds`].
pub fn installed_apworld(file: &str) -> Option<PathBuf> {
    installed_apworlds(file).into_iter().next()
}

/// Every installed copy of a world, by its file name: in `custom_worlds`, then `lib\worlds`,
/// of each of the Archipelago folders, in the order [`roots`] gives them.
///
/// There can be more than one, and which one runs is not AP64's to know. Each Archipelago
/// install loads the worlds in its own two folders: `worlds/__init__.py` scans its
/// `user_path("custom_worlds")` and its own `lib\worlds`, and on Windows `user_path` is the
/// install's folder. So which copy runs depends on which install's Launcher the player opens,
/// and within one install, on which copy declares the higher `world_version`, since
/// `load_apworlds` loads the highest and skips any other for the same game. Anything that
/// changes a world for the player has to look at every copy.
pub fn installed_apworlds(file: &str) -> Vec<PathBuf> {
    apworlds_in(&roots(), file)
}

fn apworlds_in(roots: &[PathBuf], file: &str) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for p in roots
        .iter()
        .flat_map(|r| [r.join("custom_worlds"), r.join("lib").join("worlds")])
        .map(|d| d.join(file))
        .filter(|p| p.is_file())
    {
        // AP64_ARCHIPELAGO_DIR may name one of the other two.
        let id = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if !seen.contains(&id) {
            seen.push(id);
            out.push(p);
        }
    }
    out
}

/// The `world_version` an apworld declares in its `archipelago.json`, if it declares one.
pub fn apworld_version(apworld: &[u8]) -> Option<String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(apworld)).ok()?;
    let name = z
        .file_names()
        // A name that does not decode cannot be the one this looks for.
        .filter_map(Result::ok)
        .find(|n| n.ends_with("archipelago.json") && n.matches('/').count() <= 1)?
        .to_string();
    let mut text = String::new();
    z.by_name(&name).ok()?.read_to_string(&mut text).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("world_version")?.as_str().map(str::to_string)
}

/// Archipelago's own version, from the `manifest.json` its installer writes (`[0, 6, 7]`).
pub fn archipelago_version() -> Option<String> {
    archipelago_version_in(&roots())
}

fn archipelago_version_in(roots: &[PathBuf]) -> Option<String> {
    roots.iter().find_map(|r| {
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

/// One installed copy of a world, and the version it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledCopy {
    /// The apworld, or `None` for Archipelago itself ([`ARCHIPELAGO`]).
    pub path: Option<PathBuf>,
    /// `None` when the copy cannot be read or declares no version.
    pub version: Option<String>,
}

/// Every installed copy of `world` ([`installed_apworlds`]) with the version each declares, or
/// for [`ARCHIPELAGO`], Archipelago's own version as one copy. Empty when it is not installed.
pub fn installed_versions(world: &str) -> Vec<InstalledCopy> {
    versions_in(&roots(), world)
}

fn versions_in(roots: &[PathBuf], world: &str) -> Vec<InstalledCopy> {
    if world == ARCHIPELAGO {
        return archipelago_version_in(roots)
            .map(|v| InstalledCopy {
                path: None,
                version: Some(v),
            })
            .into_iter()
            .collect();
    }
    apworlds_in(roots, world)
        .into_iter()
        .map(|p| InstalledCopy {
            version: std::fs::read(&p).ok().and_then(|b| apworld_version(&b)),
            path: Some(p),
        })
        .collect()
}

/// Notes on how this seed and this PC differ from what `profile` was measured against, for
/// the user. Empty when nothing differs or nothing can be compared. `header_name` is the
/// seed's header name, trimmed. Every installed copy of the world is compared, since which one
/// the client runs is not AP64's to know ([`installed_apworlds`]); where there is more than
/// one, the note names each copy that differs.
pub fn release_notes(profile: &Profile, header_name: &str) -> Vec<String> {
    notes_with(profile, header_name, installed_versions)
}

fn notes_with(
    profile: &Profile,
    header_name: &str,
    installed: impl Fn(&str) -> Vec<InstalledCopy>,
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
        let copies = installed(world);
        let differ: Vec<(&InstalledCopy, &String)> = copies
            .iter()
            .filter_map(|c| c.version.as_ref().map(|v| (c, v)))
            .filter(|(_, have)| *have != want)
            .collect();
        if let [(only, have)] = differ[..] {
            if copies.len() == 1 {
                let what = if only.path.is_none() {
                    format!("Archipelago here is {have}")
                } else {
                    format!("{world} here is version {have}")
                };
                notes.push(format!(
                    "{what}, and AP64 was measured against {want}: a seed it makes may differ \
                     from what AP64 has been tested with. {tail}"
                ));
                return notes;
            }
        }
        if !differ.is_empty() {
            let each: Vec<String> = differ
                .iter()
                .map(|(c, have)| {
                    let at = c
                        .path
                        .as_deref()
                        .map_or_else(|| ARCHIPELAGO.to_string(), |p| p.display().to_string());
                    format!("the copy at {at} is version {have}")
                })
                .collect();
            let (which, those) = if each.len() == 1 {
                (each[0].clone(), "that copy")
            } else {
                let (last, rest) = each.split_last().unwrap();
                (format!("{} and {last}", rest.join(", ")), "those copies")
            };
            notes.push(format!(
                "{world} is installed in {} places here, and AP64 was measured against {want}, \
                 but {which}: a seed made with {those} may differ from what AP64 has been tested \
                 with. {tail}",
                copies.len()
            ));
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

    /// One installed copy of an apworld, at `version`.
    fn one(version: &str) -> Vec<InstalledCopy> {
        vec![InstalledCopy {
            path: Some(PathBuf::from("t.apworld")),
            version: Some(version.into()),
        }]
    }

    #[test]
    fn a_different_installed_version_is_a_note_and_the_same_one_is_not() {
        let p = profile("[measured]\nworld = \"t.apworld\"\nversion = \"1.5.8\"\n");
        assert!(notes_with(&p, "T", |_| one("1.5.8")).is_empty());
        let notes = notes_with(&p, "T", |w| {
            if w == "t.apworld" {
                one("1.6.0")
            } else {
                Vec::new()
            }
        });
        assert_eq!(notes.len(), 1);
        assert!(notes[0]
            .starts_with("t.apworld here is version 1.6.0, and AP64 was measured against 1.5.8"));
        assert!(!notes[0].contains("  "), "{}", notes[0]);
    }

    #[test]
    fn a_world_that_is_not_installed_says_nothing() {
        let p = profile("[measured]\nworld = \"t.apworld\"\nversion = \"1.5.8\"\n");
        assert!(notes_with(&p, "T", |_| Vec::new()).is_empty());
    }

    #[test]
    fn archipelago_itself_is_named_as_such() {
        let p = profile("[measured]\nworld = \"Archipelago\"\nversion = \"0.6.7\"\n");
        let notes = notes_with(&p, "T", |_| {
            vec![InstalledCopy {
                path: None,
                version: Some("0.7.0".into()),
            }]
        });
        assert!(
            notes[0].starts_with("Archipelago here is 0.7.0"),
            "{}",
            notes[0]
        );
    }

    #[test]
    fn a_header_that_names_another_release_is_a_note() {
        let p = profile("[measured]\nheader_name = \"MK64 ARCHIPELAGO 0.2\"\n");
        assert!(notes_with(&p, "MK64 ARCHIPELAGO 0.2", |_| Vec::new()).is_empty());
        let notes = notes_with(&p, "MK64 ARCHIPELAGO 0.3", |_| Vec::new());
        assert!(
            notes[0].contains("\"MK64 ARCHIPELAGO 0.3\""),
            "{}",
            notes[0]
        );
        assert!(!notes[0].contains("  "), "{}", notes[0]);
    }

    #[test]
    fn a_profile_that_records_nothing_has_no_notes() {
        assert!(notes_with(&profile(""), "T", |_| one("9")).is_empty());
    }

    /// An apworld whose `archipelago.json` declares `version`, or declares none.
    fn apworld_at(version: Option<&str>) -> Vec<u8> {
        use std::io::Write as _;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("t/__init__.py", opts).unwrap();
        w.write_all(b"# world\n").unwrap();
        let json = match version {
            Some(v) => format!(r#"{{"game": "T", "world_version": "{v}"}}"#),
            None => r#"{"game": "T"}"#.to_string(),
        };
        w.start_file("t/archipelago.json", opts).unwrap();
        w.write_all(json.as_bytes()).unwrap();
        w.finish().unwrap().into_inner()
    }

    /// Archipelago installs under a fresh temporary folder, one per entry, each with
    /// `t.apworld` in its `custom_worlds` at that version (`None`: declares none).
    fn installs(name: &str, versions: &[Option<&str>]) -> (PathBuf, Vec<PathBuf>) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ap64-versions-{name}-{}-{nanos}",
            std::process::id()
        ));
        let roots: Vec<PathBuf> = (0..versions.len())
            .map(|i| dir.join(format!("install{i}")))
            .collect();
        for (root, v) in roots.iter().zip(versions) {
            let worlds = root.join("custom_worlds");
            std::fs::create_dir_all(&worlds).unwrap();
            std::fs::write(worlds.join("t.apworld"), apworld_at(*v)).unwrap();
        }
        (dir, roots)
    }

    /// Two copies, the first the release AP64 was measured against and the second not: the
    /// second is noted, by where it is, and the first is not named.
    #[test]
    fn a_second_copy_at_another_release_is_noted_by_where_it_is() {
        let p = profile("[measured]\nworld = \"t.apworld\"\nversion = \"1.5.8\"\n");
        let (dir, roots) = installs("second", &[Some("1.5.8"), Some("1.6.0")]);
        let notes = notes_with(&p, "T", |w| versions_in(&roots, w));
        assert_eq!(notes.len(), 1, "{notes:?}");
        let second = roots[1].join("custom_worlds").join("t.apworld");
        assert!(
            notes[0].starts_with(&format!(
                "t.apworld is installed in 2 places here, and AP64 was measured against 1.5.8, \
                 but the copy at {} is version 1.6.0: a seed made with that copy may differ",
                second.display()
            )),
            "{}",
            notes[0]
        );
        assert!(!notes[0].contains(&roots[0].display().to_string()));
        assert!(!notes[0].contains("  "), "{}", notes[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two copies at two other releases: both named. Two at the measured one, or one that
    /// declares no version beside one that matches: no note.
    #[test]
    fn every_copy_at_another_release_is_named_and_matching_copies_say_nothing() {
        let p = profile("[measured]\nworld = \"t.apworld\"\nversion = \"1.5.8\"\n");
        let (dir, roots) = installs("both", &[Some("1.6.0"), Some("1.7.0")]);
        let notes = notes_with(&p, "T", |w| versions_in(&roots, w));
        assert_eq!(notes.len(), 1, "{notes:?}");
        for (root, v) in roots.iter().zip(["1.6.0", "1.7.0"]) {
            let at = root.join("custom_worlds").join("t.apworld");
            assert!(
                notes[0].contains(&format!("the copy at {} is version {v}", at.display())),
                "{}",
                notes[0]
            );
        }
        assert!(notes[0].contains("those copies"), "{}", notes[0]);
        let _ = std::fs::remove_dir_all(&dir);

        for versions in [[Some("1.5.8"), Some("1.5.8")], [None, Some("1.5.8")]] {
            let (dir, roots) = installs("same", &versions);
            assert!(notes_with(&p, "T", |w| versions_in(&roots, w)).is_empty());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// One copy reads as it always has: no path, the same words.
    #[test]
    fn one_copy_at_another_release_is_noted_as_before() {
        let p = profile("[measured]\nworld = \"t.apworld\"\nversion = \"1.5.8\"\n");
        let (dir, roots) = installs("one", &[Some("1.6.0")]);
        let notes = notes_with(&p, "T", |w| versions_in(&roots, w));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0]
            .starts_with("t.apworld here is version 1.6.0, and AP64 was measured against 1.5.8"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two installs, one of them with the world in both its folders, and a root named twice:
    /// every copy, once each, in the order they are looked for.
    #[test]
    fn every_installed_copy_of_a_world_is_found_once() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ap64-installs-{}-{nanos}", std::process::id()));
        let (a, b, none) = (dir.join("a"), dir.join("b"), dir.join("none"));
        for d in [
            a.join("lib").join("worlds"),
            b.join("custom_worlds"),
            b.join("lib").join("worlds"),
            none.join("custom_worlds"),
        ] {
            std::fs::create_dir_all(&d).unwrap();
        }
        let copies = [
            a.join("lib").join("worlds").join("w.apworld"),
            b.join("custom_worlds").join("w.apworld"),
            b.join("lib").join("worlds").join("w.apworld"),
        ];
        for c in &copies {
            std::fs::write(c, b"world").unwrap();
        }
        let roots = [a.clone(), none, b, a.join("..").join("a")];
        assert_eq!(apworlds_in(&roots, "w.apworld"), copies);
        assert!(apworlds_in(&roots, "other.apworld").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_manifest_version_is_dotted() {
        let text = r#"{"buildtime": "x", "hashes": {}, "version": [0, 6, 7]}"#;
        assert_eq!(manifest_version(text).as_deref(), Some("0.6.7"));
        assert_eq!(manifest_version("not json"), None);
    }
}
