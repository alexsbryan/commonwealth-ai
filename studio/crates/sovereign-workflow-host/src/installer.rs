// SPDX-License-Identifier: AGPL-3.0-or-later
//! `HttpCorpusInstaller` — the concrete [`CorpusInstaller`] a `recipe:` step uses.
//!
//! It does NOT reimplement ingest or touch the mesh handler: it POSTs to a
//! daemon install endpoint and then polls a progress endpoint to completion
//! so a downstream workflow step can consume the corpus. Two wire targets
//! are supported behind one poll state machine:
//!
//! * [`HttpCorpusInstaller::new`] — the daemon's internal `/internal/corpus/*`
//!   routes on loopback :9742 (the same endpoint `sovereign corpus install`
//!   and the desktop use). Fully mesh-coordinated; loopback-only. This is the
//!   v0.3 / same-box path.
//! * [`HttpCorpusInstaller::from_manifest`] — the OICP v0.4 §5 ingest
//!   endpoints advertised in a host's `knowledge.ingest`
//!   (`/oicp/v1/corpus/{install,progress}` on the client port :9741), with a
//!   bearer token for non-loopback hosts. This is the path a package running
//!   against a *remote* OICP daemon uses.
//!
//! Both share the §5.3 poll state machine: a non-terminal entry = in
//! progress; `Complete`/`Failed` = terminal; observed-then-gone = complete;
//! never-appears-within-15s = already installed.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::KnowledgeManifest;
use sovereign_contracts::traits::{CorpusInstaller, InstallOutcome};

/// The daemon's internal port on loopback — same target `corpus install` uses
/// (`sovereign-cli-llm/.../inventory.rs`).
const INSTALL_BASE: &str = "http://127.0.0.1:9742";

/// Which progress wire the installer polls.
#[derive(Clone, Copy)]
enum ProgressWire {
    /// Internal `/internal/corpus/progress`: the externally-tagged
    /// `corpus_engine::IngestProgress` map (`{"Complete": {...}}`, etc.).
    Internal,
    /// OICP v0.4 §5.2 `CorpusProgressResponse`: entries carry a snake_case
    /// `phase` field (`"complete"`, `"failed"`, ...).
    Oicp,
}

pub struct HttpCorpusInstaller {
    install_url: String,
    progress_url: String,
    /// Bearer token for non-loopback OICP hosts; `None` on the loopback
    /// internal path (which is unauthenticated).
    bearer: Option<String>,
    wire: ProgressWire,
    client: reqwest::Client,
    /// Hard cap on the poll before we return "installing" (the install still
    /// runs in the background; this just bounds the step's wait).
    poll_timeout: Duration,
}

impl Default for HttpCorpusInstaller {
    fn default() -> Self {
        Self::new()
    }
}

/// A short-timeout HTTP client shared by both constructors.
fn default_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Join an OICP manifest endpoint (origin-relative like `/oicp/v1/...`, or an
/// absolute URL) onto the manifest's origin.
fn join_url(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    let base = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

impl HttpCorpusInstaller {
    /// v0.3 / loopback path: the daemon's internal ingest routes on :9742.
    pub fn new() -> Self {
        Self {
            install_url: format!("{INSTALL_BASE}/internal/corpus/install"),
            progress_url: format!("{INSTALL_BASE}/internal/corpus/progress"),
            bearer: None,
            wire: ProgressWire::Internal,
            client: default_client(),
            poll_timeout: Duration::from_secs(30 * 60),
        }
    }

    /// OICP v0.4 §5 path: install against the ingest endpoints a host
    /// advertises in its `knowledge.ingest`, resolved relative to
    /// `base_url` (the manifest origin, e.g. `http://peer:9741`).
    ///
    /// Returns `None` when the host does not advertise an ingest surface —
    /// the caller should fall back to [`HttpCorpusInstaller::new`] (loopback)
    /// or treat the corpus as uninstallable on that host.
    pub fn from_manifest(
        base_url: &str,
        knowledge: &KnowledgeManifest,
        bearer: Option<String>,
    ) -> Option<Self> {
        let ingest = knowledge.ingest.as_ref()?;
        Some(Self {
            install_url: join_url(base_url, &ingest.install_endpoint),
            progress_url: join_url(base_url, &ingest.progress_endpoint),
            bearer,
            wire: ProgressWire::Oicp,
            client: default_client(),
            poll_timeout: Duration::from_secs(30 * 60),
        })
    }

    /// Fetch the progress snapshot as raw JSON (best-effort — an empty object
    /// on any transport error, so the caller's poll loop keeps going).
    async fn progress_snapshot(&self) -> serde_json::Value {
        let mut req = self.client.get(&self.progress_url);
        if let Some(tok) = &self.bearer {
            req = req.bearer_auth(tok);
        }
        match req.send().await {
            Ok(r) => r.json().await.unwrap_or_else(|_| serde_json::json!({})),
            Err(_) => serde_json::json!({}),
        }
    }

    /// Classify the snapshot's entry for `id` under the active wire format.
    /// `None` = no entry; `Some(Ok(true))` = terminal complete;
    /// `Some(Err(msg))` = terminal failure; `Some(Ok(false))` = in progress.
    fn classify(
        &self,
        snap: &serde_json::Value,
        id: &str,
    ) -> Option<std::result::Result<bool, String>> {
        let entry = snap.get("progress").and_then(|p| p.get(id))?;
        match self.wire {
            ProgressWire::Internal => {
                if entry.get("Complete").is_some() {
                    Some(Ok(true))
                } else if entry.get("Failed").is_some() {
                    Some(Err(format!("{entry}")))
                } else {
                    Some(Ok(false))
                }
            }
            ProgressWire::Oicp => match entry.get("phase").and_then(|p| p.as_str()) {
                Some("complete") => Some(Ok(true)),
                Some("failed") => Some(Err(entry
                    .get("detail")
                    .and_then(|d| d.as_str())
                    .unwrap_or("ingest failed")
                    .to_string())),
                _ => Some(Ok(false)),
            },
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
        //    mesh-coordinated task). Same body shape on both wires:
        //    `{corpus_id, parameters}`.
        let body = serde_json::json!({ "corpus_id": id, "parameters": params });
        let mut req = self.client.post(&self.install_url).json(&body);
        if let Some(tok) = &self.bearer {
            req = req.bearer_auth(tok);
        }
        let resp = req.send().await.map_err(|e| {
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

        // 2. Poll progress to a terminal state per the §5.3 state machine.
        let started = Instant::now();
        let mut saw_entry = false;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let snap = self.progress_snapshot().await;
            match self.classify(&snap, id) {
                Some(Ok(true)) => {
                    return Ok(InstallOutcome {
                        corpus_id: id.to_string(),
                        status: "complete".into(),
                    });
                }
                Some(Err(detail)) => {
                    return Err(Error::Execution(format!(
                        "recipe `{id}`: install failed: {detail}"
                    )));
                }
                Some(Ok(false)) => {
                    // Present + in progress — keep polling.
                    saw_entry = true;
                }
                None if saw_entry => {
                    // Entry was present, now gone → finished + cleared.
                    return Ok(InstallOutcome {
                        corpus_id: id.to_string(),
                        status: "complete".into(),
                    });
                }
                None if started.elapsed() > Duration::from_secs(15) => {
                    // Install accepted but no progress ever appeared → nothing
                    // was queued → the corpus was already installed and fresh.
                    return Ok(InstallOutcome {
                        corpus_id: id.to_string(),
                        status: "already_installed".into(),
                    });
                }
                None => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::oicp::{EmbedModelInfo, IngestEndpoints};

    fn manifest_with_ingest() -> KnowledgeManifest {
        KnowledgeManifest {
            corpora: Vec::new(),
            search_endpoint: "/v1/knowledge/search".into(),
            embed_model: None::<EmbedModelInfo>,
            ingest: Some(IngestEndpoints {
                install_endpoint: "/oicp/v1/corpus/install".into(),
                progress_endpoint: "/oicp/v1/corpus/progress".into(),
                test_endpoint: Some("/oicp/v1/recipe/test".into()),
            }),
        }
    }

    #[test]
    fn from_manifest_resolves_relative_endpoints() {
        let inst =
            HttpCorpusInstaller::from_manifest("http://peer:9741/", &manifest_with_ingest(), None)
                .expect("ingest advertised");
        assert_eq!(inst.install_url, "http://peer:9741/oicp/v1/corpus/install");
        assert_eq!(
            inst.progress_url,
            "http://peer:9741/oicp/v1/corpus/progress"
        );
    }

    #[test]
    fn from_manifest_none_when_no_ingest_advertised() {
        let mut km = manifest_with_ingest();
        km.ingest = None;
        assert!(HttpCorpusInstaller::from_manifest("http://peer:9741", &km, None).is_none());
    }

    #[test]
    fn oicp_wire_reads_snake_case_phase() {
        let inst =
            HttpCorpusInstaller::from_manifest("http://peer:9741", &manifest_with_ingest(), None)
                .unwrap();
        let done = serde_json::json!({"progress": {"c": {"phase": "complete"}}});
        assert_eq!(inst.classify(&done, "c"), Some(Ok(true)));
        let failed = serde_json::json!({"progress": {"c": {"phase": "failed", "detail": "boom"}}});
        assert_eq!(inst.classify(&failed, "c"), Some(Err("boom".to_string())));
        let running = serde_json::json!({"progress": {"c": {"phase": "embedding"}}});
        assert_eq!(inst.classify(&running, "c"), Some(Ok(false)));
        assert_eq!(inst.classify(&running, "other"), None);
    }

    #[test]
    fn internal_wire_reads_externally_tagged() {
        let inst = HttpCorpusInstaller::new();
        let done = serde_json::json!({"progress": {"c": {"Complete": {"total_chunks": 1}}}});
        assert_eq!(inst.classify(&done, "c"), Some(Ok(true)));
        let running = serde_json::json!({"progress": {"c": {"Embedding": {}}}});
        assert_eq!(inst.classify(&running, "c"), Some(Ok(false)));
    }
}
