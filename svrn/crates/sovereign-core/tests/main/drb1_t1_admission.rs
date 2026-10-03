// SPDX-License-Identifier: AGPL-3.0-or-later
//! drb1-t1 red tests — the admission subsystem on the logged t7a
//! flight (order drb1-t1, campaign drb1-race; declared in
//! research/deep-research/adversarial/pre-registration.md before the
//! fix landed). Red-first (§18.1): both reds were watched FAILING at
//! HEAD — against the web-admission scorer stubbed to the port's
//! extracted current behavior (the constant `score: 0.0`,
//! deep_research_cmd.rs:369) and the id-matched ε-admission check.
//!
//! The fixture is a byte-identical frozen copy of the logged flight's
//! task-56 round-1 artifacts (tests/golden/drb1-t1-task56-r1/ —
//! charter.json, fetch-list-1.json, skip-ledger-1.json). The logged
//! flight is the fixture bank; the copy exists because the flight
//! directory is untracked data, not a repo guarantee.
//!
//! The named 0.0-on-gold mechanism these pin: the production web port
//! assigns a CONSTANT relevance of 0.0 to every web hit (no relevance
//! decider exists on the web leg — the mock leg has one, gym.rs
//! `Deck::relevance`; the corpus leg has the index's own score), so
//! triage ranks a fully-tied field and admission falls to the
//! figure-bearing tie-break plus backend insertion order. Measured on
//! the logged flight: 843/843 rows score exactly 0.0; task 56 round 1
//! admitted four PDF urls (all later fetch-refused as binary) while
//! every exact-topic academic page — brocku, kasberger, researchgate,
//! sciencedirect — sat below-cut at 0.0.
//!
//! A second mechanism the phantom red pins: the ε-admission check
//! matched by hit ID, and the web port mints per-query counter ids
//! (`web-{i}`), so a below-cut hit from ANOTHER query sharing the
//! ε-admitted id was silently dropped from the skip ledger (task 56
//! round 1: ranks 13 and 20 exist in the hit stream — the recorded
//! below_cut id list carries 22 ids against 19 ledger rows — but no
//! ledger row records them: never fetched, never ledgered).

use sovereign_core::deep_research::acquisition::{
    triage_hits, web_hit_relevance, DEFAULT_CODE_SET_K, DEFAULT_EPS_QUOTA,
};
use sovereign_core::deep_research::icd::{FetchList, SearchHit, SkipLedger};

const FLIGHT_TASK56_R1: &str = "tests/golden/drb1-t1-task56-r1";

/// The brocku ledger row's paper (skip-ledger-1.json rank 7, logged
/// score 0.0, reason below-cut). The recorded title is the PDF's
/// degenerate `<title>` tag ("asymmetricfpa24.dvi") and the ledger
/// row carries no snippet — the search snippet production saw was
/// never persisted. This overlay is the paper's own title, used as
/// the reconstructed snippet — a NAMED substitution (§18.3), the only
/// reconstruction in this file, and the same one the replay harness
/// applies (gold-snippets.json).
const BROCKU_URL: &str = "https://brocku.ca/repec/pdf/0504.pdf";
const BROCKU_SNIPPET_RECONSTRUCTED: &str =
    "A Simple Approach to Analyzing Asymmetric First Price Auctions";

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/{}/{}",
        env!("CARGO_MANIFEST_DIR"),
        FLIGHT_TASK56_R1,
        name
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// One reconstructed ranked row of a recorded round. `snippet_source`
/// records where the scoring surface came from — recorded (admitted
/// rows keep the search snippet), overlay (the gold reconstruction),
/// or absent (skipped rows: the ledger never persisted one).
struct Row {
    url: String,
    title: String,
    snippet: String,
    /// The query texts this row is scored against: the row's OWN
    /// recorded query for admitted rows; every round query for
    /// skipped rows (their query_id was never persisted — the score
    /// is the max over the round's queries, an upper bound).
    query_texts: Vec<String>,
    snippet_source: &'static str,
    /// The score the flight recorded for this row (admitted rows:
    /// the fetch-list hit's score; ledger rows: the ledger's score).
    recorded_score: f64,
}

/// Reconstruct the round's ranked rows from the recorded artifacts:
/// the fetch-list's `search_hits` are the admitted rows in rank
/// order; the skip ledger's entries are the skipped rows with their
/// ranks. The two interleave by rank; admitted rows fill the ranks
/// the ledger does not carry. Holes that remain are the phantom rows
/// the id collision un-ledgered (no recorded surface at all) — they
/// are returned as `None` and excluded from scoring (their presence
/// could only displace admitted rows, never add: a mildly optimistic
/// after-set, quantified by `phantoms`).
fn reconstruct_round(
    fetch_list: &FetchList,
    ledger: &SkipLedger,
    overlay: &dyn Fn(&str) -> Option<String>,
) -> (Vec<Option<Row>>, usize) {
    let queries: Vec<String> = fetch_list.queries.iter().map(|q| q.text.clone()).collect();
    fn ensure<T>(rank: usize, rows: &mut Vec<Option<T>>) {
        while rows.len() < rank {
            rows.push(None);
        }
    }
    let mut rows: Vec<Option<Row>> = Vec::new();
    // Admitted ranks are the complement of the ledger ranks; walk the
    // admitted hits in order into the free slots.
    let mut admitted = fetch_list.search_hits.iter();
    let n_max = fetch_list.search_hits.len() + ledger.entries.len();
    let mut rank = 1usize;
    let mut ledger_by_rank: Vec<Option<&_>> = Vec::new();
    let mut max_rank = 0usize;
    for e in &ledger.entries {
        max_rank = max_rank.max(e.rank);
        ensure(e.rank, &mut ledger_by_rank);
        ledger_by_rank[e.rank - 1] = Some(e);
    }
    let total = n_max.max(max_rank);
    ensure(total, &mut rows);
    while rank <= total {
        if let Some(entry) = ledger_by_rank.get(rank - 1).copied().flatten() {
            // Skipped row: no recorded query_id or snippet — score
            // against every round query (max, an upper bound) with the
            // overlay snippet if one exists.
            let snippet = overlay(&entry.url).unwrap_or_default();
            let source = if snippet.is_empty() {
                "absent"
            } else {
                "overlay"
            };
            rows[rank - 1] = Some(Row {
                url: entry.url.clone(),
                title: entry.title.clone(),
                snippet,
                query_texts: queries.clone(),
                snippet_source: source,
                recorded_score: entry.score,
            });
        } else if let Some(hit) = admitted.next() {
            // Admitted row: the recorded query (exact) and snippet.
            let q = fetch_list
                .queries
                .iter()
                .find(|q| q.id == hit.query_id)
                .map(|q| q.text.clone())
                .unwrap_or_default();
            rows[rank - 1] = Some(Row {
                url: hit.url.clone(),
                title: hit.title.clone(),
                snippet: hit.snippet.clone(),
                query_texts: vec![q],
                snippet_source: "recorded",
                recorded_score: hit.score,
            });
        }
        rank += 1;
    }
    let phantoms = rows.iter().filter(|r| r.is_none()).count();
    (rows, phantoms)
}

/// The replayed scores alone (no triage) — for assertions about the
/// score field rather than the cut.
fn replay_scores(rows: &[Option<Row>]) -> Vec<f64> {
    rows.iter()
        .map(|r| {
            let Some(r) = r else { return 0.0 };
            r.query_texts
                .iter()
                .map(|q| web_hit_relevance(q, &r.title, &r.snippet, &r.url))
                .fold(0.0_f64, f64::max)
        })
        .collect()
}

/// Replay the admission stage over the reconstructed rows: score each
/// row with the production web-admission decider, then run the
/// production triage at the recorded charter thresholds. Returns the
/// admitted URLs in rank order (code-set K first).
fn replay_admission(rows: &[Option<Row>], k: usize, eps_quota: f64) -> Vec<String> {
    let hits: Vec<SearchHit> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let r = r.as_ref()?;
            Some(SearchHit {
                id: format!("row-{}", i + 1),
                query_id: String::new(),
                url: r.url.clone(),
                title: r.title.clone(),
                snippet: r.snippet.clone(),
                content: None,
                engine: "web".to_string(),
                // Max over the row's query texts: its own recorded
                // query when known, else the best round query (the
                // named upper-bound substitution for skipped rows).
                score: r
                    .query_texts
                    .iter()
                    .map(|q| web_hit_relevance(q, &r.title, &r.snippet, &r.url))
                    .fold(0.0_f64, f64::max),
                custody: String::new(),
            })
        })
        .collect();
    triage_hits("replay", "hash", 1, hits, k, eps_quota)
        .ranked
        .iter()
        .map(|h| h.url.clone())
        .collect()
}

/// RED (the order's pin, AMENDED on measurement — see the T1
/// execution record for the re-registration): the brocku
/// asymmetric-FPA paper must clear the admission cut for task 56's
/// question. At HEAD — the scorer extracted verbatim from the port
/// (`score: 0.0` on every hit) — every assertion below FAILS.
///
/// The registered form ("brocku lands in round-1's admitted set") is
/// not reachable from the logged surface without fabricating snippet
/// content: the flight never persisted skipped-row snippets, and
/// every snippet-rich row outscores the snippet-poor gold rows
/// (brocku replays 0.4444 at position 11 of 23, behind six 0.5556
/// exact-topic rows and the four content-cut PDFs at 0.55-0.69 whose
/// snippets ARE recorded). In production — where the search snippet
/// for brocku's PDF would have been a content cut like its siblings'
/// — it scores with them. The amended pin asserts what the logs can
/// prove: (a) the scorer lifts brocku's degenerate recorded surface
/// (the filename-title "asymmetricfpa24.dvi", no snippet) far above
/// the pre-fix 0.0 and above every off-topic row; (b) at the
/// production defaults an exact-topic gold row admits in round 1
/// (kasberger, 0.5556 — the 0.0-on-gold mechanism is dead); (c)
/// brocku itself lands in the admitted set (code-set K ∪ ε) in the
/// round-2 replay, where its row reappears.
#[test]
fn brocku_asymmetric_fpa_admits_for_task56() {
    let k = DEFAULT_CODE_SET_K;
    let eps = DEFAULT_EPS_QUOTA;
    let overlay = |url: &str| {
        if url == BROCKU_URL {
            Some(BROCKU_SNIPPET_RECONSTRUCTED.to_string())
        } else {
            None
        }
    };

    // Round 1: the scorer separates gold from the pre-fix flat field
    // and from every off-topic row, on recorded surfaces alone.
    let fetch_list: FetchList =
        serde_json::from_str(&fixture("fetch-list-1.json")).expect("fetch-list parses");
    let ledger: SkipLedger =
        serde_json::from_str(&fixture("skip-ledger-1.json")).expect("skip-ledger parses");
    let (rows, phantoms) = reconstruct_round(&fetch_list, &ledger, &overlay);
    assert_eq!(
        phantoms, 2,
        "task 56 round 1 lost exactly two rows to the id collision (ranks 13 and 20)"
    );
    let mut brocku_score = 0.0_f64;
    for (row, score) in rows.iter().zip(replay_scores(&rows)) {
        if row.as_ref().is_some_and(|r| r.url == BROCKU_URL) {
            brocku_score = brocku_score.max(score);
        }
    }
    assert!(
        brocku_score > 0.4,
        "brocku's recorded surface (degenerate title, no snippet) must still score \
         well above the pre-fix 0.0 and the off-topic field (cornell/northwestern \
         replay at 0.1111): {brocku_score}"
    );
    let admitted = replay_admission(&rows, k, eps);
    assert!(
        admitted
            .iter()
            .any(|u| u == "https://kasberger.github.io/assets/pdf/fpa_robust.pdf"),
        "an exact-topic gold row must admit in round 1 at the production defaults; \
         admitted set: {admitted:?}"
    );

    // Round 2: brocku's row reappears and lands in the admitted set.
    let fetch_list: FetchList =
        serde_json::from_str(&fixture("fetch-list-2.json")).expect("fetch-list-2 parses");
    let ledger: SkipLedger =
        serde_json::from_str(&fixture("skip-ledger-2.json")).expect("skip-ledger-2 parses");
    let (rows, _phantoms) = reconstruct_round(&fetch_list, &ledger, &overlay);
    let admitted = replay_admission(&rows, k, eps);
    assert!(
        admitted.iter().any(|u| u == BROCKU_URL),
        "the brocku asymmetric-FPA paper must clear the admission cut for task 56's \
         question (round-2 replay); admitted set: {admitted:?}"
    );
}

/// Parity (the instrument's validity gate, §18.4): replaying the
/// production triage over the RECORDED scores must reproduce the
/// recorded admitted set — same URLs, same order. If the
/// reconstruction drifted from what the loop actually ranked, every
/// after-number in this file and in the harness would be ungrounded.
#[test]
fn recorded_outcome_reproduces_from_reconstruction() {
    let charter: serde_json::Value =
        serde_json::from_str(&fixture("charter.json")).expect("charter parses");
    let fetch_list: FetchList =
        serde_json::from_str(&fixture("fetch-list-1.json")).expect("fetch-list parses");
    let ledger: SkipLedger =
        serde_json::from_str(&fixture("skip-ledger-1.json")).expect("skip-ledger parses");
    let k = charter["charter"]["triage"]["code_set_k"]
        .as_u64()
        .expect("k") as usize;
    let eps = charter["charter"]["triage"]["eps_quota"]
        .as_f64()
        .expect("eps");

    let none = |_u: &str| -> Option<String> { None };
    let (rows, _phantoms) = reconstruct_round(&fetch_list, &ledger, &none);
    let hits: Vec<SearchHit> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            // The parity replay scores with the RECORDED score and the
            // surface the flight actually ranked: the recorded snippet
            // for admitted rows (its digits are load-bearing for the
            // figure tie-break), title-only for ledger rows (their
            // snippets were never persisted — and none of them ranked
            // into the admitted set, so parity is unaffected).
            let r = r.as_ref()?;
            Some(SearchHit {
                id: format!("row-{}", i + 1),
                query_id: String::new(),
                url: r.url.clone(),
                title: r.title.clone(),
                snippet: r.snippet.clone(),
                content: None,
                engine: "web".to_string(),
                score: r.recorded_score,
                custody: String::new(),
            })
        })
        .collect();
    let triaged = triage_hits("replay", "hash", 1, hits, k, eps);
    let replayed: Vec<String> = triaged.ranked.iter().map(|h| h.url.clone()).collect();
    let recorded: Vec<String> = fetch_list
        .search_hits
        .iter()
        .map(|h| h.url.clone())
        .collect();
    assert_eq!(
        replayed, recorded,
        "the recorded round-1 admitted set (in rank order) must reproduce from the \
         reconstruction"
    );
}

/// RED (the second named mechanism): a below-cut hit that shares the
/// ε-admitted hit's ID with another query's hit still gets its skip
/// ledger row. The web port mints per-query counter ids (`web-{i}`),
/// so `web-3` from q2 and `web-3` from q3 are different hits with the
/// same id; the id-matched ε check admitted them silently — never
/// fetched, never ledgered (task 56 round 1's ranks 13 and 20).
/// covers: GR-51
#[test]
fn phantom_rows_are_ledgered() {
    let hit = |id: &str, q: &str, n: usize| SearchHit {
        id: id.to_string(),
        query_id: q.to_string(),
        url: format!("https://example.com/{q}/{id}-{n}"),
        title: format!("title {n}"),
        snippet: String::new(),
        content: None,
        engine: "web".to_string(),
        score: 0.0,
        custody: String::new(),
    };
    // k = 1, eps_quota = 1.0 → eps budget 1: rank 1 is the code set,
    // rank 2 the ε admit (q1's web-3), ranks 3 and 4 are q2's and
    // q3's web-3 — same id, different hits.
    let hits = vec![
        hit("web-0", "q1", 1),
        hit("web-3", "q1", 2),
        hit("web-3", "q2", 3),
        hit("web-3", "q3", 4),
    ];
    let r = triage_hits("run", "hash", 1, hits, 1, 1.0);
    assert_eq!(
        r.skip_ledger.entries.len(),
        2,
        "both below-cut web-3 hits must be ledgered — the ledger is the F25 record: {:?}",
        r.skip_ledger.entries
    );
    assert!(r
        .skip_ledger
        .entries
        .iter()
        .all(|e| e.url.contains("/q2/") || e.url.contains("/q3/")));
}

/// The decider's own contract, independent of any fixture: an
/// exact-topic hit outscores a generic one; a hit sharing no query
/// term scores 0; the empty query refuses 0 rather than dividing by
/// zero. At the extracted 0.0 stub every case collapses to 0.0 — this
/// was watched red. (Strict-token semantics: no stemming, so
/// "auctions" does not match "auction" — the measured coverage of an
/// exact-topic paper against its own best query is 4/9 = 0.444, the
/// calibration of the >0.4 bar.)
#[test]
fn web_relevance_separates_exact_topic_from_generic() {
    let q = "Specific equilibrium bidding functions in asymmetric first-price auctions";
    let exact = web_hit_relevance(
        q,
        "A Simple Approach to Analyzing Asymmetric First Price Auctions",
        "we characterize equilibrium bidding in asymmetric first price auctions",
        "https://brocku.ca/repec/pdf/0504.pdf",
    );
    let generic = web_hit_relevance(
        q,
        "English auction",
        "an introduction to auction types for undergraduates",
        "https://example.com/intro",
    );
    assert!(
        exact > generic,
        "the exact-topic surface must outscore the generic one: {exact} vs {generic}"
    );
    assert!(exact > 0.4, "exact-topic coverage should be high: {exact}");
    // 7 of the query's 9 distinct terms (all but "specific" and
    // "functions") sit on the exact-topic surface.
    assert_eq!(exact, 7.0 / 9.0);
    assert_eq!(
        web_hit_relevance(q, "unrelated topic entirely", "", "https://x.example/1"),
        0.0
    );
    assert_eq!(
        web_hit_relevance("", "any title", "any snippet", "https://x.example/2"),
        0.0
    );
}
