<!-- ledger -->

**five-programs-12 · 2026-09-24 · fp-55 (bench's per-package leaf budget) · director** — this commit
- Needed: fp-55 halted pre-edit (ctl/NEEDS_HUMAN.md). The literal budget [oicp-types, sovereign-contracts] reds 8 edges that add no crate to bench's closure. The edge the budget exists for (bench → corpus-engine) is invisible to the gate, because the manifest parser drops dotted-key deps.
- Chose: the package's option 1(A) without (D). The budget is the pair plus kernel-types, sovereign-time and workspace-hack, named in the list rather than exempted by a rule in packages.rs. understanding-vocab stays red and gets one repair row. The parser fix rides in fp-55's commit, and three repair rows are minted there. Expected boundary 62 → 65.
- Because: §11 finish condition 3 says the budget exists "so the evaluator cannot link the thing it measures". The three admitted leaves add zero crates, and §12 3a (operator-approved) already names kernel-types as the identity vocabulary home. Refusing sovereign-time would put two gates in opposition again (ARCH_LAYERS.toml:885-893). understanding-vocab is not a 3a home and its read door does std::fs, so it fails 3a's leaf test. A forbid that cannot see its edge is not a gate (principle 5), hence the parser fix.

<!-- appendix -->

## five-programs-12 · 2026-09-24 — bench's budget is the closure-neutral leaves; the parser learns dotted keys

<details><summary>reasoning, evidence, package</summary>

Reproduced before deciding (director, tree clean at f5099e1e2):

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → `boundary-gate FAILED (62 violation(s))`, EXIT=1, with no `[bench]` line.
- corpus-engine/xtask/src/manifests.rs:317-322 takes the full left side of `=` as the dep name. Across tracked Cargo.toml files, the dotted internal deps are exactly `corpus-engine.workspace` in sovereign-eval:20 and sovereign-authoring-harness:12, plus `sovereign-authoring-harness.workspace` in sovereign-eval:25 (intra-bench). The same parser feeds layer-gate (layer_gate.rs:55 → internal_dep_edges), so fp-55 must report layer-gate's delta.
- Bench's leaf edges: eval → sovereign-time (:17), contracts (:19), understanding-vocab (:24), workspace-hack. tdd → kernel-types (:32), workspace-hack. The other three members → workspace-hack only. Refs: sovereign_time 1 (entity_resolution_bench.rs:200), kernel_types 2 (tdd recur/frame.rs:8, driver.rs:31), understanding_vocab::read + atoms (flywheel/mining.rs:14-15).
- understanding-vocab/src/read.rs:19 `use std::fs`. It is the atlas read door.

Pre-registered: the fp-55 commit lands 62 → 65 (+2 from the parser, +1 from the budget). Any other number gets explained before it lands.

Refused: (B), which admits understanding-vocab. It would let the evaluator read the product's artefacts through ingest's crate, which §4 rule 6 forbids and 3a's leaf test fails. (C), the literal budget: 8 repair rows with no honest repair, since hakari re-adds workspace-hack and clock-gate demands sovereign-time. (D), workspace-hack exempted by rule: that is a mechanism for N packages when only one has a budget, which is scope the row does not need.

Charter fit: this is not "widening a [[package_leaf]] budget". No leaf's `allow` changes and no leaf is admitted. The per-package budget is new, and every entry narrows bench's reach below today's global set. The row's premise (that the literal pair reds only product edges) failed the tree, and that is the standing-lesson case.

Falsified if: a sovereign-time or kernel-types edge turns out to carry product logic into bench (a later commit gives either leaf an internal dep), or the parser fix reds layer-gate in a way that shows dotted-key edges mean something other than a dependency.

</details>
