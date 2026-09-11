//! Staging for drags that leave Xfer64 for another window (SD card pane → Windows).
//!
//! Windows will not start a drag for a file that does not exist, and cart files live on the
//! cart's SD card rather than on disk. A cart drag-out therefore exports the selection into a
//! staging directory under the OS temp dir first — the existing export plan
//! (`build_cart_export_plan` + `cart_serial_export_copy_batch`) does that work — and only then
//! can `plugin:drag|start_drag` be handed paths the shell can open. That wait is why a cart
//! drag-out takes two gestures: the first stages, the second drags.
//!
//! Everything staged is a copy. The whole session directory goes when the app exits; one left
//! behind by a crash is pruned on a later start, once it is older than [`STALE_STAGING_MAX_AGE`].
//! The Windows pane needs none of this — those paths are already real.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::State;

/// Directory under the OS temp dir holding one subdirectory per Xfer64 run.
const STAGING_ROOT_NAME: &str = "xfer64-drag";

/// A staging directory from an earlier run is only pruned once it is this old — a second Xfer64
/// running right now must not have its staged files deleted out from under an in-flight drag.
const STALE_STAGING_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn staging_root() -> PathBuf {
    std::env::temp_dir().join(STAGING_ROOT_NAME)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// True when `child` is `parent` itself or sits under it.
///
/// Guards the delete commands: only a path we handed out may be removed, so a caller cannot
/// pass `..` back and take a directory of its own choosing with it.
fn is_inside(parent: &Path, child: &Path) -> bool {
    if child
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return false;
    }
    child.starts_with(parent)
}

/// Remove staging directories left by earlier runs, skipping `keep` (this run's own) and
/// anything younger than `max_age`. Best effort throughout: a directory another process is
/// still using fails to delete, and that is not an error worth failing a drag over.
fn prune_stale_staging_dirs(root: &Path, keep: Option<&Path>, now: SystemTime, max_age: Duration) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() || keep.is_some_and(|k| k == path) {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok());
        if age.is_some_and(|a| a > max_age) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// This run's staging directory, created on first use.
#[derive(Default)]
pub struct DragStagingState {
    session_dir: Mutex<Option<PathBuf>>,
    next_batch: AtomicU64,
}

impl DragStagingState {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_session_dir(&self) -> Result<PathBuf, String> {
        let mut guard = self.session_dir.lock().map_err(|e| e.to_string())?;
        if let Some(dir) = guard.as_ref() {
            if dir.is_dir() {
                return Ok(dir.clone());
            }
        }
        let root = staging_root();
        std::fs::create_dir_all(&root).map_err(|e| format!("staging directory: {e}"))?;
        prune_stale_staging_dirs(&root, None, SystemTime::now(), STALE_STAGING_MAX_AGE);
        let dir = root.join(format!("{}-{}", std::process::id(), unix_millis()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("staging directory: {e}"))?;
        *guard = Some(dir.clone());
        Ok(dir)
    }

    fn session_dir_if_any(&self) -> Option<PathBuf> {
        self.session_dir.lock().ok().and_then(|g| g.clone())
    }

    /// Drop everything this run staged. Called when the app exits; safe to call twice.
    pub fn clear(&self) {
        if let Some(dir) = self.session_dir_if_any() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        if let Ok(mut guard) = self.session_dir.lock() {
            *guard = None;
        }
    }
}

/// Create an empty directory for one drag-out and return its absolute path.
///
/// Each drag gets its own directory so the names inside it are exactly the dragged selection —
/// the shell copies what it is given, and a leftover sibling would ride along.
#[tauri::command]
pub fn drag_staging_begin(state: State<'_, DragStagingState>) -> Result<String, String> {
    let session = state.ensure_session_dir()?;
    let n = state.next_batch.fetch_add(1, Ordering::Relaxed);
    let dir = session.join(format!("d{n}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("staging directory: {e}"))?;
    Ok(dir.to_string_lossy().into_owned())
}

/// Remove one staging directory (a drag that was cancelled, or whose files went stale).
#[tauri::command]
pub fn drag_staging_release(state: State<'_, DragStagingState>, dir: String) -> Result<(), String> {
    let Some(session) = state.session_dir_if_any() else {
        return Ok(());
    };
    let path = PathBuf::from(dir.trim());
    if path == session || !is_inside(&session, &path) {
        return Err("Not a staging directory from this session.".into());
    }
    if path.is_dir() {
        std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Remove everything this run has staged.
#[tauri::command]
pub fn drag_staging_clear(state: State<'_, DragStagingState>) {
    state.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "xfer64-drag-test-{}-{}-{}",
            std::process::id(),
            tag,
            unix_millis()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A directory that is still young stays: another Xfer64 may be dragging out of it.
    #[test]
    fn prune_keeps_fresh_staging_dirs() {
        let root = test_root("fresh");
        let dir = root.join("1234-1");
        std::fs::create_dir_all(&dir).unwrap();

        prune_stale_staging_dirs(&root, None, SystemTime::now(), STALE_STAGING_MAX_AGE);

        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Age is measured against the caller's clock, so a `now` past the cutoff makes the same
    /// directory stale — which is how this is testable without backdating a directory.
    #[test]
    fn prune_removes_staging_dirs_past_max_age() {
        let root = test_root("stale");
        let dir = root.join("1234-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rom.z64"), b"staged").unwrap();

        let later = SystemTime::now() + STALE_STAGING_MAX_AGE + Duration::from_secs(60);
        prune_stale_staging_dirs(&root, None, later, STALE_STAGING_MAX_AGE);

        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// This run's own directory is never pruned, however the clock reads.
    #[test]
    fn prune_never_removes_the_live_session_dir() {
        let root = test_root("keep");
        let mine = root.join("mine");
        let theirs = root.join("theirs");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();

        let later = SystemTime::now() + STALE_STAGING_MAX_AGE + Duration::from_secs(60);
        prune_stale_staging_dirs(&root, Some(&mine), later, STALE_STAGING_MAX_AGE);

        assert!(mine.is_dir());
        assert!(!theirs.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The release guard: only paths under the session directory, and never one that climbs out
    /// of it with `..`.
    #[test]
    fn is_inside_rejects_paths_outside_the_session() {
        let session = PathBuf::from("/tmp/xfer64-drag/42-1");
        assert!(is_inside(&session, &session.join("d0")));
        assert!(is_inside(&session, &session));
        assert!(!is_inside(
            &session,
            &PathBuf::from("/tmp/xfer64-drag/99-1/d0")
        ));
        assert!(!is_inside(&session, &PathBuf::from("/tmp/somewhere-else")));
        assert!(!is_inside(&session, &session.join("../../etc")));
    }

    /// Each drag gets its own directory, and clearing takes the lot.
    #[test]
    fn staging_dirs_are_distinct_and_cleared_together() {
        let state = DragStagingState::new();
        let session = state.ensure_session_dir().unwrap();
        let a = session.join("d0");
        let b = session.join("d1");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert_ne!(a, b);

        state.clear();

        assert!(!session.exists());
        assert!(state.session_dir_if_any().is_none());
    }
}
