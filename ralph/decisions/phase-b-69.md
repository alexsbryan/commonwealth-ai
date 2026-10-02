<!-- ledger -->

**phase-b-69 · 2026-09-30 · pb-ingest-dial-daemon · director** — this commit
- Needed: pb-ingest-dial-daemon landed its outcome and every PROOF clause but one: the collaborate kickoff driven to completion on the stock binary. The row placed that e2e in the one_process_e2e pattern, which pins cw-rails to a closed port, and the pull loop reads its embed model from cw-rails' ledger, so it skips every tick; the kickoff also refuses without a JSONL or HF source. Neither premise was in the row's census.
- Chose: package option 1 with option 3 for the remainder. The row is marked done at c0676ba8a, its PROOF text corrected to what ran. The stock-binary collaborate-to-completion e2e goes to phase-c as `pc-stock-collaborate-e2e`, with its census named.
- Because:
  - The finish item is boundary.log:77, `sovereign-daemon → corpus-engine`, and it is retired: boundary-gate 17 violations, EXIT=1 at c0676ba8a (18 at the row's start), with no `sovereign-daemon → corpus-engine` line. The PLANT was watched red at 19.
  - The missing clause retires no edge and makes no lift pass, so the phase-b-29 scope guard sends it to phase-c. Folding it into this row would add a real cw-rails spawn, a ledger write and a source fixture that the row's outcome does not need.
  - The collaborate/pull path through the port is proven elsewhere: on `IngestPortDouble` (36ec453d2) daemon-side, and `ingest_with_overrides` on corpus-engine's own tests. What is unproven is the composition of the two on one stock process, and the new row names that as its outcome, not a softer one (principle 5).
- REVIEW-AFTER: pc-stock-collaborate-e2e runs. If it finds the composed collaborate path broken on the stock binary, this row landed a regression and reopens.

<!-- appendix -->

## phase-b-69 · 2026-09-30 — pb-ingest-dial-daemon lands at c0676ba8a; the stock collaborate-to-completion e2e goes to phase-c

<details><summary>reasoning, evidence, package</summary>

Reproduced at c0676ba8a:
- sovereign-daemon/Cargo.toml names no `corpus-engine` dependency (line 66 is the comment recording its removal; the remaining `corpus-engine-*` entries are leaves).
- `cargo xtask boundary-gate` from corpus-engine/: `boundary-gate FAILED (17 violation(s))`, EXIT=1; `grep -c "sovereign-daemon → corpus-engine"` on its output is 0.
- sovereign-daemon/src/auto_ingest.rs:801-811, `discover_and_spawn_pull_loops`: `state.local_embed_model()` error returns early with `pull_loops: embed model unread; skipping this tick`.
- sovereign-stock/tests/ingest_composed_e2e.rs:128 `the_stock_install_ingests_through_ingests_port` and :160 `CW_RAILS_BIN` set to a missing file, the same as one_process_e2e.rs:178,342,437.
- routes_internal/corpus_collaborate.rs:236-247: the kickoff requires `has_source_manifest` or `count_jsonl_articles`.

Not reproduced: the stock process log line quoted in the package (I did not re-run the e2e; the code path that prints it is read above).

What would falsify this:
- pc-stock-collaborate-e2e finds a collaborate ingest on the stock binary that fails where the daemon on its own engine succeeded before a350a9a5d. Then the composition broke it, and the fix is this row's, not phase-c's.
- The operator reads the PROOF clause as a bar the row could not meet (charter: pre-registered bars are the operator's). Then this decision should have been a package.

</details>
