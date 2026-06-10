// SPDX-License-Identifier: AGPL-3.0-or-later
//! Client for the desktop command bridge (`sovereign-desktop`'s
//! debug-only loopback HTTP surface, `src-tauri/src/command_bridge.rs`).
//!
//! Benches use this as an alternate **answer source**: instead of
//! building an in-process Runtime that delegates inference to the
//! daemon (the default "direct" transport), questions are dispatched
//! through the production desktop command surface — the same
//! `#[tauri::command]` handlers the UI invokes, running in a real
//! desktop process. Banks and scorers are reused untouched, so a score
//! delta between the two transports isolates the desktop layer (its
//! Runtime wiring, command glue, and embedded inference) rather than
//! bank or rubric drift.
//!
//! The desktop must be running with `SOVEREIGN_COMMAND_BRIDGE=1`
//! (the real-mode e2e harness's global-setup launches it that way, or
//! launch `target/debug/sovereign-desktop` by hand).

use serde::de::DeserializeOwned;
use serde_json::Value;

pub const DEFAULT_BRIDGE_URL: &str = "http://127.0.0.1:9745";

pub struct BridgeClient {
    base: String,
    http: reqwest::Client,
}

impl BridgeClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Fail fast with a actionable message when the bridge isn't up.
    pub async fn healthz(&self) -> Result<(), String> {
        let url = format!("{}/healthz", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                format!(
                    "desktop bridge unreachable at {url}: {e}\n\
                     Launch the desktop with SOVEREIGN_COMMAND_BRIDGE=1 \
                     (e.g. via the real-mode e2e global-setup) first."
                )
            })?;
        if !resp.status().is_success() {
            return Err(format!("bridge healthz returned {}", resp.status()));
        }
        Ok(())
    }

    /// Dispatch one Tauri command through the production invoke path.
    /// Rejections come back as the command's error value, same as the
    /// frontend `invoke()` sees.
    pub async fn invoke<T: DeserializeOwned>(
        &self,
        cmd: &str,
        args: Value,
        spec: &str,
    ) -> Result<T, String> {
        let resp = self
            .http
            .post(format!("{}/invoke", self.base))
            .header("x-sovereign-spec", spec)
            .json(&serde_json::json!({ "cmd": cmd, "args": args }))
            .send()
            .await
            .map_err(|e| format!("bridge invoke {cmd}: transport: {e}"))?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("bridge invoke {cmd}: bad response body: {e}"))?;
        if body["ok"].as_bool() != Some(true) {
            return Err(format!(
                "bridge invoke {cmd} failed: {}",
                body["error"].to_string()
            ));
        }
        serde_json::from_value(body["result"].clone())
            .map_err(|e| format!("bridge invoke {cmd}: result shape: {e}"))
    }

    /// Snapshot of the bridge's replay ring (`GET /events/recent`),
    /// optionally filtered to rows with `seq >= since_seq`. Used to
    /// await terminal events (message-complete) without racing the
    /// live SSE stream.
    pub async fn events_recent(&self, since_seq: u64) -> Result<Vec<EventRow>, String> {
        let body: Value = self
            .http
            .get(format!("{}/events/recent?since_seq={since_seq}", self.base))
            .send()
            .await
            .map_err(|e| format!("bridge events/recent: {e}"))?
            .json()
            .await
            .map_err(|e| format!("bridge events/recent: body: {e}"))?;
        serde_json::from_value(body["rows"].clone())
            .map_err(|e| format!("bridge events/recent: rows shape: {e}"))
    }

    /// Register an event name with the bridge's lazy listen_any layer
    /// so subsequent emissions land in the replay ring.
    pub async fn listen(&self, event: &str) -> Result<(), String> {
        self.http
            .post(format!("{}/listen", self.base))
            .json(&serde_json::json!({ "event": event }))
            .send()
            .await
            .map_err(|e| format!("bridge listen {event}: {e}"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EventRow {
    pub seq: u64,
    pub event: String,
    pub payload: Value,
}
