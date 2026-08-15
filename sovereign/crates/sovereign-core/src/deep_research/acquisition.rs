// SPDX-License-Identifier: AGPL-3.0-or-later
//! R4 + R5 — query forming (G, cheap) and triage as RANKER never
//! excluder.
//!
//! Query forming: every gap's `actionable_query` is the round's query
//! (formed deterministically at audit time — reproducible, zero model
//! tokens). `form_queries` materializes the ICD rows and records who
//! formed each query.
//!
//! Triage (R5): rank the round's search hits by score; the code-set K
//! is the top-K (or all when fewer); an ε-quota of below-cut fetches is
//! admitted by rank — the cut is a rank boundary, never an exclusion.
//! Every skipped hit lands in the skip ledger ICD (F25): the ledger is
//! the answer to "what did the loop see and not fetch, and why?".

use super::icd::{FetchList, FormedQuery, Gap, SearchHit, SkipEntry, SkipLedger, TriageOutcome};

/// Materialize the round's queries from its gaps. Cheap and
/// deterministic: the gap's actionable query IS the query. `G` (the
/// provider) is deliberately not consulted — a reproducible thin loop
/// spends tokens on judgment, not on re-forming text it already wrote.
///
/// `preplanned` (t1d fix 2 — breadth): the plan's acquisition frontier,
/// appended AFTER the gap queries and formed_by "plan-subquestion". The
/// caller decides which rounds carry the frontier (the loop: round 1
/// only — the initial acquisition; rounds 2+ are gap-targeted
/// follow-ups).
pub fn form_queries(
    run_id: &str,
    charter_hash: &str,
    round: u32,
    gaps: &[Gap],
    preplanned: &[String],
) -> FetchList {
    let mut queries: Vec<FormedQuery> = gaps
        .iter()
        .enumerate()
        .map(|(i, g)| FormedQuery {
            id: format!("q{}", i + 1),
            text: g.actionable_query.clone(),
            from_gap_id: Some(g.id.clone()),
            formed_by: "gap-template".to_string(),
            provider: "deterministic".to_string(),
            // t1d fix 3: the floor's record rides the query into the
            // fetch list — the artifact is self-describing.
            corroboration: g.corroboration.clone(),
        })
        .collect();
    let mut next = queries.len() + 1;
    for q in preplanned {
        queries.push(FormedQuery {
            id: format!("q{next}"),
            text: q.clone(),
            from_gap_id: None,
            formed_by: "plan-subquestion".to_string(),
            provider: "deterministic".to_string(),
            corroboration: None,
        });
        next += 1;
    }
    FetchList {
        icd: "fetch_list".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        queries,
        search_hits: Vec::new(),
        triage: TriageOutcome {
            code_set_k: Vec::new(),
            eps_admits: Vec::new(),
            below_cut: Vec::new(),
            threshold: 0.0,
            eps_quota: 0.0,
        },
    }
}

/// The triage result for one round's hits.
#[derive(Debug, Clone)]
pub struct TriageResult {
    /// The ranked hits, code-set K first (the fetch list's search_hits
    /// order is the rank order).
    pub ranked: Vec<SearchHit>,
    pub outcome: TriageOutcome,
    pub skip_ledger: SkipLedger,
}

/// Rank the hits, cut at K, admit an ε-quota of below-cut fetches by
/// rank. The threshold recorded is the score of the last code-set
/// member (a rank boundary, not a semantic bar). Ties are broken by
/// insertion order — deterministic.
pub fn triage_hits(
    run_id: &str,
    charter_hash: &str,
    round: u32,
    mut hits: Vec<SearchHit>,
    k: usize,
    eps_quota: f64,
) -> TriageResult {
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    let k = k.min(hits.len());
    let code_set: Vec<SearchHit> = hits.iter().take(k).cloned().collect();
    let below_cut: Vec<SearchHit> = hits.iter().skip(k).cloned().collect();
    let threshold = code_set.last().map(|h| h.score).unwrap_or(0.0);

    let eps_budget = ((k as f64) * eps_quota).ceil() as usize;
    let eps_admits: Vec<SearchHit> = below_cut.iter().take(eps_budget).cloned().collect();

    // Skip ledger: every hit not in {code set ∪ ε admits} gets a row —
    // the ledger records the loop's judgment, not a silent drop.
    let mut entries = Vec::new();
    for (rank, hit) in hits.iter().enumerate() {
        let admitted = rank < k || eps_admits.iter().any(|a| a.id == hit.id);
        if admitted {
            continue;
        }
        let reason = if rank < k + eps_budget {
            "beyond-eps-quota".to_string()
        } else {
            "below-cut".to_string()
        };
        entries.push(SkipEntry {
            url: hit.url.clone(),
            title: hit.title.clone(),
            score: hit.score,
            rank: rank + 1,
            reason,
            decision: "skip".to_string(),
        });
    }

    let admitted_ids: Vec<String> = code_set
        .iter()
        .map(|h| h.id.clone())
        .chain(eps_admits.iter().map(|h| h.id.clone()))
        .collect();

    TriageResult {
        ranked: code_set
            .iter()
            .cloned()
            .chain(eps_admits.iter().cloned())
            .collect(),
        outcome: TriageOutcome {
            code_set_k: code_set.iter().map(|h| h.id.clone()).collect(),
            eps_admits: eps_admits.iter().map(|h| h.id.clone()).collect(),
            below_cut: below_cut.iter().map(|h| h.id.clone()).collect(),
            threshold,
            eps_quota,
        },
        skip_ledger: SkipLedger {
            icd: "skip_ledger".to_string(),
            version: super::icd::ICD_VERSION,
            run_id: run_id.to_string(),
            charter_hash: charter_hash.to_string(),
            round,
            entries,
        },
    }
}

/// Attach the round's search hits to the fetch list (rank order).
pub fn attach_hits(fetch_list: &mut FetchList, hits: Vec<SearchHit>) {
    fetch_list.search_hits = hits;
}

/// Recover the admitted hit ids (the fetch list's triage outcome).
pub fn admitted_ids(fetch_list: &FetchList) -> Vec<String> {
    fetch_list
        .triage
        .code_set_k
        .iter()
        .chain(fetch_list.triage.eps_admits.iter())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, score: f64) -> SearchHit {
        SearchHit {
            id: id.to_string(),
            query_id: "q1".to_string(),
            url: format!("https://example.com/{id}"),
            title: format!("title {id}"),
            snippet: String::new(),
            engine: "duckduckgo".to_string(),
            score,
        }
    }

    #[test]
    fn code_set_k_with_eps_quota() {
        let hits = vec![
            hit("h1", 0.9),
            hit("h2", 0.8),
            hit("h3", 0.7),
            hit("h4", 0.6),
        ];
        let r = triage_hits("run", "hash", 1, hits, 2, 0.5);
        // K = 2 → h1, h2; eps quota = ceil(2*0.5) = 1 → h3 admitted.
        assert_eq!(
            r.outcome.code_set_k,
            vec!["h1".to_string(), "h2".to_string()]
        );
        assert_eq!(r.outcome.eps_admits, vec!["h3".to_string()]);
        assert_eq!(r.ranked.len(), 3);
        assert_eq!(r.outcome.threshold, 0.8);
        // h4 skipped, ledger records it with a reason.
        assert_eq!(r.skip_ledger.entries.len(), 1);
        assert_eq!(r.skip_ledger.entries[0].url, "https://example.com/h4");
        assert_eq!(r.skip_ledger.entries[0].reason, "below-cut");
        assert_eq!(r.skip_ledger.entries[0].rank, 4);
    }

    #[test]
    fn no_hits_no_ledger() {
        let r = triage_hits("run", "hash", 1, vec![], 2, 0.5);
        assert!(r.ranked.is_empty());
        assert!(r.skip_ledger.entries.is_empty());
        assert_eq!(r.outcome.threshold, 0.0);
    }

    #[test]
    fn fewer_hits_than_k_takes_all() {
        let hits = vec![hit("h1", 0.5)];
        let r = triage_hits("run", "hash", 1, hits, 2, 0.5);
        assert_eq!(r.outcome.code_set_k, vec!["h1".to_string()]);
        assert!(r.skip_ledger.entries.is_empty());
    }

    #[test]
    fn queries_come_from_gaps_deterministically() {
        let gaps = vec![Gap {
            id: "g1".to_string(),
            text: "The Meridian Bridge was completed in 1873.".to_string(),
            actionable_query: "Meridian Bridge completion date 1873".to_string(),
            from_claim_id: Some("c2".to_string()),
            corroboration: None,
        }];
        let fl = form_queries("run", "hash", 2, &gaps, &[]);
        assert_eq!(fl.queries.len(), 1);
        assert_eq!(fl.queries[0].text, "Meridian Bridge completion date 1873");
        assert_eq!(fl.queries[0].formed_by, "gap-template");
        assert_eq!(fl.queries[0].from_gap_id.as_deref(), Some("g1"));
    }

    /// t1d fix 3 (second-origin): the floor's corroboration record
    /// rides the formed query into the fetch list — the artifact is
    /// self-describing (why this query: a capped claim's missing
    /// origin). Preplanned queries carry none.
    #[test]
    fn floor_record_rides_the_formed_query() {
        let record = crate::deep_research::icd::CorroborationRecord {
            origins: vec!["https://gym.example/one".to_string()],
            support_chunks: 1,
            floor: 2,
            passes_floor: false,
        };
        let gaps = vec![Gap {
            id: "g1".to_string(),
            text: "The Gini index rose to 0.55 by 2024.".to_string(),
            actionable_query: "0.55 Gini index rose 2024".to_string(),
            from_claim_id: Some("c1".to_string()),
            corroboration: Some(record.clone()),
        }];
        let fl = form_queries("run", "hash", 2, &gaps, &["preplanned query".to_string()]);
        assert_eq!(fl.queries.len(), 2);
        assert_eq!(fl.queries[0].corroboration, Some(record));
        assert_eq!(
            fl.queries[1].corroboration, None,
            "a preplanned query carries no floor record"
        );
        assert_eq!(fl.queries[1].formed_by, "plan-subquestion");
    }
}
