//! User **Send to** folder shortcut for quick upload (`xfer64 upload --picker`).

#[cfg(windows)]
use std::path::PathBuf;

/// Send-to shortcut name; keep in sync with `windows/installer-hooks.nsh` and `windows/wix-sendto-uninstall.ps1`.
#[cfg(windows)]
const SHORTCUT_NAME: &str = "Xfer64 upload.lnk";

#[cfg(windows)]
fn ps_single_quote_escape(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(windows)]
fn send_to_lnk_path() -> Result<PathBuf, String> {
    let app_data = dirs::data_dir().ok_or("Could not resolve AppData (APPDATA).")?;
    Ok(app_data
        .join("Microsoft")
        .join("Windows")
        .join("SendTo")
        .join(SHORTCUT_NAME))
}

#[cfg(windows)]
pub fn is_installed() -> bool {
    send_to_lnk_path().map(|p| p.is_file()).unwrap_or(false)
}

#[cfg(windows)]
pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let lnk = send_to_lnk_path()?;
    if let Some(parent) = lnk.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let lnk_s = ps_single_quote_escape(&lnk.to_string_lossy());
    let exe_s = ps_single_quote_escape(&exe.to_string_lossy());
    let script = format!(
        "$WshShell = New-Object -ComObject WScript.Shell; $s = $WshShell.CreateShortcut('{lnk_s}'); $s.TargetPath = '{exe_s}'; $s.Arguments = 'upload --picker'; $s.Save()"
    );
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        return Err(format!("Could not create Send to shortcut.\n{err}\n{out}"));
    }
    Ok(())
}

#[cfg(windows)]
pub fn remove() -> Result<(), String> {
    let lnk = send_to_lnk_path()?;
    if lnk.is_file() {
        std::fs::remove_file(&lnk).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn is_installed() -> bool {
    false
}

#[cfg(not(windows))]
pub fn install() -> Result<(), String> {
    Err("Send to is only available on Windows.".into())
}

#[cfg(not(windows))]
pub fn remove() -> Result<(), String> {
    Err("Send to is only available on Windows.".into())
}

pub fn supported() -> bool {
    cfg!(windows)
}

#[tauri::command]
pub fn explorer_send_to_upload_supported() -> bool {
    supported()
}

#[tauri::command]
pub fn explorer_send_to_upload_is_installed() -> bool {
    is_installed()
}

#[tauri::command]
pub fn explorer_send_to_upload_install() -> Result<(), String> {
    install()
}

#[tauri::command]
pub fn explorer_send_to_upload_remove() -> Result<(), String> {
    remove()
}
