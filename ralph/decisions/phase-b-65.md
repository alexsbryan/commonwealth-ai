<!-- ledger -->

**phase-b-65 · 2026-09-29 · pb-serve-distributes · director (supervisor resolution 1)** — this commit
- Needed: pb-serve-distributes parked past twice its LIFT (65 commits, 13,381 changed Rust lines against ~5,750) with its compile-time outcome delivered and its runtime outcome (standalone serve distributing, registering, the mesh-of-two PROOF and the BAR) unbuilt.
- Chose: split. pb-serve-distributes is `[x]` at 99cae3b33 on the edge it closed; the remainder is the new row pb-serve-distributes-standalone, which pb-serve-ranks, pb-mesh-exit-transport and pb-mesh-exit-mesh now also depend on. The registration wire's home is routed as that row's first, trialed step: the four types to `oicp_types::origin` (§12 3a rung 2), the register/renew loop to sovereign-turn-client.
- Because:
  - The charter's split test: the two outcomes need different proofs. The edge half is proved at compile time (BOUNDARY, LAYER, the daemon's tests); the standalone half by a two-node run and a release-profile bar.
  - Raising the LIFT would keep an outcome that already landed hostage to one that has not started, and the scope guard exists to stop exactly that.
  - Extend, never re-own: `OriginKind` already moved to oicp-types for the same reason (commonwealth-* and sovereign-* cannot see each other), and one register/renew loop in a shared leaf serves svrn's work origin and serve's origins alike (principle 8). No leaf is admitted.
  - BOUNDARY EXIT=1, 20 violations at 99cae3b33 (director re-run), delta 0, both fp-10 rows absent from quality/ARCH_LAYERS.toml. This commit changes no Rust.

<!-- appendix -->

## phase-b-65 · 2026-09-29 — pb-serve-distributes splits at its closed edge; the standalone runtime proof becomes its own row

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md at 99cae3b33 (removed by this
commit). Its option (c)1 is taken; (c)2 is routed rather than decided,
because the director did not trial it and the charter forbids a rewrite it
cannot trial.

Reproduced by the director:

- `git log --grep='^pb-serve-distributes'` → 65 commits; summed
  `--shortstat -- '*.rs'` → 7,421 inserted + 5,960 deleted = 13,381 (the
  package's split of the two, 8,120/5,261, differs; the total matches).
- sovereign-daemon/Cargo.toml names neither sovereign-inference nor
  sovereign-compute (one comment line at :47); `git grep` of their paths in
  sovereign-daemon/src hits comments only.
- `cargo xtask boundary-gate` (toolbox, corpus-engine/): FAILED, 20
  violations, EXIT=1. `grep fp-10 quality/ARCH_LAYERS.toml` → one comment,
  no `[[exception]]`.
- `sovereign_serve::serve()` destructures `ServeAssembly { routes, run_lock,
  .. }` (sovereign-serve lib.rs:455), dropping `distribute`.
- `OriginRegistration`/`OriginClaim`/`Admit`/`Framing` are in
  commonwealth-media origins.rs; commonwealth-media's Cargo.toml does not
  name oicp-types; sovereign-serve and sovereign-turn-client both do.
- `keep_registered` is sovereign-daemon work_origin.rs:239;
  `register_origin`/`renew_origin` rails_client.rs:171/:183.
- The daemon's `vram_plan.rs` is gone; no `ReloadSource::Assembly` or
  serving `InProcess` arm remains in sovereign-daemon/src.
- `scripts/ralph.py plan --queue phase-b` after the edit: 87/103 done, no open
  row lacking `- finish` or `- trial`.

Not run by the director: TEST(sovereign-serve), TEST(sovereign-compute),
TEST(sovereign-cli-daemon). They move to the new row's check list, where the
PROOF exercises them; the worker's TEST(sovereign-daemon) 1161/0 is taken from
the package.

Falsified if: the registration-wire trial goes red on LAYER (commonwealth-media
→ oicp-types disallowed) or raises BOUNDARY — then the home is the operator's
(a leaf question), and the row parks with the trial; or pb-serve-ranks finds
it needs something only the edge half was meant to deliver and the edge half
did not.

</details>
