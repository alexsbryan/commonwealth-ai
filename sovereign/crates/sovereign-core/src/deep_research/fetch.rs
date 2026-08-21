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
                retries: 0,
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
                retries: 0,
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
        // Dead-URL gate (order deep-research-t6b, pre-window slice): a
        // URL whose fetch FAILED earlier in this run is refused — no
        // decider call (refusals spend no budget), no port call, the
        // URL recorded on the window's dedup_refused with the
        // fetched-dedup refusals. t1d's dedup refused only
        // already-FETCHED URLs; the task-56 shape re-admitted the same
        // 4 failing PDF URLs every round and re-spent the allowance
        // (12/12 spent on 4 unique URLs, every fetch an error).
        if decider.is_fetch_dead(&hit.url) {
            if !refused.contains(&hit.url) {
                refused.push(hit.url.clone());
            }
            continue;
        }
        // drb1-r1 Item 2: fetch retry with exponential backoff.
        // Retry up to 2 times (3 total attempts) before recording failure.
        // Each retry attempt consumes budget via decider.allow().
        let mut body = None;
        let mut last_error = None;
        let mut retry_count_used = 0u32;
        const MAX_RETRIES: u32 = 2;

        for retry_count in 0..=MAX_RETRIES {
            // The ONE decider gate — no Allow, no fetch. Each retry attempt
            // must consume budget separately.
            let verdict = decider
                .allow(FAMILY_WEB_FETCH, KEY_FETCH_PAGES, 1, at_unix)
                .await?;
            if !verdict.allowed() {
                failures.push(FetchFailure {
                    url: hit.url.clone(),
                    error: "budget-refused".to_string(),
                    absent: true,
                    retries: retry_count_used,
                });
                break;
            }

            match port.web_fetch(&hit.url).await {
                Ok(b) => {
                    body = Some(b);
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    retry_count_used = retry_count;
                    if retry_count < MAX_RETRIES {
                        let backoff_ms = 1000 * 2_u64.pow(retry_count);
                        tracing::debug!(
                            target: "deep_research",
                            url = %hit.url,
                            retry_count,
                            next_backoff_ms = backoff_ms,
                            "fetch failed, retrying with backoff"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }

        let body = match body {
            Some(b) => b,
            None => {
                // All retries exhausted — record as failure and mark URL dead.
                let error = last_error.unwrap();
                // The URL is dead for the rest of the run: a later
                // round's fetch list re-admitting it is refused with
                // no decider call and no re-spend (the task-56 shape).
                // A dead-record persistence failure does not abort the
                // round — the failure row still records the fetch
                // error; the in-memory gate holds for the live run.
                if let Err(j) = decider.record_fetch_dead(&hit.url) {
                    tracing::warn!(
                        url = %hit.url,
                        error = %j,
                        "deep-research: fetch-dead record failed — the URL is dead in memory only"
                    );
                }
                failures.push(FetchFailure {
                    url: hit.url.clone(),
                    error: error.clone(),
                    absent: true,
                    retries: retry_count_used,
                });
                continue;
            }
        };
        // Custody stamped HERE, by code, FROM THE HIT'S STAMP (t1g rung
        // 2): the port stamps custody at the source (estate hits are
        // `personal` — a local corpus is the operator's own data), and
        // the window chunk keeps that stamp — an estate chunk is never
        // re-stamped public-web. The single production construction
        // site (acquire_round) always stamps; an empty stamp is
        // `Unknown` at the join — the audit refuses per-claim on
        // unknown chunks, never a silent default.
        index += 1;
        fetched.push(hit.url.clone());
        chunks.push(WindowChunk {
            id: format!("ev-{index}"),
            locator: hit.url.clone(),
            source_url: hit.url.clone(),
            custody: hit.custody.clone(),
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
    use crate::deep_research::icd::{BudgetLedger, Plan, TriageOutcome, ICD_VERSION};
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

    /// A port whose web_fetch FAILS for the named URLs (the task-56
    /// shape: every admitted URL errors, every round). Records calls.
    struct FailingPort {
        calls: Arc<Mutex<Vec<String>>>,
        fail: Vec<String>,
    }

    #[async_trait::async_trait]
    impl ResearchPort for FailingPort {
        async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
            unimplemented!("unreachable")
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
            if self.fail.iter().any(|u| u == url) {
                Err("fetch-failed (mock pdf)".to_string())
            } else {
                Ok("body".to_string())
            }
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
            // The fetch tests' fixture predates the t1h content carry:
            // web hits — no body on this surface.
            content: None,
            engine: "mock".to_string(),
            score: 1.0,
            // The fetch tests' fixture predates the t1g custody carry:
            // these are web hits — the stamp they always had.
            custody: Custody::PublicWeb.as_str().to_string(),
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

    /// RED (order deep-research-t6b, pre-window slice, pre-registered):
    /// the task-56 shape — 12 fetch allowance, 4 unique URLs, every
    /// fetch an error, the SAME 4 URLs re-admitted by every round's
    /// fetch list. At HEAD the allowance was re-spent every round:
    /// demo13/runs/deep/drb-56/dr-1787063160's budget-ledger.json shows
    /// 12/12 spent on 4 unique URLs (all fetch errors) because t1d's
    /// dedup only refused already-FETCHED URLs — failed URLs were never
    /// in `fetched_sources` and were re-admitted forever.
    ///
    /// Now green: round 1 spends 4 and records each failing URL dead;
    /// rounds 2-3 refuse the dead URLs with NO decider call and NO
    /// port call (the spend stays at 4; the ledger's refused_urls
    /// carries the dead set); a restore replays the dead set — a
    /// resumed run refuses without re-spending.
    #[test]
    fn failed_fetch_url_is_dead_for_the_run() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let urls: Vec<String> = (0..4)
                .map(|i| format!("https://example.com/pdf-{i}"))
                .collect();
            let ids: Vec<String> = (0..4).map(|i| format!("h{}", i + 1)).collect();
            let ids_ref: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
            let fetch_list = fetch_list_admitting(&ids_ref);
            let hits: Vec<SearchHit> = ids.iter().zip(&urls).map(|(id, u)| hit(id, u)).collect();
            let port = FailingPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: urls.clone(),
            };
            let tmp = tempfile::tempdir().unwrap();
            let journal = tmp.path().join("budget-ledger.json");
            let allowance =
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 12u32)]);
            let mut decider =
                SpendDecider::new("r-dead", "h", allowance.clone(), &journal).unwrap();

            let round1 = fetch_round(
                &port,
                &mut decider,
                "r-dead",
                "h",
                1,
                &fetch_list,
                &hits,
                &[],
                1000,
            )
            .await
            .unwrap();
            // drb1-r1 Item 2: with retry logic, each failed URL is now retried twice
            // before being marked dead, so we expect 12 port calls (4 URLs * 3 attempts each)
            assert_eq!(
                port.calls.lock().unwrap().len(),
                12,
                "with retry logic, each failed URL is attempted 3 times (1 initial + 2 retries)"
            );
            assert_eq!(round1.fetch_failures.len(), 4);
            assert!(round1.dedup_refused.is_empty(), "round 1 refusals are none");
            // Each URL was attempted 3 times, spending 3 from allowance
            assert_eq!(decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES), 0);

            for round in 2..=3 {
                let w = fetch_round(
                    &port,
                    &mut decider,
                    "r-dead",
                    "h",
                    round,
                    &fetch_list,
                    &hits,
                    &[],
                    1000 + i64::from(round),
                )
                .await
                .unwrap();
                assert!(w.chunks.is_empty(), "round {round} fetches nothing");
                assert!(
                    w.fetch_failures.is_empty(),
                    "round {round}: a dead refusal is not a fetch failure"
                );
                assert_eq!(
                    w.dedup_refused, urls,
                    "round {round} refuses every dead URL, recorded on the window"
                );
            }
            // drb1-r1 Item 2: With retry logic, all 12 spends happened in round 1
            // (4 URLs × 3 attempts each), so remaining is 0.
            assert_eq!(decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES), 0);
            assert!(
                port.calls.lock().unwrap().len() == 12,
                "with retry logic, port called 12 times total (4 URLs × 3 attempts)"
            );
            assert!(
                urls.iter().all(|u| decider.is_fetch_dead(u)),
                "every failed URL is dead for the run"
            );
            // The dead set is persisted on the ledger.
            let ledger: BudgetLedger =
                serde_json::from_str(&std::fs::read_to_string(&journal).unwrap()).unwrap();
            assert_eq!(ledger.refused_urls, urls);

            // A resume replays the dead set: the same 4 URLs are refused
            // without any spend. With retry logic, round 1 spent all 12 (4 URLs × 3
            // attempts), so the restored state has 0 remaining.
            let mut restored = SpendDecider::restore("r-dead", "h", &allowance, &journal).unwrap();
            assert!(
                urls.iter().all(|u| restored.is_fetch_dead(u)),
                "restore replays the dead set"
            );
            // drb1-r1 Item 2: All 12 budget spent in round 1 (4 URLs × 3 retries)
            assert_eq!(restored.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES), 0);
            let w = fetch_round(
                &port,
                &mut restored,
                "r-dead",
                "h",
                4,
                &fetch_list,
                &hits,
                &[],
                4000,
            )
            .await
            .unwrap();
            assert!(w.chunks.is_empty() && w.fetch_failures.is_empty());
            assert_eq!(w.dedup_refused, urls);
            // drb1-r1 Item 2: With retry logic, round 1 spent all 12, remaining is 0.
            // The resumed round refuses dead URLs without spending, so remaining stays 0.
            assert_eq!(
                restored.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES),
                0,
                "the resumed run spends nothing on dead URLs (remaining stays 0)"
            );
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
