//! AP64: add the M64P cart agent to an Archipelago N64 seed, then play it on the cart.
//!
//! Patch: load a seed (drop or Browse), which detects the game and verifies it against
//! its profile; choose where the output goes; patch. The loaded ROM stays here, in
//! [`AppState`], so the page never holds tens of megabytes.
//!
//! Play: see [`play`].

mod play;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ap64_core::{apply, builtin, default_output, detect, Bundle, Detection};
use serde::Serialize;
use tauri::{AppHandle, State};

struct Loaded {
    path: PathBuf,
    rom: Vec<u8>,
    detection: Detection,
}

struct AppState {
    bundles: Vec<Bundle>,
    loaded: Mutex<Option<Loaded>>,
    play: play::Play,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileInfo {
    id: String,
    name: String,
    /// Kept for the developer rows: a player has already been given the right ROM by
    /// Archipelago, so which release it is is not a choice they are making here.
    release: String,
    /// The randomizer the profile was measured against.
    randomizer: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadResult {
    path: String,
    file_name: String,
    size: usize,
    detection: Detection,
    /// The profile that will be applied, when exactly one passes.
    chosen: Option<String>,
    default_output: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PatchResult {
    output: String,
    size: usize,
    sha1: String,
    summary: Vec<String>,
}

#[tauri::command]
fn profiles(state: State<'_, AppState>) -> Vec<ProfileInfo> {
    state
        .bundles
        .iter()
        .map(|b| ProfileInfo {
            id: b.profile.id.clone(),
            name: b.profile.name.clone(),
            release: b.profile.release.clone(),
            randomizer: b.profile.randomizer.clone(),
        })
        .collect()
}

#[tauri::command]
fn pick_rom() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Choose a patched Archipelago seed")
        .add_filter("N64 ROM", &["z64", "n64", "v64"])
        .add_filter("All files", &["*"])
        .pick_file()
        .map(|p| p.display().to_string())
}

#[tauri::command]
fn pick_output(suggested: String) -> Option<String> {
    let suggested = Path::new(&suggested);
    let mut dialog = rfd::FileDialog::new()
        .set_title("Save the ROM with the agent")
        .add_filter("N64 ROM (z64)", &["z64"]);
    if let Some(dir) = suggested.parent() {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = suggested.file_name().and_then(|n| n.to_str()) {
        dialog = dialog.set_file_name(name);
    }
    dialog.save_file().map(|p| p.display().to_string())
}

/// ROMs are at most 64 MiB; anything much larger is not one.
const MAX_ROM: u64 = 80 << 20;

#[tauri::command]
fn load_rom(path: String, state: State<'_, AppState>) -> Result<LoadResult, String> {
    let path = PathBuf::from(path);
    let len = std::fs::metadata(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if len > MAX_ROM {
        return Err(format!(
            "{} is {} MiB, too large for an N64 ROM",
            path.display(),
            len >> 20
        ));
    }
    let mut rom = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let detection = detect(&state.bundles, &mut rom).map_err(|e| e.to_string())?;
    let result = LoadResult {
        path: path.display().to_string(),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size: rom.len(),
        chosen: detection.chosen().map(|r| r.profile_id.clone()),
        default_output: default_output(&path).display().to_string(),
        detection: detection.clone(),
    };
    *state.loaded.lock().unwrap() = Some(Loaded {
        path,
        rom,
        detection,
    });
    Ok(result)
}

#[tauri::command]
fn patch_rom(output: String, state: State<'_, AppState>) -> Result<PatchResult, String> {
    let guard = state.loaded.lock().unwrap();
    let loaded = guard.as_ref().ok_or("no seed loaded")?;
    let chosen = loaded
        .detection
        .chosen()
        .ok_or("no profile passes every check")?;
    let bundle = state
        .bundles
        .iter()
        .find(|b| b.profile.id == chosen.profile_id)
        .ok_or("profile missing")?;
    let output = PathBuf::from(output);
    if output == loaded.path {
        return Err("the output would overwrite the seed; choose another name".into());
    }
    let patched = apply(bundle, &loaded.rom).map_err(|e| e.to_string())?;
    std::fs::write(&output, &patched.rom).map_err(|e| format!("{}: {e}", output.display()))?;
    Ok(PatchResult {
        output: output.display().to_string(),
        size: patched.rom.len(),
        sha1: patched.sha1,
        summary: patched.summary,
    })
}

#[tauri::command]
fn play_games(state: State<'_, AppState>) -> Vec<play::Game> {
    play::games(&state.bundles)
}

#[tauri::command]
fn play_start(
    game: String,
    url: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.play.start(app, &state.bundles, game, url)
}

#[tauri::command]
fn play_stop(state: State<'_, AppState>) {
    state.play.stop();
}

#[tauri::command]
fn play_status(state: State<'_, AppState>) -> play::Status {
    let mut s = state.play.status();
    if s.state.is_empty() {
        s.state = "idle".into();
    }
    s
}

/// Whether the chosen game's Archipelago client is ready for AP64; `None` for a game whose
/// client needs nothing.
#[tauri::command]
fn play_client_setup(game: String, state: State<'_, AppState>) -> Option<play::ClientSetup> {
    play::client_setup(&state.bundles, &game)
}

/// Make the fix [`play_client_setup`] reported as needed, keeping the original among AP64's own
/// files.
#[tauri::command]
fn play_client_fix(
    game: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    use tauri::Manager as _;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    play::fix_client(&state.bundles, &game, &dir.join("backups"))
}

#[tauri::command]
fn play_default_url() -> &'static str {
    play::DEFAULT_URL
}

#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())
}

/// The session's log, for the log window to show what it missed (`play://log` carries the rest).
///
/// Through `AppState` like every other command here: asking for `State<Play>` compiles, since
/// the type is right, and then fails at runtime because that is not what was managed -- which
/// looked exactly like a log that only records while its window is open.
#[tauri::command]
fn play_log(state: State<'_, AppState>) -> Vec<String> {
    state.play.log_lines()
}

/// Show the session log in a window of its own, or bring it forward if it is already up.
///
/// Its own window rather than a panel in the main one: a log is the one thing here worth
/// resizing, keeping open beside the game, or dragging to another screen, and the main window is
/// sized to its content and cannot be resized at all.
///
/// **`async` on purpose.** A synchronous command runs on the main thread, and building a window
/// there while the event loop is running deadlocks it on Windows: the new window appears, and
/// then nothing responds -- it cannot be closed, and the main window's buttons do nothing.
/// Declaring the command async puts it on the async runtime instead, which is Tauri's documented
/// way round it. Nothing here awaits; the point is only which thread it runs on.
#[tauri::command]
async fn open_log_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager as _;
    if let Some(w) = app.get_webview_window("log") {
        w.show()
            .and_then(|_| w.set_focus())
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&app, "log", tauri::WebviewUrl::App("log.html".into()))
        .title("AP64 — Session log")
        .inner_size(620.0, 420.0)
        .min_inner_size(360.0, 200.0)
        .resizable(true)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Bounds on [`fit_window_height`], in logical pixels: a page that measured nothing, or a runaway
/// one, must not leave the window unusable.
const WINDOW_MIN_HEIGHT: f64 = 320.0;
const WINDOW_MAX_HEIGHT: f64 = 1600.0;

/// Size the window's content area to `height` logical pixels, keeping its width.
///
/// The window is not resizable, so it is the page's job to ask for the room it needs: it calls
/// this whenever what it shows changes height -- developer details, a dialog opening, a longer
/// status. Nothing in AP64 scrolls as a result, which is the point.
#[tauri::command]
fn fit_window_height(window: tauri::WebviewWindow, height: f64) -> Result<(), String> {
    if !height.is_finite() {
        return Err(format!("not a height: {height}"));
    }
    let height = height.clamp(WINDOW_MIN_HEIGHT, WINDOW_MAX_HEIGHT);
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let width = window
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(scale)
        .width;
    window
        .set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| e.to_string())
}

pub fn run() {
    let bundles = builtin().expect("built-in profiles are valid (checked by tests)");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            bundles,
            loaded: Mutex::new(None),
            play: play::Play::default(),
        })
        .invoke_handler(tauri::generate_handler![
            fit_window_height,
            open_log_window,
            play_log,
            profiles,
            pick_rom,
            pick_output,
            load_rom,
            patch_rom,
            play_games,
            play_start,
            play_stop,
            play_status,
            play_default_url,
            play_client_setup,
            play_client_fix,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running AP64");
}
