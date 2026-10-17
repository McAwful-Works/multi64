//! Ordered copy plans for interactive (per-file) overwrite prompts.
//!
//! Cart walks use [`CartSession`](multi64_sc64_sd::CartSession) so the same logic applies to SC64 and EverDrive sessions.

use crate::explorer::same_entry;
use multi64_sc64_sd::{cart_path_parts, fat_name_key, CartSession, SessionEntry};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractiveCopyStep {
    /// "fs" | "export" | "import"
    pub mode: String,
    pub src_pc: Option<String>,
    pub dest_pc: Option<String>,
    pub cart_path: Option<String>,
    pub bytes: u64,
    pub conflict_if_exists: bool,
    /// Create the destination directory rather than copy a file.
    ///
    /// Plans used to contain file steps only, so a directory with no files in it was never
    /// created at the destination and the copy still reported success. Directory steps are
    /// emitted only when the destination is missing, and always before their contents.
    ///
    /// **Every consumer must check this, not just `mode`.** A directory step carries the same
    /// `mode` as a file step, so filtering on `mode` alone routes it into the file path and it
    /// fails with "Source is not a file." The consumers are `cart_serial_import_copy_batch`,
    /// `cart_serial_export_copy_batch` and `run_headless_import_upload` (CLI and Send-to
    /// picker); the last of those was missed when this field was added and broke folder uploads.
    pub is_dir: bool,
}

/// Where one step of a copy writes, for [`refuse_shared_destinations`].
pub(crate) struct Destination<'a> {
    /// What is copied there: a PC path, or a cart path for an export.
    pub source: &'a str,
    /// The destination path, as shown to the user.
    pub dest: &'a str,
    pub is_dir: bool,
    pub on_cart: bool,
}

/// The destination folded as the file system there compares names: a FAT or exFAT card, or
/// Windows, both ignore letter case. Separators and outer slashes are normalized first.
fn destination_key(d: &Destination<'_>) -> Vec<u16> {
    let path = if d.on_cart {
        cart_parent_trimmed(d.dest)
    } else {
        d.dest
            .trim()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_string()
    };
    fat_name_key(&path)
}

/// Refuses a copy in which two sources would land on one destination.
///
/// Every source keeps its own name where it is copied to, so two files called `game.z64` from
/// different folders — as an Explorer search can turn up — map to the same path. Each step's
/// conflict was checked against the destination as it stood when the plan was built, before
/// either was copied, so neither asked first and the second silently replaced the first. A file
/// and a folder sharing a destination are refused too; two folders merge, and whatever inside them
/// shares a destination is caught as a file.
///
/// The plan builders run this, so the user is told before anything is copied, and so do the batch
/// commands, which do not trust the plan they are sent.
pub(crate) fn refuse_shared_destinations<'a>(
    dests: impl IntoIterator<Item = Destination<'a>>,
) -> Result<(), String> {
    let mut files: HashMap<Vec<u16>, Destination<'a>> = HashMap::new();
    let mut dirs: HashMap<Vec<u16>, Destination<'a>> = HashMap::new();
    for d in dests {
        let key = destination_key(&d);
        let earlier = if d.is_dir {
            files.get(&key)
        } else {
            files.get(&key).or_else(|| dirs.get(&key))
        };
        if let Some(earlier) = earlier {
            let name = d
                .dest
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(d.dest);
            let place = if d.on_cart { " on the cart" } else { "" };
            return Err(format!(
                "Two of the items being copied are both called \"{name}\": {} and {} would both \
                 be copied to {}{place}. Copy them one at a time, into different folders.",
                earlier.source, d.source, d.dest
            ));
        }
        if d.is_dir {
            dirs.entry(key).or_insert(d);
        } else {
            files.insert(key, d);
        }
    }
    Ok(())
}

/// [`refuse_shared_destinations`] over a plan's steps.
fn refuse_shared_plan_destinations(plan: &[InteractiveCopyStep]) -> Result<(), String> {
    refuse_shared_destinations(plan.iter().filter_map(|s| {
        let on_cart = s.mode == "import";
        let dest = if on_cart { &s.cart_path } else { &s.dest_pc };
        let source = if s.mode == "export" {
            &s.cart_path
        } else {
            &s.src_pc
        };
        Some(Destination {
            source: source.as_deref()?,
            dest: dest.as_deref()?,
            is_dir: s.is_dir,
            on_cart,
        })
    }))
}

fn cart_parent_trimmed(s: &str) -> String {
    s.trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

fn pc_cart_root_path(cart_parent: &str, dest_name: &str) -> String {
    let cart_base = cart_parent_trimmed(cart_parent);
    if cart_base.is_empty() {
        dest_name.to_string()
    } else {
        format!("{cart_base}/{dest_name}")
    }
}

/// True when `path` is itself a symlink / reparse point, without following it.
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

fn sorted_dir_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let v: Vec<_> = fs::read_dir(path)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    let mut pairs: Vec<(String, fs::DirEntry)> = v
        .into_iter()
        .map(|e| {
            let nl = e.file_name().to_string_lossy().to_lowercase();
            (nl, e)
        })
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs.into_iter().map(|(_, e)| e).collect())
}

/// Cart `list_dir` order for copy plans: case-insensitive name only (one `to_lowercase` per entry).
fn sort_session_entries_by_name_case_insensitive(entries: &mut Vec<SessionEntry>) {
    if entries.len() <= 1 {
        return;
    }
    let mut decorated: Vec<(String, SessionEntry)> = entries
        .drain(..)
        .map(|e| (e.name.to_lowercase(), e))
        .collect();
    decorated.sort_by(|a, b| a.0.cmp(&b.0));
    *entries = decorated.into_iter().map(|(_, e)| e).collect();
}

/// A directory-creation step for a PC → PC plan.
///
/// Without one, a folder holding no files at all copies as nothing: the file steps are what
/// create directories on the way past, so an empty source folder leaves no trace at the
/// destination. Emitted only when the destination is missing, and always before its contents —
/// the same shape `build_cart_export_plan` uses.
fn fs_dir_step(dest: &Path) -> InteractiveCopyStep {
    InteractiveCopyStep {
        mode: "fs".into(),
        src_pc: None,
        dest_pc: Some(dest.to_string_lossy().into_owned()),
        cart_path: None,
        bytes: 0,
        conflict_if_exists: false,
        is_dir: true,
    }
}

/// Whether `path` is `folder` or inside it, however either is spelled: both are resolved to the
/// file system's own final path first.
fn is_inside(path: &Path, folder: &Path) -> bool {
    match (fs::canonicalize(path), fs::canonicalize(folder)) {
        (Ok(p), Ok(f)) => p.starts_with(f),
        _ => false,
    }
}

/// PC → PC folder: one step per file (folders expanded depth-first).
pub fn build_fs_copy_plan(
    src_paths: &[PathBuf],
    dest_dir: &Path,
) -> Result<Vec<InteractiveCopyStep>, String> {
    if !dest_dir.is_dir() {
        return Err("Destination must be an existing folder.".into());
    }
    let mut out = Vec::new();
    for src in src_paths {
        if !src.exists() {
            continue;
        }
        let meta = fs::metadata(src).map_err(|e| e.to_string())?;
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| "Invalid source path.".to_string())?;
        let target = dest_dir.join(name);
        // Dropped into the folder it is already in, under another spelling of that folder (a
        // `subst` drive, a junction, an 8.3 name) that the frontend's string comparison missed:
        // nothing to do, as for the same spelling.
        if same_entry(src, &target) {
            continue;
        }
        if meta.is_dir() && is_inside(dest_dir, src) {
            return Err("A folder cannot be copied into itself.".into());
        }
        if meta.is_file() {
            if target.exists() && target.is_dir() {
                return Err("Cannot copy file over an existing folder.".into());
            }
            let conflict = target.exists() && target.is_file();
            out.push(InteractiveCopyStep {
                mode: "fs".into(),
                src_pc: Some(src.to_string_lossy().into_owned()),
                dest_pc: Some(target.to_string_lossy().into_owned()),
                cart_path: None,
                bytes: meta.len(),
                conflict_if_exists: conflict,
                is_dir: false,
            });
        } else if meta.is_dir() {
            if target.exists() && target.is_file() {
                return Err("Cannot copy folder over an existing file.".into());
            }
            if !target.exists() {
                out.push(fs_dir_step(&target));
            }
            append_fs_dir_steps(src, &target, &mut out)?;
        }
    }
    refuse_shared_plan_destinations(&out)?;
    Ok(out)
}

fn append_fs_dir_steps(
    src_dir: &Path,
    dest_dir: &Path,
    out: &mut Vec<InteractiveCopyStep>,
) -> Result<(), String> {
    let entries = sorted_dir_entries(src_dir)?;
    for e in entries {
        let path = e.path();
        // symlink_metadata, not metadata: following a link that points at an ancestor
        // (a Windows junction, say) recurses until the stack is exhausted.
        if is_symlink(&path) {
            continue;
        }
        let meta = e.metadata().map_err(|e| e.to_string())?;
        let fname = e.file_name().to_string_lossy().into_owned();
        let dest = dest_dir.join(&fname);
        if meta.is_dir() {
            if dest.exists() && dest.is_file() {
                return Err("Cannot copy folder over an existing file.".into());
            }
            if !dest.exists() {
                out.push(fs_dir_step(&dest));
            }
            append_fs_dir_steps(&path, &dest, out)?;
        } else {
            if dest.exists() && dest.is_dir() {
                return Err("Cannot copy file over an existing folder.".into());
            }
            let conflict = dest.exists() && dest.is_file();
            out.push(InteractiveCopyStep {
                mode: "fs".into(),
                src_pc: Some(path.to_string_lossy().into_owned()),
                dest_pc: Some(dest.to_string_lossy().into_owned()),
                cart_path: None,
                bytes: meta.len(),
                conflict_if_exists: conflict,
                is_dir: false,
            });
        }
    }
    Ok(())
}

/// Device names Windows reserves in every folder, with or without an extension (`nul.txt` too).
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The This PC path a cart entry called `name` exports to inside `parent`.
///
/// Cart names come straight from the card's directory entries (FAT long names, or the EverDrive-64
/// PRO firmware), so they are refused unless they are one ordinary Windows file name. `x:y` would
/// write an NTFS alternate data stream and report success; `\`, `/` or a drive prefix would reach
/// outside `parent`; a device name opens the device; a trailing dot or space is stripped by Windows,
/// so two cart names would land on one file. Refusing is safer than renaming: a renamed export
/// would not match its cart name when copied back.
fn pc_child_path(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let refuse = || {
        Err(format!(
            "Can't copy {name:?} to This PC: that name isn't allowed in a Windows file name."
        ))
    };
    if name.is_empty() || name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return refuse();
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return refuse();
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
    {
        return refuse();
    }
    let path = parent.join(name);
    // Belt and braces: whatever the checks above missed, the result must be a direct child.
    if path.parent() != Some(parent) || path.file_name() != Some(std::ffi::OsStr::new(name)) {
        return refuse();
    }
    Ok(path)
}

/// Cart → PC: one step per file.
pub fn build_cart_export_plan(
    session: &CartSession,
    cart_paths: &[String],
    to_pc_parent: &Path,
) -> Result<Vec<InteractiveCopyStep>, String> {
    if !to_pc_parent.is_dir() {
        return Err("Destination must be an existing folder.".into());
    }
    let mut out = Vec::new();
    for raw in cart_paths {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let (parent, name) = cart_path_parts(raw);
        let list = session.list_dir(&parent).map_err(|e| e.to_string())?;
        let entry = list
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("Not found on cart: {raw}"))?;
        let dest_top = pc_child_path(to_pc_parent, &entry.name)?;
        if entry.is_dir {
            if dest_top.exists() && dest_top.is_file() {
                return Err("Cannot copy cart folder over an existing file on This PC.".into());
            }
            if !dest_top.exists() {
                out.push(InteractiveCopyStep {
                    mode: "export".into(),
                    src_pc: None,
                    dest_pc: Some(dest_top.to_string_lossy().into_owned()),
                    cart_path: Some(entry.path.clone()),
                    bytes: 0,
                    conflict_if_exists: false,
                    is_dir: true,
                });
            }
            append_cart_export_steps(session, &entry.path, &dest_top, &mut out)?;
        } else {
            if dest_top.exists() && dest_top.is_dir() {
                return Err("Cannot copy file over an existing folder on This PC.".into());
            }
            let conflict = dest_top.exists() && dest_top.is_file();
            out.push(InteractiveCopyStep {
                mode: "export".into(),
                src_pc: None,
                dest_pc: Some(dest_top.to_string_lossy().into_owned()),
                cart_path: Some(entry.path.clone()),
                bytes: entry.size,
                conflict_if_exists: conflict,
                is_dir: false,
            });
        }
    }
    refuse_shared_plan_destinations(&out)?;
    Ok(out)
}

fn append_cart_export_steps(
    session: &CartSession,
    cart_dir: &str,
    dest_dir: &Path,
    out: &mut Vec<InteractiveCopyStep>,
) -> Result<(), String> {
    let mut kids: Vec<_> = session.list_dir(cart_dir).map_err(|e| e.to_string())?;
    sort_session_entries_by_name_case_insensitive(&mut kids);
    for e in kids {
        if e.name == "." || e.name == ".." {
            continue;
        }
        let dest = pc_child_path(dest_dir, &e.name)?;
        if e.is_dir {
            if dest.exists() && dest.is_file() {
                return Err("Cannot copy cart folder over an existing file on This PC.".into());
            }
            if !dest.exists() {
                out.push(InteractiveCopyStep {
                    mode: "export".into(),
                    src_pc: None,
                    dest_pc: Some(dest.to_string_lossy().into_owned()),
                    cart_path: Some(e.path.clone()),
                    bytes: 0,
                    conflict_if_exists: false,
                    is_dir: true,
                });
            }
            append_cart_export_steps(session, &e.path, &dest, out)?;
        } else {
            if dest.exists() && dest.is_dir() {
                return Err("Cannot copy file over an existing folder on This PC.".into());
            }
            let conflict = dest.exists() && dest.is_file();
            out.push(InteractiveCopyStep {
                mode: "export".into(),
                src_pc: None,
                dest_pc: Some(dest.to_string_lossy().into_owned()),
                cart_path: Some(e.path.clone()),
                bytes: e.size,
                conflict_if_exists: conflict,
                is_dir: false,
            });
        }
    }
    Ok(())
}

/// PC → cart: one step per file.
pub fn build_pc_import_plan(
    session: &CartSession,
    from_pc_paths: &[PathBuf],
    cart_parent: &str,
) -> Result<Vec<InteractiveCopyStep>, String> {
    let mut out = Vec::new();
    for src in from_pc_paths {
        if !src.exists() {
            continue;
        }
        let meta = fs::metadata(src).map_err(|e| e.to_string())?;
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| "Invalid source path.".to_string())?;
        let cart_root = pc_cart_root_path(cart_parent, name);
        if meta.is_file() {
            match session
                .cart_path_entry_kind(&cart_root)
                .map_err(|e| e.to_string())?
            {
                Some(true) => {
                    return Err("Cannot copy file over an existing folder on the cart.".into());
                }
                Some(false) => {
                    out.push(InteractiveCopyStep {
                        mode: "import".into(),
                        src_pc: Some(src.to_string_lossy().into_owned()),
                        dest_pc: None,
                        cart_path: Some(cart_root.clone()),
                        bytes: meta.len(),
                        conflict_if_exists: true,
                        is_dir: false,
                    });
                }
                None => {
                    out.push(InteractiveCopyStep {
                        mode: "import".into(),
                        src_pc: Some(src.to_string_lossy().into_owned()),
                        dest_pc: None,
                        cart_path: Some(cart_root),
                        bytes: meta.len(),
                        conflict_if_exists: false,
                        is_dir: false,
                    });
                }
            }
        } else if meta.is_dir() {
            match session
                .cart_path_entry_kind(&cart_root)
                .map_err(|e| e.to_string())?
            {
                Some(false) => {
                    return Err("Cannot copy folder over an existing file on the cart.".into());
                }
                kind => {
                    // Emit the directory itself when it is missing, before its contents, so an
                    // empty folder still gets created on the card.
                    if kind.is_none() {
                        out.push(InteractiveCopyStep {
                            mode: "import".into(),
                            src_pc: Some(src.to_string_lossy().into_owned()),
                            dest_pc: None,
                            cart_path: Some(cart_root.clone()),
                            bytes: 0,
                            conflict_if_exists: false,
                            is_dir: true,
                        });
                    }
                    append_pc_import_steps(session, src, &cart_root, &mut out)?;
                }
            }
        }
    }
    refuse_shared_plan_destinations(&out)?;
    Ok(out)
}

fn append_pc_import_steps(
    session: &CartSession,
    src_dir: &Path,
    cart_base: &str,
    out: &mut Vec<InteractiveCopyStep>,
) -> Result<(), String> {
    let entries = sorted_dir_entries(src_dir)?;
    for e in entries {
        let path = e.path();
        // See append_fs_dir_steps: links are skipped so a loop cannot recurse forever.
        if is_symlink(&path) {
            continue;
        }
        let meta = e.metadata().map_err(|e| e.to_string())?;
        let fname = e.file_name().to_string_lossy().into_owned();
        let cart_sub = if cart_base.is_empty() {
            fname.clone()
        } else {
            format!("{cart_base}/{fname}")
        };
        if meta.is_dir() {
            match session
                .cart_path_entry_kind(&cart_sub)
                .map_err(|e| e.to_string())?
            {
                Some(false) => {
                    return Err("Cannot copy folder over an existing file on the cart.".into());
                }
                kind => {
                    if kind.is_none() {
                        out.push(InteractiveCopyStep {
                            mode: "import".into(),
                            src_pc: Some(path.to_string_lossy().into_owned()),
                            dest_pc: None,
                            cart_path: Some(cart_sub.clone()),
                            bytes: 0,
                            conflict_if_exists: false,
                            is_dir: true,
                        });
                    }
                    append_pc_import_steps(session, &path, &cart_sub, out)?;
                }
            }
        } else {
            match session
                .cart_path_entry_kind(&cart_sub)
                .map_err(|e| e.to_string())?
            {
                Some(true) => {
                    return Err("Cannot copy file over an existing folder on the cart.".into());
                }
                Some(false) => {
                    out.push(InteractiveCopyStep {
                        mode: "import".into(),
                        src_pc: Some(path.to_string_lossy().into_owned()),
                        dest_pc: None,
                        cart_path: Some(cart_sub.clone()),
                        bytes: meta.len(),
                        conflict_if_exists: true,
                        is_dir: false,
                    });
                }
                None => {
                    out.push(InteractiveCopyStep {
                        mode: "import".into(),
                        src_pc: Some(path.to_string_lossy().into_owned()),
                        dest_pc: None,
                        cart_path: Some(cart_sub),
                        bytes: meta.len(),
                        conflict_if_exists: false,
                        is_dir: false,
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file step must never be tagged as a directory step.
    #[test]
    fn fs_plan_marks_file_steps_as_not_dir() {
        let tmp = std::env::temp_dir().join(format!("xfer64_plan_{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = fs::create_dir_all(&src);
        let _ = fs::create_dir_all(&dst);
        let f = src.join("a.bin");
        fs::write(&f, b"hello").unwrap();

        let plan = build_fs_copy_plan(std::slice::from_ref(&f), &dst).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(!plan[0].is_dir);
        assert_eq!(plan[0].bytes, 5);
        let _ = fs::remove_dir_all(&tmp);
    }

    /// An empty source folder must still arrive at the destination — its own step is the only
    /// thing that can create it, since there are no file steps to do it on the way past.
    #[test]
    fn fs_plan_creates_folders_with_no_files_in_them() {
        let tmp = std::env::temp_dir().join(format!("xfer64_plan_dirs_{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let empty = src.join("saves");
        let nested = src.join("roms").join("hacks");
        let _ = fs::create_dir_all(&empty);
        let _ = fs::create_dir_all(&nested);
        let _ = fs::create_dir_all(&dst);

        let plan = build_fs_copy_plan(std::slice::from_ref(&src), &dst).unwrap();

        let dirs: Vec<&str> = plan
            .iter()
            .filter(|s| s.is_dir)
            .map(|s| s.dest_pc.as_deref().unwrap_or_default())
            .collect();
        assert!(dirs.iter().any(|d| d.ends_with("saves")), "{dirs:?}");
        assert!(dirs.iter().any(|d| d.ends_with("hacks")), "{dirs:?}");
        // A parent is always created before what goes inside it.
        let roms = plan
            .iter()
            .position(|s| s.is_dir && s.dest_pc.as_deref().is_some_and(|d| d.ends_with("roms")));
        let hacks = plan
            .iter()
            .position(|s| s.is_dir && s.dest_pc.as_deref().is_some_and(|d| d.ends_with("hacks")));
        assert!(roms < hacks, "parent directory step must come first");
        let _ = fs::remove_dir_all(&tmp);
    }

    /// Two files of one name from different folders, as an Explorer search turns up, cannot both
    /// be copied into one folder: the plan is refused, naming both, before anything is copied.
    /// Names that differ only in letter case are one name on Windows and on the card.
    ///
    /// Fail-first, demonstrated: without the check, the plan was two steps with no conflict, and
    /// the second copy replaced the first without asking.
    #[test]
    fn fs_plan_refuses_two_sources_with_one_name() {
        let tmp = std::env::temp_dir().join(format!("xfer64_plan_twins_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let (a, b, dst) = (tmp.join("a"), tmp.join("b"), tmp.join("dst"));
        for d in [&a, &b, &dst] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(a.join("game.z64"), b"first").unwrap();
        fs::write(b.join("GAME.z64"), b"second").unwrap();

        let err = build_fs_copy_plan(&[a.join("game.z64"), b.join("GAME.z64")], &dst).unwrap_err();
        assert!(err.contains("both called \"GAME.z64\""), "{err}");
        assert!(
            err.contains(&*a.join("game.z64").to_string_lossy()),
            "{err}"
        );

        // Folders of one name merge; it is files inside them that share a name that collide.
        fs::create_dir_all(a.join("saves")).unwrap();
        fs::create_dir_all(b.join("saves")).unwrap();
        fs::write(a.join("saves").join("one.eep"), b"1").unwrap();
        fs::write(b.join("saves").join("two.eep"), b"2").unwrap();
        build_fs_copy_plan(&[a.join("saves"), b.join("saves")], &dst).expect("folders merge");
        fs::write(b.join("saves").join("ONE.eep"), b"1 again").unwrap();
        build_fs_copy_plan(&[a.join("saves"), b.join("saves")], &dst)
            .expect_err("a file inside both folders collides");
        let _ = fs::remove_dir_all(&tmp);
    }

    /// A drop into the folder an item is already in is nothing to do, and a folder dropped into
    /// its own subfolder is refused, however the destination is spelled. `x\..` stands in for a
    /// `subst` drive, a junction or an 8.3 name, which the frontend's string comparison misses.
    ///
    /// Fail-first, demonstrated: without these checks, the first plan held a step copying
    /// `game.z64` onto itself.
    #[test]
    fn fs_plan_sees_through_other_spellings_of_the_source_folder() {
        let tmp = std::env::temp_dir().join(format!("xfer64_plan_alias_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::create_dir_all(tmp.join("x")).unwrap();
        fs::write(src.join("game.z64"), b"rom").unwrap();
        let src_again = tmp.join("x").join("..").join("src");

        let plan = build_fs_copy_plan(&[src.join("game.z64")], &src_again).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
        let plan =
            build_fs_copy_plan(std::slice::from_ref(&src), &tmp.join("x").join("..")).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
        let err =
            build_fs_copy_plan(std::slice::from_ref(&src), &src_again.join("inner")).unwrap_err();
        assert!(err.contains("into itself"), "{err}");
        let _ = fs::remove_dir_all(&tmp);
    }

    fn import_step(src: &str, cart: &str, is_dir: bool) -> InteractiveCopyStep {
        InteractiveCopyStep {
            mode: "import".into(),
            src_pc: Some(src.into()),
            dest_pc: None,
            cart_path: Some(cart.into()),
            bytes: 1,
            conflict_if_exists: false,
            is_dir,
        }
    }

    /// Cart destinations collide as the card compares names: ignoring case and outer slashes. A
    /// file and a folder at one path collide too; two folders do not.
    #[test]
    fn import_steps_sharing_a_cart_path_are_refused() {
        let file = |s, c| import_step(s, c, false);
        let dir = |s, c| import_step(s, c, true);
        let err = refuse_shared_plan_destinations(&[
            file(r"C:\a\game.z64", "roms/game.z64"),
            file(r"C:\b\game.z64", "/roms/GAME.Z64"),
        ])
        .unwrap_err();
        assert!(err.contains(r"C:\a\game.z64 and C:\b\game.z64"), "{err}");
        assert!(err.contains("on the cart"), "{err}");
        refuse_shared_plan_destinations(&[dir(r"C:\a\x", "x"), file(r"C:\b\x", "x")])
            .expect_err("a file where a folder goes");
        refuse_shared_plan_destinations(&[dir(r"C:\a\x", "x"), dir(r"C:\b\x", "x")])
            .expect("two folders merge");
        refuse_shared_plan_destinations(&[
            file(r"C:\a\game.z64", "roms/game.z64"),
            file(r"C:\a\game.z64", "saves/game.z64"),
        ])
        .expect("different folders on the cart");
    }

    /// `is_symlink` must not follow the link; that is what keeps a junction loop from
    /// recursing until the stack is exhausted.
    #[test]
    fn is_symlink_is_false_for_plain_entries() {
        let tmp = std::env::temp_dir().join(format!("xfer64_link_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let f = tmp.join("plain.bin");
        fs::write(&f, b"x").unwrap();
        assert!(!is_symlink(&f));
        assert!(!is_symlink(&tmp));
        assert!(!is_symlink(&tmp.join("does-not-exist")));
        let _ = fs::remove_dir_all(&tmp);
    }

    /// Cart names come from the card's directory entries, so they can hold anything. A name that
    /// isn't a single, ordinary Windows file name must not become part of a This PC path: `x:y`
    /// writes an NTFS alternate data stream, `\` and `/` climb into other folders, and a drive
    /// prefix or device name reaches outside the destination altogether.
    #[test]
    fn cart_names_unsafe_on_windows_are_refused() {
        let parent = std::env::temp_dir().join("xfer64_names");
        for bad in [
            "x:y",
            "C:evil",
            r"a\b",
            "a/b",
            r"..\..\escape",
            "..",
            ".",
            "",
            "tab\there",
            "nul\0byte",
            "CON",
            "nul.txt",
            "Com1",
            "LPT9.z64",
            "trailing.",
            "trailing ",
            "what?",
            "a*b",
            "pipe|",
            "\"quoted\"",
            "<angle>",
        ] {
            assert!(
                pc_child_path(&parent, bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn ordinary_cart_names_stay_directly_under_the_destination() {
        let parent = std::env::temp_dir().join("xfer64_names");
        for good in [
            "sm64.z64",
            "Mario Kart 64 (USA).z64",
            "CONSOLE.txt",
            "com10",
            "saves",
            ".hidden",
            "ゼルダ.z64",
        ] {
            let path = pc_child_path(&parent, good).unwrap();
            assert_eq!(path.parent(), Some(parent.as_path()), "{good:?}");
            assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(good));
        }
    }
}
