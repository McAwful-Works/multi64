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
    release: String,
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

#[tauri::command]
fn play_default_url() -> &'static str {
    play::DEFAULT_URL
}

#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())
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
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running AP64");
}
