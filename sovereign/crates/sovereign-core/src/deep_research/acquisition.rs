// SPDX-License-Identifier: AGPL-3.0-or-later
//! R4 + R5 — query forming (G, cheap) and triage as RANKER never
//! excluder.
//!
//! Query forming: every gap's `actionable_query` is the round's query
//! (formed deterministically at audit time — reproducible, zero model
//! tokens). `form_queries` materializes the ICD rows and records who
//! formed each query. The figure-hunting step (order deep-research-t1e)
//! is the generic "what measures and numbers does this question
//! imply?" — the question's OWN figure specifiers (its digits and its
//! measure-family words), folded into any sub-question or gap query
//! that carries none. SHAPE, never the test: the bank's named measures
//! (Gini, Case-Shiller, 80/20, ...) never enter this lexicon or any
//! prompt — the model names those from its own knowledge, under a
//! generic instruction.
//!
//! Triage (R5): rank the round's search hits by score; the code-set K
//! is the top-K (or all when fewer); an ε-quota of below-cut fetches is
//! admitted by rank — the cut is a rank boundary, never an exclusion.
//! Admission favors figure-bearing hits (order deep-research-t1e): a
//! hit whose title or snippet carries a figure token outranks a
//! same-scored figure-less hit, so the K-cut does not silently exclude
//! the evidence the figures live in (the t1d journal's cap: wiki-
//! inequality rank 5 and brookings rank 7 cut at K=4 on the v1 flight,
//! all scores tied at 0.9). Every skipped hit lands in the skip ledger
//! ICD (F25): the ledger is the answer to "what did the loop see and
//! not fetch, and why?".

use super::figure_tokens;
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
            // t6f rung 2: gap-derived acquisition — use the gap text itself
            // as the search query. The gap phrasing is already search-shaped
            // by design (order deep-research-t6f).
            text: g.text.clone(),
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
            admission_rule: ADMISSION_RULE_SCORE_THEN_FIGURE.to_string(),
        },
    }
}

/// The generic measure-family lexicon — the CLOSED set of words that
/// name a measure or statistic (an index, a ratio, a share, a rate, a
/// count, a price, a median ...). Applied ONLY to the question's own
/// text: the figure-hunting step is shape ("what measures and numbers
/// does this question imply?"), never bank-derived. Direction words
/// (change, increase, decline) are deliberately absent — they describe
/// movement, not a measure. The bank's NAMED measures (Gini, Case-
/// Shiller, 80/20, white share, ...) never enter this list or any
/// prompt; naming those is the model's job under the generic
/// instruction (deep_research_cmd.rs plan_subquestions).
const MEASURE_WORDS: &[&str] = &[
    "index",
    "ratio",
    "share",
    "rate",
    "percent",
    "percentage",
    "median",
    "average",
    "mean",
    "count",
    "number",
    "price",
    "income",
    "earnings",
    "wage",
    "salary",
    "employment",
    "jobs",
    "population",
    "mobility",
    "cost",
    "rent",
    "poverty",
    "wealth",
    "proportion",
    "statistic",
    "metric",
    "estimate",
    "amount",
    "total",
    "level",
];

/// The question's OWN figure specifiers — the answer to "what measures
/// and numbers does this question imply?", read from the question's own
/// text: its figure tokens (digit runs, in text order) followed by its
/// measure-family words (MEASURE_WORDS ∩ question, in text order),
/// deduped. Deterministic C-class, zero model tokens, applied to the
/// question — never to any bank text. One decider, one name (§10.6):
/// every consumer (frontier fold-in, gap-query fold-in, the scorer's
/// presence measurement) reads THIS.
pub fn figure_specifiers(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in figure_tokens(question) {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    let lower = question.to_ascii_lowercase();
    for w in lower.split(|c: char| !c.is_alphanumeric()) {
        if MEASURE_WORDS.contains(&w) && !out.iter().any(|s| s.to_ascii_lowercase() == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// Does the text carry a figure specifier — a digit run or a
/// measure-family word (whole-word, case-insensitive)? The fold-in
/// rule's guard: a sub-question or query that already carries a
/// specifier stands as drafted; one that carries none gets the
/// question's specifiers folded in.
pub fn has_figure_specifier(text: &str) -> bool {
    if !figure_tokens(text).is_empty() {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| MEASURE_WORDS.contains(&w))
}

/// R1 fold-in (order deep-research-t1e): the acquisition frontier's
/// sub-questions are the round-1 queries; a sub-question that carries
/// NO figure specifier (no digit, no measure word) gets the question's
/// own specifiers folded in — the plan artifact's sub-questions carry
/// figure specifiers for a question whose own text implies figures,
/// structurally, whatever the draft returned. A sub-question that
/// already carries a specifier stands as drafted (the model's named
/// measures — Gini, ratio, Case-Shiller — are never overwritten).
pub fn figure_hunt_frontier(frontier: Vec<String>, question: &str) -> Vec<String> {
    let specs = figure_specifiers(question);
    if specs.is_empty() {
        return frontier;
    }
    frontier
        .into_iter()
        .map(|sub| {
            if has_figure_specifier(&sub) {
                sub
            } else {
                format!("{sub} ({})", specs.join(", "))
            }
        })
        .collect()
}

/// R4 fold-in: a gap query (the claim's prose template) that carries
/// no figure specifier gets the question's own specifiers appended —
/// a thematic claim's follow-up query still hunts the figures the
/// question implies, so the numbers never silently drop out of the
/// acquisition. The floor-capped FACT query already carries the
/// claim's figures and never passes through here.
pub fn figure_hunt_query(query: String, question_specifiers: &[String]) -> String {
    if has_figure_specifier(&query) || question_specifiers.is_empty() {
        query
    } else {
        format!("{query} ({})", question_specifiers.join(", "))
    }
}

/// The triage admission preference (R5, order deep-research-t1e): a
/// hit is figure-bearing when its own title, snippet, or BODY carries
/// a figure token — the evidence's figures are on the hit, and the
/// K-cut must not silently exclude the hits the figures live in
/// (the t1d journal's v1 shape: wiki-inequality and brookings cut at
/// rank 5 and 7, all scores tied at 0.9, insertion order deciding).
/// The body joined the decider in t1h — the corpus leg's boundary: the
/// corpus surface's titles are digit-free document names and its
/// snippets are term-centered 600-char cuts, so the body is where the
/// digits live (the t1g v1 flight's chunk 65, the Gini-bearing
/// source-report chunk, skipped at rank 6 — skip-ledger-1.json).
/// Deterministic, reuses the one figure-token decider.
pub fn figure_bearing(hit: &SearchHit) -> bool {
    !figure_tokens(&hit.title).is_empty()
        || !figure_tokens(&hit.snippet).is_empty()
        || hit
            .content
            .as_deref()
            .is_some_and(|c| !figure_tokens(c).is_empty())
}

/// The one admission rule's name, recorded on the triage outcome
/// (glassbox — the artifact names the decider it ran).
pub const ADMISSION_RULE_SCORE_THEN_FIGURE: &str = "score-then-figure-bearing";

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
/// member (a rank boundary, not a semantic bar). Ties break on
/// figure-bearing-ness first (t1e — the K-cut must not silently
/// exclude the hits the figures live in), then insertion order —
/// deterministic. The admission rule's name rides the outcome.
pub fn triage_hits(
    run_id: &str,
    charter_hash: &str,
    round: u32,
    mut hits: Vec<SearchHit>,
    k: usize,
    eps_quota: f64,
) -> TriageResult {
    hits.sort_by(|a, b| {
        b.score.total_cmp(&a.score).then_with(|| {
            let fb_a = figure_bearing(a);
            let fb_b = figure_bearing(b);
            fb_b.cmp(&fb_a)
        })
    });
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
            admission_rule: ADMISSION_RULE_SCORE_THEN_FIGURE.to_string(),
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
            // The title must carry NO digit: figure_bearing reads the
            // title, and the ids ("h1", "h2") leak digits — a digit
            // in the title would saturate the tie-break (every hit
            // figure-bearing, insertion order deciding again).
            title: format!(
                "title {}",
                id.chars()
                    .filter(|c| c.is_ascii_alphabetic())
                    .collect::<String>()
            ),
            snippet: String::new(),
            // The triage fixture carries no body by default — the
            // tests that exercise the body fill it explicitly.
            content: None,
            engine: "duckduckgo".to_string(),
            score,
            // The triage tests' fixture predates the t1g custody carry;
            // triage never reads the stamp.
            custody: String::new(),
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

    /// The figure-specifier extractor reads the question's OWN text —
    /// its digit runs and its generic measure-family words — and adds
    /// nothing the question lacks. Shape, never the test: no bank
    /// vocabulary appears in the lexicon (the bank's NAMED measures —
    /// Gini, Case-Shiller, 80/20, white share — are not generic
    /// measure words and never enter this module).
    #[test]
    fn figure_specifiers_come_from_the_question_own_text() {
        let q = "How did income inequality and housing affordability evolve \
                 across US cities from 1980 to 2024?";
        assert_eq!(
            figure_specifiers(q),
            vec!["1980".to_string(), "2024".to_string(), "income".to_string()]
        );
        // A question with no figures implies none.
        assert!(figure_specifiers("What happened in American cities?").is_empty());
        // A question that carries measure words keeps them (the
        // question's own words — the lexicon never adds a measure the
        // question lacks).
        let s2 = figure_specifiers("What is the price-to-income ratio in California?");
        assert!(s2.iter().any(|s| s == "price"));
        assert!(s2.iter().any(|s| s == "income"));
        assert!(s2.iter().any(|s| s == "ratio"));
        // has_figure_specifier: a digit or a measure word.
        assert!(has_figure_specifier("What was the ratio in 1980?"));
        assert!(has_figure_specifier("How did the index evolve?"));
        assert!(!has_figure_specifier(
            "What were the primary drivers of the change?"
        ));
    }

    /// RED-first (order deep-research-t1e — R5 admission): the K-cut
    /// stops cutting figure-bearing hits — admission ranking favors
    /// them.
    ///
    /// The HEAD failure shape (measured in the t1d battery,
    /// dr-1786754967): every v1 hit scored 0.9 (the deck's default),
    /// so insertion order decided the code-set K, and the
    /// figure-bearing hits (wiki-income's Gini 0.485, brookings'
    /// 95/20 9.3) were cut at ranks 5 and 7 while same-scored
    /// figure-less ties were admitted. The fixed ranker breaks ties on
    /// figure-bearing-ness first: a hit whose title or snippet carries
    /// a figure token outranks a same-scored figure-less hit, so the
    /// K-cut cannot silently exclude the hits the figures live in.
    /// Watch-it-fail: on the pre-fix shape (score-only sort) the
    /// figure-bearing hit sits below the cut by insertion order and
    /// the admission assertion fails.
    #[test]
    fn triage_favors_figure_bearing_hits() {
        let mut figureless_hit = |id: &str| hit(id, 0.9);
        let mut figure_bearing_hit = |id: &str| SearchHit {
            snippet: "The Gini index reached 0.485 in 2018.".to_string(),
            ..hit(id, 0.9)
        };
        // All ties at 0.9 — the deck's default — with the figure-
        // bearing hit at insertion position 3, beyond K=2.
        let hits = vec![
            figureless_hit("h1"),
            figureless_hit("h2"),
            figure_bearing_hit("h3"),
            figureless_hit("h4"),
        ];
        let r = triage_hits("run", "hash", 1, hits, 2, 0.0);
        assert!(
            r.outcome.code_set_k.iter().any(|id| id == "h3"),
            "the figure-bearing hit must be admitted into the code-set K, \
             not cut by insertion order: {:?}",
            r.outcome.code_set_k
        );
        assert_eq!(
            r.outcome.admission_rule, ADMISSION_RULE_SCORE_THEN_FIGURE,
            "the outcome records the admission rule it ran"
        );
        // A lower-scored figure-bearing hit does NOT outrank a
        // higher-scored figure-less hit — the preference breaks ties,
        // it never overrides score.
        let hits = vec![
            figureless_hit("h1"), // 0.9, figure-less
            {
                let mut h = figure_bearing_hit("h2");
                h.score = 0.8;
                h
            },
        ];
        let r = triage_hits("run", "hash", 1, hits, 1, 0.0);
        assert_eq!(
            r.outcome.code_set_k,
            vec!["h1".to_string()],
            "score still decides first — the figure preference is a tie-break"
        );
    }

    /// RED (order deep-research-t1h, H1 — the corpus-leg triage
    /// boundary, pre-registered in adversarial/pre-registration.md):
    /// "a corpus hit whose BODY carries the figure-bearing digit but
    /// whose title does not is admitted by the triage ahead of
    /// figure-free hits". The corpus surface's titles are digit-free
    /// document names and its snippets are term-centered 600-char cuts
    /// (gym.rs estate_search) — the body is the only digit carrier,
    /// and inside LanceDB's quantized f32 top bucket the tie must not
    /// fall to insertion order. The t1g v1 flight's chunk 65 (the
    /// source-report chunk carrying Gini 0.5469) lost exactly this
    /// boundary at rank 6 (skip-ledger-1.json, below-cut).
    /// Watched red: fails at HEAD — figure_bearing reads title+snippet
    /// only, the body is invisible, insertion order decides.
    #[test]
    fn triage_admits_body_figure_over_figure_free_at_equal_score() {
        // The corpus-surface shape: digit-free title, term-centered
        // digit-free snippet cut, digit-bearing BODY.
        let mut body_figure = hit("c65", 0.03333333507180214);
        body_figure.snippet =
            "urban areas generate substantial wealth and attract educated".to_string();
        body_figure.content = Some(
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string(),
        );
        // The fully figure-free hit arrives FIRST — insertion order
        // must not decide inside the score tie.
        let figure_free = hit("c40", 0.03333333507180214);
        let r = triage_hits("run", "hash", 1, vec![figure_free, body_figure], 1, 0.0);
        assert_eq!(
            r.outcome.code_set_k,
            vec!["c65".to_string()],
            "the body-figure hit must win the tie over the figure-free hit"
        );
        assert_eq!(r.ranked[0].id, "c65");
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
        // t6f rung 2: gap text is now used as the query (gap phrasing is search-shaped)
        assert_eq!(fl.queries[0].text, "The Meridian Bridge was completed in 1873.");
        assert_eq!(fl.queries[0].formed_by, "gap-template");
        assert_eq!(fl.queries[0].from_gap_id.as_deref(), Some("g1"));
    }

    /// t6f rung 2: gap-derived acquisition — round N+1's search queries
    /// are composed from the gap ledger's open texts (the gap text itself,
    /// not the actionable_query template). The gap phrasing is already
    /// search-shaped by design (order: deep-research-t6f).
    #[test]
    fn gap_derived_queries_use_gap_text() {
        let gaps = vec![Gap {
            id: "g1".to_string(),
            text: "What year did the Meridian Bridge open?".to_string(),
            actionable_query: "Meridian Bridge completion year".to_string(),
            from_claim_id: Some("c2".to_string()),
            corroboration: None,
        }];
        let fl = form_queries("run", "hash", 2, &gaps, &[]);
        assert_eq!(fl.queries.len(), 1);
        // The gap text itself is the query (search-shaped by design)
        assert_eq!(fl.queries[0].text, "What year did the Meridian Bridge open?");
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
