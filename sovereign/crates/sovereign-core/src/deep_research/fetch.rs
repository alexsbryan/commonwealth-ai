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
///
/// Dedup (order deep-research-t1d fix 1): `already_fetched` carries the
/// URLs fetched by prior rounds (the run's `fetched_sources`). An
/// admitted hit whose URL was already fetched — in a prior round or
/// earlier in THIS round — is refused: no decider call (refusals spend
/// no budget), no port call, and the URL recorded on the window's
/// `dedup_refused`. Refusals are not fetch failures: the source was
/// acquired once and is never re-fetched (the merged window already
/// dedups chunks by URL — first wins — so the evidence is untouched;
/// only the re-fetch and its spend are refused).
pub async fn fetch_round(
    port: &dyn ResearchPort,
    decider: &mut SpendDecider,
    run_id: &str,
    charter_hash: &str,
    round: u32,
    fetch_list: &FetchList,
    hits: &[SearchHit],
    already_fetched: &[String],
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
            dedup_refused: Vec::new(),
            derived_custody: Custody::PublicWeb.as_str().to_string(),
        });
    }

    let mut chunks: Vec<WindowChunk> = Vec::new();
    let mut failures: Vec<FetchFailure> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    // The dedup set: prior rounds' URLs plus this round's as it fills.
    let mut fetched: Vec<String> = already_fetched.to_vec();
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
        // Dedup gate: an already-fetched URL is refused — no decider
        // call (spends nothing), no port call, recorded on the window.
        if fetched.iter().any(|u| u == &hit.url) {
            if !refused.contains(&hit.url) {
                refused.push(hit.url.clone());
            }
            continue;
        }
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
        fetched.push(hit.url.clone());
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
        dedup_refused: refused,
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
    use crate::deep_research::estate::{AlignmentDecision, EstateListing, PortHit};
    use crate::deep_research::icd::{Plan, TriageOutcome, ICD_VERSION};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// A counting mock port: records every web_fetch call per URL.
    /// A refused fetch must never reach the port.
    struct CountingPort {
        calls: Arc<Mutex<Vec<String>>>,
        bodies: HashMap<String, String>,
    }

    #[async_trait::async_trait]
    impl ResearchPort for CountingPort {
        async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
            unimplemented!("unreachable: fetch_round calls only terminal_poll + web_fetch")
        }
        async fn estate_search(
            &self,
            _c: &[String],
            _q: &str,
            _l: usize,
        ) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_search(&self, _b: &str, _q: &str, _l: usize) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_fetch(&self, url: &str) -> Result<String, String> {
            self.calls.lock().unwrap().push(url.to_string());
            Ok(self.bodies.get(url).cloned().unwrap_or_default())
        }
        async fn terminal_poll(&self) -> Result<(), String> {
            Ok(())
        }
        async fn draft(&self, _p: &str, _s: Option<&str>, _u: &[String]) -> Result<String, String> {
            unimplemented!("unreachable")
        }
        async fn alignment_decision(
            &self,
            _p: &Plan,
            _r: &Path,
        ) -> Result<AlignmentDecision, String> {
            Ok(AlignmentDecision::Proceed)
        }
    }

    fn fetch_list_admitting(ids: &[&str]) -> FetchList {
        FetchList {
            icd: "fetch_list".to_string(),
            version: ICD_VERSION,
            run_id: "r-dedup".to_string(),
            charter_hash: "h".to_string(),
            round: 2,
            queries: Vec::new(),
            search_hits: Vec::new(),
            triage: TriageOutcome {
                code_set_k: ids.iter().map(|s| s.to_string()).collect(),
                eps_admits: Vec::new(),
                below_cut: Vec::new(),
                threshold: 0.0,
                eps_quota: 0.0,
                admission_rule: crate::deep_research::acquisition::ADMISSION_RULE_SCORE_THEN_FIGURE
                    .to_string(),
            },
        }
    }

    fn hit(id: &str, url: &str) -> SearchHit {
        SearchHit {
            id: id.to_string(),
            query_id: format!("q-{id}"),
            url: url.to_string(),
            title: "t".to_string(),
            snippet: String::new(),
            engine: "mock".to_string(),
            score: 1.0,
        }
    }

    /// RED (order deep-research-t1d fix 1, declared in
    /// pre-registration.md): "a round-2 fetch of an already-fetched URL
    /// is refused". Failed at HEAD — the round's fetch list admitted the
    /// same URL twice (the t1c-observed shape: two gaps, two queries,
    /// both matching the same exemplar hit) and fetch_round fetched it
    /// twice: the port was called for both admissions, two chunks landed
    /// in the window, and the round's budget paid for both. (Watched
    /// red: pass 0 fail 1 at HEAD, 2026-08-14.)
    ///
    /// Now green: within a round the second admission of the same URL
    /// is refused (recorded on `dedup_refused`, no decider call, no
    /// port call); across rounds a round-2 fetch list re-admitting a
    /// round-1 URL is refused the same way. Refusals are not failures —
    /// `fetch_failures` stays empty, `dedup_refused` carries the record.
    #[test]
    fn already_fetched_url_is_refused() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let url = "https://example.com/dup".to_string();
            let fresh_port = || CountingPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                bodies: HashMap::from([(url.clone(), "the body".to_string())]),
            };
            let tmp = tempfile::tempdir().unwrap();
            let make_decider = || {
                SpendDecider::new(
                    "r-dedup",
                    "h",
                    HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 4u32)]),
                    &tmp.path().join("budget-ledger.json"),
                )
                .unwrap()
            };
            let fetch_list = fetch_list_admitting(&["h1", "h2"]);
            let hits = vec![hit("h1", &url), hit("h2", &url)];

            // (a) WITHIN a round: the same URL admitted twice fetches once.
            let port = fresh_port();
            let mut decider = make_decider();
            let window = fetch_round(
                &port,
                &mut decider,
                "r-dedup",
                "h",
                2,
                &fetch_list,
                &hits,
                &[],
                1234,
            )
            .await
            .unwrap();
            let calls = port
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|u| **u == url)
                .count();
            assert_eq!(
                calls, 1,
                "the second fetch of an already-fetched URL must be refused"
            );
            assert_eq!(window.chunks.len(), 1, "one chunk for one fetch");
            assert_eq!(window.dedup_refused, vec![url.clone()]);
            assert!(window.fetch_failures.is_empty());

            // (b) ACROSS rounds: a round-2 fetch list re-admitting a
            // round-1 URL is refused before the port is called.
            let port = fresh_port();
            let mut decider = make_decider();
            let first = fetch_round(
                &port,
                &mut decider,
                "r-dedup",
                "h",
                1,
                &fetch_list,
                &hits,
                &[],
                1000,
            )
            .await
            .unwrap();
            assert_eq!(first.chunks.len(), 1, "round 1 fetched once");
            let round2 = fetch_round(
                &port,
                &mut decider,
                "r-dedup",
                "h",
                2,
                &fetch_list,
                &hits,
                &[url.clone()],
                2000,
            )
            .await
            .unwrap();
            assert!(
                round2.chunks.is_empty(),
                "round 2 must not re-fetch an already-fetched URL"
            );
            assert_eq!(round2.dedup_refused, vec![url.clone()]);
            let calls = port.calls.lock().unwrap().len();
            assert_eq!(calls, 1, "round 2 never reached the port");
        });
    }

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
            dedup_refused: Vec::new(),
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
