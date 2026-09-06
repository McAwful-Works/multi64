//! Ordered copy plans for interactive (per-file) overwrite prompts.
//!
//! Cart walks use [`CartSession`](multi64_sc64_sd::CartSession) so the same logic applies to SC64 and EverDrive sessions.

use multi64_sc64_sd::{cart_path_parts, CartSession, SessionEntry};
use serde::Serialize;
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
    pub is_dir: bool,
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
            append_fs_dir_steps(src, &target, &mut out)?;
        }
    }
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
            .ok_or_else(|| format!("cart path not found: {raw}"))?;
        let dest_top = to_pc_parent.join(&entry.name);
        if entry.is_dir {
            if dest_top.exists() && dest_top.is_file() {
                return Err("Cannot copy cart folder over an existing file on the PC.".into());
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
                return Err("Cannot copy file over an existing folder on the PC.".into());
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
        let dest = dest_dir.join(&e.name);
        if e.is_dir {
            if dest.exists() && dest.is_file() {
                return Err("Cannot copy cart folder over an existing file on the PC.".into());
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
                return Err("Cannot copy file over an existing folder on the PC.".into());
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
}
