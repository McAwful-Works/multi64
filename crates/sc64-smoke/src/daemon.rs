//! Pausing `multi64d` while this tool holds the cart's serial port.
//!
//! A cart has one serial device, and `multi64d` keeps it open, so every other process that opens
//! it takes part in the daemon's handshake (`CLAUDE.md`, "Two independent USB stacks"):
//! `POST /v1/serial/release` first, `POST /v1/serial/resume` after. As in Xfer64, the daemon is
//! released whenever it answers `/health`, whatever port it reports: a link that is already
//! released or faulted still needs the resume that pairs with a release.

use std::time::Duration;

/// ureq 3 keeps timeouts on the agent, so each call builds a one-shot agent with its own deadline.
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn url(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim().trim_end_matches('/'))
}

fn post(base: &str, path: &str, timeout: Duration) -> Result<(), String> {
    let url = url(base, path);
    agent(timeout)
        .post(&url)
        .send_empty()
        .map(|_| ())
        .map_err(|e| format!("POST {url}: {e}"))
}

/// `multi64d` released for as long as this lives. Dropping it without [`Pause::resume`] still
/// resumes, best effort, so an early return cannot leave the bridge released.
pub struct Pause {
    /// The daemon's address while it is released by us; `None` once resumed, or if it was not up.
    released: Option<String>,
}

/// Release `multi64d` at `base` if it answers there.
///
/// A failed release is still followed by a resume: the daemon may apply a release that timed out
/// here, and nothing else would resume it.
pub fn pause(base: &str) -> Result<Pause, String> {
    let health = url(base, "/health");
    let up = agent(Duration::from_secs(1))
        .get(&health)
        .call()
        .is_ok_and(|r| r.status().as_u16() == 200);
    if !up {
        println!("multi64d: not answering at {base}; opening the port directly");
        return Ok(Pause { released: None });
    }
    if let Err(e) = post(base, "/v1/serial/release", Duration::from_secs(5)) {
        let _ = post(base, "/v1/serial/resume", Duration::from_secs(15));
        return Err(format!("could not pause multi64d: {e}"));
    }
    println!("multi64d: released the serial port; it is resumed when this tool exits");
    Ok(Pause {
        released: Some(base.to_string()),
    })
}

impl Pause {
    /// Resume `multi64d` if this released it.
    pub fn resume(mut self) -> Result<(), String> {
        let Some(base) = self.released.take() else {
            return Ok(());
        };
        post(&base, "/v1/serial/resume", Duration::from_secs(15)).map_err(|e| {
            format!(
                "could not resume multi64d, which stays released and drops what clients send: {e}"
            )
        })?;
        println!("multi64d: resumed");
        Ok(())
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        if let Some(base) = self.released.take() {
            let _ = post(&base, "/v1/serial/resume", Duration::from_secs(15));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_join_with_one_slash() {
        assert_eq!(
            url("http://127.0.0.1:38765", "/health"),
            "http://127.0.0.1:38765/health"
        );
        assert_eq!(
            url(" http://127.0.0.1:38765/ ", "/v1/serial/release"),
            "http://127.0.0.1:38765/v1/serial/release"
        );
    }
}
