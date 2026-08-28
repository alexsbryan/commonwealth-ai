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
//!
//! drb1-t2 (order drb1-t2, campaign drb1-race — the AIQ §1.3 ph.3
//! fetch-then-judge shape adopted at this seam): the walk queue is the
//! round's FULL non-noise candidate list (triage's `candidates`), the
//! round's fetch share caps the spend (the r2b split applied to the
//! fetch family), failures fall through to the next candidate — with
//! same-query affinity (the AIQ preferred/fallback shape: a failed top
//! pick's query gets its next candidate first) — permanent error
//! classes do not retry, and every SUCCESSFUL fetch is judged on
//! CONTENT before it enters the window (`acquisition::judge_content`,
//! the one admission scorer on the content surface). Fetched-but-
//! refused pages are recorded on the window's `content_refused` with
//! the measured score and named reason, and every fetched source lands
//! in the per-run source registry rows this returns (AIQ §1.4 — the
//! T3 writer's citation whitelist surface).

use super::acquisition::{admitted_ids, judge_content};
use super::budget::{SpendDecider, FAMILY_WEB_FETCH, KEY_FETCH_PAGES};
use super::estate::DraftLeg;
use super::estate::ResearchPort;
use super::icd::{
    ContentRefusal, EvidenceWindow, FetchFailure, FetchList, SearchHit, SourceRegistryRow,
    SourceType, UrlHealth, WindowChunk,
};
use crate::types::{join_custody, Custody};

/// The per-chunk content cap — a fetched page larger than this is
/// truncated with a visible marker (glassbox: truncation is declared,
/// never silent).
pub const CHUNK_CONTENT_CAP: usize = 50_000;

// 50,000 (raised from 12,000 on 2026-08-24). The compose leg is a
// RETRIEVAL system, not a prompt-stuffer: `synthesize` slices every chunk
// into 1,400-char passages, embeds them, and hands the section writer the
// top `SECTION_PASSAGES` (8) by cosine with at most `PER_SOURCE_CAP` (3)
// from any one source. So the section prompt is 8 x 1,400 chars NO MATTER
// how large a chunk is — growing this cap grows the pool the ranker
// chooses from, and costs storage and embedding, never context.
//
// It was starving that ranker. At the old effective cap of 4,000 (the
// shared extractor's chat-snippet default, which bound before this
// constant ever did) a source yields 4 passages and the ranker picks 3 of
// 4 — no selection at all. Measured over the 45 pages a logged DRB-I
// flight fetched: median page 22,293 chars, mean 32,777, max 101,653.
// At 50,000 the median page is kept WHOLE and the ranker picks 3 of ~19.

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

/// The retry classification for a fetch error — the ONE classifier
/// (a closed set over OUR OWN port's error formats, never a guess
/// about arbitrary text): `Permanent` outcomes cannot improve with a
/// retry (binary refusal, HTTP failure status) and get a single
/// attempt; everything else — including unclassifiable text — keeps
/// drb1-r1's transient retry-with-backoff, conservatively. Measured on
/// the logged t7a flight: all 12 recorded fetch failures are binary
/// refusals (permanent), so retry-everything burned 3 budget units
/// per unfetchable URL with zero recoveries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// Retry with backoff (drb1-r1's shape: 3 total attempts).
    Transient,
    /// One attempt, no retry — the outcome cannot change.
    Permanent(UrlHealth),
}

/// Classify a port fetch error. Markers are the exact formats our own
/// port emits (`deep_research_cmd.rs` / `sovereign-tools-base`):
/// binary refusal carries `non-text payload`, HTTP failures carry
/// `HTTP <code> for`. Unknown text classifies Transient — the safe
/// default drb1-r1 shipped.
pub fn classify_fetch_error(error: &str) -> RetryClass {
    if error.contains("non-text payload") {
        return RetryClass::Permanent(UrlHealth::Binary);
    }
    // "HTTP 404 for …" / "HTTP 503 for …" — a failure status is
    // permanent on the run's timescale.
    if let Some(rest) = error.strip_prefix("HTTP ") {
        if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return RetryClass::Permanent(UrlHealth::HttpStatus);
        }
    }
    if error.contains("HTTP 4") || error.contains("HTTP 5") {
        return RetryClass::Permanent(UrlHealth::HttpStatus);
    }
    RetryClass::Transient
}

/// The fetch surface that served a URL — the registry's `type`. One
/// accessor (the fetch leg, the port's PDF routing, and the replay
/// harness all call THIS).
pub fn source_type_of(url: &str) -> SourceType {
    if url.starts_with("estate:") {
        return SourceType::Estate;
    }
    let lower = url.to_ascii_lowercase();
    // Strip a query string/fragment before the extension check.
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    if path.ends_with(".pdf") {
        return SourceType::Pdf;
    }
    SourceType::Web
}

/// The round's fetch outcome: the window plus the registry rows the
/// round contributed (every FETCHED source — window-admitted or
/// content-refused).
#[derive(Debug, Clone)]
pub struct FetchRoundOutcome {
    pub window: EvidenceWindow,
    pub registry_rows: Vec<SourceRegistryRow>,
}

/// The round's fetch policy (drb1-t2) — the whitelisted knobs the
/// fetch leg reads, threaded from the charter through RunConfig:
/// the round's fetch share (the r2b split over the fetch family —
/// `usize::MAX` when the caller does not split) and the content
/// admission floors (see `acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR`
/// for the calibration record).
#[derive(Debug, Clone)]
pub struct FetchPolicy {
    pub round_fetch_cap: usize,
    pub content_coverage_floor: f64,
    pub prose_line_floor: usize,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            round_fetch_cap: usize::MAX,
            content_coverage_floor: super::acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR,
            prose_line_floor: super::acquisition::DEFAULT_PROSE_LINE_FLOOR,
        }
    }
}

/// drb1-r1 Item 2: the transient retry ceiling (3 total attempts).
const MAX_RETRIES: u32 = 2;

/// Pages fetched concurrently within a round.
///
/// AIQ's `max_research_concurrency` is 6 and its budget is up to 100 source
/// calls per job; ours walked the queue ONE page at a time, which is fine at
/// a 4-page round share and is the wall-clock term at a hundred. This is
/// network I/O against many different hosts, not daemon inference, so it does
/// not share `AUDIT_CONCURRENCY`'s ceiling (that number tracks the daemon's
/// `max_concurrent_turns`).
///
/// The concurrency is deliberately confined to the NETWORK. The budget
/// decider stays sequential — it is the single mutable owner of the run's
/// allowance, and a spend that races is a spend that cannot be audited.
const FETCH_CONCURRENCY: usize = 6;

/// One URL's fetch, retries included. Pure I/O against the port: it touches
/// no budget, no dedup set and no window, which is what makes it safe to run
/// several at once. Returns the body (when one arrived), the last error, the
/// retry count used, and the URL's health.
async fn fetch_with_retries(
    port: &dyn ResearchPort,
    url: &str,
) -> (Option<String>, Option<String>, u32, UrlHealth) {
    let mut body = None;
    let mut last_error = None;
    let mut retry_count_used = 0u32;
    let mut health = UrlHealth::Dead;
    for retry_count in 0..=MAX_RETRIES {
        match port.web_fetch(url).await {
            Ok(b) => {
                body = Some(b);
                break;
            }
            Err(e) => {
                last_error = Some(e);
                retry_count_used = retry_count;
                let class = classify_fetch_error(last_error.as_deref().unwrap_or_default());
                if let RetryClass::Permanent(h) = class {
                    health = h;
                    break;
                }
                health = UrlHealth::Dead;
                if retry_count < MAX_RETRIES {
                    let backoff_ms = 1000 * 2_u64.pow(retry_count);
                    tracing::debug!(
                        target: "deep_research",
                        url = %url,
                        retry_count,
                        next_backoff_ms = backoff_ms,
                        "fetch failed, retrying with backoff"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }
    (body, last_error, retry_count_used, health)
}

/// Fetch the round's candidate queue into the evidence window.
/// `at_unix` is the run's round timestamp (journaled into the budget
/// ledger). Returns the raw window (pre-tags) plus the registry rows;
/// enrichment (R7) runs after.
///
/// Dedup (order deep-research-t1d fix 1): `already_fetched` carries the
/// URLs fetched by prior rounds (the run's `fetched_sources`). A
/// candidate whose URL was already fetched — in a prior round or
/// earlier in THIS round — is refused: no decider call (refusals spend
/// no budget), no port call, and the URL recorded on the window's
/// `dedup_refused`. Refusals are not fetch failures: the source was
/// acquired once and is never re-fetched (the merged window already
/// dedups chunks by URL — first wins — so the evidence is untouched;
/// only the re-fetch and its spend are refused).
///
/// drb1-t2: `candidates` is the triage queue (all non-noise ranked
/// rows) — not just the K ∪ ε tiers; the walk is bounded by
/// `policy.round_fetch_cap` (the r2b round split over the fetch
/// family) and by the decider itself, and admission to the window is
/// decided on CONTENT after fetch. Tier members the walk never reached
/// are recorded as failures (`round-cap` / `budget-refused`) — never
/// silently un-ledgered.
///
/// `next_evidence_id` is the RUN's evidence-handle counter, not the
/// round's. A window id is resolved against the MERGED window (every
/// round's chunks in one list — `number_citations` finds a chunk with
/// `.find(|c| c.id == id)`), so a counter that restarts each round
/// mints round 2's `ev-1` on top of round 1's, and every citation to
/// it renders round 1's URL. The counter is threaded, not local, for
/// the same reason the decider is: it is run-scoped mutable state with
/// exactly one owner. The caller advances nothing by hand — this
/// function is the only writer.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_round(
    port: &dyn ResearchPort,
    decider: &mut SpendDecider,
    run_id: &str,
    charter_hash: &str,
    round: u32,
    fetch_list: &FetchList,
    candidates: &[SearchHit],
    already_fetched: &[String],
    next_evidence_id: &mut usize,
    at_unix: i64,
    policy: &FetchPolicy,
) -> Result<FetchRoundOutcome, String> {
    // F17 terminal-state poll first: an unreachable terminal means the
    // leg spends nothing and every planned fetch is recorded absent.
    if let Err(e) = port.terminal_poll().await {
        let failures: Vec<FetchFailure> = candidates
            .iter()
            .map(|hit| FetchFailure {
                url: hit.url.clone(),
                error: format!("terminal-poll-failed: {e}"),
                absent: true,
                retries: 0,
                health: UrlHealth::Terminal,
            })
            .collect();
        return Ok(FetchRoundOutcome {
            window: EvidenceWindow {
                icd: "evidence_window".to_string(),
                version: super::icd::ICD_VERSION,
                run_id: run_id.to_string(),
                charter_hash: charter_hash.to_string(),
                round,
                chunks: Vec::new(),
                fetch_failures: failures,
                dedup_refused: Vec::new(),
                content_refused: Vec::new(),
                derived_custody: Custody::PublicWeb.as_str().to_string(),
            },
            registry_rows: Vec::new(),
        });
    }

    let query_text = |hit: &SearchHit| -> String {
        fetch_list
            .queries
            .iter()
            .find(|q| q.id == hit.query_id)
            .map(|q| q.text.clone())
            .unwrap_or_default()
    };

    let mut chunks: Vec<WindowChunk> = Vec::new();
    let mut failures: Vec<FetchFailure> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut content_refused: Vec<ContentRefusal> = Vec::new();
    let mut registry_rows: Vec<SourceRegistryRow> = Vec::new();
    // The dedup set: prior rounds' URLs plus this round's as it fills.
    let mut fetched: Vec<String> = already_fetched.to_vec();
    let mut spent_round = 0usize;
    // The walk queue, in rank order. On a failure the next SAME-QUERY
    // candidate is promoted to the front (AIQ's per-query fallback
    // shape: the failed top pick's query gets first claim on the freed
    // budget, instead of the round-global next rank starving it).
    let mut queue: Vec<SearchHit> = candidates.to_vec();
    // The walk runs in WAVES (2026-08-24): the gates and the budget are
    // decided sequentially, then the wave's pages are fetched
    // concurrently, then the results are processed in queue order. The
    // decider never sees concurrency — it is the single mutable owner of
    // the allowance, and a spend that races is a spend that cannot be
    // audited. What overlaps is only the network wait.
    //
    // Ordering is preserved: results are zipped back onto the wave in the
    // order the queue admitted them, so the window's `ev-N` ids and the
    // registry rows come out exactly as the sequential walk produced them.
    'walk: loop {
        if queue.is_empty() || spent_round >= policy.round_fetch_cap {
            break;
        }
        // ---- Phase A: admit a wave. Gates + budget, no network.
        let mut wave: Vec<SearchHit> = Vec::new();
        while wave.len() < FETCH_CONCURRENCY
            && !queue.is_empty()
            && spent_round < policy.round_fetch_cap
        {
            let hit = queue.remove(0);
            // Dedup gate: an already-fetched URL is refused — no decider
            // call (spends nothing), no port call, recorded on the window.
            if fetched.iter().any(|u| u == &hit.url)
                || wave.iter().any(|h: &SearchHit| h.url == hit.url)
            {
                if !refused.contains(&hit.url) {
                    refused.push(hit.url.clone());
                }
                continue;
            }
            // Dead-URL gate (order deep-research-t6b, pre-window slice): a
            // URL whose fetch FAILED earlier in this run is refused — no
            // decider call (refusals spend no budget), no port call, the
            // URL recorded on the window's dedup_refused with the
            // fetched-dedup refusals. t1d's dedup only refused
            // already-FETCHED URLs; the task-56 shape re-admitted the same
            // 4 failing PDF URLs every round and re-spent the allowance
            // (12/12 spent on 4 unique URLs, every fetch an error).
            if decider.is_fetch_dead(&hit.url) {
                if !refused.contains(&hit.url) {
                    refused.push(hit.url.clone());
                }
                continue;
            }
            // The ONE decider gate — no Allow, no fetch. ONE unit per URL,
            // charged before the retry ladder (acquisition tune,
            // 2026-08-24). It used to be one unit per ATTEMPT, which billed
            // the `web-fetch:pages` key three pages for a dead URL that
            // delivered none — the ledger's key says pages and the charter
            // field is `web_fetch_pages`, so the attempt was never the unit
            // (§10.6: one name, one meaning). Worse, `spent_round` moved
            // with it, so a single dead URL could exhaust the whole round
            // cap and file the untouched candidates behind it as
            // `round-fetch-cap` — spend they never received. Measured on the
            // logged t7a flight: task 56 spent 9 of 12 pages on four dead
            // PDFs for one chunk; 16 of the flight's 61 spent pages were
            // retries. Retries are still bounded (MAX_RETRIES) and a dead
            // URL is still recorded dead for the run, so what the ladder
            // costs now is wall clock, not the page budget. Red:
            // `one_dead_url_does_not_eat_the_rounds_fetch_allowance`.
            let verdict = decider
                .allow(FAMILY_WEB_FETCH, KEY_FETCH_PAGES, 1, at_unix)
                .await?;
            if !verdict.allowed() {
                failures.push(FetchFailure {
                    url: hit.url.clone(),
                    error: "budget-refused".to_string(),
                    absent: true,
                    retries: 0,
                    health: UrlHealth::BudgetRefused,
                });
                // Next candidate — a refusal is this URL's whole story. The
                // pre-fix `break` fell through to the no-body arm below and
                // unwrapped a `last_error` that a first-attempt refusal
                // never sets.
                continue;
            }
            spent_round += 1;
            wave.push(hit);
        }
        if wave.is_empty() {
            if queue.is_empty() {
                break 'walk;
            }
            continue 'walk;
        }

        // ---- Phase B: the network, concurrently. `fetch_with_retries`
        // touches no budget, no dedup set and no window, so several may
        // run at once. `buffered`, NOT `buffer_unordered` — Phase C zips
        // these back onto `wave` by position.
        use futures::StreamExt as _;
        let wave_urls: Vec<String> = wave.iter().map(|h| h.url.clone()).collect();
        let results: Vec<(Option<String>, Option<String>, u32, UrlHealth)> = futures::stream::iter(
            // OWNED items, not `.iter()`. A borrowed item gives the
            // closure a higher-ranked lifetime, and the resulting
            // stream future then fails the `Send` check where the
            // whole run is spawned (`sovereign-desktop`'s
            // deep_research_commands: "implementation of `Send` is
            // not general enough" naming `&dyn ResearchPort`). The
            // shipping audit leg has always iterated owned keys —
            // this matches it.
            wave_urls
                .into_iter()
                .map(|url| async move { fetch_with_retries(port, &url).await }),
        )
        .buffered(FETCH_CONCURRENCY)
        .collect()
        .await;

        // ---- Phase C: process in wave order. Sequential again.
        for (hit, (body, last_error, retry_count_used, health)) in
            wave.into_iter().zip(results.into_iter())
        {
            let Some(body) = body else {
                // All retries exhausted — record as failure and mark URL
                // dead. The URL is dead for the rest of the run: a later
                // round's fetch list re-admitting it is refused with no
                // decider call and no re-spend (the task-56 shape). A
                // dead-record persistence failure does not abort the
                // round — the failure row still records the fetch error;
                // the in-memory gate holds for the live run.
                // Always Some: the gate above `continue`s on refusal, so
                // reaching here means the ladder ran at least once.
                let error = last_error.expect("a fetch attempt ran before the no-body arm");
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
                    health,
                });
                // The fallback promotion (drb1-t2): the failed pick's
                // query gets its next candidate first.
                promote_next_same_query(&mut queue, &hit.query_id);
                continue;
            };

            // drb1-t2 — content admission (fetch-then-judge). Estate
            // retrievals are exempt: their admission already happened on
            // the estate's own search surface (the index scored the chunk
            // into the round); the content gate exists for pages the loop
            // could not see before fetching. Web pages (HTML or
            // extracted-PDF text) are judged on content.
            let capped = cap_content(&body);
            let stype = source_type_of(&hit.url);
            let verdict = if stype == SourceType::Estate {
                None
            } else {
                Some(judge_content(
                    &query_text(&hit),
                    &hit.title,
                    &capped,
                    &hit.url,
                    policy.content_coverage_floor,
                    policy.prose_line_floor,
                ))
            };
            let admitted = verdict.as_ref().is_none_or(|v| v.admits);
            fetched.push(hit.url.clone());
            if admitted {
                // Custody stamped HERE, by code, FROM THE HIT'S STAMP (t1g
                // rung 2): the port stamps custody at the source (estate
                // hits are `personal` — a local corpus is the operator's
                // own data), and the window chunk keeps that stamp — an
                // estate chunk is never re-stamped public-web. The single
                // production construction site (acquire_round) always
                // stamps; an empty stamp is `Unknown` at the join — the
                // audit refuses per-claim on unknown chunks, never a
                // silent default.
                *next_evidence_id += 1;
                chunks.push(WindowChunk {
                    id: format!("ev-{}", *next_evidence_id),
                    locator: hit.url.clone(),
                    source_url: hit.url.clone(),
                    custody: hit.custody.clone(),
                    provenance_class: "known".to_string(),
                    // drb1-t5: scrub C0 control bytes at the ONE production
                    // construction site. T2 made PDF fetching real, and PDF
                    // extraction leaves interior NULs in the text: measured
                    // 2026-08-22, 4 of task 56's chunks carried 7-17 NULs
                    // each and the estate held nearly every codepoint in
                    // 0..32. The embed backend refuses a whole batch on one
                    // of them ("Embed tokenization failed: input contains an
                    // interior NUL at byte 785"), so a single bad PDF takes
                    // down the binder, the writer's retrieval, and the page.
                    content: super::scrub_control(&capped),
                    ingested_into: None,
                    tags: Vec::new(),
                });
            } else {
                let v = verdict.expect("admitted false implies verdict is Some");
                content_refused.push(ContentRefusal {
                    url: hit.url.clone(),
                    title: hit.title.clone(),
                    coverage: v.coverage,
                    prose_line: v.prose_line,
                    reason: v.reason.clone(),
                });
                tracing::debug!(
                    target: "deep_research",
                    url = %hit.url,
                    coverage = v.coverage,
                    prose_line = v.prose_line,
                    reason = %v.reason,
                    "drb1-t2: fetched page content-refused (recorded, never silently un-ledgered)"
                );
            }
            // The registry (drb1-t2, AIQ §1.4): every FETCHED source —
            // admitted or content-refused — is a citation-whitelist row.
            registry_rows.push(SourceRegistryRow {
                url: hit.url.clone(),
                title: hit.title.clone(),
                source_type: stype,
                round,
                admitted,
            });
        }
    }

    // Tier members the walk never reached (the round cap or the
    // decider stopped it mid-queue) are recorded — never silently
    // un-ledgered (the phantom-row invariant, fetch-leg side). The
    // round-cap hold is not a decider refusal: the r2b split reserved
    // the spend for the later rounds, and the record names that.
    let tier_ids = admitted_ids(fetch_list);
    let walked: Vec<String> = chunks
        .iter()
        .map(|c| c.source_url.clone())
        .chain(content_refused.iter().map(|r| r.url.clone()))
        .chain(failures.iter().map(|f| f.url.clone()))
        .chain(refused.iter().cloned())
        .collect();
    for hit in candidates {
        if !tier_ids.contains(&hit.id) || walked.contains(&hit.url) {
            continue;
        }
        let round_capped = spent_round >= policy.round_fetch_cap;
        failures.push(FetchFailure {
            url: hit.url.clone(),
            error: if round_capped {
                "round-fetch-cap".to_string()
            } else {
                "budget-refused".to_string()
            },
            absent: true,
            retries: 0,
            health: if round_capped {
                UrlHealth::RoundCap
            } else {
                UrlHealth::BudgetRefused
            },
        });
    }

    Ok(FetchRoundOutcome {
        window: EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: super::icd::ICD_VERSION,
            run_id: run_id.to_string(),
            charter_hash: charter_hash.to_string(),
            round,
            chunks,
            fetch_failures: failures,
            dedup_refused: refused,
            content_refused,
            derived_custody: Custody::PublicWeb.as_str().to_string(),
        },
        registry_rows,
    })
}

/// Promote the next candidate from `query_id`'s fallback list to the
/// front of the walk queue (AIQ's per-query fallback shape — when the
/// top pick fails, the same query's next candidate fetches first).
/// No-op when the failed pick was its query's last candidate.
fn promote_next_same_query(queue: &mut Vec<SearchHit>, query_id: &str) {
    if let Some(pos) = queue
        .iter()
        .position(|h| h.query_id == query_id)
        .filter(|&p| p > 0)
    {
        let hit = queue.remove(pos);
        queue.insert(0, hit);
    }
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
    use crate::deep_research::acquisition::{
        triage_hits, DEFAULT_CONTENT_COVERAGE_FLOOR, DEFAULT_PROSE_LINE_FLOOR,
    };
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
        async fn draft(
            &self,
            _leg: DraftLeg,
            _p: &str,
            _s: Option<&str>,
            _u: &[String],
        ) -> Result<String, String> {
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
        async fn draft(
            &self,
            _leg: DraftLeg,
            _p: &str,
            _s: Option<&str>,
            _u: &[String],
        ) -> Result<String, String> {
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
            refused_queries: Vec::new(),
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

    /// The drb1-t2 fetch tests need query-bearing fetch lists and
    /// prose-bearing bodies (the content gate reads both); the older
    /// dedup/dead fixtures below keep their minimal shape — their
    /// assertions are about the gates, not the content verdicts, and
    /// an empty query means the prose floor (or absence of a body)
    /// decides. This policy disables the content gate for those
    /// legacy-shaped fixtures so they keep pinning THEIR gates.
    fn content_gate_off() -> FetchPolicy {
        FetchPolicy {
            round_fetch_cap: usize::MAX,
            content_coverage_floor: 0.0,
            prose_line_floor: 0,
        }
    }

    fn production_policy() -> FetchPolicy {
        FetchPolicy {
            round_fetch_cap: usize::MAX,
            content_coverage_floor: DEFAULT_CONTENT_COVERAGE_FLOOR,
            prose_line_floor: DEFAULT_PROSE_LINE_FLOOR,
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
                &mut 0usize,
                1234,
                &content_gate_off(),
            )
            .await
            .unwrap()
            .window;
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
                &mut 0usize,
                1000,
                &content_gate_off(),
            )
            .await
            .unwrap()
            .window;
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
                &mut 0usize,
                2000,
                &content_gate_off(),
            )
            .await
            .unwrap()
            .window;
            assert!(
                round2.chunks.is_empty(),
                "round 2 must not re-fetch an already-fetched URL"
            );
            assert_eq!(round2.dedup_refused, vec![url.clone()]);
            let calls = port.calls.lock().unwrap().len();
            assert_eq!(calls, 1, "round 2 never reached the port");
        });
    }

    /// RED (2026-08-27): "round 2's evidence handles do not collide with
    /// round 1's". Failed at HEAD — `fetch_round` opened `let mut index =
    /// 0usize` on every call, so a two-round flight minted `ev-1` twice.
    ///
    /// The collision is not merely wasteful. The merged window is the
    /// scope a handle is RESOLVED against, and `number_citations` resolves
    /// with `.find(|c| c.id == id)`, so the second `ev-1` is unreachable
    /// and every citation written against it renders the FIRST chunk's
    /// URL — a claim drawn from round 2 ships with a round-1 source, exit
    /// 0, no warning, and a report that reads correctly (§7.5).
    ///
    /// Measured on bed dr-1787807617 before the fix: 7 ids covering 24 of
    /// 62 chunks, 5 of them from this site (the other 12 were
    /// `estate_window` numbering by query, fixed in 9588704c2).
    #[test]
    fn evidence_handles_are_unique_across_rounds() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let urls: Vec<String> = (1..=4)
                .map(|i| format!("https://example.com/p{i}"))
                .collect();
            let port = CountingPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                bodies: urls
                    .iter()
                    .map(|u| (u.clone(), "the body".to_string()))
                    .collect(),
            };
            let tmp = tempfile::tempdir().unwrap();
            let mut decider = SpendDecider::new(
                "r-ids",
                "h",
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 8u32)]),
                &tmp.path().join("budget-ledger.json"),
            )
            .unwrap();

            // The counter the CONTROLLER owns: one per run, threaded
            // across the rounds exactly as `acquire_round` threads it.
            let mut next_evidence_id = 0usize;

            let round1 = fetch_round(
                &port,
                &mut decider,
                "r-ids",
                "h",
                1,
                &fetch_list_admitting(&["h1", "h2"]),
                &[hit("h1", &urls[0]), hit("h2", &urls[1])],
                &[],
                &mut next_evidence_id,
                1000,
                &content_gate_off(),
            )
            .await
            .unwrap()
            .window;

            let already: Vec<String> = round1.chunks.iter().map(|c| c.source_url.clone()).collect();
            let round2 = fetch_round(
                &port,
                &mut decider,
                "r-ids",
                "h",
                2,
                &fetch_list_admitting(&["h3", "h4"]),
                &[hit("h3", &urls[2]), hit("h4", &urls[3])],
                &already,
                &mut next_evidence_id,
                2000,
                &content_gate_off(),
            )
            .await
            .unwrap()
            .window;

            assert_eq!(round1.chunks.len(), 2, "round 1 fetched both");
            assert_eq!(round2.chunks.len(), 2, "round 2 fetched both");

            let ids: Vec<&str> = round1
                .chunks
                .iter()
                .chain(round2.chunks.iter())
                .map(|c| c.id.as_str())
                .collect();
            assert_eq!(
                ids,
                vec!["ev-1", "ev-2", "ev-3", "ev-4"],
                "the counter is run-scoped: round 2 continues where round 1 stopped"
            );
            let distinct: std::collections::BTreeSet<&&str> = ids.iter().collect();
            assert_eq!(
                distinct.len(),
                ids.len(),
                "no handle names two chunks in the merged window"
            );
            assert_eq!(next_evidence_id, 4, "the caller sees the advanced counter");
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
    /// Now green (and green on the ORIGINAL number again since the
    /// acquisition tune of 2026-08-24 — drb1-r1's per-attempt billing
    /// had moved the spend to 12 and the assertions were edited to
    /// follow it, leaving this comment as the only record of the
    /// intent): round 1 spends 4 and records each failing URL dead;
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
                &mut 0usize,
                1000,
                &content_gate_off(),
            )
            .await
            .unwrap()
            .window;
            // drb1-r1 Item 2: with retry logic, each failed URL is now retried twice
            // before being marked dead, so we expect 12 port calls (4 URLs * 3 attempts each)
            assert_eq!(
                port.calls.lock().unwrap().len(),
                12,
                "with retry logic, each failed URL is attempted 3 times (1 initial + 2 retries)"
            );
            assert_eq!(round1.fetch_failures.len(), 4);
            assert!(round1.dedup_refused.is_empty(), "round 1 refusals are none");
            // Four dead URLs cost four pages — the doc comment's
            // pre-registered number, restored. drb1-r1's retry ladder
            // billed per attempt (4 x 3 = 12, allowance drained to 0)
            // and these assertions were edited to match it while the
            // doc kept describing the intent; the acquisition tune bills
            // the page again (§10.6).
            assert_eq!(decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES), 8);

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
                    &mut 0usize,
                    1000 + i64::from(round),
                    &content_gate_off(),
                )
                .await
                .unwrap()
                .window;
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
            // Rounds 2-3 refuse the dead URLs with no decider call, so
            // the spend stays at round 1's four pages.
            assert_eq!(decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES), 8);
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
            // without any spend. With retry logic, round 1 spent all 12 (4 URLs
            // × 3 attempts), so the restored state has 0 remaining.
            let mut restored = SpendDecider::restore("r-dead", "h", &allowance, &journal).unwrap();
            assert!(
                urls.iter().all(|u| restored.is_fetch_dead(u)),
                "restore replays the dead set"
            );
            assert_eq!(restored.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES), 8);
            let w = fetch_round(
                &port,
                &mut restored,
                "r-dead",
                "h",
                4,
                &fetch_list,
                &hits,
                &[],
                &mut 0usize,
                4000,
                &content_gate_off(),
            )
            .await
            .unwrap()
            .window;
            assert!(w.chunks.is_empty() && w.fetch_failures.is_empty());
            assert_eq!(w.dedup_refused, urls);
            assert_eq!(
                restored.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES),
                8,
                "the resumed run spends nothing on dead URLs (remaining holds \
                 at round 1's four pages)"
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
            content_refused: Vec::new(),
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

    // -----------------------------------------------------------------
    // drb1-t2 — fetch-then-judge + the fetch leg (order drb1-t2).
    // Pre-registered in adversarial/pre-registration.md before the
    // change; watched red at HEAD (compile-red for the new surface —
    // FetchPolicy, content_refused, the registry rows, candidates —
    // none existed; the assertion-level shape is documented per test).
    // -----------------------------------------------------------------

    /// The task-65 metadata-page shape, byte-identical cuts vendored
    /// from the logged flight's evidence windows
    /// (tests/golden/drb1-t2-fetch/).
    fn golden(name: &str) -> String {
        let path = format!(
            "{}/tests/golden/drb1-t2-fetch/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
    }

    fn query_hit(id: &str, qid: &str, url: &str, title: &str) -> SearchHit {
        SearchHit {
            id: id.to_string(),
            query_id: qid.to_string(),
            url: url.to_string(),
            title: title.to_string(),
            snippet: String::new(),
            content: None,
            engine: "web".to_string(),
            score: 1.0,
            custody: Custody::PublicWeb.as_str().to_string(),
        }
    }

    fn fetch_list_queries(q1: &str) -> FetchList {
        FetchList {
            icd: "fetch_list".to_string(),
            version: ICD_VERSION,
            run_id: "r-t2".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            queries: vec![crate::deep_research::icd::FormedQuery {
                id: "q1".to_string(),
                text: q1.to_string(),
                from_gap_id: None,
                formed_by: "gap-template".to_string(),
                provider: "deterministic".to_string(),
                corroboration: None,
            }],
            search_hits: Vec::new(),
            triage: TriageOutcome {
                code_set_k: vec!["h1".to_string()],
                eps_admits: Vec::new(),
                below_cut: Vec::new(),
                threshold: 0.0,
                eps_quota: 0.0,
                admission_rule: crate::deep_research::acquisition::ADMISSION_RULE_SCORE_THEN_FIGURE
                    .to_string(),
            },
            refused_queries: Vec::new(),
        }
    }

    /// RED `jobs_board_row_never_spends_a_fetch` (order drb1-t2): a
    /// careers-page row inside the code-set K is demoted pre-fetch —
    /// the port is NEVER called for it, and its skip-ledger row carries
    /// `noise-demoted`. At HEAD no demotion existed: the row fetched.
    #[test]
    fn jobs_board_row_never_spends_a_fetch() {
        let noise_hit = SearchHit {
            id: "n1".to_string(),
            query_id: "q1".to_string(),
            url: "https://www.okta.com/company/careers/product/senior-product-manager".to_string(),
            title: "Senior Product Manager, Okta Device".to_string(),
            snippet: "Apply now for the senior product manager role".to_string(),
            content: None,
            engine: "web".to_string(),
            score: 0.9,
            custody: Custody::PublicWeb.as_str().to_string(),
        };
        let good = query_hit(
            "h1",
            "q1",
            "https://research.example/paper",
            "A Study of Payment Tablet Devices",
        );
        let triaged = triage_hits("r", "h", 1, vec![noise_hit, good], 5, 0.1);
        assert!(
            triaged
                .candidates
                .iter()
                .all(|h| !h.url.contains("careers")),
            "the careers row never enters the fetch queue: {:?}",
            triaged.candidates
        );
        assert!(
            triaged
                .skip_ledger
                .entries
                .iter()
                .any(|e| e.url.contains("careers")
                    && e.reason.starts_with("noise-demoted:careers-path")),
            "the careers row is ledgered with its noise class: {:?}",
            triaged.skip_ledger.entries
        );
        // And through the fetch leg: the port never sees the noise url.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let port = CountingPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                bodies: HashMap::from([(
                    "https://research.example/paper".to_string(),
                    "A long prose paragraph about payment tablet devices used for payments \
                     and SaaS applications in commercial settings with figures and detail \
                     that carries the query terms across the body of the page."
                        .to_string(),
                )]),
            };
            let tmp = tempfile::tempdir().unwrap();
            let mut decider = SpendDecider::new(
                "r-t2",
                "h",
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 6u32)]),
                &tmp.path().join("budget-ledger.json"),
            )
            .unwrap();
            let mut fetch_list = fetch_list_queries("payment tablet devices SaaS applications");
            fetch_list.triage.code_set_k =
                triaged.candidates.iter().map(|h| h.id.clone()).collect();
            let out = fetch_round(
                &port,
                &mut decider,
                "r-t2",
                "h",
                1,
                &fetch_list,
                &triaged.candidates,
                &[],
                &mut 0usize,
                1234,
                &production_policy(),
            )
            .await
            .unwrap();
            assert!(
                port.calls
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|u| !u.contains("careers")),
                "the port is never called for the demoted careers row"
            );
            assert_eq!(out.window.chunks.len(), 1);
        });
    }

    /// RED `binary_refused_pages_route_to_fallback` (order drb1-t2):
    /// with the REAL binary-refusal marker, top-pick failures cost one
    /// attempt each and the walk continues to the next candidates —
    /// chunks land where HEAD's retry-everything burned the whole
    /// allowance for zero chunks.
    #[test]
    fn binary_refused_pages_route_to_fallback() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // The two top picks fail with the port's actual binary
            // refusal text; the next three serve prose bodies.
            let fail_urls: Vec<String> = (0..2)
                .map(|i| format!("https://scholar.example/paper-{i}.pdf"))
                .collect();
            let ok_urls: Vec<String> = (0..3)
                .map(|i| format!("https://site.example/article-{i}"))
                .collect();
            // A dedicated port for the REAL permanent marker (the
            // shared FailingPort fixture's error text is deliberately
            // transient-class for the drb1-r1 retry pins):
            struct BinaryPort {
                calls: Arc<Mutex<Vec<String>>>,
                fail: Vec<String>,
            }
            #[async_trait::async_trait]
            impl ResearchPort for BinaryPort {
                async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
                    unimplemented!()
                }
                async fn estate_search(
                    &self,
                    _c: &[String],
                    _q: &str,
                    _l: usize,
                ) -> Result<Vec<PortHit>, String> {
                    unimplemented!()
                }
                async fn web_search(
                    &self,
                    _b: &str,
                    _q: &str,
                    _l: usize,
                ) -> Result<Vec<PortHit>, String> {
                    unimplemented!()
                }
                async fn web_fetch(&self, url: &str) -> Result<String, String> {
                    self.calls.lock().unwrap().push(url.to_string());
                    if self.fail.iter().any(|u| u == url) {
                        Err(format!(
                            "fetch {url}: non-text payload (application/pdf) — binary \
                             content refused (would poison the evidence window)"
                        ))
                    } else {
                        Ok("A substantial prose paragraph reporting the equilibrium \
                           bidding functions in asymmetric first-price auctions with \
                           the supporting figures and comparative statics discussed \
                           across the paper's sections."
                            .to_string())
                    }
                }
                async fn terminal_poll(&self) -> Result<(), String> {
                    Ok(())
                }
                async fn draft(
                    &self,
                    _leg: DraftLeg,
                    _p: &str,
                    _s: Option<&str>,
                    _u: &[String],
                ) -> Result<String, String> {
                    unimplemented!()
                }
                async fn alignment_decision(
                    &self,
                    _p: &Plan,
                    _r: &Path,
                ) -> Result<AlignmentDecision, String> {
                    Ok(AlignmentDecision::Proceed)
                }
            }
            let port = BinaryPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: fail_urls.clone(),
            };
            let mut hits: Vec<SearchHit> = fail_urls
                .iter()
                .chain(ok_urls.iter())
                .enumerate()
                .map(|(i, u)| query_hit(&format!("h{}", i + 1), "q1", u, "Asymmetric Auctions"))
                .collect();
            for (i, h) in hits.iter_mut().enumerate() {
                h.score = 1.0 - i as f64 * 0.01;
            }
            let triaged = triage_hits("r", "h", 1, hits, 2, 0.0);
            let mut fetch_list = fetch_list_queries("asymmetric first price auctions");
            fetch_list.triage.code_set_k = vec!["h1".to_string(), "h2".to_string()];
            let tmp = tempfile::tempdir().unwrap();
            let mut decider = SpendDecider::new(
                "r-t2",
                "h",
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 6u32)]),
                &tmp.path().join("budget-ledger.json"),
            )
            .unwrap();
            let out = fetch_round(
                &port,
                &mut decider,
                "r-t2",
                "h",
                1,
                &fetch_list,
                &triaged.candidates,
                &[],
                &mut 0usize,
                1234,
                &production_policy(),
            )
            .await
            .unwrap();
            assert_eq!(
                port.calls.lock().unwrap().len(),
                5,
                "two permanent binary failures (one attempt each) + three fallback fetches"
            );
            assert_eq!(
                out.window.chunks.len(),
                3,
                "the fallback candidates land chunks where HEAD burned the allowance on retries"
            );
            assert!(out
                .window
                .fetch_failures
                .iter()
                .all(|f| f.health == UrlHealth::Binary));
            assert_eq!(
                decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES),
                1,
                "5 spends of the 6 allowance (no retry on permanent failures)"
            );
        });
    }

    /// RED `metadata_only_page_is_content_rejected_with_reason` (order
    /// drb1-t2): a task-65-shaped page (the vendored byte-identical
    /// recorded cuts) is fetched then content-rejected — no chunk, the
    /// refusal recorded WITH score and reason, and the source
    /// registered. At HEAD the chunk landed in the window unjudged.
    #[test]
    fn metadata_only_page_is_content_rejected_with_reason() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // The chrome cut's recorded query (task 58 round 1, q1 —
            // byte-exact from the flight's fetch-list-1.json): the
            // content verdict is query-relative, so the pin replays
            // the query the calibration measured (coverage 0.19,
            // longest line 138 — both under the floors).
            let chrome_query = "Exploring Horizontal Gene Transfer (HGT) in Plants and animals \
                 (ie Non-Microbial Systems)\nYou could examine instances of horizontal gene \
                 transfer in eukaryotes—particularly plants and animals—and evaluate the \
                 evolutionary significance of these transfers. Its very rare and therefore \
                 must be a really interesting reason behind this adaptation!\nEspecially as \
                 this horizontal gene transfer has been well -studied in microbial systems, \
                 but not in plants and animals (this is a relatively new discovery).  \
                 Understanding  how commonly genes move between eukaryotic species and \
                 whether these transfers confer benefits would be really interesting to \
                 find out";
            let cases: [(&str, &str, bool); 3] = [
                // (name, query, admits?) — the chrome-only cut refuses
                // under ITS recorded query, the empty extraction
                // refuses (query-independent: nothing arrived), the
                // chrome+prose cut admits (its long prose line is
                // real body text whatever the query).
                ("chrome-frontiersin.txt", chrome_query, false),
                ("empty-semanticscholar.txt", "any query at all", false),
                ("prose-pmc7184763.txt", "any query at all", true),
            ];
            for (name, query, admits) in cases {
                let body = golden(name);
                let url = format!("https://fixture.example/{name}");
                let port = CountingPort {
                    calls: Arc::new(Mutex::new(Vec::new())),
                    bodies: HashMap::from([(url.clone(), body.clone())]),
                };
                let hit = query_hit("h1", "q1", &url, "A Crop Phenotyping Page");
                let triaged = triage_hits("r", "h", 1, vec![hit], 5, 0.1);
                let mut fetch_list = fetch_list_queries(query);
                fetch_list.triage.code_set_k = vec!["h1".to_string()];
                let tmp = tempfile::tempdir().unwrap();
                let mut decider = SpendDecider::new(
                    "r-t2",
                    "h",
                    HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 4u32)]),
                    &tmp.path().join("budget-ledger.json"),
                )
                .unwrap();
                let out = fetch_round(
                    &port,
                    &mut decider,
                    "r-t2",
                    "h",
                    1,
                    &fetch_list,
                    &triaged.candidates,
                    &[],
                    &mut 0usize,
                    1234,
                    &production_policy(),
                )
                .await
                .unwrap();
                if admits {
                    assert_eq!(
                        out.window.chunks.len(),
                        1,
                        "{name}: the chrome+prose cut admits (real body text present)"
                    );
                    assert!(out.window.content_refused.is_empty());
                } else {
                    assert_eq!(
                        out.window.chunks.len(),
                        0,
                        "{name}: the metadata-only page does not enter the window"
                    );
                    assert_eq!(
                        out.window.content_refused.len(),
                        1,
                        "{name}: refusal recorded"
                    );
                    let r = &out.window.content_refused[0];
                    assert_eq!(r.url, url);
                    assert!(
                        !r.reason.is_empty(),
                        "{name}: the refusal carries a named reason"
                    );
                    assert!(
                        r.reason.starts_with("empty-content")
                            || r.reason.starts_with("content-below-threshold"),
                        "{name}: the reason is one of the named classes: {}",
                        r.reason
                    );
                    // The registry row exists — fetched, not admitted.
                    assert_eq!(out.registry_rows.len(), 1);
                    assert_eq!(out.registry_rows[0].url, url);
                    assert!(!out.registry_rows[0].admitted);
                }
            }
        });
    }

    /// RED `every_fetched_source_lands_in_the_registry` (order
    /// drb1-t2): window-admitted AND content-refused sources both land
    /// in the registry with url + title + type; the fetch-failed URL
    /// does not (it produced no source).
    #[test]
    fn every_fetched_source_lands_in_the_registry() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let prose = "A long prose paragraph that carries the query terms payment \
                         tablet devices and SaaS applications across the body of the page \
                         with supporting detail and figures reported over multiple sentences.";
            let pdf_url = "https://scholar.example/paper.pdf".to_string();
            let port = CountingPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                bodies: HashMap::from([
                    (
                        "https://site.example/article".to_string(),
                        prose.to_string(),
                    ),
                    (
                        pdf_url.clone(),
                        "A Simple Approach to Analyzing Asymmetric First Price Auctions \
                         with equilibrium bidding functions characterized for the \
                         asymmetric case across several long paragraphs of prose."
                            .to_string(),
                    ),
                ]),
            };
            let h1 = query_hit(
                "h1",
                "q1",
                "https://site.example/article",
                "Payment Tablets",
            );
            let h2 = query_hit("h2", "q1", &pdf_url, "Asymmetric FPA paper");
            let triaged = triage_hits("r", "h", 1, vec![h1, h2], 5, 0.1);
            let mut fetch_list =
                fetch_list_queries("payment tablet devices asymmetric auctions SaaS");
            fetch_list.triage.code_set_k = vec!["h1".to_string(), "h2".to_string()];
            let tmp = tempfile::tempdir().unwrap();
            let mut decider = SpendDecider::new(
                "r-t2",
                "h",
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 4u32)]),
                &tmp.path().join("budget-ledger.json"),
            )
            .unwrap();
            let out = fetch_round(
                &port,
                &mut decider,
                "r-t2",
                "h",
                1,
                &fetch_list,
                &triaged.candidates,
                &[],
                &mut 0usize,
                1234,
                &production_policy(),
            )
            .await
            .unwrap();
            let urls: Vec<&str> = out.registry_rows.iter().map(|r| r.url.as_str()).collect();
            assert!(urls.contains(&"https://site.example/article"));
            assert!(urls.contains(&pdf_url.as_str()));
            let pdf_row = out.registry_rows.iter().find(|r| r.url == pdf_url).unwrap();
            assert_eq!(
                pdf_row.source_type,
                SourceType::Pdf,
                "the .pdf url's registry row names its fetch surface"
            );
            assert!(out.registry_rows.iter().all(|r| !r.title.is_empty()));
        });
    }

    /// A port whose every fetch fails TRANSIENTLY — the class that
    /// earns the full retry ladder (`classify_fetch_error` reads no
    /// HTTP status and no binary payload here, so it returns
    /// `RetryClass::Transient`).
    struct AlwaysTransientFailPort {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ResearchPort for AlwaysTransientFailPort {
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
            Err("connection reset by peer".to_string())
        }
        async fn terminal_poll(&self) -> Result<(), String> {
            Ok(())
        }
        async fn draft(
            &self,
            _leg: DraftLeg,
            _p: &str,
            _s: Option<&str>,
            _u: &[String],
        ) -> Result<String, String> {
            unimplemented!("unreachable")
        }
    }

    /// RED-first (acquisition tune, 2026-08-24): ONE dead URL must not
    /// consume a whole round's fetch allowance.
    ///
    /// THE UNITS MISMATCH. The budget's family and key are
    /// `web-fetch:pages` and the charter field is `web_fetch_pages` —
    /// the declared resource is PAGES. The retry ladder charged one unit
    /// per ATTEMPT ("Each retry attempt must consume budget
    /// separately"), so a URL that fails its 3 attempts billed 3 pages
    /// for the 0 pages it delivered. Two counters, two meanings, one
    /// name (§10.6).
    ///
    /// THE HARM, MEASURED. On the logged t7a flight, task 56 spent 9 of
    /// its 12 pages and put ONE chunk in the window: four dead PDF URLs
    /// at up to 3 attempts each ate the allowance, and rounds 1 and 3
    /// produced no evidence at all. Across the nine-task flight, 16 of
    /// 61 spent pages went to dead-URL retries.
    ///
    /// THE SHAPE HERE is that pathology minimised: three candidates, a
    /// round cap of three, every URL dead. Attempt-billing spends the
    /// whole cap on candidate ONE — the walk breaks on
    /// `spent_round >= round_fetch_cap` before it ever reaches
    /// candidates two and three, which are then filed `round-fetch-cap`
    /// as though the budget had been spent on them. Page-billing
    /// attempts all three.
    ///
    /// Retries stay bounded and still happen — MAX_RETRIES caps the
    /// ladder and `record_fetch_dead` keeps a dead URL from being
    /// re-attempted in a later round. What changes is only WHO PAYS for
    /// the retry: the wall clock, not the page budget.
    ///
    /// WATCH IT FAIL: at HEAD the port sees 3 calls, all for
    /// `article-0`, and the window files `article-1` / `article-2` as
    /// `round-fetch-cap`.
    #[test]
    fn one_dead_url_does_not_eat_the_rounds_fetch_allowance() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let urls: Vec<String> = (0..3)
                .map(|i| format!("https://site.example/article-{i}"))
                .collect();
            let hits: Vec<SearchHit> = urls
                .iter()
                .enumerate()
                .map(|(i, u)| {
                    let mut h = query_hit(
                        &format!("h{}", i + 1),
                        "q1",
                        u,
                        "Payment Tablet Devices SaaS Article",
                    );
                    h.score = 0.9 - i as f64 * 0.1;
                    h
                })
                .collect();
            let triaged = triage_hits("r", "h", 1, hits, 5, 0.1);
            let calls = Arc::new(Mutex::new(Vec::new()));
            let port = AlwaysTransientFailPort {
                calls: calls.clone(),
            };
            let fetch_list = fetch_list_queries("payment tablet devices SaaS");
            let tmp = tempfile::tempdir().unwrap();
            let ledger_path = tmp.path().join("budget-ledger.json");
            let mut decider = SpendDecider::new(
                "r-t2",
                "h",
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 12u32)]),
                &ledger_path,
            )
            .unwrap();
            let policy = FetchPolicy {
                // ceil(9 / 3 rounds) — the r2b round share the DRB-I
                // settings produce, and the number one dead URL ate.
                round_fetch_cap: 3,
                content_coverage_floor: DEFAULT_CONTENT_COVERAGE_FLOOR,
                prose_line_floor: DEFAULT_PROSE_LINE_FLOOR,
            };
            let out = fetch_round(
                &port,
                &mut decider,
                "r-t2",
                "h",
                1,
                &fetch_list,
                &triaged.candidates,
                &[],
                &mut 0usize,
                1234,
                &policy,
            )
            .await
            .expect("the round lands");

            let attempted: std::collections::HashSet<String> =
                calls.lock().unwrap().iter().cloned().collect();
            assert_eq!(
                attempted.len(),
                3,
                "every candidate must get its attempt — one dead URL's \
                 retries billed the whole round cap and starved the rest \
                 (port saw {:?})",
                calls.lock().unwrap()
            );
            let capped: Vec<&FetchFailure> = out
                .window
                .fetch_failures
                .iter()
                .filter(|f| f.error == "round-fetch-cap")
                .collect();
            assert!(
                capped.is_empty(),
                "no candidate may be filed round-fetch-cap while the \
                 allowance held pages for it: {:?}",
                capped.iter().map(|f| &f.url).collect::<Vec<_>>()
            );
            let spent: u32 = 12 - decider.remaining(FAMILY_WEB_FETCH, KEY_FETCH_PAGES);
            assert_eq!(
                spent, 3,
                "three dead URLs cost three pages — the ledger's unit is \
                 the page, not the attempt"
            );
        });
    }

    /// RED `fetch_queue_extends_beyond_the_code_set` (order drb1-t2):
    /// under permissive triage the candidate queue extends past the
    /// K ∪ ε tiers, and the round cap bounds the walk — the
    /// r2b-split shape over the fetch family.
    #[test]
    fn fetch_queue_extends_beyond_the_code_set() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let urls: Vec<String> = (0..6)
                .map(|i| format!("https://site.example/article-{i}"))
                .collect();
            let hits: Vec<SearchHit> = urls
                .iter()
                .enumerate()
                .map(|(i, u)| {
                    let mut h = query_hit(
                        &format!("h{}", i + 1),
                        "q1",
                        u,
                        "Payment Tablet Devices SaaS Article",
                    );
                    h.score = 0.9 - i as f64 * 0.1;
                    h
                })
                .collect();
            let triaged = triage_hits("r", "h", 1, hits, 2, 0.0);
            assert_eq!(
                triaged.candidates.len(),
                6,
                "the queue carries every non-noise row, past the K=2 tier"
            );
            assert_eq!(triaged.ranked.len(), 2);
            // The round cap holds the walk at its share: cap 3 of 6
            // candidates → three fetches. Below-tier rows the walk
            // never reached keep their triage ledger rows (the
            // acquire_round rewrite drops only the FETCHED ones).
            let port = CountingPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                bodies: urls
                    .iter()
                    .map(|u| {
                        (
                            u.clone(),
                            "A long prose paragraph about payment tablet devices used \
                             for SaaS applications with the query terms present."
                                .to_string(),
                        )
                    })
                    .collect(),
            };
            let mut fetch_list = fetch_list_queries("payment tablet devices SaaS");
            fetch_list.triage.code_set_k = vec!["h1".to_string(), "h2".to_string()];
            let tmp = tempfile::tempdir().unwrap();
            let mut decider = SpendDecider::new(
                "r-t2",
                "h",
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 12u32)]),
                &tmp.path().join("budget-ledger.json"),
            )
            .unwrap();
            let policy = FetchPolicy {
                round_fetch_cap: 3,
                content_coverage_floor: DEFAULT_CONTENT_COVERAGE_FLOOR,
                prose_line_floor: DEFAULT_PROSE_LINE_FLOOR,
            };
            let out = fetch_round(
                &port,
                &mut decider,
                "r-t2",
                "h",
                1,
                &fetch_list,
                &triaged.candidates,
                &[],
                &mut 0usize,
                1234,
                &policy,
            )
            .await
            .unwrap();
            assert_eq!(out.window.chunks.len(), 3, "the round cap bounds the walk");
            assert_eq!(port.calls.lock().unwrap().len(), 3);

            // The round-cap record: when failures consume the round's
            // share BEFORE a tier member is reached, that member gets a
            // `round-cap` row — never silently un-ledgered. Two top
            // picks fail binary (one attempt each, permanent class),
            // the cap is 2, so tier members h3/h4 are never reached.
            struct BinaryFailPort {
                calls: Arc<Mutex<Vec<String>>>,
                fail: Vec<String>,
            }
            #[async_trait::async_trait]
            impl ResearchPort for BinaryFailPort {
                async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
                    unimplemented!()
                }
                async fn estate_search(
                    &self,
                    _c: &[String],
                    _q: &str,
                    _l: usize,
                ) -> Result<Vec<PortHit>, String> {
                    unimplemented!()
                }
                async fn web_search(
                    &self,
                    _b: &str,
                    _q: &str,
                    _l: usize,
                ) -> Result<Vec<PortHit>, String> {
                    unimplemented!()
                }
                async fn web_fetch(&self, url: &str) -> Result<String, String> {
                    self.calls.lock().unwrap().push(url.to_string());
                    if self.fail.iter().any(|u| u == url) {
                        Err("non-text payload (application/pdf) — binary content \
                             refused (would poison the evidence window)"
                            .to_string())
                    } else {
                        Ok("Prose about payment tablet devices for SaaS applications \
                            across a full paragraph with the query terms."
                            .to_string())
                    }
                }
                async fn terminal_poll(&self) -> Result<(), String> {
                    Ok(())
                }
                async fn draft(
                    &self,
                    _leg: DraftLeg,
                    _p: &str,
                    _s: Option<&str>,
                    _u: &[String],
                ) -> Result<String, String> {
                    unimplemented!()
                }
                async fn alignment_decision(
                    &self,
                    _p: &Plan,
                    _r: &Path,
                ) -> Result<AlignmentDecision, String> {
                    Ok(AlignmentDecision::Proceed)
                }
            }
            let port = BinaryFailPort {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: vec![urls[0].clone(), urls[1].clone()],
            };
            let mut fetch_list = fetch_list_queries("payment tablet devices SaaS");
            fetch_list.triage.code_set_k =
                (1..=4).map(|i| format!("h{i}")).collect::<Vec<_>>().clone();
            let tmp = tempfile::tempdir().unwrap();
            let mut decider = SpendDecider::new(
                "r-t2",
                "h",
                HashMap::from([(format!("{FAMILY_WEB_FETCH}:{KEY_FETCH_PAGES}"), 12u32)]),
                &tmp.path().join("budget-ledger.json"),
            )
            .unwrap();
            let policy = FetchPolicy {
                round_fetch_cap: 2,
                content_coverage_floor: DEFAULT_CONTENT_COVERAGE_FLOOR,
                prose_line_floor: DEFAULT_PROSE_LINE_FLOOR,
            };
            let out = fetch_round(
                &port,
                &mut decider,
                "r-t2",
                "h",
                1,
                &fetch_list,
                &triaged.candidates,
                &[],
                &mut 0usize,
                1234,
                &policy,
            )
            .await
            .unwrap();
            assert_eq!(out.window.chunks.len(), 0, "both top picks failed");
            assert!(
                out.window
                    .fetch_failures
                    .iter()
                    .any(|f| f.health == UrlHealth::RoundCap),
                "the un-reached tier members record round-cap rows: {:?}",
                out.window.fetch_failures
            );
        });
    }

    /// The calibration pin (the instrument-validity companion to the
    /// replay harness): the vendored recorded cuts measure exactly the
    /// numbers the floors were derived from — the chrome cut's longest
    /// line sits under the prose floor, the prose cut's over it, and
    /// the classifier's permanent/transient split reads our own port's
    /// markers.
    #[test]
    fn content_floor_calibration_pins_the_recorded_cuts() {
        use crate::deep_research::acquisition::prose_line_length;
        let chrome = golden("chrome-frontiersin.txt");
        let prose = golden("prose-pmc7184763.txt");
        let empty = golden("empty-semanticscholar.txt");
        assert_eq!(prose_line_length(&chrome), 138);
        assert!(prose_line_length(&chrome) < DEFAULT_PROSE_LINE_FLOOR);
        // Bytes, not chars: the recorded cut carries multibyte dashes
        // (762 bytes over 760 chars) — the floor reads byte length.
        assert_eq!(prose_line_length(&prose), 762);
        assert!(prose_line_length(&prose) >= DEFAULT_PROSE_LINE_FLOOR);
        assert_eq!(prose_line_length(&empty), 0);
        // The retry classifier over the port's own markers.
        assert_eq!(
            classify_fetch_error(
                "fetch https://x.example/a.pdf: non-text payload (application/pdf) — \
                 binary content refused (would poison the evidence window)"
            ),
            RetryClass::Permanent(UrlHealth::Binary)
        );
        assert_eq!(
            classify_fetch_error("HTTP 404 for https://x.example/a"),
            RetryClass::Permanent(UrlHealth::HttpStatus)
        );
        assert_eq!(
            classify_fetch_error("error sending request: connection reset"),
            RetryClass::Transient
        );
        assert_eq!(
            classify_fetch_error("fetch-failed (mock pdf)"),
            RetryClass::Transient
        );
        // The registry's type accessor.
        assert_eq!(source_type_of("https://x.example/a.pdf"), SourceType::Pdf);
        assert_eq!(
            source_type_of("https://x.example/a.pdf?download=1"),
            SourceType::Pdf
        );
        assert_eq!(source_type_of("https://x.example/a"), SourceType::Web);
        assert_eq!(source_type_of("estate:corpus:12"), SourceType::Estate);
    }
}
