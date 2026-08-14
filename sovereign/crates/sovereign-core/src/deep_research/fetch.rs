// SPDX-License-Identifier: AGPL-3.0-or-later
//! R6 — the gated fetch + the custody-stamped evidence window.
//!
//! Every fetch attempt goes through the ONE run-scoped decider
//! (`SpendDecider::allow`, fail-closed) — the port's `web_fetch` is
//! never called without an `Allow`. Custody is stamped by this code,
//! never a model: web-fetched content is `public-web` with the source
//! URL; the derived custody is the max-restrictiveness join computed at
//! window creation (R-7). Fetch failures are recorded absent per-source
//! (F17); the terminal-state poll gates the whole leg — an unreachable
//! terminal records every planned fetch as absent and spends nothing.

use super::acquisition::admitted_ids;
use super::budget::{SpendDecider, FAMILY_WEB_FETCH, KEY_FETCH_PAGES};
use super::estate::ResearchPort;
use super::icd::{EvidenceWindow, FetchFailure, FetchList, SearchHit, WindowChunk};
use crate::types::{join_custody, Custody};

/// The per-chunk content cap — a fetched page larger than this is
/// truncated with a visible marker (glassbox: truncation is declared,
/// never silent).
pub const CHUNK_CONTENT_CAP: usize = 12_000;

/// The visible truncation marker appended to over-cap content.
pub const TRUNCATION_MARKER: &str = "\n\n[truncated: content exceeds the evidence chunk cap]";

/// Cap a fetched body at the chunk cap, declaring the truncation.
pub fn cap_content(body: &str) -> String {
    if body.chars().count() > CHUNK_CONTENT_CAP {
        let mut trimmed: String = body.chars().take(CHUNK_CONTENT_CAP).collect();
        trimmed.push_str(TRUNCATION_MARKER);
        trimmed
    } else {
        body.to_string()
    }
}

/// Fetch the round's admitted hits into the evidence window. `at_unix`
/// is the run's round timestamp (journaled into the budget ledger).
/// Returns the raw window (pre-tags); enrichment (R7) runs after.
pub async fn fetch_round(
    port: &dyn ResearchPort,
    decider: &mut SpendDecider,
    run_id: &str,
    charter_hash: &str,
    round: u32,
    fetch_list: &FetchList,
    hits: &[SearchHit],
    at_unix: i64,
) -> Result<EvidenceWindow, String> {
    // F17 terminal-state poll first: an unreachable terminal means the
    // leg spends nothing and every planned fetch is recorded absent.
    if let Err(e) = port.terminal_poll().await {
        let failures: Vec<FetchFailure> = admitted_ids(fetch_list)
            .iter()
            .map(|id| FetchFailure {
                url: hits
                    .iter()
                    .find(|h| &h.id == id)
                    .map(|h| h.url.clone())
                    .unwrap_or_else(|| id.clone()),
                error: format!("terminal-poll-failed: {e}"),
                absent: true,
            })
            .collect();
        return Ok(EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: super::icd::ICD_VERSION,
            run_id: run_id.to_string(),
            charter_hash: charter_hash.to_string(),
            round,
            chunks: Vec::new(),
            fetch_failures: failures,
            derived_custody: Custody::PublicWeb.as_str().to_string(),
        });
    }

    let mut chunks: Vec<WindowChunk> = Vec::new();
    let mut failures: Vec<FetchFailure> = Vec::new();
    let mut index = 0usize;
    for id in admitted_ids(fetch_list) {
        let Some(hit) = hits.iter().find(|h| h.id == id) else {
            failures.push(FetchFailure {
                url: id.clone(),
                error: "hit-missing-from-round".to_string(),
                absent: true,
            });
            continue;
        };
        // The ONE decider gate — no Allow, no fetch.
        let verdict = decider
            .allow(FAMILY_WEB_FETCH, KEY_FETCH_PAGES, 1, at_unix)
            .await?;
        if !verdict.allowed() {
            failures.push(FetchFailure {
                url: hit.url.clone(),
                error: "budget-refused".to_string(),
                absent: true,
            });
            continue;
        }
        let body = match port.web_fetch(&hit.url).await {
            Ok(b) => b,
            Err(e) => {
                failures.push(FetchFailure {
                    url: hit.url.clone(),
                    error: e,
                    absent: true,
                });
                continue;
            }
        };
        // Custody stamped HERE, by code: public-web, source URL kept.
        index += 1;
        chunks.push(WindowChunk {
            id: format!("ev-{index}"),
            locator: hit.url.clone(),
            source_url: hit.url.clone(),
            custody: Custody::PublicWeb.as_str().to_string(),
            provenance_class: "known".to_string(),
            content: cap_content(&body),
            ingested_into: None,
            tags: Vec::new(),
        });
    }

    Ok(EvidenceWindow {
        icd: "evidence_window".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        chunks,
        fetch_failures: failures,
        derived_custody: Custody::PublicWeb.as_str().to_string(),
    })
}

/// The derived-custody join over a chunk set (R-7): max-restrictiveness
/// (personal > peer > public-web), unknown poisons. Computed at window
/// creation — the audit refuses per-claim on unknown chunks.
pub fn derive_custody(chunks: &[WindowChunk]) -> String {
    let custodies: Vec<Custody> = chunks
        .iter()
        .map(|c| Custody::parse_wire(&c.custody).unwrap_or(Custody::Unknown))
        .collect();
    if custodies.is_empty() {
        return Custody::PublicWeb.as_str().to_string();
    }
    join_custody(&custodies).as_str().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_visible() {
        let body = "x".repeat(CHUNK_CONTENT_CAP + 100);
        let content = cap_content(&body);
        assert!(content.contains("[truncated"));
        assert_eq!(
            content.chars().count(),
            CHUNK_CONTENT_CAP + TRUNCATION_MARKER.chars().count()
        );
        // Under-cap content passes through untouched.
        assert_eq!(cap_content("small"), "small");
    }

    #[test]
    fn custody_join_max_restrictiveness() {
        let mk = |custody: &str| WindowChunk {
            id: "c".to_string(),
            locator: "https://example.com".to_string(),
            source_url: "https://example.com".to_string(),
            custody: custody.to_string(),
            provenance_class: "known".to_string(),
            content: "x".to_string(),
            ingested_into: None,
            tags: Vec::new(),
        };
        let window = EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "r".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            chunks: vec![mk("public-web"), mk("peer")],
            fetch_failures: Vec::new(),
            derived_custody: String::new(),
        };
        assert_eq!(derive_custody(&window.chunks), "peer");
        let window = EvidenceWindow {
            chunks: vec![mk("public-web"), mk("unknown")],
            ..window
        };
        assert_eq!(derive_custody(&window.chunks), "unknown");
        let window = EvidenceWindow {
            chunks: vec![mk("public-web"), mk("personal")],
            ..window
        };
        assert_eq!(derive_custody(&window.chunks), "personal");
    }
}
