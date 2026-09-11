//! **Xfer64** — dual-pane file manager for N64 flash-cart SD over USB serial ↔ Windows (SummerCart64, EverDrive beta, and related). A Multi64 product.

use std::path::PathBuf;
use tauri::Manager;

mod cancel;
mod cart_probe;
mod cart_serial_sd;
mod cli_upload;
mod copy_plan;
mod daemon;
mod dev_log;
mod drag_out;
// Only Windows calls into the promise machinery, but it is plain Rust in construction — the
// descriptor layout and the pipe are tested on every platform, so the module stays compiled and
// tested everywhere rather than being cfg'd out of reach of CI.
#[cfg_attr(not(windows), allow(dead_code))]
mod drag_promise;
mod explorer;
mod progress;
mod send_to_windows;
mod upload_picker;

pub use cli_upload::{parse_upload_args, run_cli_upload, run_cli_upload_from_args};

#[tauri::command]
fn xfer64_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_drag::init())
        .manage(cancel::ExplorerCancelState::default())
        .manage(drag_out::DragStagingState::new())
        .manage(explorer::ExplorerPathCache::default())
        .manage(cart_serial_sd::ExplorerCartSerialState::new())
        // Must be registered before `.setup()` so windows that load immediately can invoke IPC
        // (e.g. `cart_serial_list_dir_page` needs `State<ExplorerSettingsState>`).
        .manage(dev_log::ExplorerSettingsState::load())
        .setup(|app| {
            let picker_json = std::env::var("MULTI64_XFER64_UPLOAD_PICKER_PATHS").ok();
            let picker_paths: Vec<PathBuf> = picker_json
                .as_ref()
                .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
                .map(|v| v.into_iter().map(PathBuf::from).collect())
                .unwrap_or_default();
            app.manage(upload_picker::UploadPickerState {
                pc_paths: picker_paths.clone(),
            });

            let handle = app.handle().clone();
            let settings = app.state::<dev_log::ExplorerSettingsState>();
            let snap = settings.snapshot();
            let dev = dev_log::ExplorerDevLog::new(handle);
            dev.set_developer_mode(snap.developer_mode);
            app.manage(dev);
            if let Some(cart_serial) = app.try_state::<cart_serial_sd::ExplorerCartSerialState>() {
                if let Some(ref com) = snap.preferred_com {
                    if let Ok(mut g) = cart_serial.preferred_com.lock() {
                        *g = Some(com.clone());
                    }
                }
            }
            if !picker_paths.is_empty() {
                // Main stays hidden (see tauri.conf). Only the upload-picker window is shown.
                if let Some(pw) = app.get_webview_window("upload-picker") {
                    let _ = pw.show();
                    let _ = pw.set_focus();
                }
            } else if let Some(main) = app.get_webview_window("main") {
                // Normal launch: main starts hidden so Send-to (picker) never flashes the explorer.
                let _ = main.show();
                let _ = main.set_focus();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let label = window.label();
                if label == "main" || label == "upload-picker" {
                    // Staged drag-out copies are this run's alone; take them with us.
                    if let Some(staging) = window
                        .app_handle()
                        .try_state::<drag_out::DragStagingState>()
                    {
                        staging.clear();
                    }
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            cancel::explorer_cancel_operation,
            cancel::explorer_reset_cancel,
            daemon::explorer_daemon_probe,
            daemon::explorer_daemon_release,
            daemon::explorer_daemon_resume,
            drag_out::drag_staging_begin,
            drag_out::drag_staging_release,
            drag_out::drag_staging_clear,
            explorer::fs_list_dir,
            explorer::fs_list_dir_page,
            explorer::fs_path_info,
            explorer::fs_mkdir,
            explorer::fs_rename,
            explorer::build_fs_copy_plan,
            explorer::fs_copy_one_file,
            explorer::explorer_emit_progress,
            explorer::fs_remove,
            explorer::pick_folder,
            explorer::fs_user_dirs,
            explorer::fs_parent,
            cart_serial_sd::cart_serial_list_ports,
            cart_serial_sd::cart_serial_suggest_port,
            cart_serial_sd::cart_serial_probe_status,
            cart_serial_sd::cart_serial_invalidate_probe_cache,
            cart_serial_sd::cart_serial_ed64_linear_hint_bases,
            cart_serial_sd::cart_serial_probe_ed64_linear_base,
            cart_serial_sd::cart_serial_set_preferred_com,
            cart_serial_sd::cart_serial_list_dir_page,
            cart_serial_sd::cart_serial_path_info,
            cart_serial_sd::build_cart_export_plan,
            cart_serial_sd::build_cart_import_plan,
            cart_serial_sd::cart_serial_export_copy_one,
            cart_serial_sd::cart_serial_import_copy_one,
            cart_serial_sd::cart_serial_export_copy_batch,
            cart_serial_sd::cart_serial_import_copy_batch,
            cart_serial_sd::cart_serial_remove_cart,
            cart_serial_sd::cart_serial_mkdir_cart,
            cart_serial_sd::cart_serial_rename_cart,
            cart_serial_sd::drag_start_cart_promise,
            dev_log::explorer_get_settings,
            dev_log::explorer_set_settings,
            dev_log::explorer_set_quick_upload_cart_path,
            dev_log::explorer_set_quick_upload_overwrite,
            dev_log::explorer_dev_log_get,
            dev_log::explorer_dev_log_clear,
            dev_log::explorer_open_dev_shell,
            dev_log::explorer_close_dev_shell,
            send_to_windows::explorer_send_to_upload_supported,
            send_to_windows::explorer_send_to_upload_is_installed,
            send_to_windows::explorer_send_to_upload_install,
            send_to_windows::explorer_send_to_upload_remove,
            upload_picker::upload_picker_get_paths,
            upload_picker::upload_picker_run,
            upload_picker::upload_picker_close,
            xfer64_app_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
