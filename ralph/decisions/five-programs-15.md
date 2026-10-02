<!-- ledger -->

**five-programs-15 · 2026-09-24 · fp-58 (sovereign-eval → understanding-vocab: neither arm closes it) · director** — this commit
- Needed: fp-58 halted (ctl/NEEDS_HUMAN.md). The row offered a daemon dial or moving the miner to [ingest], and the worker's census showed that neither one closes the edge.
- Chose: park the row, not guess. fp-58 now depends on a new `HUMAN-fp58-bench-atlas-read` row, which carries three options and a recommendation. The HUMAN row sits LAST in the queue so every other ready row drains first. No code changed. The boundary gate stays at 63.
- Because: every arm that still closes the edge is reserved by the charter. Those arms are admitting understanding-vocab (or its types half) to [bench]'s `leaf_budget`, an `[[exception]]`, and a new path-keyed atoms route (new capability, and runs would need a serving process). The one ladder arm the charter would allow, a port trait, is refused on principle 2: its implementer would be sovereign-cli-llm's bench_cmd, and §11's cli-llm split already sizes that as bench's own group, so the edge would go red again when that split lands.

<!-- appendix -->

## five-programs-15 · 2026-09-24 — park fp-58 behind an operator row; recommend admitting understanding-vocab to [bench] with its fs door feature-gated

<details><summary>reasoning, evidence, package</summary>

Reproduced by the director at 2d9a9e59d:

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → `FAILED (63 violation(s))`, including `[bench] sovereign-eval → understanding-vocab`.
- flywheel/mining.rs:14-15 imports `atoms::AtomEnvelope` and `read::{read_atlas_atoms, ATLAS_DIRNAME}`. understanding-vocab/src/read.rs:19 has `use std::fs`, and its decode goes through the crate-private `AtomsFileWire` inside the fs fn (:37-44).
- `mine_claims` has exactly three callers, all inside sovereign-eval: generators/corpus.rs:81 (Present) and :134 (HeldOutSlice withheld path), plus mechanism_fidelity/classes/attribution.rs:110 (and its test at :252). Every one passes a `&Path`.
- The drivers outside eval are sovereign-cli-llm bench_cmd/{flywheel.rs, mechanism_fidelity.rs, gate.rs, promote.rs, mod.rs}. FIVE_PROGRAMS §11 "The cli-llm split" measures bench_cmd as part of the 60,519-line bench group.

The package's census of the two row arms held (see ctl/NEEDS_HUMAN.resolved-fp58-20260924.md). The director added one arm the package did not price, the port trait, which is 3a rung 2's last bullet, and refused it for the whack-a-mole reason above.

Recommendation to the operator is (a): gate understanding-vocab's `read` door behind a default-on feature and lift the pure bytes → `AtomsFile` decoder out of it. eval then does its own `fs::read` and admits the crate to [bench]'s budget. The added closure is serde, serde_json, blake3 and kernel-types (already budgeted). None of that is the producer the evaluator measures, which is corpus-engine.

Falsified if either of these turns out to be true:
- an existing [bench] budget leaf already carries the atoms schema;
- the miner's callers can be served from an installed corpus id without losing the withheld-slice source.

Either one would make a charter-covered arm exist.

</details>
