<!-- ledger -->

**engine-swap-3 · 2026-10-23 · es-f1-rpc-peer-child · director** — this commit
- Needed: MACHINES.md gave es-f1 `needs` none, but its proof cannot run on the Mac: the e2e's last step (SIGKILL the daemon, no orphans) rests on the supervisor's parent-death signal, which is Linux-only, and the e2e reads `/proc`. The Mac session committed the code and unit tests and stopped with HEADROOM, the e2e and NO-ORPHANS never run.
- Chose: the row's `needs` becomes `[linux]`, MACHINES.md says why; acceptance unchanged. The Halo resumes this lane at 01792faf8 and owes HEADROOM, `rpc_peer_child_e2e` (its `booted:` line pasted) and NO-ORPHANS. The macOS orphan gap is filed as a finding, not a row.
- Because: the census was wrong about the OS, not the work; `linux` names the structural reason (es-portable's models-by-role does not lift it), and only the Halo provides it.

<!-- appendix -->

## engine-swap-3 · 2026-10-23 — es-f1's live proof is Linux's: needs [linux], the Halo resumes the lane

<details><summary>reasoning, evidence, package</summary>

Reproduced on the Mac in the lane at 01792faf8 (Darwin 27.0.0 arm64):

- `serve/crates/sovereign-compute/src/supervisor/child.rs:170` gates the
  `prctl(PR_SET_PDEATHSIG, SIGKILL)` block on `#[cfg(target_os = "linux")]`;
  the only portable piece is `process_group(0)` (`:168`). A SIGKILLed daemon
  on macOS reparents its children rather than killing them.
- `distributions/crates/sovereign-stock/tests/rpc_peer_child_e2e.rs:23` and
  `llama_server_router_e2e.rs:25` are both `#![cfg(target_os = "linux")]`;
  on the Mac the e2e compiles to nothing, so it cannot go red or green there.
- `target/debug` in this lane holds no `llama-server` or `ggml-rpc-server`,
  and `~/dev/commonwealth-ai/sovereign/models` has no `Qwen3.5-0.8B*` (both
  already noted in MACHINES.md as Mac gaps).

Worker's Mac checks, from `../control/package.md`: LINT exit 0 (45 crates);
TEST(sovereign-compute) 199/0; TEST(sovereign-serve) 265/0 after building
`cw-rails` into the scoped target; arch/docs/env/layer gates exit 0;
concept-gate COULD-NOT-JUDGE (no indexed corpus at the lane path).

Not chosen: `[halo]`. The pinned GGUF is a Halo reason too, but es-portable
removes it, while the PDEATHSIG and `/proc` reasons stay until a macOS
equivalent exists; `linux` is the one that stays true. The Halo is the only
host providing it, so the routing is the same today.

Finding, not a row (scope is frozen): on macOS the supervisor's children
outlive a SIGKILLed daemon (`child.rs:170` has no non-Linux branch). A Mac
peer (HUMAN-es-f3-mac-peer, es-f3) or a Mac router leaves
`ggml-rpc-server`/`llama-server` holding its port and memory after a daemon
crash. Reported to the operator; the worker also recorded it as a lesson.

</details>
