<!-- ledger -->

**five-programs-30 · 2026-09-24 · fp-25 · director** — this commit
- Needed: fp-25's rpc-worker half landed (6b0a8e8a0). The worker halted because the rest of the edge needs three placement choices that the row does not name.
- Chose: park fp-25 on a new operator row, HUMAN-fp25-setup-host (the fp-7 and fp-10 shape). The rpc-worker half is recorded as landed. Recommendation to the operator: (b), an `[[exception]] package = "svrn"` for cli-daemon → sovereign-inference, which is fp-10's answer applied to first-run setup.
- Because: every path that closes the edge crosses a line the charter reserves for the operator. Contracts fails the charter's fs-free test: `capacity.rs:96` and `setup_planner.rs:339-401` both call std::fs, and setup_planner also uses reqwest. A new leaf is "admitting a new shared leaf". cli-daemon cannot own setup_planner, because desktop, cli-llm and sovereign-daemon use it too. The exec probe would move GPU detection into a binary the Windows sidecar staging does not build, and that binary has no windows-* features, which is end-user-observable. The exception itself is operator-only. Measured at boundary-gate 56.

<!-- appendix -->

## five-programs-30 · 2026-09-24 — fp-25's remainder is the operator's: park on HUMAN-fp25-setup-host

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp25b-20260924.md. Everything below was reproduced at 6b0a8e8a0.

- `cargo xtask boundary-gate` reports 56 violations. The only cli-daemon line is `sovereign-cli-daemon → sovereign-inference`.
- The `sovereign_inference` / `sovereign-inference` mentions in cli-daemon's src and Cargo.toml number 37, counting non-comment lines by grep.
- TSV:61 (`docs/FIVE_PROGRAMS_DECISIONS.tsv`) has fix_shape `exec+wire`, behaviour_delta "none if first-run setup stays available", and decision_needed `none`. The row names "portable moves" but gives them no home.
- `setup_planner` has users outside cli-daemon: sovereign-desktop (setup_flow.rs, setup_plan.rs, commands/hardware.rs, commands/models.rs), sovereign-cli-llm/corpus_snapshot_cmd.rs, sovereign-daemon/assets_http.rs, and the sovereign-core egress census tests. It does its I/O with std::fs and reqwest (setup_planner.rs:339-409).
- `capacity` reads the weights file with std::fs::metadata (capacity.rs:96). Inside inference it is used by reranker_standalone.rs:128-138; outside, by sovereign-daemon build/preflight.rs:119-125.
- `select_profile` is pure (hardware.rs:53). Besides cli-daemon, sovereign-daemon/assets_http.rs:89,118 uses it. Moving that one fn closes nothing, because detect_hardware, LlamaLogs and setup_planner still keep the edge.
- `sovereign-daemon/src/daemon_cmd/vram_plan.rs` differs from cli-daemon's twin only in the help-module path. That is a principle-8 duplicate, and it becomes an exec target whichever option the operator picks.
- scripts/stage-daemon-sidecar.sh:91 builds `-p sovereign-cli-daemon` only. Windows GPU arrives through cli-daemon's `windows-vulkan` / `windows-cuda` features (Cargo.toml:103-104 → sovereign-inference). sovereign-daemon's Cargo.toml has no windows-* feature. An exec probe served by sovereign-daemon would enumerate no GPU on a staged Windows build unless the staging contract changes as well.

Options the HUMAN row carries: (a) the exec phase. It needs a `--probe-hardware` Launch variant served by bin/sovereign-daemon.rs; a home for setup_planner and capacity, either a new shared leaf or planning exec'd into sovereign-daemon; the windows-* features forwarded on sovereign-daemon; and stage-daemon-sidecar building both binaries. That is multi-row and changes a documented build contract. (b) The exception, recommended. It costs no code and takes the gate from 56 to 55, and the reason is TSV:61's own first-run argument. It is deleted when Phase B (the fp-10 answer) moves serving and the probe together. (c) A new `sovereign-setup-planner` shared leaf (fs + reqwest) for setup_planner, capacity and select_profile. That removes the portable moves, but detect_hardware and LlamaLogs still need (a)'s exec, so on its own it closes nothing.

This decision does not cover the stale `:260` cite in docs/ENV_FLAGS.md:116, and it does not cover the gap where sovereign-daemon is not staged for the desktop. Both were noted in the package and are outside this row.

Falsifier: a charter-legal home turns up that closes the edge without an exception, a new leaf, or a staging change. For example, setup_planner and capacity might turn out to have no fs or reqwest use behind a feature, or cli-daemon's Windows sidecar might not need GPU enumeration at setup. In that case the park was wrong, and fp-25 should resume as a worker row.

Gate at decision: boundary-gate 56 violation(s).

</details>
