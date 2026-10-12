<!-- ledger -->

**engine-swap-1 · 2026-10-12 · es-m3-move-agnostic · director** — this commit
- Needed: the row asked for six moves to `sovereign-compute` with a re-export at the old path and the parity tests moved to `oicp-client`; both are impossible as written.
- Chose: shrink es-m3 to `gguf_meta` (re-exported) plus `setup_planner` and `engine_conformance` (callers repointed, no re-export); split `hardware`/`capacity`/`cpu_compat`/`engine_capture` into es-m3-move-agnostic-after-g2 behind es-g2; delete the parity tests with the tries in es-g2.
- Because: `sovereign-compute` depends on `sovereign-inference`, so an old-path re-export is a cycle, and the engine still uses four of the six; P itself says the parity tests are deleted with the tries.

<!-- appendix -->

## engine-swap-1 · 2026-10-12 — es-m3 shrinks to the moves with no engine user; the rest waits for G2

<details><summary>reasoning, evidence, package</summary>

The premise was false (charter: "A false row premise: verify the census,
rewrite the row"). Reproduced in the lane at 95ae59e68:

- `serve/crates/sovereign-compute/Cargo.toml:32` is
  `sovereign-inference = { workspace = true }`. A `pub use
  sovereign_compute::x` inside `sovereign-inference` needs the reverse edge.
- Engine users inside SI, by `git grep -nE "(crate|super)::<m>\b"`:
  `hardware` in nine `embedded/` files plus `llama.rs:405` and
  `reranker_standalone.rs:135`; `capacity` in `reranker_standalone.rs:136-146`
  and `served_kind.rs:484`; `cpu_compat` in `embedded/model_slot.rs:1524,2282`;
  `engine_capture` in `engine_factory.rs:223`. `setup_planner` and
  `engine_conformance` have none, and `setup_planner`'s one SI import,
  `hardware::ProfileName`, is itself a re-export of
  `sovereign_contracts::daemon_wire::ProfileName` (`hardware.rs:16`).
- External callers of the two: `sovereign-compute` (assets.rs:27,
  setup_reads.rs:17), `sovereign-serve` setup_probe.rs:23 (serve depends on
  compute), SI's `examples/custom_engine.rs:49`; cli-daemon mentions them only
  in comments. So repointing is closed and no twin is left.
- `oicp-client/Cargo.toml` has no `llguidance`, and the tests import
  `crate::url_constraint`, `crate::evidence_id_constraint`,
  `crate::llguidance_constraint` (on llama token types), all deleted in G2.
  ENGINE_SWAP_PLAN.md: "Its parity test with the tries (`bd3ac4ea6`) has done
  its job and is deleted with them." Deleting a test of deleted code is not
  deleting a test to make a gate pass; the bench lane's `llg-parity` twin stays.

Dropping the old-path re-export for two modules departs from the row's
mechanics, not its outcome (the modules land where ARCH 12 puts them, one
copy each). PASS BARs unchanged except TEST(oicp-client), which only covered
the dropped test move.

Findings for the operator, not acted on (scope frozen): the same cycle sits
under es-g1 ("re-exported at the old paths" for `engine_factory`/`served_kind`)
and es-m4 ("the engine keeps calling the moved planner" in compute). Their
directors will hit it; the likely shape is the same, order the move after the
engine delete or repoint callers.

Falsified if: `sovereign-compute` drops its `sovereign-inference` dependency
before es-g2 (then the re-export works and the split was unnecessary), or a
crate below SI that may carry `sysinfo`/process probes turns up as the right
home for `hardware`.

</details>
