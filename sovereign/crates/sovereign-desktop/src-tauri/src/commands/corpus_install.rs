// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::state::{self, AppState, DesktopConfig};

#[tauri::command]
pub async fn diagnose_corpus(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    drop(engine_guard);
    Ok(engine.diagnose_indexes().await)
}

/// Kick off a corpus install via the daemon's unified install
/// endpoint. The daemon is the single owner of ingest lifecycle —
/// Desktop is a thin client that says "start" and then watches. The
/// continuous `spawn_corpus_status_poller` (started at backend
/// bootstrap) emits `corpus-progress` events for whichever ingests
/// the daemon is running, whether Desktop initiated them or a prior
/// session's auto-collaborate loop did.
///
/// Clicking Install a second time while the daemon is already
/// ingesting this corpus is a no-op: `/internal/corpus/install` is
/// idempotent and returns `spawned: false`.
#[tauri::command]
pub async fn install_corpus(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let daemon = state.internal_base_url();
    let install_url = format!("{daemon}/internal/corpus/install");
    let resp = client
        .post(&install_url)
        .json(&serde_json::json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/install: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/install returned {status}: {body}"
        ));
    }

    // Flip the UI state immediately so the Install button reacts
    // before the next status-poller tick lands. The poller (running
    // in the background) will overwrite this stub payload with real
    // progress on its very next pass.
    let initial = CorpusProgressPayload {
        corpus_id: corpus_id.clone(),
        phase: "downloading".into(),
        percent: 0.0,
        chunks_processed: 0,
        message: Some("Starting…".into()),
    };
    if let Ok(mut map) = state.install_progress.try_write() {
        map.insert(corpus_id.clone(), initial.clone());
    }
    let _ = app_handle.emit("corpus-progress", initial);

    Ok(())
}

/// Tauri command: expand an installed corpus by relaxing its filter
/// scope (e.g. promote Wikipedia from Core to Full). Returns
/// immediately; progress streams on the existing `corpus-progress`
/// event channel — same surface as `install_corpus` so the
/// `CorpusProgressBanner` and `KnowledgeStatus` row light up
/// automatically.
#[tauri::command]
pub async fn lc_expand_corpus(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/corpus/expand");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/expand: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/expand returned {status}: {body}"
        ));
    }

    // Mirror install_corpus's optimistic flip: surface a "downloading"
    // stub so the UI reacts before the next status poll lands.
    let initial = CorpusProgressPayload {
        corpus_id: corpus_id.clone(),
        phase: "extracting".into(),
        percent: 0.0,
        chunks_processed: 0,
        message: Some("Expanding scope…".into()),
    };
    if let Ok(mut map) = state.install_progress.try_write() {
        map.insert(corpus_id.clone(), initial.clone());
    }
    let _ = app_handle.emit("corpus-progress", initial);

    Ok(())
}

/// Tauri command: ask the daemon whether `corpus_id` can be expanded
/// (i.e. has an active filter scope with `expandable=true` in
/// `_corpus_meta.json`). Returns `false` if the corpus isn't installed
/// or has no filter, `true` if a relaxed scope would add documents.
///
/// Reads `_corpus_meta.json` directly from the per-corpus index dir
/// rather than going through the daemon — the file is local and
/// avoiding the round-trip keeps the Settings render snappy.
#[tauri::command]
pub async fn lc_can_expand(corpus_id: String) -> Result<bool, String> {
    let mut path = match dirs::home_dir() {
        Some(h) => h,
        None => return Ok(false),
    };
    path.push(".sovereign");
    path.push("indexes");
    path.push(format!("{corpus_id}-canonical"));
    path.push("_corpus_meta.json");
    if !path.exists() {
        // Try the partition-of-self variant.
        path.pop();
        path.pop();
        path.push(format!("{corpus_id}-local"));
        path.push("_corpus_meta.json");
        if !path.exists() {
            return Ok(false);
        }
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };
    // Probe-only deserialize — we only care about the `scope` block.
    #[derive(serde::Deserialize)]
    struct ScopeProbe {
        #[serde(default)]
        scope: Option<ScopeBody>,
    }
    #[derive(serde::Deserialize)]
    struct ScopeBody {
        #[serde(default)]
        expandable: bool,
    }
    let probe: ScopeProbe = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    Ok(probe.scope.map(|s| s.expandable).unwrap_or(false))
}

/// Tauri command: read `/internal/enrichment/status?corpus_id=…` and
/// surface the generic per-corpus enrichment progress (phase,
/// fraction-complete, message, error) to any UI component that
/// renders a corpus card. Works for every pipeline that writes
/// `_enrichment_state.json` — watched folders, structural atlas
/// post-install, conversation RAPTOR, future pipelines.
#[tauri::command]
pub async fn lc_enrichment_status(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/enrichment/status");
    let resp = client
        .get(&url)
        .query(&[("corpus_id", corpus_id.as_str())])
        .send()
        .await
        .map_err(|e| format!("GET /internal/enrichment/status: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/enrichment/status returned {status}: {body}"
        ));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("parse enrichment status body: {e}"))
}

/// Tauri command: ask the daemon to fire one watcher tick now,
/// bypassing the 24h interval. Powers the "Run tick now" affordance
/// under the Newsworthy chip — the only path operators have to
/// recover from a stale snapshot or kick off the first portal ingest
/// after this node becomes leader.
#[tauri::command]
pub async fn lc_newsworthy_tick(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/newsworthy/tick");
    let resp = client
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("POST /internal/newsworthy/tick: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/newsworthy/tick returned {status}: {body}"
        ));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("parse newsworthy tick body: {e}"))
}

/// Tauri command: read `/internal/newsworthy/status` and surface it
/// to the desktop. Powers the per-layer "watcher status" line under
/// the Newsworthy chip in Settings → Knowledge — gives the user the
/// glassbox view the layered-corpus UI was missing (role, last tick,
/// tracked total, current leader). Returns the parsed JSON body as a
/// `serde_json::Value` so the Svelte side can iterate the shape
/// without a duplicated TypeScript schema; the backend route is the
/// source of truth.
#[tauri::command]
pub async fn lc_newsworthy_status(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/newsworthy/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /internal/newsworthy/status: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/newsworthy/status returned {status}: {body}"
        ));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("parse newsworthy status body: {e}"))
}

/// Tauri command: install the default Wikipedia corpus — `wikipedia`
/// Core (curated Vital Articles L5). Runs via the existing
/// `/internal/corpus/install` daemon endpoint, so progress streams on
/// the unchanged `corpus-progress` event channel.
///
/// Was a two-layer install (`wikipedia-simple` Layer 0 + Core); Simple
/// is now parked in the desktop "Coming soon" bucket until it ships as
/// a proper HF subset of Core, so install is Core-only. Newsworthy and
/// Catalog are opt-in add-on toggles, not part of the default install.
/// (Name kept for call-site stability; it's the "install Wikipedia" entry.)
#[tauri::command]
pub async fn lc_start_layered_setup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<String>, String> {
    install_corpus(app_handle, state, "wikipedia".into()).await?;
    Ok(vec!["wikipedia".into()])
}

/// Spawn the background poller that reads
/// `/internal/corpus/status` every second and forwards every active
/// entry to the `corpus-progress` Tauri event channel.
///
/// Starts at backend bootstrap and runs for the life of the process.
/// Without this the UI only sees ingests Desktop itself kicked off —
/// a daemon-driven resume after a crash/close would run invisibly and
/// the user would still see the "Install" button for a corpus the
/// daemon is actively ingesting (the bug we're fixing).
///
/// Emits a terminal `complete` event for corpora that disappear from
/// the snapshot after a grace window, so the Svelte `installing`
/// state flips back to `installed` without waiting for the next
/// `list_corpora` refresh.
pub fn spawn_corpus_status_poller(app_handle: tauri::AppHandle, state: Arc<AppState>) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "corpus status poller: failed to build HTTP client — poller disabled"
                );
                return;
            }
        };
        let daemon = state.internal_base_url();
        let url = format!("{daemon}/internal/corpus/status");
        // Track what was seen last tick so we can detect terminations
        // (corpus disappeared from the snapshot → emit complete).
        let mut last_seen: std::collections::HashSet<String> = Default::default();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(_) => continue, // Daemon may be restarting; retry next tick.
            };
            let snapshot: CorpusStatusResponse = match resp.json().await {
                Ok(b) => b,
                Err(_) => continue,
            };

            let current: std::collections::HashSet<String> = snapshot
                .entries
                .iter()
                .map(|e| e.corpus_id.clone())
                .collect();

            // Emit terminal "complete" for entries that were active last
            // tick but have dropped off this tick.
            for gone in last_seen.difference(&current) {
                let last_phase = state
                    .install_progress
                    .read()
                    .await
                    .get(gone)
                    .map(|p| p.phase.clone())
                    .unwrap_or_default();
                if last_phase != "complete" && last_phase != "failed" {
                    let final_payload = CorpusProgressPayload {
                        corpus_id: gone.clone(),
                        phase: "complete".into(),
                        percent: 100.0,
                        chunks_processed: 0,
                        message: Some("Done".into()),
                    };
                    if let Ok(mut map) = state.install_progress.try_write() {
                        map.insert(gone.clone(), final_payload.clone());
                    }
                    let _ = app_handle.emit("corpus-progress", final_payload);
                }
            }
            last_seen = current;

            for entry in &snapshot.entries {
                let payload = status_entry_to_payload(entry);
                if let Ok(mut map) = state.install_progress.try_write() {
                    map.insert(entry.corpus_id.clone(), payload.clone());
                }
                let _ = app_handle.emit("corpus-progress", payload);
            }
        }
    });
}

/// Convert a `CorpusStatusEntry` from the daemon into the
/// frontend-shaped `CorpusProgressPayload`. Prefers the
/// daemon-computed `estimated_fraction` for the percent; falls back
/// to a sensible phase + message when no progress event is known yet.
fn status_entry_to_payload(entry: &CorpusStatusEntry) -> CorpusProgressPayload {
    use corpus_engine::IngestProgress as P;

    // Shard-scan progress is the primary signal. For filtered ingests
    // (Wikipedia Core, etc.) the iterator must scan the entire source
    // ZIP, so shards-completed/shards-total tracks wall-clock honestly
    // even when most articles are rejected. An earlier revision tried
    // `docs_processed / expected_docs` as a "filter-aware" percent —
    // wrong, because docs are *sections* (~10× the accepted article
    // count for `wikipedia_jsonl`) while expected_docs is the title
    // count, so the ratio hit 100% with hours of work still ahead.
    // The "X / Y articles" string in the message line below carries
    // the filter-scope context without conflating units in the bar.
    let percent = entry
        .estimated_fraction
        .map(|f| (f * 100.0).clamp(0.0, 100.0))
        .unwrap_or(0.0);

    let (phase, chunks_processed, message) = match entry.progress.as_ref() {
        Some(P::Downloading {
            percent: dp,
            bytes_downloaded,
            bytes_total,
        }) => {
            let msg = bytes_total
                .map(|t| {
                    format!(
                        "{:.0} / {:.0} MB ({:.0}%)",
                        *bytes_downloaded as f64 / 1_048_576.0,
                        t as f64 / 1_048_576.0,
                        dp,
                    )
                })
                .unwrap_or_else(|| format!("{:.0} MB", *bytes_downloaded as f64 / 1_048_576.0));
            ("downloading".to_string(), 0u64, Some(msg))
        }
        Some(P::Extracting {
            documents_processed,
        }) => (
            "extracting".to_string(),
            0,
            Some(format!("{} articles", documents_processed)),
        ),
        Some(P::Chunking { chunks_created }) => (
            "chunking".to_string(),
            *chunks_created,
            Some(format!("{} chunks", chunks_created)),
        ),
        Some(P::Embedding {
            chunks_embedded,
            docs_processed,
            chunks_per_sec,
            expected_docs,
            ..
        }) => (
            "embedding".to_string(),
            *chunks_embedded,
            Some(format_embed_message(
                *chunks_embedded,
                *docs_processed,
                *chunks_per_sec,
                *expected_docs,
            )),
        ),
        Some(P::Indexing { chunks_indexed, .. }) => (
            "indexing".to_string(),
            *chunks_indexed,
            Some(format!("{} chunks indexed", pretty_count(*chunks_indexed))),
        ),
        Some(P::OptimizingIndex { current_chunks }) => (
            "optimizing_index".to_string(),
            *current_chunks,
            Some(format!(
                "Retraining vector index over {} chunks",
                pretty_count(*current_chunks)
            )),
        ),
        Some(P::Complete {
            total_chunks,
            duration_secs,
        }) => (
            "complete".to_string(),
            *total_chunks,
            Some(format!("Done in {duration_secs}s")),
        ),
        Some(P::Enriching { phase, detail, .. }) => {
            (format!("enriching_{phase}"), 0, Some(detail.clone()))
        }
        None => {
            // No IngestProgress event yet this session — this is the
            // classic "daemon resumed after Desktop close" state. Use
            // on-disk counters so the user still sees "something is
            // happening" instead of a stuck spinner.
            let phase = if entry.canonical_in_progress || entry.partition_in_progress {
                "embedding"
            } else {
                "downloading"
            };
            let msg = if entry.committed_iter_pos > 0 {
                // When the sampler has published a total estimate we
                // prefer `M/N sections` over a raw running count —
                // it's the same info the progress bar encodes but
                // more legible at a glance on the details line.
                match entry.estimated_total_sections {
                    Some(total) if total > 0 => Some(format!(
                        "Resuming · {}/{} sections",
                        pretty_count(entry.committed_iter_pos),
                        pretty_count(total),
                    )),
                    _ => Some(format!(
                        "Resuming · {} sections committed",
                        pretty_count(entry.committed_iter_pos),
                    )),
                }
            } else {
                Some("Starting…".into())
            };
            (phase.to_string(), entry.committed_iter_pos, msg)
        }
    };

    CorpusProgressPayload {
        corpus_id: entry.corpus_id.clone(),
        phase,
        percent,
        chunks_processed,
        message,
    }
}

/// Format the embed-phase message line that both the live-event and
/// polling paths emit. Centralises the format so the two paths can't
/// drift, and threads the filter-derived denominator into the line
/// when known.
///
/// Unit nuance: for `wikipedia_jsonl` `docs_processed` counts emitted
/// `ExtractedDoc`s — i.e. sections, ~2.5× the article count — while
/// `expected_docs` from a `title_list` filter is the title count.
/// We clamp the displayed numerator to the expected count so the
/// "X / Y articles" reading matches the percent (also clamped) rather
/// than overshooting into "128k / 51k articles" near the end. The
/// displayed number is approximate at the per-article level but
/// communicates the right scale, which is what the operator needs.
pub(crate) fn format_embed_message(
    chunks_embedded: u64,
    docs_processed: u64,
    chunks_per_sec: f32,
    expected_docs: Option<u64>,
) -> String {
    match expected_docs {
        Some(total) if total > 0 => {
            let displayed = docs_processed.min(total);
            format!(
                "{} chunks · {} / {} articles · {:.0}/s",
                pretty_count(chunks_embedded),
                pretty_count(displayed),
                pretty_count(total),
                chunks_per_sec,
            )
        }
        _ => format!(
            "{} chunks · {} docs · {:.0}/s",
            pretty_count(chunks_embedded),
            pretty_count(docs_processed),
            chunks_per_sec,
        ),
    }
}

/// Compact count formatter for UI messages: 7_265_216 → "7.3M".
pub(crate) fn pretty_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Wire-level DTO for the daemon's `/internal/corpus/status`
/// response. Mirrors `commonwealth_api::routes_internal::CorpusStatusEntry`.
#[derive(Debug, serde::Deserialize)]
struct CorpusStatusResponse {
    entries: Vec<CorpusStatusEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct CorpusStatusEntry {
    corpus_id: String,
    #[allow(dead_code)]
    active: bool,
    progress: Option<corpus_engine::IngestProgress>,
    #[allow(dead_code)]
    shards_completed: usize,
    #[allow(dead_code)]
    shards_total: usize,
    committed_iter_pos: u64,
    #[allow(dead_code)]
    canonical_present: bool,
    #[allow(dead_code)]
    partition_present: bool,
    canonical_in_progress: bool,
    partition_in_progress: bool,
    estimated_fraction: Option<f32>,
    #[serde(default)]
    estimated_total_sections: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    estimated_total_articles: Option<u64>,
}

/// Remove (wipe) a corpus on this node via the daemon. Destructive —
/// deletes canonical + every partition-* sibling dir for `corpus_id`.
///
/// Replaces the old direct `engine.remove_index` call — that path
/// ignored in-flight ingest tasks and left `<corpus>-partition-*/`
/// dirs on disk. The daemon route handles both (signal cancel, await
/// task exit, wipe canonical + every partition sibling). The
/// `confirm_wipe: true` body field is the daemon-side guardrail
/// against accidental wipes; this command is the explicit "remove"
/// surface so it always passes it.
#[tauri::command]
pub async fn remove_corpus(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/corpus/cancel");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "corpus_id": corpus_id,
            "confirm_wipe": true,
        }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/cancel: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/cancel returned {status}: {body}"
        ));
    }

    // Clear any stale progress entry so the UI returns to "not_installed".
    if let Ok(mut map) = state.install_progress.try_write() {
        map.remove(&corpus_id);
    }
    Ok(())
}

/// Pause an in-progress corpus ingest on this node via the daemon.
/// Non-destructive — committed chunks and `_corpus_meta.json` are kept
/// so a subsequent `install_corpus` call resumes from the last flush.
///
/// This is what the UI's in-progress "Cancel" button calls. The
/// destructive variant lives behind the `Remove` action on installed
/// corpora and goes through `remove_corpus` above.
#[tauri::command]
pub async fn pause_corpus(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/corpus/pause");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/pause: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/pause returned {status}: {body}"
        ));
    }

    // Clear the in-memory progress entry so the UI immediately reflects
    // "stopped". On-disk state is intact — `install_corpus` resumes.
    if let Ok(mut map) = state.install_progress.try_write() {
        map.remove(&corpus_id);
    }
    Ok(())
}

#[tauri::command]
pub async fn get_corpus_progress(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<CorpusProgressPayload>, String> {
    let map = state.install_progress.read().await;
    Ok(map.get(&corpus_id).cloned())
}
