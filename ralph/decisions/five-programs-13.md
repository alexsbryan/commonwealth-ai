<!-- ledger -->

**five-programs-13 · 2026-09-24 · fp-55 (landing: the fan-in cap the parser fix exposes) · director** — this commit
- Needed: fp-55 halted at landing (ctl/NEEDS_HUMAN.md). With the patch applied, layer-gate reds on one fan-in cap, corpus-engine 16 → 18, with 0 layer violations. The two edges are the dotted-key deps the old parser could not see.
- Chose: the package's option (A). fp-55's commit hand-corrects that one line of quality/baselines/fan_in.tsv to 18 and names it as an instrument correction. The two bench → corpus-engine repair rows `--tighten` it back to 16. The closure printout at boundary_gate.rs:175 rides in the same commit.
- Because: the cap was set by a parser that could not see these edges. Both edges predate the cap (2026-05-04 and 2026-06-16, against the cap's 2026-07-11). So 16 was never a measurement, and correcting it absorbs no growth (principle 7). (C) would keep a gate blind that decision 12 ordered opened (principle 5). (B) would leave every later LAYER check and the pre-push gate red. The printout goes wrong because of the budget this row introduces, so fixing it is part of the row (principle 1). REVIEW-AFTER: the charter reserves `--update-baseline` on a dirty tree for the operator. This is one hand-edited line on an unchanged manifest set, not an absorb-all re-pin, but the operator should confirm the line.

<!-- appendix -->

## five-programs-13 · 2026-09-24 — correct the fan-in cap the blind parser set; tighten it back as the repairs land

<details><summary>reasoning, evidence, package</summary>

Reproduced by the director at cccfd2555:

- `git apply ralph/next/five-programs/ctl/fp-55.patch` (clean), then `cargo xtask layer-gate` (toolbox, corpus-engine/) → `79 members, 435 internal edges … ✗ fan-in of corpus-engine grew 16 → 18`, `layer-gate FAILED (0 layer violations, 1 fan-in)`, EXIT=1. The patch was then reverted and the tree is clean. The worker measured 432 edges at HEAD, a delta of +3.
- `quality/baselines/fan_in.tsv:8` reads `16 corpus-engine`. The file was added in d43fde651 on 2026-07-11.
- `git log -S'corpus-engine.workspace'`: sovereign-eval/Cargo.toml:20 arrived in 6623ea053 (2026-05-04), and sovereign-authoring-harness/Cargo.toml:12 in 555ded27f (2026-06-16). Both predate the cap.

Refused: (B), because it leaves layer-gate red across every intervening row and the pre-push gate. (C), because splitting the parser fix off defers a known-blind forbid, contrary to decision 12's same-commit order, and it buys nothing: the cap would have to be corrected when the parser lands anyway.

Guard: the worker edits exactly the corpus-engine line. If any other cap moves when the patch is applied, that is a new finding and the worker halts.

Falsified if: layer-gate at the fp-55 commit reports a fan-in other than 18 for corpus-engine, or moves any other cap. Also falsified if either dotted edge turns out not to be a real dependency (the crate compiles without it), in which case the repair is to delete the edge, not to correct the cap.

</details>
