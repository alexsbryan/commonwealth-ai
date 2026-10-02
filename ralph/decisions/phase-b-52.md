<!-- ledger -->

**phase-b-52 · 2026-09-29 · pb-ingest-dial-daemon-tests · director** — this commit
- Needed: pb-ingest-dial-daemon-tests stopped at its census (scope guard, phase-b-29). The census priced the work at ~4,500-5,000 lines against a LIFT of 1,400 and a halt line of 2,800, and put three forks to the operator: split the row, where the composed e2es live, and whether the double may scan `index_dir` itself.
- Chose:
  - Split four ways by what each file asks of the engine. -slot takes the 17 files that only fill an engine field; the double gains `IngestPort` and a harness double lands. -reads takes the 18 files that read fixture indexes. -merge takes the four fold/pull e2es. The parent id keeps the last six composed-ingest files and the grep-0 PROOF, so pb-ingest-dial-daemon and pb-distribution depend on it unchanged.
  - Fork 2: (b). The composed e2es split at the port as the rule already states (FIVE_PROGRAMS "Where a cross-program test lives"). The composed-on-a-process proof stays with pb-ingest-dial-daemon, whose PROOF already drives collaborate/pull and a watched-folder ingest on the stock binary. The defect fold_ingest_cross_node_merge_e2e.rs:37-49 guards is kept on both sides, written into -merge. Option (a) was refused.
  - Fork 3: neither option. The double's listing delegates to `corpus_index::FsIndexSource`, the reader `CorpusEngine` itself delegates to, following the `opening_indexes_under_index_dir` precedent.
- Because:
  - Principle 8. `FsIndexSource` is the one scan (corpus-engine engine/mod.rs:1424-1431 delegates to it). A disk scan in the double would be a second copy, and hand-programmed lists per test would be a third source of truth for the same fixture.
  - Principle 12 and the distribution rule. Option (a) would move in-process tests that name `internal_router`, `AppState` and `ingest_executor` into sovereign-stock. The distribution gate scans stock's `src/` alone because "the crate's tests drive the built binary" (distribution_gate.rs:77). Stock's tests would become a second home for daemon internals, and stock would need a corpus-engine face before pb-ingest-dial-daemon grows `HostedIngest`.
  - The charter: split when proofs differ. -slot's PLANT is on the double, -reads' is on the leaf reader, -merge's is on `PartitionMergePort`'s implementor, and the parent's is on `LocalCorpusPort`'s. Every child's LIFT fits under the 1,400 the parent was priced at.
  - Boundary gate: 20 violations, EXIT=1 at ba9b04f2e (`cargo xtask boundary-gate`, corpus-engine/). This commit changes no Rust.

<!-- appendix -->

## phase-b-52 · 2026-09-29 — pb-ingest-dial-daemon-tests split four ways; composed e2es split at the port; the double reads through the leaf's reader

<details><summary>reasoning, evidence, package</summary>

Reproduced at ba9b04f2e:
- 113 `corpus_engine` lines in 45 test files.
- 45 `CorpusEngine::new`, in 38 files that build or reach an engine.
- `IngestPortDouble` implements `PartitionMergePort`, `EnrichConfigPort`, `IndexSource`, `CorpusReadPort`, `IngestPluginPort`, `CatalogIngestPort` and `LocalCorpusPort`, but not `IngestPort` (daemon.rs:259, ~39 own methods).
- `EngineHarness::new(Arc<CorpusEngine>)` is at port.rs:27, built at tests/main/common/mod.rs:683 (the package said :668).
- sovereign-daemon's Cargo.toml does not enable `corpus-index/test-doubles`.
- Stock's `max_code_lines` does not count `tests/`: size_gate `count` routes test-tree lines to the second counter. Option (a) was therefore gate-feasible, and it was refused on the design, not on the cap.
- The tier-3 files import `sovereign_daemon::server::internal_router`, `state::AppState`, `ingest_executor::*` and `corpus_watch_http::corpus_watch_router`, and `crate::common::*` fixtures.

Not checked by the director: the per-file tier assignment. The worker's census assigned each file to a tier. A file a child finds misassigned moves to the child that owns its tier, and that move is not a new stop: -slot's double refuses unprogrammed methods by name, so a misfiled read fails loudly.

What would falsify this:
- a -merge or parent-row split that cannot keep the gossip-visibility assertion on either side (the defect class fold_ingest_cross_node_merge_e2e.rs:37-49 names);
- a -reads file whose engine read is not a delegation to `FsIndexSource` and cannot be re-asserted on the implementor.

Either one reopens fork 2 as option (a), which is an edit to the stock distribution row and to this doc's test-home rule.

</details>
