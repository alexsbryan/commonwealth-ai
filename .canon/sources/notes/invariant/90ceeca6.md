# OptimizeAction::Index IS NOT IDEMPOTENT — GATE IT ON num_unindexed_rows, OR MAINTENANCE DEGRADES WHAT IT MAINTAINS. Found and fixed…

`OptimizeAction::Index` IS NOT IDEMPOTENT — GATE IT ON `num_unindexed_rows`, OR MAINTENANCE DEGRADES WHAT IT MAINTAINS. Found and fixed 2026-08-05 while building `svrn corpus optimize`.

WHAT HAPPENS UNGATED: every `table.optimize(OptimizeAction::Index(..))` call writes NEW index versions and removes none. Measured on this box:
  wikipedia `_indices`: 24 -> 30 -> 33 -> 36 entries across four passes, ending at 2.4 GB
  sep (ALREADY HEALTHY at 1 version / 3 indices): one pass took it to 4 versions / 9 indices for ZERO benefit
This matters specifically because the whole point of the command is a CADENCE against continuously-appended corpora (`wikipedia-newsworthy`). An ungated version compounds the damage every cycle — the maintenance tool becomes a slow leak.

THE GATE (`corpus-engine/src/index/maintain.rs`): run the index phase only when
`unindexed_rows_before > 0 || fragments_removed > 0`, where `unindexed_rows()` sums
`IndexStatistics::num_unindexed_rows` over `table.list_indices()`. Best-effort by design — a
missing/failed stats call yields 0, which routes to "decline the work". It is never used to
claim an index IS healthy, only to refuse unnecessary writes.
VERIFIED BY WATCHING IT DECLINE (ARCH §18.1), not by assuming: re-running against `sep` and
`wikipedia` now leaves versions AND indices byte-identical (sep 4/9 -> 4/9, wikipedia
3631/36 -> 3631/36) and prints why it skipped.

`num_unindexed_rows` IS ALSO THE DIAGNOSTIC FOR A SLOW CORPUS — lancedb's own `Table::optimize`
docs (lancedb-0.27.2 table.rs:667-672) state that searches run the index over indexed data AND
A FLAT SCAN over unindexed data, then merge. That is vendor confirmation of the flat-scan theory
this whole investigation rested on, and it is why every ANN ablation came back flat
(nprobes 50->10, refine 30->off, overfetch 16->4 all left wikipedia at 5.03-5.11s).

POST-MAINTENANCE STATE: wikipedia now reports `unindexed_rows_before=0` and still costs ~3.0s
against sep's 0.85s. So the residual gap is NOT unindexed rows — it is unexplained and should
not be attributed to fragmentation. 1.95M rows vs sep's ~188k is 10.4x, so some of it is
legitimate scale.

SECOND TRAP, SAME COMMAND: `InstalledIndex::path` is the CORPUS directory, not the `.lance`
dataset (`open_index` resolves `chunks.lance` inside it). Counting the corpus dir gives zeros
for fragments/versions/indices — which then reads as "nothing moved". It printed exactly that
on the first real wikipedia run while compaction had merged 4,620 fragments into 3. Any tool
reporting on-disk dataset shape must resolve the nested dataset first.
