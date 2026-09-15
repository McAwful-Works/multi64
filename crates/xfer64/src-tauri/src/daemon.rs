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

/// The daemon address when no window supplied one: `MULTI64_DAEMON_LISTEN`, else the default. The
/// headless `xfer64 upload` uses it; in the app, Auto's cart hint asks the address the windows last
/// probed ([`ExplorerCartSerialState::hint_listen`]) and falls back to this only before any probe.
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
    /// multi64d answers `/health`.
    pub up: bool,
}

/// Whether multi64d answers at `listen`, and so has to be paused before Xfer64 opens the cart.
///
/// Also records `listen` in `st` as the address Auto's cart hint asks
/// ([`ExplorerCartSerialState::hint_listen`]): cart work always probes first, so the hint reaches
/// the daemon this probe is about, not `MULTI64_DAEMON_LISTEN`'s (#179).
///
/// Never resolves Auto: that probes ports, which fails while multi64d holds the cart's port, and the
/// error used to skip the release this probe exists to decide on.
pub fn explorer_daemon_probe_snapshot(
    st: &ExplorerCartSerialState,
    _snap: &ExplorerSettingsSnapshot,
    listen: &str,
) -> Result<DaemonProbe, String> {
    st.remember_daemon_listen(listen);
    // Callers release and resume whenever `up`, not by `serialActive` or a COM match: COM strings
    // were too brittle (`COM3` vs `\\.\COM3`, an empty `serial`), and a link that is already
    // released or faulted still needs the resume that pairs with a release.
    Ok(DaemonProbe {
        up: daemon_health_ok(listen),
    })
}

/// Whether multi64d is up, so the window has to pause it around cart work.
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
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// A multi64d stand-in on an ephemeral port: answers `/health` and `GET /` (naming a port that
    /// is not plugged in, so no hint results) and logs each request's path.
    fn fake_daemon() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let listen = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&req);
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = if path == "/" {
                    r#"{"serial":"COM_NOT_PLUGGED_IN","serialActive":true,"cart":"sc64"}"#
                } else {
                    "ok"
                };
                log.lock().unwrap().push(path);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (listen, seen)
    }

    /// #179 follow-up: Auto's cart hint asks multi64d at the address the window probed it at, not
    /// at `MULTI64_DAEMON_LISTEN`. The probe runs on a command's detached copy of the state, as in
    /// the app, and the address still reaches the managed state the next command copies.
    #[test]
    fn the_cart_hint_asks_the_daemon_the_window_probed() {
        let (listen, seen) = fake_daemon();
        let st = ExplorerCartSerialState::new();
        assert_eq!(
            st.hint_listen(),
            default_listen(),
            "nothing probed yet, as in the headless upload"
        );

        let probe_listen = listen.clone();
        let probe = tauri::async_runtime::block_on(cart_serial_sd::spawn_with_cart_state(
            &st,
            "test",
            move |st| {
                explorer_daemon_probe_snapshot(
                    st,
                    &ExplorerSettingsSnapshot::default(),
                    &probe_listen,
                )
            },
        ))
        .unwrap();
        assert!(probe.up);
        assert_eq!(st.hint_listen(), listen);

        assert_eq!(cart_serial_sd::auto_cart_hint(&st), None);
        assert_eq!(*seen.lock().unwrap(), ["/health", "/health", "/"]);
    }

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
