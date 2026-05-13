//! HTTP coordination with **`multi64d`** so Xfer64 can temporarily take the COM port.

use crate::cart_serial_sd::{self, ExplorerCartSerialState};
use crate::dev_log::{ExplorerSettingsSnapshot, ExplorerSettingsState};
use serde::Serialize;
use std::time::Duration;
use tauri::State;

fn with_http_base(listen: &str, path: &str) -> String {
    let base = listen.trim().trim_end_matches('/');
    if base.starts_with("http://") || base.starts_with("https://") {
        format!("{base}{path}")
    } else {
        format!("http://{base}{path}")
    }
}

fn daemon_get_root(listen: &str) -> Result<serde_json::Value, String> {
    let url = with_http_base(listen, "/");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(2))
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    if resp.status() != 200 {
        return Err(format!("GET {}: HTTP {}", url, resp.status()));
    }
    let body = resp.into_string().map_err(|e| format!("read body: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("JSON: {e}"))
}

fn daemon_health_ok(listen: &str) -> bool {
    let url = with_http_base(listen, "/health");
    ureq::get(&url)
        .timeout(Duration::from_secs(1))
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonProbe {
    pub up: bool,
    pub daemon_serial: Option<String>,
    pub explorer_serial: String,
    pub needs_yield: bool,
}

/// Whether multi64d is up and using the same COM port as Xfer64 (conflict).
pub fn explorer_daemon_probe_snapshot(
    st: &ExplorerCartSerialState,
    snap: &ExplorerSettingsSnapshot,
    listen: &str,
) -> Result<DaemonProbe, String> {
    let explorer_serial = cart_serial_sd::resolve_com_port(st, snap)?;
    if !daemon_health_ok(listen) {
        return Ok(DaemonProbe {
            up: false,
            daemon_serial: None,
            explorer_serial,
            needs_yield: false,
        });
    }

    let v = match daemon_get_root(listen) {
        Ok(j) => j,
        Err(_) => {
            return Ok(DaemonProbe {
                up: true,
                daemon_serial: None,
                explorer_serial,
                needs_yield: false,
            });
        }
    };

    let daemon_serial = v
        .get("serial")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Older multi64d without `serialActive` defaults to true (assume link held).
    let serial_active = v
        .get("serialActive")
        .and_then(|x| x.as_bool())
        .unwrap_or(true);

    // True when multi64d still holds the COM port. Xfer64 front end uses `up` (daemon
    // health) to always release/resume around cart access — COM string match was too brittle
    // (`COM3` vs `\\.\COM3`, empty `serial` in JSON, etc.).
    let needs_yield = serial_active;

    Ok(DaemonProbe {
        up: true,
        daemon_serial,
        explorer_serial,
        needs_yield,
    })
}

/// Whether multi64d is up and using the same COM port as Xfer64 (conflict).
#[tauri::command]
pub fn explorer_daemon_probe(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    listen: String,
) -> Result<DaemonProbe, String> {
    let snap = settings.snapshot();
    explorer_daemon_probe_snapshot(&st, &snap, &listen)
}

pub fn explorer_daemon_release_listen(listen: &str) -> Result<(), String> {
    let url = with_http_base(listen, "/v1/serial/release");
    let resp = ureq::post(&url)
        .timeout(Duration::from_secs(5))
        .send_bytes(&[])
        .map_err(|e| format!("POST {url}: {e}"))?;
    if resp.status() != 200 {
        return Err(format!("multi64d release failed: HTTP {}", resp.status()));
    }
    Ok(())
}

#[tauri::command]
pub fn explorer_daemon_release(listen: String) -> Result<(), String> {
    explorer_daemon_release_listen(&listen)
}

pub fn explorer_daemon_resume_listen(listen: &str) -> Result<(), String> {
    let url = with_http_base(listen, "/v1/serial/resume");
    let resp = ureq::post(&url)
        .timeout(Duration::from_secs(10))
        .send_bytes(&[])
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    if status != 200 {
        let hint = resp.into_string().unwrap_or_default();
        return Err(format!("multi64d resume failed: HTTP {status} {hint}"));
    }
    Ok(())
}

#[tauri::command]
pub fn explorer_daemon_resume(listen: String) -> Result<(), String> {
    explorer_daemon_resume_listen(&listen)
}
