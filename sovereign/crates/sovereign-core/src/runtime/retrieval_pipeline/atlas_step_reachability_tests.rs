// SPDX-License-Identifier: AGPL-3.0-or-later
//! Does the atlas step reach a wiki-class store?
//!
//! Three tests, answering one question in the order its links can be checked.
//! They live together because a green on any one of them alone reads as an
//! answer and is not:
//!
//! 1. Is `atlas_grounding` in the pipelines that search a corpus? (the step
//!    LIST — no Runtime needed)
//! 2. Is the one pipeline without it the one without a corpus search? (the
//!    only legitimate omission, pinned so it stays the only one)
//! 3. Does the step's BODY reach a wiki-class store, and does it refuse when
//!    the provider cannot serve one? (the returned `StepLedger`) — lives in
//!    `sovereign-tools/tests/main/atlas_step_reachability.rs`, beside the
//!    corpus-engine store its fixture writes (FIVE_PROGRAMS §12 D6).
//!
//! (3) exists because the channel that would have shown it was dark: `svrn
//! eval run` emits no `sovereign_core` tracing even at `RUST_LOG=info`, and
//! reading that silence as "the code never runs" was wrong. A ledger is a
//! VALUE the step returns, so no subscriber has to be in the loop for the
//! answer to be observable.

use super::*;

/// THE ATLAS STEP IS IN EVERY PIPELINE THAT SEARCHES A CORPUS.
///
/// Written while chasing why a `--prod-pipeline` wikipedia eval showed no
/// atlas contribution. The hypothesis on the table was that a pipeline-level
/// predicate skips the step for a wiki-class corpus before
/// `apply_atlas_grounding` is ever called. This pins the half of that
/// question that can be answered without a Runtime: whether the step is in
/// the list at all. It is, for both pipelines `retrieve_evidence`
/// dispatches to (`kq_pipeline` for KnowledgeQuery / ComparisonQuery,
/// `deep_pipeline` for DeepQuery), and it is in `shared_core_steps` so
/// neither can drop it by drifting apart.
///
/// What this test does NOT establish, said plainly so nobody reads more
/// into a green: that the step's body runs, or that it reaches a store.
/// Those need the step's returned `StepLedger`, not its name — see
/// `the_atlas_step_reaches_a_wiki_class_store` (header, item 3).
#[test]
fn atlas_grounding_is_in_every_corpus_searching_pipeline() {
    for (name, steps) in [
        ("kq", kq_pipeline().step_names()),
        ("deep", deep_pipeline(true).step_names()),
    ] {
        assert!(
            steps.contains(&"atlas_grounding"),
            "{name}: atlas_grounding must be in the pipeline — a corpus \
             search that cannot reach the atlas grounds nothing, and the \
             absence is invisible from outside"
        );
    }
}

/// The ONE legitimate omission, pinned so it stays the only one.
///
/// `deep_pipeline(false)` is the attached-doc turn: no corpus search, so no
/// pool to seed from and no query embedding computed. Dropping the step
/// there is deliberate (see the doc comment on `deep_pipeline`). Pinning it
/// means a future change that drops `atlas_grounding` from a SEARCHING
/// pipeline cannot hide behind "it was already conditional".
#[test]
fn the_only_pipeline_without_atlas_grounding_is_the_one_without_corpus_search() {
    let without = deep_pipeline(false).step_names();
    assert!(
        !without.contains(&"atlas_grounding"),
        "attached-doc turns intentionally drop atlas grounding"
    );
    // And it is dropped for the stated reason — the corpus head is gone
    // too, not just the grounding step. `raptor_grounding_early` was the
    // sibling this checked against until ei-5c retired it; `store_search` is
    // the head itself, and a step whose absence proves the same condition.
    assert!(
        !without.contains(&"store_search"),
        "the same no-corpus-search condition drops the corpus head; \
         if these two ever diverge the reason given here is stale"
    );
}
