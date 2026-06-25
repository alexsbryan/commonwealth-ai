// SPDX-License-Identifier: AGPL-3.0-or-later
//! `HttpCorpusInstaller` — the concrete [`CorpusInstaller`] a `recipe:` step uses.
//!
//! It does NOT reimplement ingest or touch the mesh handler: it POSTs to the
//! daemon's existing internal `/internal/corpus/install` (the same endpoint
//! `sovereign corpus install` and the desktop use — fully mesh-coordinated) and
//! then polls `/internal/corpus/progress` to completion so a downstream workflow
//! step can consume the corpus. The endpoint is the daemon's internal port on
//! loopback (matching `corpus install`'s own client), so the same impl serves both
//! the CLI run path and a daemon-triggered workflow (it hits its own loopback).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{CorpusInstaller, InstallOutcome};

/// The daemon's internal port on loopback — same target `corpus install` uses
/// (`sovereign-cli-llm/.../inventory.rs`).
const INSTALL_BASE: &str = "http://127.0.0.1:9742";

pub struct HttpCorpusInstaller {
    base: String,
    client: reqwest::Client,
    /// Hard cap on the poll before we return "installing" (the install still runs
    /// in the background; this just bounds the step's wait).
    poll_timeout: Duration,
}

impl Default for HttpCorpusInstaller {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpCorpusInstaller {
    pub fn new() -> Self {
        Self {
            base: INSTALL_BASE.to_string(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            poll_timeout: Duration::from_secs(30 * 60),
        }
    }
}

#[async_trait]
impl CorpusInstaller for HttpCorpusInstaller {
    async fn ensure_installed(
        &self,
        id: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<InstallOutcome> {
        // 1. Fire the install (fire-and-forget; the daemon spawns the
        //    mesh-coordinated task). Same body shape as `corpus install`.
        let body = serde_json::json!({ "corpus_id": id, "parameters": params });
        let resp = self
            .client
            .post(format!("{}/internal/corpus/install", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                Error::Execution(format!(
                    "recipe `{id}`: install request failed (is the daemon running?): {e}"
                ))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            return Err(Error::Execution(format!(
                "recipe `{id}`: daemon rejected install ({status}): {detail}"
            )));
        }

        // 2. Poll progress to a terminal state. The map is `corpus_id ->
        //    IngestProgress`, externally tagged: `{"Complete": {...}}` /
        //    `{"Failed": {...}}` / `{"Embedding": {...}}` etc.
        let started = Instant::now();
        let mut saw_entry = false;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let snap: serde_json::Value = match self
                .client
                .get(format!("{}/internal/corpus/progress", self.base))
                .send()
                .await
            {
                Ok(r) => r.json().await.unwrap_or_else(|_| serde_json::json!({})),
                Err(_) => serde_json::json!({}),
            };
            let entry = snap.get("progress").and_then(|p| p.get(id));
            if let Some(e) = entry {
                saw_entry = true;
                if e.get("Complete").is_some() {
                    return Ok(InstallOutcome {
                        corpus_id: id.to_string(),
                        status: "complete".into(),
                    });
                }
                if e.get("Failed").is_some() {
                    return Err(Error::Execution(format!(
                        "recipe `{id}`: install failed: {e}"
                    )));
                }
                // else: in progress — keep polling.
            } else if saw_entry {
                // Entry was present, now gone → finished + cleared.
                return Ok(InstallOutcome {
                    corpus_id: id.to_string(),
                    status: "complete".into(),
                });
            } else if started.elapsed() > Duration::from_secs(15) {
                // Install accepted but no progress ever appeared → nothing was
                // queued → the corpus was already installed and fresh.
                return Ok(InstallOutcome {
                    corpus_id: id.to_string(),
                    status: "already_installed".into(),
                });
            }
            if started.elapsed() > self.poll_timeout {
                // Bound the wait; the install continues in the background.
                return Ok(InstallOutcome {
                    corpus_id: id.to_string(),
                    status: "installing".into(),
                });
            }
        }
    }
}
