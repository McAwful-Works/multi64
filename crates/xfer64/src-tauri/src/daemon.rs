//! HTTP coordination with **`multi64d`** so Xfer64 can temporarily take the COM port.

use crate::cart_probe::DetectedCartKind;
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

/// ureq 3 moved timeouts from the request builder to agent config, so each call
/// builds a one-shot agent carrying its own deadline.
fn daemon_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn daemon_get_root(listen: &str) -> Result<serde_json::Value, String> {
    let url = with_http_base(listen, "/");
    let mut resp = daemon_agent(Duration::from_secs(2))
        .get(&url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(format!("GET {url}: HTTP {status}"));
    }
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read body: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("JSON: {e}"))
}

fn daemon_health_ok(listen: &str) -> bool {
    let url = with_http_base(listen, "/health");
    daemon_agent(Duration::from_secs(1))
        .get(&url)
        .call()
        .map(|r| r.status().as_u16() == 200)
        .unwrap_or(false)
}

/// The daemon address for lookups the backend makes on its own: `MULTI64_DAEMON_LISTEN`, else the
/// default. A window pointed at another address just gets no hint, and Auto probes as before.
pub fn default_listen() -> String {
    std::env::var("MULTI64_DAEMON_LISTEN").unwrap_or_else(|_| "http://127.0.0.1:38765".into())
}

/// The port and cart a running `multi64d` reports, when Auto can use them instead of probing.
///
/// Used only when the daemon answers `/health`, its `GET /` names a `serial` port and a known `cart`
/// (daemon API §1.1), and that port is still plugged in. A released link still counts, since Xfer64
/// is usually the one that released it. Anything else is `None`, and Auto probes ports as before.
pub fn daemon_cart_hint(listen: &str) -> Option<(String, DetectedCartKind)> {
    if !daemon_health_ok(listen) {
        return None;
    }
    let root = daemon_get_root(listen).ok()?;
    let present: Vec<String> = serialport::available_ports()
        .ok()?
        .into_iter()
        .map(|p| p.port_name)
        .collect();
    cart_hint_from_root(&root, &present)
}

/// [`daemon_cart_hint`] once the daemon has answered: `root` is its `GET /`, `present` the ports
/// enumerated now. Returns the port as enumerated, so a `\\.\COM4`-style path still matches.
fn cart_hint_from_root(
    root: &serde_json::Value,
    present: &[String],
) -> Option<(String, DetectedCartKind)> {
    let serial = root.get("serial")?.as_str()?.trim();
    let serial = serial.strip_prefix(r"\\.\").unwrap_or(serial);
    if serial.is_empty() {
        return None;
    }
    let kind = match root.get("cart")?.as_str()? {
        "sc64" => DetectedCartKind::Sc64,
        "ed64pro" => DetectedCartKind::Ed64Pro,
        "ed64" => DetectedCartKind::Ed64Beta,
        _ => return None,
    };
    let port = present.iter().find(|p| p.eq_ignore_ascii_case(serial))?;
    Some((port.clone(), kind))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonProbe {
    pub up: bool,
    pub daemon_serial: Option<String>,
    /// The COM port pinned in Xfer64, if any. Auto is never resolved here: see below.
    pub explorer_serial: Option<String>,
}

/// Whether multi64d is up and using the same COM port as Xfer64 (conflict).
pub fn explorer_daemon_probe_snapshot(
    st: &ExplorerCartSerialState,
    _snap: &ExplorerSettingsSnapshot,
    listen: &str,
) -> Result<DaemonProbe, String> {
    // Only a pinned port. Resolving Auto used to happen here, first -- and it probes ports, which
    // fails while multi64d holds the cart's port. The error then skipped the release this probe
    // exists to decide on, so Auto-detect could never find a cart the daemon was using.
    let explorer_serial = cart_serial_sd::pinned_com_port(st);
    if !daemon_health_ok(listen) {
        return Ok(DaemonProbe {
            up: false,
            daemon_serial: None,
            explorer_serial,
        });
    }

    let v = match daemon_get_root(listen) {
        Ok(j) => j,
        Err(_) => {
            return Ok(DaemonProbe {
                up: true,
                daemon_serial: None,
                explorer_serial,
            });
        }
    };

    let daemon_serial = v
        .get("serial")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Callers release and resume whenever `up`, not by `serialActive` or a COM match: COM strings
    // were too brittle (`COM3` vs `\\.\COM3`, an empty `serial`), and a link that is already
    // released or faulted still needs the resume that pairs with a release.
    Ok(DaemonProbe {
        up: true,
        daemon_serial,
        explorer_serial,
    })
}

/// Whether multi64d is up and using the same COM port as Xfer64 (conflict).
///
/// `async` because resolving the COM port can run a full serial auto-detect scan and the health
/// and root probes each block on HTTP; a sync command would do all of that on the main thread.
#[tauri::command]
pub async fn explorer_daemon_probe(
    st: State<'_, ExplorerCartSerialState>,
    settings: State<'_, ExplorerSettingsState>,
    listen: String,
) -> Result<DaemonProbe, String> {
    let snap = settings.snapshot();
    cart_serial_sd::spawn_with_cart_state(&st, "explorer_daemon_probe", move |st| {
        explorer_daemon_probe_snapshot(st, &snap, &listen)
    })
    .await
}

pub fn explorer_daemon_release_listen(listen: &str) -> Result<(), String> {
    let url = with_http_base(listen, "/v1/serial/release");
    let resp = daemon_agent(Duration::from_secs(5))
        .post(&url)
        .send_empty()
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(format!("Couldn't pause the Multi64 bridge: HTTP {status}"));
    }
    Ok(())
}

/// `async` so the 5 s release timeout cannot stall the window event loop.
#[tauri::command]
pub async fn explorer_daemon_release(listen: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || explorer_daemon_release_listen(&listen))
        .await
        .map_err(|e| format!("bridge release task: {e}"))?
}

pub fn explorer_daemon_resume_listen(listen: &str) -> Result<(), String> {
    let url = with_http_base(listen, "/v1/serial/resume");
    let mut resp = daemon_agent(Duration::from_secs(10))
        .post(&url)
        .send_empty()
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status().as_u16();
    if status != 200 {
        let hint = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(format!(
            "Couldn't resume the Multi64 bridge: HTTP {status} {hint}"
        ));
    }
    Ok(())
}

/// `async` so the 10 s resume timeout cannot stall the window event loop.
#[tauri::command]
pub async fn explorer_daemon_resume(listen: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || explorer_daemon_resume_listen(&listen))
        .await
        .map_err(|e| format!("bridge resume task: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ports(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_daemon_on_a_present_port_names_the_cart() {
        let root = json!({"serial": "COM4", "serialActive": true, "cart": "sc64"});
        assert_eq!(
            cart_hint_from_root(&root, &ports(&["COM5", "COM4"])),
            Some(("COM4".to_string(), DetectedCartKind::Sc64))
        );
        let root = json!({"serial": r"\\.\com6", "serialActive": false, "cart": "ed64pro"});
        assert_eq!(
            cart_hint_from_root(&root, &ports(&["COM6"])),
            Some(("COM6".to_string(), DetectedCartKind::Ed64Pro)),
            "a released link and a device-path spelling still count"
        );
        let root = json!({"serial": "COM7", "cart": "ed64"});
        assert_eq!(
            cart_hint_from_root(&root, &ports(&["COM7"])).map(|h| h.1),
            Some(DetectedCartKind::Ed64Beta)
        );
    }

    /// Stale or incomplete answers fall back to probing.
    #[test]
    fn anything_short_of_a_known_cart_on_a_present_port_is_no_hint() {
        let present = ports(&["COM4"]);
        for root in [
            json!({"serial": "COM9", "cart": "sc64"}),
            json!({"serial": "", "cart": "sc64"}),
            json!({"serial": "COM4"}),
            json!({"serial": "COM4", "cart": ""}),
            json!({"serial": "COM4", "cart": "n64dd"}),
            json!({}),
        ] {
            assert_eq!(cart_hint_from_root(&root, &present), None, "{root}");
        }
    }
}
