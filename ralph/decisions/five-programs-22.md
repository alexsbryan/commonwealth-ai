<!-- ledger -->

**five-programs-22 · 2026-09-24 · fp-10 (model serving has no serving host) · director** — this commit
- Needed: fp-10's worker halted before editing. The row says the daemon's inference, rpc-worker and compute supervisions "become serving-surface clients", but the daemon is the only process that serves a model, and the host §12 D2 names (cw-rails) is forbidden every sovereign-* edge.
- Chose: park fp-10 behind a new operator row, HUMAN-fp10-serving-host, placed last with three options and a recommendation (keep by `[[exception]]` until Phase B is minted). Same shape as five-programs-21 (fp-9). No code changed. Boundary gate FAILED at 62 violations (package run at ba832a97d, both daemon→inference/compute edges red).
- Because: every arm that closes the edges is the operator's under the charter — a new serving binary reverses D2's "do not build a new binary" and is Phase B, not a row; an `[[exception]]` row is reserved; doing the row literally makes every inference route on a stock install report absence, an end-user behaviour change. The loop keeps moving: fp-14, fp-42 and fp-47 are dep-ready.

<!-- appendix -->

## five-programs-22 · 2026-09-24 — fp-10 parked on the owner of model serving

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp10-20260924.md. Reproduced at ba832a97d:

- `grep -rn sovereign_inference sovereign/crates/sovereign-daemon/src` → 43 lines over 14 files; `sovereign_compute` → 20 in src, 6 in tests. The package said 42 / 19+6; the extra line each is within a doc comment and changes nothing.
- build/inference.rs:271 `sovereign_inference::engine_factory::build_engine(config)`; :454 `sovereign_compute::manager::build_compute_layer_with_distributed`; bin/sovereign-daemon.rs:59 `sovereign_compute::child_main::run`, :64 `sovereign_inference::rpc_worker_main::run`. The daemon constructs the engine and re-execs both children.
- quality/ARCH_LAYERS.toml:676-679 `[[forbid]] from = "commonwealth-rails" to = "sovereign-*"`, no except, reason: the binary is lifted out of the monorepo. cw-rails cannot own engine construction.
- sovereign-inference/src/embedded/rpc_distribution.rs:2385-2390 re-execs `current_exe()` for the rpc-worker; sovereign-cli-daemon/src/lib.rs:161 carries the second `rpc_worker_main::run` site. fp-25 depends on whichever binary owns serving.
- docs/FIVE_PROGRAMS.md:1037-1046 (D2) and :1128 (Phase B, "make the serving binary own the verbs", ~20 edges). fp-16 (d1aaa2843) recorded the same missing prerequisite for the mesh dial.

Why I did not decide: (a) a new cmnwlth serving binary reverses D2's text and is a phase; (b) is an `[[exception]]` row; (c) re-homing the daemon's serving half is a placement move of phase size. A literal stub dial is forbidden by §11 ("do not fake") and changes every chat answer. Recommendation (b) is written on the HUMAN row.

Falsified if a census finds a process other than the svrn daemon (or a sovereign CLI) already constructing an engine that a cmnwlth-package binary could expose — then (a) is an extension, not a new binary, and parking was unnecessary — or if ARCH_LAYERS gains a sovereign-inference except on the cw-rails forbid.

</details>
