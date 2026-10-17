//! Closing Xfer64 never cuts an operation off part-way.
//!
//! Exiting kills the copy thread outright: no `Drop` runs, so the cart's SD session is never
//! released (an SC64 keeps refusing to boot with "SD card is locked by the PC side"), the Multi64
//! bridge that the window paused is never resumed, and a write on the card is left half-done.
//!
//! So each window reports whether it has work under way ([`explorer_set_busy`]: an operation, or
//! a cart read with the bridge paused around it). While any window does, a close request is held
//! and the window that was closed is told ([`CLOSE_REQUESTED_EVENT`]); it decides what to do —
//! the main window asks whether to cancel, Quick upload cancels as its own Close button does —
//! and calls [`explorer_close_when_idle`]. Xfer64 then exits when the last window reports idle,
//! which the windows do only after the operation has ended and the bridge has been resumed.
//!
//! Holding a close must never make Xfer64 impossible to close. An operation that does not honor
//! its cancel, a window that never reports idle, or a window that cannot answer at all would
//! otherwise leave Task Manager as the only way out, which is the abrupt exit this module exists to
//! avoid. So a close asked for [`FORCE_AFTER`] times while work is under way exits anyway: the user
//! has insisted, and the windows say so.

use crate::drag_out::DragStagingState;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, Window};

/// Sent to a window whose close was held because work is under way.
pub const CLOSE_REQUESTED_EVENT: &str = "xfer64-close-requested";

/// The close request, counted while work is under way, that exits regardless.
pub const FORCE_AFTER: u32 = 3;

#[derive(Clone, Serialize)]
pub struct CloseRequestedPayload {
    /// Label of the window that was closed. Every window hears the event; only this one acts.
    pub window: String,
}

/// Which windows have work under way, and whether Xfer64 should exit once none do.
#[derive(Default)]
pub struct CloseGuardState {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Per window label: the sequence number of its latest report, and whether it is busy.
    windows: HashMap<String, (u64, bool)>,
    close_when_idle: bool,
    /// Close requests held since nothing was last busy.
    held: u32,
}

impl Inner {
    fn busy(&self) -> bool {
        self.windows.values().any(|&(_, busy)| busy)
    }
}

impl CloseGuardState {
    /// Whether any window has work under way.
    pub fn is_busy(&self) -> bool {
        self.inner.lock().map(|g| g.busy()).unwrap_or(false)
    }

    /// Record `window`'s report. Reports can arrive out of order, so one older than the last seen
    /// (`seq` not above it) is ignored. Returns `true` when Xfer64 should exit now: a close is
    /// waiting and nothing is busy any more.
    pub fn set_busy(&self, window: &str, seq: u64, busy: bool) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        let entry = g.windows.entry(window.to_string()).or_insert((0, false));
        if seq <= entry.0 {
            return false;
        }
        *entry = (seq, busy);
        if !g.busy() {
            g.held = 0;
        }
        g.close_when_idle && !g.busy()
    }

    /// Count a close request that arrived while work is under way. Returns `true` when it is the
    /// [`FORCE_AFTER`]th since nothing was last busy, and Xfer64 should exit regardless.
    pub fn hold_close(&self) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return true;
        };
        g.held += 1;
        g.held >= FORCE_AFTER
    }

    /// Exit once nothing is busy. Returns `true` when that is already the case.
    pub fn close_when_idle(&self) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return true;
        };
        g.close_when_idle = true;
        !g.busy()
    }
}

/// Exit Xfer64, taking this run's staged drag-out copies with it.
pub fn exit_app(app: &AppHandle) {
    if let Some(staging) = app.try_state::<DragStagingState>() {
        staging.clear();
    }
    app.exit(0);
}

/// A close request for the main window or Quick upload: exit at once when nothing is running,
/// otherwise hold the close and tell the window, which answers with [`explorer_close_when_idle`].
pub fn on_close_requested(window: &Window, api: &tauri::CloseRequestApi) {
    let app = window.app_handle();
    let busy = app
        .try_state::<CloseGuardState>()
        .is_some_and(|g| g.is_busy());
    if !busy {
        exit_app(app);
        return;
    }
    if app
        .try_state::<CloseGuardState>()
        .is_some_and(|g| g.hold_close())
    {
        // Asked for again and again while the operation went on: the user would rather lose its
        // cleanup than keep a window that will not close.
        exit_app(app);
        return;
    }
    api.prevent_close();
    let _ = app.emit(
        CLOSE_REQUESTED_EVENT,
        CloseRequestedPayload {
            window: window.label().to_string(),
        },
    );
}

/// A window's report of whether it has work under way. `seq` increases with each report from
/// that window.
#[tauri::command]
pub fn explorer_set_busy(
    app: AppHandle,
    window: Window,
    state: State<'_, CloseGuardState>,
    seq: u64,
    busy: bool,
) {
    if state.set_busy(window.label(), seq, busy) {
        exit_app(&app);
    }
}

/// Close Xfer64 as soon as no window has work under way: now, if none does.
#[tauri::command]
pub fn explorer_close_when_idle(app: AppHandle, state: State<'_, CloseGuardState>) {
    if state.close_when_idle() {
        exit_app(&app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_running_is_not_busy() {
        let g = CloseGuardState::default();
        assert!(!g.is_busy());
        assert!(
            g.close_when_idle(),
            "a close with nothing running exits at once"
        );
    }

    /// The close waits for the operation, and exits only on the report that it has ended.
    #[test]
    fn a_close_during_an_operation_waits_for_it_to_end() {
        let g = CloseGuardState::default();
        assert!(!g.set_busy("main", 1, true));
        assert!(g.is_busy());
        assert!(!g.close_when_idle(), "the operation is still running");
        assert!(g.set_busy("main", 2, false), "it ended: exit now");
    }

    #[test]
    fn every_window_has_to_be_idle() {
        let g = CloseGuardState::default();
        g.set_busy("main", 1, true);
        g.set_busy("upload-picker", 1, true);
        assert!(!g.close_when_idle());
        assert!(!g.set_busy("upload-picker", 2, false), "main is still busy");
        assert!(g.set_busy("main", 2, false));
    }

    /// A late "busy" report must not undo the "idle" one sent after it.
    #[test]
    fn a_report_older_than_the_last_is_ignored() {
        let g = CloseGuardState::default();
        g.set_busy("main", 2, false);
        g.set_busy("main", 1, true);
        assert!(!g.is_busy());
        g.set_busy("main", 3, true);
        assert!(g.is_busy());
    }

    /// An operation that never stops must not leave Xfer64 impossible to close.
    #[test]
    fn closing_again_and_again_while_busy_exits_anyway() {
        let g = CloseGuardState::default();
        g.set_busy("main", 1, true);
        for n in 1..FORCE_AFTER {
            assert!(!g.hold_close(), "close request {n} is held");
        }
        assert!(g.hold_close(), "close request {FORCE_AFTER} exits");
    }

    /// The count starts over once the work has ended, so an insistent close during one operation
    /// does not carry into the next.
    #[test]
    fn held_closes_are_forgotten_once_nothing_is_busy() {
        let g = CloseGuardState::default();
        g.set_busy("main", 1, true);
        for _ in 1..FORCE_AFTER {
            assert!(!g.hold_close());
        }
        g.set_busy("main", 2, false);
        g.set_busy("main", 3, true);
        assert!(!g.hold_close(), "a new operation starts the count over");
    }

    #[test]
    fn going_idle_without_a_close_waiting_does_not_exit() {
        let g = CloseGuardState::default();
        g.set_busy("main", 1, true);
        assert!(!g.set_busy("main", 2, false));
    }
}
