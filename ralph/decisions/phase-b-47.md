<!-- ledger -->

**phase-b-47 · 2026-09-28 · pb-ingest-dial-tools-close → tests split at the port; -close splits into -doubles, -e2e-local, -e2e-engine and a narrowed -close (BOUNDARY 23) · director** — this commit
- Needed: the worker stopped -close at census with no code written. The row's proviso ("tests may keep it as a dev-dependency only if the gate does not count it") is false: the gate counts dev edges. So 65 test-module sites, examples/triage_dump.rs and ten tests/ files (5,143 lines) must stop naming corpus-engine too, which is more than twice the row's LIFT. The package asked where cross-program tests live, how to split, and what happens to the `inference_to_inference_fn` re-export.
- Chose: tests split at the port (package option (b), the pb-grants-merge pattern). The svrn side drives one double per port, and the doubles live beside each trait in its leaf behind a dependency-free `test-doubles` feature. The engine side is re-proven on the implementor in corpus-engine over the same fixtures. -close splits by proof into -doubles (test modules, example, recipe tests, the doubles), -e2e-local (four local-corpus e2es), -e2e-engine (three engine-subject tests) and a narrowed -close. -close keeps the 12 non-test sites, the catalog port and `process::HostedIngest` carrying only that port, and BOUNDARY −2. The standalone absence for `wikipedia_fetch` and `post_finalize_corpus` moves to pb-ingest-dial-daemon, which owns the 16 hard-wired `IngestAtlas` sites that it needs. The re-export goes, and its four riders repoint to corpus-engine's path, opening no edge.
- Because: charter "a false row premise" and "splitting when proofs differ", plus FIVE_PROGRAMS §2c "extend, never re-own". The test-double-plus-implementor proof is already the queue's answer for grants (REVIEW-pb-preflight-3), and phase-b-38 already refused a dev-dependency proof. Options (a) (a new no-package crate) and (c) (a gate that exempts dev edges) are operator decisions, and (b) needs neither. Boundary gate at 451cc7ee2: EXIT=1, 23 violations; the trial gives 21. This commit touches no Rust. REVIEW-AFTER: pb-ingest-dial-tools-doubles lands (was its ~1,400-line LIFT honest, and did any real-engine assertion fail to find an implementor-side home?).

<!-- appendix -->

## phase-b-47 · 2026-09-28 — cross-program tests split at the port; -close splits four ways

<details><summary>reasoning, evidence, package</summary>

Reproduced this session at 451cc7ee2:
- corpus-engine/xtask/src/boundary_gate.rs:156 prints "dep closure incl. dev+build edges", and `package_budget_flags_a_breach_on_every_edge_kind` (:710-755) pins that a `DepKind::Dev` edge breaches. The row's proviso resolves against it.
- `grep -rlE corpus_engine:: tests examples` in sovereign-tools: ten test files, 5,143 lines (the package's per-file counts match), plus examples/triage_dump.rs.
- Trial (compile, reverted): removing corpus-engine and sovereign-enrichment-catalog from sovereign-tools in every dependency kind, with `treesitter = []`, then `cargo check -p sovereign-tools --all-targets`. Result: 89 errors (86 E0433, 3 E0432) at 77 distinct sites in 22 lib-target files. Of those, 12 are non-test (conv_tiered_provider.rs:36,479,480; corpus/mod.rs:25,213; corpus/wikipedia.rs:21; enrichment_bootstrap.rs:31,105; atlas_context_manager.rs:61; local_corpus/atlas_dispatch.rs:82; local_corpus/watched/enrich.rs:50,108) and 65 are under `#[cfg(test)]`. Integration and example targets are not reached because the lib fails first. `cargo xtask boundary-gate` gives 21 violations with EXIT=1, and `cargo xtask layer-gate` passes. Raw: target/ralph/phase-b/trials/t-ingest-dtools-close-alltargets-451cc7e.json. The package listed sec_edgar.rs as a non-test site, but its one site is in a test module.
- The four riders of the corpus/mod.rs:213 re-export are sovereign-cli-llm recipe_cmd.rs:351, bench_cmd/vault_report.rs:841, chat_cmd/bootstrap.rs:242 and sovereign-daemon bootstrap.rs:241. Both crates already link corpus-engine.
- sovereign-daemon/src/tool_registry.rs:48,63 hard-wire `corpus_engine::IngestAtlas`, so registry-reached tools cannot report absence until pb-ingest-dial-daemon threads the port. The daemon's Cargo.toml names no enrichment crate, so the catalog implementor can only arrive through a face. `svrn daemon start` launches sovereign-stock (sovereign-cli-daemon/src/daemon_bin.rs:19), so the default install keeps the watched-folder config write. `process::run` already takes `HostedServe` and `HostedCode` (process.rs:20), which is the pattern `HostedIngest` follows.
- `ForegroundSignal` is the leaf corpus-engine-yield's (src/lib.rs:82), so turn_foreground_lease's svrn assertion needs no engine.
- Split rows use the `pb-ingest-dial-tools-<suffix>` form, which scope.txt admits without a new line.

Options not taken: (a) a new no-package test crate, or stock `[[test]]` targets with the distribution gate taught to exempt them. That adds a crate the charter leaves to the operator and makes a test home in no program. (c) Changing the gate to exempt dev edges. That weakens a gate that says deliberately why it counts them, and it is the operator's call.

What would falsify this:
- A real-engine assertion in one of the e2es cannot be split: it needs svrn's code and the engine in one process, and the stock-binary proof in pb-ingest-dial-daemon cannot observe it. That case needs a composed test home, which is option (a) and goes to the operator.
- -doubles' measured changed lines exceed ~2,000. Then the per-port double is heavier than priced (for example `AtlasPort`'s 19 methods), and the split should be re-cut by port.
- -close cannot mint `HostedIngest` with only the catalog port without pb-ingest-dial-daemon's threading. Then the two rows touch the same sites and should fold.

</details>
