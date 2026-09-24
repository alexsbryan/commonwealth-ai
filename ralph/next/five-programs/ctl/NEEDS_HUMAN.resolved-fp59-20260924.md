# NEEDS_HUMAN — fp-59: the move needs three more widenings than the row names

## (a) The unit

STATE.md fp-59, marked `[~]` (uncommitted), no code edited, tree = HEAD 353e70edc:

> MOVE the corpus-engine-writing core tests beside their owner (§12 D6 …): the three
> async tests in `sovereign-core/src/runtime/retrieval_pipeline/atlas_step_reachability_tests.rs`
> … and `sovereign-core/tests/main/core_tests.rs:2502-2540` … move to
> `sovereign-tools/tests/main/` …; widen `apply_atlas_grounding` (atlas_grounding.rs:119,
> `pub(crate)`) to `#[doc(hidden)] pub`; the two sync step-list tests (lines 40, 62) stay.

## (b) What I ran and found

    git grep -n "wiki_store::\|build_persistent_ann_seed_table\|wikipedia_types" -- sovereign/crates/sovereign-core
      → only atlas_step_reachability_tests.rs:165,167,170,230,447,467   (premise holds)

Every item the two ledger tests touch is already reachable from outside the
crate except `apply_atlas_grounding`, which the row widens: `RuntimeParts`,
`stubs`, `lane::{Lane, LaneSources}` (all `pub`, fields `pub`),
`retrieval_ledger::StepLedger` (`pub`, `considered`/`accounted` `pub`,
`total_accounted` `pub`), `AtlasWalkEcho`/`AtlasWalkNodeEcho` (re-exported at
runtime.rs:100, fields `pub`), `atlas_context::AtlasGraph` (lib.rs:5 re-export).

The THIRD async test, `a_walked_deep_query_turn_carries_the_echo_out_of_retrieval`
(reachability_tests.rs:489-555), calls `rt.prepare_knowledge_context(...)` and reads
`kc.atlas_walk`. All three are crate-private and the row does not name them:

    sovereign-core/src/runtime/retrieval/mod.rs:113   pub(crate) async fn prepare_knowledge_context
    sovereign-core/src/runtime/types.rs:27            pub(crate) struct KnowledgeContext   (in private `mod types`, runtime.rs:236)
    sovereign-core/src/runtime/types.rs:36            pub(crate) atlas_walk: Option<AtlasWalkEcho>

No public surface returns this echo short of a full streamed turn.

Not a decision, noted for the worker: the core_tests.rs test uses that file's
`MockStore` (:73), `RecordingInference` (:2214) and `drain` (:2391). In
sovereign-tools it would use `sovereign_store::memory::InMemoryStateStore` (tools
already depends on sovereign-store, Cargo.toml:145) and a small streaming stub
in the new file; the originals stay, other core tests use them.

## (c) What the operator must decide

1. How does test 3 cross the crate line? Options, recommended first:
   (a) widen the three items above to `#[doc(hidden)] pub` alongside
       `apply_atlas_grounding` — the same verb the row already uses, and the
       test moves whole. `KnowledgeContext` stays in its private module, so it
       becomes usable but not nameable from outside.
   (b) rewrite test 3 to drive `handle_message_stream` and read the persisted
       `ATLAS_WALK_META_KEY` (runtime.rs:99) off the stored message. This keeps
       the widening at one item but changes the test's subject, so it is no
       longer a move (principle 2).
   (c) leave test 3 in core. That breaks the row's purpose: its fixture
       (`write_wiki_atlas`) is the corpus-engine writer use, and fp-66 cannot
       drop the dependency while it stays.

## (d) To resume

Edit or mark the row in ralph/next/five-programs/STATE.md (e.g. append "widen
also prepare_knowledge_context, KnowledgeContext and its atlas_walk field"),
then `rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
