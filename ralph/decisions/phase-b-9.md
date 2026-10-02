<!-- ledger -->

**phase-b-9 · 2026-09-26 · pb-hostkit · director** — this commit
- Needed: pb-hostkit's census (NEEDS_HUMAN, before any edit) found that its first-content list contradicts its own proof. dirs.rs resolves through `sovereign_contracts::rebrand` (dirs.rs:26, :33), so moving it breaks the kit's `allow = ["workspace-hack"]` and puts sovereign-* in cw-rails' closure against `[[forbid]] commonwealth-rails → sovereign-*` (ARCH_LAYERS:677).
- Chose:
  - The kit's first content is run_lock alone. Its lock-file name becomes a caller argument, and the `svrn` hint in `Held`'s Display moves to the daemon's caller with the printed text unchanged.
  - dirs.rs and help.rs stay in sovereign-cli-base.
  - dispatcher.rs moves to sovereign-turn-client with urls.rs.
  - The fs4 collapse in scip is confirmed (same semantics), so flock mechanisms go 4 → 1.
- Because:
  - The first-match ladder in §12 3a places each module. dirs.rs and help.rs are used by svrn alone (help's consumers are sovereign-cli-mesh and sovereign-cli-shared only), so rung 1 keeps them there.
  - §2c: the kit's root "is always supplied by the caller", it "names no program's vocabulary", and "locating its binary" is sovereign-turn-client's half.
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-9 · 2026-09-26 — pb-hostkit's kit starts as the lock alone; dirs/help stay, dispatcher goes to the client

<details><summary>reasoning, evidence, package</summary>

Reproduced at 2173da55a: `grep -n sovereign_contracts sovereign-cli-base/src/dirs.rs` gives :26 `rebrand::svrnmesh_root()` and :33 `rebrand::work_atlas_toml()`. dispatcher.rs:9-27 hard-codes `sovereign-cli`, `svrn` and `~/.local/bin/sovereign`. `ARCH_LAYERS.toml:677` forbids commonwealth-rails → sovereign-* with no except list. cli-base totals 877 lines. run_lock.rs lives at sovereign-contracts/src/run_lock.rs. Its only out-of-crate users are daemon_cmd/boot.rs:224 (acquire) and cli-llm bench_cmd/ablate.rs:335 (`path_for`). Its Display at :116 names `svrn daemon status`. `ROOT_LOCK` is at commonwealth-rails/src/lib.rs:134, with `claim_root` at :138.

This departs from the package in one place. The package recommended moving help.rs into the kit because it is neutral mechanism. The ladder is first-match, and rung 1 ("one program uses it → that program") fires before rung 5. Moving a svrn-only module into the kit would create a kit module nobody outside svrn calls, which principle 12 names as the wrong line. If a second program adopts the Help builder, it moves then.

Falsified if: a second program's binary (cw-rails, corpus-mcp, the code server) needs `Help` or a path resolver. Then that module moves to the kit, with its root still supplied by the caller. Also falsified if the daemon's refusal text changes once the hint moves to its caller. That is an observable-behaviour change and goes to the operator.

</details>
