// SPDX-License-Identifier: AGPL-3.0-or-later
//! R4 — the gated search walk.
//!
//! Every search goes through the ONE run-scoped decider
//! (`SpendDecider::allow`, fail-closed) — the port's `web_search` /
//! `estate_search` is never called without an `Allow`. The SOURCE is a
//! closed set decided once at launch (t1g rung 2): `Mock` — the deck's
//! term-ranked surface; `Corpus` — the estate's corpus-search surface;
//! `Web` (rung 3, order deep-research-t2a) — the real web leg through
//! the port, routed identically to `Mock` (the port's `web_search`
//! carries the run's consent grant to the egress boundary). Same
//! ledger, same allowance — the protocol is unchanged, only the source
//! routes differently. A refused query spends nothing and is journaled
//! in the budget ledger; the ledger is the record.
//!
//! The walk runs in WAVES (2026-08-24), the shape the R6 fetch leg took
//! the same day (`fetch::fetch_round`): the allowance is decided
//! sequentially, then the wave's queries reach the backend
//! concurrently, then the hits are processed in wave order. This module
//! exists as a sibling of `fetch` so that shape is testable the way
//! fetch's is — the walk was inline in the loop until the waves landed,
//! where the only observable of its ordering was a ranked artifact
//! several stages downstream.

use super::budget::{SpendDecider, FAMILY_WEB_SEARCH};
use super::estate::ResearchPort;
use super::icd::{FormedQuery, ResidueRow, SearchHit};
use super::SearchSource;

/// Hits requested per query. One number, one meaning — the two source
/// legs ask the same depth so a run's hit budget does not silently
/// depend on which surface answered.
const HITS_PER_QUERY: usize = 10;

/// How many R4 searches may be in flight at once.
///
/// The walk was strictly serial until 2026-08-24, which is fine at a
/// 4-query round share and is the wall-clock term at twenty. Like the
/// fetch leg (`fetch::FETCH_CONCURRENCY`) this is network I/O and not
/// daemon inference, so it does not share `AUDIT_CONCURRENCY`'s ceiling
/// (that number tracks the daemon's `max_concurrent_turns`) — but it is
/// not `FETCH_CONCURRENCY` either: every query in a wave hits the SAME
/// backend host, where the fetch walk fans out across many. One host is
/// the rate-limit surface, so the wave is deliberately narrower than
/// fetch's six.
///
/// The concurrency is confined to the port call. The budget decider
/// stays sequential — it is the single mutable owner of the run's
/// allowance, and a spend that races is a spend that cannot be audited.
/// `SpendDecider::allow` takes `&mut self`, so that is structural here
/// and not a convention to remember.
pub const SEARCH_CONCURRENCY: usize = 4;

/// One query's search against the round's source. Pure I/O through the
/// port: it touches no budget, no counter and no residue, which is what
/// makes it safe to run several at once.
///
/// This is a NAMED async fn and not an inline `async move` block for the
/// same reason `fetch::fetch_with_retries` is: the loop's future is
/// spawned behind `tokio::spawn` in the desktop command, and an inline
/// block capturing `&dyn ResearchPort` fails the higher-ranked `Send`
/// check ("implementation of `Send` is not general enough").
async fn search_one(
    port: &dyn ResearchPort,
    policy: &SearchPolicy,
    text: &str,
) -> Result<Vec<super::estate::PortHit>, String> {
    match policy.source {
        SearchSource::Mock | SearchSource::Web => port
            .web_search(&policy.web_backend, text, HITS_PER_QUERY)
            .await
            .map_err(|e| format!("web search: {e}")),
        SearchSource::Corpus => port
            .estate_search(&policy.estate_corpus_ids, text, HITS_PER_QUERY)
            .await
            .map_err(|e| format!("corpus search: {e}")),
    }
}

/// Where a round's searches are routed, and under which ledger key.
/// Frozen at launch — nothing here is re-read mid-round.
pub struct SearchPolicy {
    pub source: SearchSource,
    /// The budget-ledger key for this source (`source_budget_key`).
    pub source_key: String,
    pub web_backend: String,
    pub estate_corpus_ids: Vec<String>,
}

/// What one round's walk produced. The caller owns the loop's counters
/// and residue; this returns them rather than reaching into them.
#[derive(Debug)]
pub struct SearchRoundOutcome {
    /// Every hit, in query order — the triage input.
    pub hits: Vec<SearchHit>,
    /// GAP-3: the searched-but-absent queries, recorded at the moment
    /// the empty result is known (never reconstructed later from the
    /// triage ledger, where the absence is lost).
    pub residue: Vec<ResidueRow>,
    /// Searches the decider actually allowed — the loop's `search_calls`.
    pub calls: u32,
}

/// Walk `queries` through the decider and the round's search source.
///
/// Ordering is preserved by construction: hits are zipped back onto the
/// wave by position (`buffered`, never `buffer_unordered`), so `hits`,
/// `residue` and therefore the triage ranking come out exactly as the
/// serial walk produced them.
///
/// A backend error ends the round, and it is the FIRST failing query in
/// wave order that reports it — the same error the serial walk would
/// have surfaced.
pub async fn search_round(
    port: &dyn ResearchPort,
    decider: &mut SpendDecider,
    round: u32,
    queries: &[FormedQuery],
    at_unix: i64,
    policy: &SearchPolicy,
) -> Result<SearchRoundOutcome, String> {
    use futures::StreamExt as _;

    let mut out = SearchRoundOutcome {
        hits: Vec::new(),
        residue: Vec::new(),
        calls: 0,
    };
    let mut qi = 0usize;
    while qi < queries.len() {
        // ---- Phase A: admit a wave. The decider, no network.
        let mut wave: Vec<&FormedQuery> = Vec::new();
        while wave.len() < SEARCH_CONCURRENCY && qi < queries.len() {
            let query = &queries[qi];
            qi += 1;
            let verdict = decider
                .allow(FAMILY_WEB_SEARCH, &policy.source_key, 1, at_unix)
                .await?;
            if !verdict.allowed() {
                continue;
            }
            out.calls += 1;
            wave.push(query);
        }
        if wave.is_empty() {
            continue;
        }
        tracing::debug!(
            target: "deep_research",
            round,
            wave = wave.len(),
            width = SEARCH_CONCURRENCY,
            source = policy.source.as_str(),
            "R4 search wave admitted"
        );

        let wave_texts: Vec<String> = wave.iter().map(|q| q.text.clone()).collect();
        // ---- Phase B: the backend, concurrently. The port call touches
        // no budget, no counter and no residue, which is what makes it
        // safe to run several at once. `buffered`, NOT
        // `buffer_unordered` — Phase C zips these back onto `wave` by
        // position.
        let results: Vec<Result<Vec<super::estate::PortHit>, String>> = futures::stream::iter(
            // OWNED items, not `.iter()` — see the same note on the
            // fetch leg: a borrowed item gives the closure a
            // higher-ranked lifetime and the spawned run future
            // then fails its `Send` check.
            wave_texts
                .into_iter()
                .map(|text| async move { search_one(port, policy, &text).await }),
        )
        .buffered(SEARCH_CONCURRENCY)
        .collect()
        .await;

        // ---- Phase C: process in wave order. Sequential again.
        for (query, hits) in wave.into_iter().zip(results.into_iter()) {
            let hits = hits?;
            if hits.is_empty() {
                out.residue.push(ResidueRow {
                    query: query.text.clone(),
                    round,
                });
            }
            for h in hits {
                out.hits.push(SearchHit {
                    id: h.id.clone(),
                    query_id: query.id.clone(),
                    url: h.url,
                    title: h.title,
                    snippet: h.snippet,
                    // The body carries through (t1h — the triage
                    // decider reads it over the snippet cut).
                    content: h.content,
                    engine: policy.source.as_str().to_string(),
                    score: h.score,
                    custody: h.custody.as_str().to_string(),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deep_research::estate::{AlignmentDecision, DraftLeg, EstateListing, PortHit};
    use crate::deep_research::icd::Plan;
    use crate::types::Custody;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// A port whose search LATENCY is inverted against query order: the
    /// first query of a wave waits longest, the last waits least, so a
    /// reshuffling combinator cannot pass by luck. Records the order the
    /// port was ENTERED (proving the wave overlapped) alongside the
    /// order it RETURNED.
    struct InvertedLatencyPort {
        entered: Arc<Mutex<Vec<String>>>,
        /// Queries whose search answers EMPTY (the residue shape).
        empty_for: Vec<String>,
        /// Millis the first query waits; each later one waits less.
        head_delay_ms: u64,
    }

    #[async_trait::async_trait]
    impl ResearchPort for InvertedLatencyPort {
        async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
            unimplemented!("unreachable: search_round calls only web_search / estate_search")
        }
        async fn estate_search(
            &self,
            _c: &[String],
            q: &str,
            l: usize,
        ) -> Result<Vec<PortHit>, String> {
            self.web_search("corpus", q, l).await
        }
        async fn web_search(&self, _b: &str, q: &str, _l: usize) -> Result<Vec<PortHit>, String> {
            self.entered.lock().unwrap().push(q.to_string());
            // "q-0" waits longest, "q-3" least — inverted against the
            // order the wave admitted them.
            let n: u64 = q
                .rsplit('-')
                .next()
                .and_then(|d| d.parse().ok())
                .unwrap_or(0);
            tokio::time::sleep(std::time::Duration::from_millis(
                self.head_delay_ms.saturating_sub(n * 8),
            ))
            .await;
            if self.empty_for.iter().any(|e| e == q) {
                return Ok(Vec::new());
            }
            Ok(vec![PortHit {
                id: format!("hit-for-{q}"),
                url: format!("https://example.test/{q}"),
                title: q.to_string(),
                snippet: String::new(),
                content: None,
                score: 1.0,
                source: "web:test".to_string(),
                custody: Custody::PublicWeb,
            }])
        }
        async fn web_fetch(&self, _url: &str) -> Result<String, String> {
            unimplemented!("unreachable")
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

    fn queries(n: usize) -> Vec<FormedQuery> {
        (0..n)
            .map(|i| FormedQuery {
                id: format!("q-{i}"),
                text: format!("q-{i}"),
                from_gap_id: None,
                formed_by: "test".to_string(),
                provider: "test".to_string(),
                corroboration: None,
            })
            .collect()
    }

    fn policy() -> SearchPolicy {
        SearchPolicy {
            source: SearchSource::Web,
            source_key: "web".to_string(),
            web_backend: "test".to_string(),
            estate_corpus_ids: Vec::new(),
        }
    }

    fn decider(tmp: &Path, allowance: u32) -> SpendDecider {
        SpendDecider::new(
            "r-search",
            "h",
            HashMap::from([(format!("{FAMILY_WEB_SEARCH}:web"), allowance)]),
            &tmp.join("budget-ledger.json"),
        )
        .unwrap()
    }

    /// RED-first (2026-08-24, the wave refactor): the walk's hits must
    /// come out in QUERY order even when the backend answers the wave
    /// out of order. The triage ranking, the residue and every artifact
    /// downstream are built from this sequence, so `buffered` (ordered)
    /// is correct and `buffer_unordered` would silently reshuffle the
    /// acquisition. Watched red with `buffer_unordered` substituted:
    /// the query_id sequence comes back reversed within each wave.
    #[tokio::test]
    async fn hits_come_back_in_query_order_when_the_backend_answers_backwards() {
        let entered = Arc::new(Mutex::new(Vec::new()));
        let port = InvertedLatencyPort {
            entered: entered.clone(),
            empty_for: Vec::new(),
            head_delay_ms: 40,
        };
        let tmp = tempfile::tempdir().unwrap();
        let mut d = decider(tmp.path(), 8);
        let qs = queries(8);
        let out = search_round(&port, &mut d, 1, &qs, 1234, &policy())
            .await
            .unwrap();

        // The load-bearing assertion is the PAIRING, not the id order:
        // the walk stamps `query_id` from the wave position, so a
        // reshuffled result set still comes out with ordered ids — and
        // every hit attributed to the wrong query. Assert the hit the
        // backend produced FOR this query landed ON this query.
        let want: Vec<String> = (0..8).map(|i| format!("q-{i}")).collect();
        let paired: Vec<(&str, &str)> = out
            .hits
            .iter()
            .map(|h| (h.query_id.as_str(), h.id.as_str()))
            .collect();
        let expected: Vec<(String, String)> = want
            .iter()
            .map(|q| (q.clone(), format!("hit-for-{q}")))
            .collect();
        assert_eq!(
            paired,
            expected
                .iter()
                .map(|(q, h)| (q.as_str(), h.as_str()))
                .collect::<Vec<_>>(),
            "the walk's hits must be zipped back by position — a reshuffled \
             wave attributes every hit to the wrong query, and the triage \
             input, the source registry and the citations are built from it"
        );
        // The wave really did overlap: the whole first wave enters the
        // port before any of it returns, so the ENTERED order is the
        // admit order while the completion order is its inverse.
        let entered = entered.lock().unwrap().clone();
        assert_eq!(entered.len(), 8, "every admitted query reached the backend");
        assert_eq!(
            &entered[..SEARCH_CONCURRENCY],
            &want[..SEARCH_CONCURRENCY],
            "the first wave enters the backend together, in admit order"
        );
    }

    /// The wave is bounded: no more than `SEARCH_CONCURRENCY` searches
    /// are ever in flight. Measured by the port itself — it counts its
    /// own live callers and records the high-water mark.
    #[tokio::test]
    async fn no_more_than_the_wave_width_is_ever_in_flight() {
        struct GaugePort {
            live: Arc<Mutex<(usize, usize)>>,
        }
        #[async_trait::async_trait]
        impl ResearchPort for GaugePort {
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
            async fn web_search(
                &self,
                _b: &str,
                _q: &str,
                _l: usize,
            ) -> Result<Vec<PortHit>, String> {
                {
                    let mut g = self.live.lock().unwrap();
                    g.0 += 1;
                    g.1 = g.1.max(g.0);
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                self.live.lock().unwrap().0 -= 1;
                Ok(Vec::new())
            }
            async fn web_fetch(&self, _u: &str) -> Result<String, String> {
                unimplemented!("unreachable")
            }
            async fn terminal_poll(&self) -> Result<(), String> {
                Ok(())
            }
            async fn draft(
                &self,
                _l: DraftLeg,
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
        let live = Arc::new(Mutex::new((0usize, 0usize)));
        let port = GaugePort { live: live.clone() };
        let tmp = tempfile::tempdir().unwrap();
        let mut d = decider(tmp.path(), 12);
        let qs = queries(12);
        search_round(&port, &mut d, 1, &qs, 1234, &policy())
            .await
            .unwrap();
        let peak = live.lock().unwrap().1;
        assert!(
            peak > 1,
            "the walk must actually overlap the network — peak in-flight was {peak}"
        );
        assert!(
            peak <= SEARCH_CONCURRENCY,
            "the wave must be bounded at {SEARCH_CONCURRENCY} — peak in-flight was {peak}"
        );
    }

    /// The decider is the single mutable owner of the allowance and
    /// never sees concurrency: it spends exactly the admitted count,
    /// stops at the allowance, and the queries past it reach no backend
    /// at all. A wave that raced the meter could overspend it.
    #[tokio::test]
    async fn the_allowance_bounds_the_walk_and_the_overflow_never_reaches_the_backend() {
        let entered = Arc::new(Mutex::new(Vec::new()));
        let port = InvertedLatencyPort {
            entered: entered.clone(),
            empty_for: Vec::new(),
            head_delay_ms: 12,
        };
        let tmp = tempfile::tempdir().unwrap();
        // Six units of allowance against ten queries — one full wave,
        // then a half wave, then refusals.
        let mut d = decider(tmp.path(), 6);
        let qs = queries(10);
        let out = search_round(&port, &mut d, 1, &qs, 1234, &policy())
            .await
            .unwrap();

        assert_eq!(out.calls, 6, "the decider allowed exactly the allowance");
        assert_eq!(out.hits.len(), 6, "one hit per allowed query");
        assert_eq!(
            entered.lock().unwrap().len(),
            6,
            "a refused query spends nothing AND reaches no backend"
        );
        assert_eq!(
            d.remaining(FAMILY_WEB_SEARCH, "web"),
            0,
            "the meter is exact — a raced spend would not land here"
        );
        let got: Vec<&str> = out.hits.iter().map(|h| h.query_id.as_str()).collect();
        assert_eq!(got, vec!["q-0", "q-1", "q-2", "q-3", "q-4", "q-5"]);
    }

    /// GAP-3: an empty result is report content, recorded in wave order
    /// at the moment it is known. The empty queries here are the ones
    /// the backend answers FIRST, so an unordered walk would file them
    /// ahead of themselves.
    #[tokio::test]
    async fn searched_but_absent_queries_are_recorded_in_wave_order() {
        let port = InvertedLatencyPort {
            entered: Arc::new(Mutex::new(Vec::new())),
            // q-1 and q-3 answer empty; q-3 is also the fastest of the
            // first wave, so it completes before q-1.
            empty_for: vec!["q-1".to_string(), "q-3".to_string()],
            head_delay_ms: 40,
        };
        let tmp = tempfile::tempdir().unwrap();
        let mut d = decider(tmp.path(), 6);
        let qs = queries(6);
        let out = search_round(&port, &mut d, 1, &qs, 1234, &policy())
            .await
            .unwrap();

        let residue: Vec<&str> = out.residue.iter().map(|r| r.query.as_str()).collect();
        assert_eq!(
            residue,
            vec!["q-1", "q-3"],
            "the residue records absence in the order the round searched it"
        );
        assert!(
            out.residue.iter().all(|r| r.round == 1),
            "every residue row carries the round that searched it"
        );
        assert_eq!(out.calls, 6, "an empty result still spent its search");
        assert_eq!(out.hits.len(), 4, "the four answering queries carried hits");
    }

    /// A backend error ends the round, and it is the FIRST failing query
    /// in wave order that reports it — the same error the serial walk
    /// would have surfaced, not whichever future happened to lose.
    #[tokio::test]
    async fn the_first_failing_query_in_wave_order_reports_the_error() {
        struct FailingPort;
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
            async fn web_search(
                &self,
                _b: &str,
                q: &str,
                _l: usize,
            ) -> Result<Vec<PortHit>, String> {
                // q-1 and q-2 both fail; q-2 fails FIRST in wall-clock
                // (it sleeps less), but q-1 is first in wave order.
                let n: u64 = q
                    .rsplit('-')
                    .next()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_millis(40 - n * 8)).await;
                if q == "q-1" || q == "q-2" {
                    return Err(format!("backend down for {q}"));
                }
                Ok(Vec::new())
            }
            async fn web_fetch(&self, _u: &str) -> Result<String, String> {
                unimplemented!("unreachable")
            }
            async fn terminal_poll(&self) -> Result<(), String> {
                Ok(())
            }
            async fn draft(
                &self,
                _l: DraftLeg,
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
        let tmp = tempfile::tempdir().unwrap();
        let mut d = decider(tmp.path(), 4);
        let qs = queries(4);
        let err = search_round(&FailingPort, &mut d, 1, &qs, 1234, &policy())
            .await
            .expect_err("a backend error ends the round");
        assert!(
            err.contains("q-1"),
            "the FIRST failing query in wave order must report — got: {err}"
        );
    }
}
