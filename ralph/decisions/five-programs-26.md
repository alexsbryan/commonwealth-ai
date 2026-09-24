<!-- ledger -->

**five-programs-26 · 2026-09-24 · fp-59 (the third async test's surface widens too) · director** — this commit
- Needed: the fp-59 worker found that the third async test, `a_walked_deep_query_turn_carries_the_echo_out_of_retrieval`, calls `Runtime::prepare_knowledge_context` and reads `KnowledgeContext.atlas_walk`. All three are `pub(crate)` and the row did not name them. It asked how the test crosses the crate line.
- Chose: option (a). Widen `prepare_knowledge_context`, `KnowledgeContext` and its `atlas_walk` field to `#[doc(hidden)] pub` together with `apply_atlas_grounding`. The test moves whole, and `KnowledgeContext` stays in private `mod types`. I corrected the row and reopened it (`[ ]`). No code changed. Boundary gate FAILED at 62 violations (reproduced at 353e70edc, EXIT=1).
- Because: the row already uses this verb for `apply_atlas_grounding`, and runtime.rs:208-218 follows the same doc-hidden-for-integration-tests pattern (`retrieval_ledger`, `retrieval_pipeline`). Option (b), driving `handle_message_stream`, changes the test's subject, so it is a rewrite and not a move (principle 2). Option (c) leaves `write_wiki_atlas` in core, which keeps the edge fp-66 has to drop.

<!-- appendix -->

## five-programs-26 · 2026-09-24 — fp-59 widens the knowledge-context surface so the echo test moves whole

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp59-20260924.md. Reproduced at 353e70edc:

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → FAILED (62), EXIT=1.
- sovereign-core/src/runtime/retrieval/mod.rs:113 is `pub(crate) async fn prepare_knowledge_context`. types.rs:27 is `pub(crate) struct KnowledgeContext` and :36 is `pub(crate) atlas_walk: Option<AtlasWalkEcho>`. runtime.rs:236 is `mod types;` (private) and :206 is `pub mod retrieval;`.
- runtime/retrieval/atlas_grounding.rs:119 is `pub(crate) async fn apply_atlas_grounding`. The row cited it as `atlas_grounding.rs:119`, and I corrected the path in the row text.
- reachability_tests.rs:489-555 is test 3. It builds its `Runtime` from `RuntimeParts`, `stubs`, and `LaneSources`, which are all public, and then calls `prepare_knowledge_context`.
- No workspace or crate `unreachable_pub`/`private_interfaces` lint is configured (grep of Cargo.toml, sovereign-core Cargo.toml and lib.rs is empty). A `pub` struct in a private module is unnameable but legal in a public signature.

Nothing here widens a dep budget or adds an exception, so the charter's "false row premise" clause covers it and the operator does not need to decide. The row now names every item the move needs.

Falsified if the widened `KnowledgeContext` fails LINT in the toolbox (a private-interface lint would force naming the type), or if the moved test needs any further `pub(crate)` item. In either case the worker halts again with the item named, and the right move is a re-export under the existing `#[doc(hidden)] pub mod retrieval`, not option (b).

</details>
