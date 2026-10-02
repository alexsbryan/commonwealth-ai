<!-- ledger -->

**phase-b-40 · 2026-09-28 · pb-ingest → the lift names its own crates/ as the source tree; the row is done · director** — this commit
- Needed: LIFT(ingest) failed on three corpus-engine tests (recipe_schema ×2, the enrichment_type census) that panic with SOVEREIGN_WORKSPACE_ROOT unset. The package offered (A) move ~850 lines to an xtask gate in a new row, (B) re-express per crate, (C) point the knob at the monorepo.
- Chose: a fourth option, smaller than (B). `[lift.<id>] tree = { VAR = "<sandbox-relative dir>" }` names a directory of the sandbox itself. ingest sets `SOVEREIGN_WORKSPACE_ROOT = "crates"`. The census's roots become every `<tree>/*/src` plus `<tree>/sovereign/crates/*/src`, and it asserts that `corpus-engine/src` is among them. pb-ingest is marked `[x]`.
- Because: the knob already means "the source tree", and a lift's `crates/` holds each closure crate by name, just as the repo root holds corpus-engine, corpus-index and understanding-vocab. So recipe_schema's paths resolve unchanged, and they are checked against the lifted sources, not this repository's (unlike C). The census then checks the lifted closure, which is the part the ingest developer takes. Principle 11: the existing knob and the existing carry mechanism are reused, with no new resolver and no moved gate. Principle 5: the census was watched failing in the lift.

<!-- appendix -->

## phase-b-40 · 2026-09-28 — LIFT(ingest)'s source gates read the lift's own crates/

<details><summary>reasoning, evidence, package</summary>

Evidence, all run this session:
- In-repo: `cargo test -p corpus-engine --features treesitter --test main -- enrichment_type recipe_schema` gave 5 passed.
- `scripts/program-lift.sh --sandbox ingest` gave PASSED: tests/main 199 passed / 0 failed, corpus-engine lib 1729/0. The RUN smoke indexed the fixture with 4 embedding calls and no chat. Log: target/ralph/phase-b/lift-ingest-director.log. Record: target/program-lift/ingest/last.json.
- PLANT: appending `enrichment_type == "atlas"` to the lifted crates/sovereign-pipeline/src/lib.rs made the census FAIL, naming that line. It was reverted.
- `cargo xtask boundary-gate` gave EXIT=1 with 39 violations, delta 0.

The monorepo sweep now covers the top-level crates as well (corpus-index, understanding-vocab, oicp-types, …), not only corpus-engine and sovereign/crates. It found no new hits. The census had already ignored the one doc-comment mention at corpus-engine/src/recipe.rs:549.

What would falsify this: a closure crate that recipe_schema reads, but that the lift lays out under a name other than its repo-root directory name. The test would then panic naming the path, so the failure would be loud, not silent. Another falsifier: a monorepo-only invariant that the census would need to check against crates outside the ingest closure. That check still runs in-repo on every TEST.

</details>
