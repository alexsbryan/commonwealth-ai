<!-- ledger -->

**five-programs-47 · 2026-09-24 · fp-95 · director** — this commit
- Needed: fp-95's worker stopped before editing because its second BAR (`fabric.contribution_emitter` = 0 over the whole daemon src) cannot reach 0 in this row. Only daemon.rs:4051 is fp-95's; the other three hits are corpus_queue.rs:86, :260, :609, where the daemon hands sovereign-grants' `FoldRecovery` and `ShardManager::with_emitter` a concrete `ContributionEmitter`.
- Chose: rescope the BAR to daemon.rs. The whole-src = 0 remains fp-88's premise, which already names fp-94 and fp-95 as the two rows that clear the readers. fp-95 keeps its dependency on fp-82 only. Boundary gate 54 (EXIT=1), unchanged; no code moved in this commit.
- Because: five-programs-44 assigned the corpus_queue.rs grants construction sites to fp-94, and fp-81's bar already excused them on that ground. Making fp-95 depend on fp-94 instead would park a self-contained move behind HUMAN-fp94-grants-seam for nothing the move needs (the smaller reversible step).

<!-- appendix -->

## five-programs-47 · 2026-09-24 — fp-95's emitter BAR covers daemon.rs; the grants hits stay fp-94's

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp95-20260924.md (git-excluded), reproduced at e4bc64352.

- `git grep -nw 'fabric\.contribution_emitter' -- sovereign/crates/sovereign-daemon/src` returns exactly daemon.rs:4051 and corpus_queue.rs:86, :260, :609.
- `git grep -n run_storage_snapshot_loop` finds the definition at contributions.rs:174, tests at :378 and :411, the only non-test caller at daemon.rs:4056, and the doc at storage_snapshot_e2e.rs:4.
- STATE.md fp-81 ("bar corpus_queue.rs's sovereign-grants construction sites ... until fp-94 (five-programs-44)") and fp-88's premise ("no reader since fp-82, fp-94 (grants) and fp-95 (the snapshot loop)") both already split the readers this way.
- `cargo xtask boundary-gate` (corpus-engine/, toolbox): 54 violations, EXIT=1.

Options: (a) rescope the BAR to daemon.rs (chosen); (b) fp-95 depends on fp-94, which is blocked on an operator row.

Falsified if a `fabric.contribution_emitter` reader outside daemon.rs and corpus_queue.rs's grants sites appears before fp-88, or if fp-94's answer leaves one of the corpus_queue.rs hits in place (fp-88's premise would then still fail).

</details>
