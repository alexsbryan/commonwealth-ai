// SPDX-License-Identifier: AGPL-3.0-or-later
//! drb1-r2b red tests — the round-allowance split (the gap round keeps
//! its ammunition).
//!
//! Order drb1-r2b, campaign drb1-race; declared in
//! research/deep-research/adversarial/pre-registration.md before the
//! fix landed. Red-first (§18.1): the main test was watched failing at
//! HEAD — the pre-fix commit exhausts the whole search allowance in
//! round 1 and the gap round never fires.
//!
//! The seed-02 shape this pins (runs-r3a loop, dr-1787328255): round 1
//! forms more queries than the 12-search allowance holds (2 survey-gap
//! queries + 10 frontier sub-questions), spends all 12 (the R1
//! consume-the-remaining-budget shape), and the between-rounds budget
//! gate then refuses the gap round entry — round-2 search_calls 0,
//! gaps flat 2→2, no fetch-list-2.json. On exactly the hardest
//! questions the campaign's closure mechanism — round N+1's
//! gap-derived queries — starves. The split: no round but the last may
//! exhaust the search allowance; every later round enters queryable.

use async_trait::async_trait;
use futures::Stream;
use sovereign_core::deep_research::budget::round_allowance_cap;
use sovereign_core::deep_research::gym::{Deck, MockBackendImpl, MockDraftSurface};
use sovereign_core::deep_research::icd::Artifact;
use sovereign_core::deep_research::{run, RunConfig, SearchSource};
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};
use std::path::PathBuf;
use std::sync::Arc;

/// The same minimal stub the loop's own tests use (gym_deck.rs): every
/// judge call returns `"no"` → the forced-choice parse fails → the
/// judge is None → could-not-judge. Deterministic across runs — the
/// deck and the scripted draft control everything.
struct GarbageJudge;
#[async_trait]
impl InferenceProvider for GarbageJudge {
    async fn complete(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: "no".into(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "test".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        std::pin::Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
    > {
        unimplemented!()
    }
    async fn embed(&self, _t: &str) -> sovereign_core::error::Result<Vec<f32>> {
        Ok(vec![])
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

/// The seed-02 charter shape: 12 searches, 12 fetches, 3 rounds. The
/// allowance a single round may spend under the split is
/// ceil(12/3) = 4.
const ALLOWANCE: u32 = 12;
const MAX_ROUNDS: u32 = 3;
const ROUND_ONE_CAP: u32 = 4;

/// The scripted draft: 12 unique lines → a 12-sub-question frontier
/// (FRONTIER_MAX), so round 1 forms strictly more queries than the
/// allowance holds (its own audit gaps + 12 frontier) — the shape that
/// exhausts a 12-search allowance at HEAD. The audit over the empty
/// window leaves every claim open, so the loop keeps gap-derived
/// queries alive in every round (the clean-deck spin).
const DRAFT: &str = "DeepSeek released R1 in January 2025 and it repriced AI compute.\n\
     Nvidia lost the largest single-day market capitalization in its history.\n\
     R1 demonstrated frontier reasoning at a fraction of incumbent training cost.\n\
     Export controls pushed Chinese labs toward efficiency rather than scale.\n\
     DeepSeek V3 reported roughly 2.79 million GPU-hours of training.\n\
     The claimed V3 training cost was about 5.6 million dollars.\n\
     Nvidia lost approximately 589 billion dollars of market value.\n\
     About one trillion dollars was erased across the tech complex.\n\
     Markets repriced the cost of frontier model training.\n\
     Analysts questioned the AI capital expenditure thesis.\n\
     Hyperscaler infrastructure spending plans came under review.\n\
     The release date of R1 was January 20 2025.\n";

const QUESTION: &str = "Why did the DeepSeek R1 release reprice AI compute and Nvidia in 2025?";

/// The seed-02 starvation shape on the shipped `run()`: the clean deck
/// (zero hits, empty estate) with a scripted 12-line frontier. Zero
/// API, zero daemon calls, deterministic.
async fn drill_once(run_dir: PathBuf) -> sovereign_core::deep_research::icd::Manifest {
    let deck = Deck::parse("version = 1\n", &[]).expect("clean deck builds");
    let port = Arc::new(MockBackendImpl::new(
        deck,
        MockDraftSurface::Scripted(DRAFT.to_string()),
    ));
    let outcome = run(
        RunConfig {
            run_id: "dr-r2b-red".to_string(),
            question: QUESTION.to_string(),
            seed_id: None,
            run_dir,
            max_rounds: MAX_ROUNDS,
            code_set_k: 3,
            eps_quota: 0.1,
            content_coverage_floor:
                sovereign_core::deep_research::acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR,
            prose_line_floor: sovereign_core::deep_research::acquisition::DEFAULT_PROSE_LINE_FLOOR,
            evidence_window_max_chunks: 20,
            estate_corpus_ids: Vec::new(),
            web_backend: MockBackendImpl::BACKEND_ID.to_string(),
            search_source: SearchSource::Mock,
            web_search_allowance: ALLOWANCE,
            web_fetch_allowance: ALLOWANCE,
            posture: ShardingPrivacy::LocalOnly,
            consent: None,
            max_rounds_override: None,
            max_search_override: None,
            max_fetch_override: None,
        },
        port,
        Arc::new(GarbageJudge),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .await
    .expect("drill run completes");
    outcome.manifest
}

fn fresh_run_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dr-r2b-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// RED (order drb1-r2b item 1): a run whose round 1 would exhaust the
/// search allowance must leave round 2 with a non-zero queryable
/// allowance and must actually fire its gap-derived queries.
///
/// Watched fail at HEAD (pre-split): round-1 search_calls 12/12
/// (exhausted), round 2 gated out before acquiring — the manifest's
/// only round-2 row is finish's audit row (search_calls 0), there is
/// no fetch-list-2.json, and the run closes done-partial at round 2
/// with gaps flat — the seed-02 arithmetic exactly.
#[tokio::test]
async fn r2b_round_split_keeps_the_gap_round_firing() {
    let run_dir = fresh_run_dir("split");
    let manifest = drill_once(run_dir.clone()).await;

    let round_row = |n: u32| {
        manifest
            .rounds
            .iter()
            .find(|r| r.round == n)
            .unwrap_or_else(|| panic!("round {n} missing from the manifest"))
    };

    // (1) Round 1 did NOT exhaust the allowance: it spent exactly its
    // fair share, ceil(12/3) = 4 — never the whole 12.
    let r1 = round_row(1);
    assert!(
        r1.search_calls > 0,
        "round 1 must still search (the split caps, it never zeroes)"
    );
    assert_eq!(
        r1.search_calls, ROUND_ONE_CAP,
        "round 1 spends its fair share ceil(allowance/max_rounds) — the split's arithmetic"
    );

    // (2) The gap round FIRED: round 2 has an acquire row with real
    // spend (at HEAD the only round-2 row is finish's audit row with
    // search_calls 0).
    let r2 = round_row(2);
    assert!(
        r2.search_calls > 0,
        "the gap round must fire its gap-derived queries — at HEAD it never entered acquire"
    );

    // (3) The round-2 recorder carries gap-derived queries (the order's
    // pin: fetch-list-2 entries with from_gap_id). At HEAD the file
    // does not exist at all.
    let fl2_json = std::fs::read_to_string(run_dir.join("fetch-list-2.json"))
        .expect("the gap round must write its fetch list (at HEAD the round never ran)");
    let Artifact::FetchList(fl2) = Artifact::parse(&fl2_json).expect("fetch-list-2 parses") else {
        panic!("fetch-list-2 must parse as the fetch-list ICD");
    };
    assert!(
        fl2.queries.iter().any(|q| q.from_gap_id.is_some()),
        "round 2's queries must be gap-derived (from_gap_id set)"
    );

    // (4) The final round was not starved either — it entered with
    // allowance left and spent some (the R1 consume-the-remaining-budget
    // shape belongs HERE, on the last round).
    let r3 = round_row(3);
    assert!(
        r3.search_calls > 0,
        "the final round may spend everything left — it must not be budget-zero at entry"
    );

    // (5) The recorder lists only EXECUTED queries: round 1's list is
    // truncated to the cap (at HEAD it lists formed-but-unexecuted
    // queries past the allowance — a lie by implication).
    let fl1_json = std::fs::read_to_string(run_dir.join("fetch-list-1.json")).unwrap();
    let Artifact::FetchList(fl1) = Artifact::parse(&fl1_json).expect("fetch-list-1 parses") else {
        panic!("fetch-list-1 must parse as the fetch-list ICD");
    };
    assert_eq!(
        fl1.queries.len(),
        ROUND_ONE_CAP as usize,
        "the round-1 fetch list records exactly the queries the round executed"
    );

    // (6) Interaction check (order item 2): the ledger instruments read
    // REAL consumption. Per-round search_calls sum to the manifest's
    // spent; spent + remaining == the charter allowance; the journal's
    // allow entries (not the manifest) carry the same count.
    let spent: u32 = manifest
        .budget
        .spent
        .get("web-search:mock")
        .copied()
        .unwrap_or(0);
    let remaining: u32 = manifest
        .budget
        .remaining
        .get("web-search:mock")
        .copied()
        .unwrap_or(0);
    let rounds_sum: u32 = manifest.rounds.iter().map(|r| r.search_calls).sum();
    assert_eq!(
        rounds_sum, spent,
        "per-round search_calls must sum to budget.spent (unmasked consumption)"
    );
    assert_eq!(
        spent + remaining,
        ALLOWANCE,
        "spent + remaining must equal the charter allowance"
    );
    let ledger: sovereign_core::deep_research::icd::BudgetLedger =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("budget-ledger.json")).unwrap())
            .expect("budget ledger parses");
    let journal_allows: u32 = ledger
        .entries
        .iter()
        .filter(|e| e.family == "web-search" && e.decision == "allow")
        .map(|e| e.units)
        .sum();
    assert_eq!(
        journal_allows, spent,
        "the journal's allow entries must agree with the manifest's spent"
    );
    assert!(
        spent >= ROUND_ONE_CAP + 2,
        "the split must have preserved budget for rounds 2+3 to actually spend ({spent} spent)"
    );
}

/// Order item 1's degrade constraint: the split must degrade sensibly
/// at max-rounds 2..3 and allowances 4..12, never hand a non-final
/// round the whole meter, and always leave the final round open.
#[test]
fn r2b_cap_degrades_sensibly_across_rounds_and_allowances() {
    // 12 @ 3 rounds (the r3a charter shape): 4 / 4 / everything left.
    assert_eq!(round_allowance_cap(12, 3), 4);
    assert_eq!(round_allowance_cap(8, 2), 4, "after round 1 spent its 4");
    assert_eq!(
        round_allowance_cap(4, 1),
        4,
        "the final round spends it all"
    );
    // 12 @ 2 rounds: 6 / 6.
    assert_eq!(round_allowance_cap(12, 2), 6);
    assert_eq!(round_allowance_cap(6, 1), 6);
    // 4 @ 3 rounds: 2 / 1 / 1 — every round keeps a query.
    assert_eq!(round_allowance_cap(4, 3), 2);
    assert_eq!(round_allowance_cap(2, 2), 1);
    assert_eq!(round_allowance_cap(1, 1), 1);
    // 4 @ 2 rounds: 2 / 2.
    assert_eq!(round_allowance_cap(4, 2), 2);
    assert_eq!(round_allowance_cap(2, 1), 2);
    // Degenerate: one unit, two rounds — the opening round gets the
    // unit (ceil); a structurally empty round 1 serves nobody.
    assert_eq!(round_allowance_cap(1, 2), 1);

    // The invariants, exhaustively over the order's stated ranges.
    for remaining in 0..=12u32 {
        for rounds_left in 1..=3u32 {
            let cap = round_allowance_cap(remaining, rounds_left);
            assert!(cap <= remaining, "cap never exceeds remaining");
            if remaining > 0 {
                assert!(cap >= 1, "a live meter always allows at least one ask");
            }
            if rounds_left >= 2 && remaining >= 2 {
                assert!(
                    cap < remaining,
                    "a non-final round can never exhaust the meter ({cap} of {remaining}, {rounds_left} rounds left)"
                );
            }
            if rounds_left <= 1 {
                assert_eq!(cap, remaining, "the final round may spend everything left");
            }
        }
    }
}
