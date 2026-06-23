# Mesh / Scheduler / P2P QA

How we test the distributed substrate (gossip, membership, scheduler/admission,
transport) for correctness and resilience before it ships to a wide user base.
Three complementary layers, plus the structural fixes the harness surfaced and
gates. Companion to the design in the QA plan; this is the operational how-to.

## Why this exists

The mesh primitives are well unit-tested in isolation, but the *emergent*
behaviour — convergence under partition, decay under clock skew, admission under
load, fan-out under an unscoped query — was previously unproven, and three
resource-exhaustion bugs had already crash-looped the daemon in production. This
QA layer makes those behaviours observable, reproducible, and gated.

## Layer 1 — Deterministic simulation (in-process, CI-able)

N in-process nodes running the **real** gossip path (`run_one_round`) against
live in-process axum servers, through a per-node fault-injecting transport, with
a seeded fault schedule. Quiesce-then-assert against the invariant pack. Fast,
no GPU/network/weights — runs in CI.

```
cargo test -p sovereign-mesh --features dst --test dst_scenarios
```

- Harness: `commonwealth-test-harness/src/fault/` (FaultTransport, FaultProxy,
  FaultPolicy, seeded FaultSchedule) + `MockLlamaServer` knobs.
- Driver + invariant pack: `sovereign-mesh/src/dst.rs` (`DstMesh`). Wire-fault
  catalog: `slow_peer` / `truncate_stream` (throttle / cut a gossip edge) +
  `clock_jump_back` (non-monotonic wall clock), on top of `crash` / `skew_node` /
  `partition` / `clear_faults`. Assert on outcome **class** (reconverges once
  healed / times out), never on latency.
- Clock injection: `commonwealth-core::{Clock, SystemClock, TestClock}` — drives
  per-node skew deterministically (and is the seam the offline-decay fix uses).

A failing scenario prints its `seed`; re-run with that seed to reproduce. Gossip
peer-selection itself is **unseeded** (thread-local) — only the fault *schedule*
is seeded — so a heal-then-reconverge fixpoint can plateau on a stable-but-not-
yet-agreed state (one node still shows a healed peer offline). Heal-then-assert
scenarios therefore quiesce on **agreement**, not just stability:
`gossip_until_quiescent_agreed` requires every up node's member view to be
pairwise identical before the invariant pack runs, while plain
`gossip_until_quiescent` stays stability-only — correct *while* a partition is
active, where the two sides legitimately disagree. The
`agreed_quiesce_rejects_stable_disagreement` scenario pins the distinction (an
active partition reaches stability but **not** agreement). This removed the
former `partition_then_heal` coin-flip at the DST layer; the multi-process soak
quiesces on the same agreement principle via `wait_online_eq`. Agreement needs a
**larger round budget** than stability (it's strictly harder to reach): the
reconverge calls use 32 / 40 rounds, not the old 16 — an overnight 5× surfaced a
~1-in-15 `MaxRoundsExceeded{16}` tail (agreement not yet propagated to all nodes
under unseeded gossip), and 32 verifies 40/40 on the two reconverge tests. The
loop returns early once agreement lands, so the larger budget only costs on the
slow tail.

## Layer 2 — Multi-process soak (real bytes)

Real `sovereign daemon` processes forming one mesh over real TCP gossip, driven
through real faults — a genuine `kill -9` crash, real wall-clock offline-decay, a
**resume** restart (a production restart resumes from `mesh.json`; it does not
re-join), and a **kill-9-startup-window torture** (kill again mid-startup) — in
repeated cycles, asserting the invariant pack at every checkpoint *after an
agreement-based quiesce* (`wait_online_eq`). On any violation it writes a
**forensic bundle** to `mesh-soak-repro/` (each node's live id vs node_id-file vs
`mesh.json` self-id, config ports, daemon identity events) so an intermittent
failure is root-causable offline without re-running — this is what cracked the
restart-identity bug (a leaked-loop-var data-dir cross-wire in the harness, not a
daemon bug). The verdict prints a **coverage-accounting** grid (faults ×
invariant pack). Assertions are Rust (`check-invariants`); orchestration is shell.

```
# local backend — VALIDATED. Real subprocesses, fully isolated (see below).
scripts/mesh-soak.sh --nodes 3 --minutes 30 --seed 42 --gate

# podman backend — the nightly/CI scaling target. Adds the OS-fault catalog that
# needs containers: real network partitions (network disconnect), cgroup memory
# limits (the real OOM path), tc/netem. Mirrors the local mechanics; needs an
# image carrying sovereign-cli. Not yet exercised on this host.
MESH_SOAK_BACKEND=podman scripts/mesh-soak.sh --nodes 5 --minutes 60 --seed 42
```

**Isolation (load-bearing).** The local backend re-execs the whole soak inside a
rootless network namespace (`unshare -rn`, loopback-only). The daemon has no
mDNS-disable knob and the CLI `mesh join` is hardcoded to `:9741`, so on the bare
host the test nodes would advertise into — and try to join — the operator's *real
production mesh*. The netns seals them to `lo`: they self-advertise `127.0.0.1`,
join each other over a localhost relay (`POST /v1/mesh/join` directly, never the
CLI), and never touch the host mesh. Verified: the host's real mesh member count
is unchanged across a full soak.

**Tiny model by design.** A mesh soak needs daemons that boot + gossip, not infer;
the eager model load just has to succeed. `primary` points at a small embedding
GGUF (~600 MB/node, override with `MESH_SOAK_MODEL`), so N nodes fit in RAM.

- Assertion engine: `sovereign mesh check-invariants --nodes <a:port,...> [--expect-live <id,...>] [--json]`
  polls `GET /v1/mesh/status` (the **client** port — the internal port 404s) and
  evaluates convergence / no-ghost / liveness; exits non-zero on violation. Pure
  eval is unit-tested in `sovereign-cli-llm/src/mesh_soak.rs`.
- Findings stream to `mesh-soak-findings.jsonl`; `--gate` runs Layer 3 at the end.
- The local backend needs only a `cargo build --bins` plus `ip` + `unshare`. The
  podman backend's OOM / partition / netem faults are its reason to exist in CI.

## Layer 3 — Load / SLO regression gate

Distils a soak run's findings into SLIs and gates each against a committed
baseline (direction + tolerance — the same shape as the `lane_baseline` quality
gate). Establish a baseline first, then ratchet — no absolute numbers from the air.

```
# after a soak run (writes mesh-soak-findings.jsonl):
sovereign mesh soak-gate mesh-soak-findings.jsonl --baseline mesh-slo-baseline.json --update-baseline  # first run: capture
sovereign mesh soak-gate mesh-soak-findings.jsonl --baseline mesh-slo-baseline.json                     # later: gate (exit 1 on regression)
```

SLIs gated today: `invariant_violation_rate`, `load_success_rate`, `load_p50_ms`,
`load_p99_ms` (from the soak's per-request load samples). Recovery-time and
sustained throughput are the next SLIs to add as the load driver grows. The
extraction + gate logic is unit-tested in `sovereign-cli-llm/src/mesh_soak.rs`.

## The invariant pack

Checked at quiescence by both Layer 1 (in-process) and Layer 2 (over HTTP):

| Invariant | Holds when | Fix that made it true |
|---|---|---|
| Convergence | all live nodes agree on the member set | (already held) |
| No split-brain | one leader / disjoint rendezvous ownership per view | (already held) |
| No ghost members | a left/revoked node is absent mesh-wide | **F — tombstones** |
| Unique ids | no two live nodes claim the same node_id; identity survives restart | node-id durability (daemon, already correct) + harness `boot_node` fix |
| Admission safety | peer in-flight ≤ ceiling, → 0 at quiescence | **B — headless ceiling** |
| Bounded fan-out | a search opens ≤ MAX_FANOUT_CORPORA, skips oversized | **C — fan-out bound** |
| Liveness | every up node seen live by every peer (skew-immune) | **A — offline-decay** |

`Unique ids` is the net that caught the restart-identity bug. `Admission safety`
and `Bounded fan-out` are now **HTTP-observable** too — the status surface was
widened with `peer_inflight_current/ceiling`, `fanout_inflight_current`, and
`active_corpus_ingests` (the daemon's `glassbox_signals`; `fanout_inflight` is the
outbound-peer-fan-out gauge maintained by `FanoutGuard` in `routes_knowledge.rs`)
— so the multi-process soak asserts them, not just the DST layer. Both are inert
under the cheap embed-only crash lane (no peer-inference, no shared model) and go
**live in the ingest×inference lane** (below) + DST.

Fault catalog (Layer 1 mechanism / Layer 2 mechanism): partition (empty
endpoints / `iptables`), crash (shutdown / `kill -9`), wire faults (FaultProxy /
`tc`), clock skew (TestClock / `libfaketime`), overload (LoadDriver + set
ceiling), giant unsealed corpus (mock 1M-row / real corpus + cgroup mem).

## Layer 2b — Ingest × inference contention (real-model lane)

Shared/mesh corpus ingest is a *resource* adversary, not a topology fault: the
ingest embed pipeline shares the single llama engine with interactive inference,
and `should_yield_to_foreground()` is **advisory only** (`state.rs`). The worst
real incidents lived here (the unscoped-fan-out OOM that crash-looped the daemon
twice). `scripts/mesh-soak.sh --workload ingest` boots a **real generative
primary** (`MESH_SOAK_MODEL`, default `models/Qwen3.5-2B.Q6_K.gguf`) + the 0.6B
embed with `yield_to_foreground_secs < 30`, installs the real `chaos-secret-agent`
corpus daemon-side (`POST /internal/corpus/install` — the daemon owns the ingest,
so it genuinely competes for the engine), and drives a steady stream of
`/v1/chat/completions` at the same node, asserting under contention:

- **ForegroundLiveness** — chat keeps returning within an SLO while ingest runs
  (asserted on outcome *class*, not absolute ms).
- **IngestProgress** — the per-corpus progress phase *advances* across polls
  (forward progress / non-stalling); a frozen phase under load is the failure
  (heavy chat *throttling* ingest is correct, *starving* it is not).
- **AdmissionSafety / BoundedFanOut** go **live** — real chat fan-out drives
  `peer_inflight` / `fanout_inflight` > 0 (inert in the embed-only crash lane).
- the base invariant pack holds **during** ingest (checked every cycle).

**Validated 2026-06-22** on a 3× `Qwen3.5-2B` netns mesh: ingest advanced through
3 progress phases while **27/27 chats returned 200 within SLO** (latency rose to
~1.9 s during the heavy embed phase, then recovered to ~0.66 s) — both progressed,
no crash, base invariants green every cycle (**210 invariant cell-checks, 0
failures, PASS**). A wrinkle the run surfaced: the soak daemon's engine has no
local recipe-override dir and `chaos-secret-agent` isn't in the bundled catalog,
so the lane exports `SOVEREIGN_RECIPES_DIR=~/.sovereign/recipes` (registry
resolution step 1b) — without it the install 200s with `spawned:false` ("No
registry entry"). Needs ~3 GB/node and the chaos corpus cached once
(`scripts/setup-chaos-corpus.sh`); stop the production 35B daemon first.

## Layer 3 — OS-fault tier (rootless; no podman)

Faults that need an OS/kernel boundary the netns can't reach. **Decision: these
run rootless — cgroup-v2 + mount-ns + pre-write — *not* a podman/container
backend.** The dev box is inside a toolbox (itself a podman container); a
podman-in-podman backend is fragile (no `crun` in this toolbox, `setgroups()`-
blocked image builds), and toolbox-as-node loses the very isolation the soak needs
(toolboxes share the host network namespace → they'd cross-talk with the
operator's real mesh and can't be partitioned). The faults below need bounded
memory / a bad file / a blocked link — none of which require a separate rootfs:

- **corrupt-persisted-state** (`--workload corrupt`, **landed; recovery
  validated**) — pre-write garbage into a node's durable `mesh.json`, then resume.
  The daemon must fail-safe (never adopt a colliding or garbage id); `UniqueIds` +
  `NoGhost` + convergence are the net. Container-free, runs in the existing netns.
  Real netns run: the daemon **resumed with an intact, consistent identity** — the
  separate `node_id` file survived the corrupt `mesh.json`, and the forensic bundle
  confirms `live == node_id_file == mesh.json_self` on every node, so fail-safe
  recovery + no id collision is proven. **The post-recovery *reconverge* check
  fails — and the OS-fault tier surfacing this is the point.** With the full decay
  window, the forensics show the resumed node comes back with an intact id but an
  **empty member table** (the corrupt `mesh.json` wiped its peer list); in the
  netns (no mDNS) it has no peers to gossip to, and its peers — having decayed it
  while it was down — no longer gossip to it, so there is **no reconvergence path
  without re-discovery**. Real finding, not a harness-timing flake: a node that
  loses its *membership* (not its identity) needs re-discovery. The lane should
  `join_to_founder` after a corrupt-state resume (model: catastrophic state loss =
  re-join), in contrast to the crash lane's intact-`mesh.json` bare-resume; in a
  real deployment mDNS would bridge it. Tracked as a follow-up.
- **cgroup-OOM** (next) — boot the daemon under a rootless memory cgroup
  (`systemd-run --user --scope -p MemoryMax=<bytes>`; validated available on this
  box — cgroup-v2 `memory` controller is delegated to the user slice). An ingest
  blow-up is OOM-killed in isolation; the existing kill→decay→restart→reconverge
  machinery asserts recovery. The rootless equivalent of `podman run --memory`,
  delivering the plan's "giant-corpus OOM can't take the daemon down" proof with no
  container.
- **disk-full** (next) — a small `tmpfs` data mount filled to ENOSPC (mount-ns,
  rootless); assert the daemon degrades safely (`mesh.json` write fails → no
  corruption, recovers on free).
- **partition** — already covered by the netns backend (policy / `iptables`-class
  edge cuts); the OS-fault tier adds nothing here.

Run from a context with the user systemd session bus (the toolbox or host);
operator-gated like the overnight soak, not part of the in-session cheap run.

## Structural fixes landed (and the invariant each gates)

- **A** — skew-immune offline-decay: decay measures local-observation staleness
  (`AppState::peer_last_contact` + `MergeReport.observed`), not the peer's
  gossiped `last_seen`. Fixes the "~9 min flap". (`gossip_skewed_last_seen_does_not_false_decay`)
- **B** — headless admission ceiling: `DaemonSection.max_peer_inflight` (default
  1) applied at daemon boot; the bare-`usize::MAX` default replaced by a named
  constant. (`max_peer_inflight_defaults_to_1`)
- **C** — bounded knowledge fan-out: `select_fanout_corpora` caps corpora count
  and, on the unsealed path, skips oversized corpora — server-side, so a missing
  client `corpora` arg can't bypass it. (`unsealed_search_skips_the_giant_corpus`)
- **D** — frontdoor limits: explicit request-body cap (`DefaultBodyLimit`) +
  slow-loris guard (`RequestBodyTimeoutLayer`) on both routers.
- **E** — doc reconciliation: `ARCHITECTURE.md` no longer asserts mutual-TLS or
  majority-vote revocation as shipped (neither is); points to the real posture.
- **F** — membership tombstone: `revoke_member` stamps a gossiped `removed_at`;
  event-time LWW in `merge_from` makes a removal out-compete stale live copies
  (the immortal-ghost fix), while a genuine rejoin still wins.
  (`tombstone_is_not_resurrected_by_stale_live_record`)

Graceful self-`leave` now gossips a self-tombstone before shutdown
(`gossip::announce_departure`, wired into `daemon::leave`) so a clean exit
propagates immediately instead of waiting on decay; `revoke_member` and
offline-decay remain the fallbacks for an unannounced drop.

## Go / no-go before going wide

Tie the decision to widen to a concrete bar, not a vibe:

1. DST invariant pack green across the fixed-seed set (CI, blocking once
   promoted). The suite is now **reliable**: the intermittent
   `partition_then_heal` failure was removed by agreed-quiescence —
   heal-then-assert scenarios wait for pairwise-identical views, not mere
   stability (`gossip_until_quiescent_agreed`), verified 5× clean
   single-threaded — so "DST green across seeds" is a real gate, not a coin-flip.
2. A clean overnight Layer-2 soak under the full fault schedule, including the
   OS-fault tier (corrupt-persisted-state today; cgroup-OOM / disk-full next —
   all **rootless, no podman**, per the toolbox decision in Layer 3).
3. The four **must-pass** invariants hold throughout: admission-safety,
   bounded-fan-out, no-double-emit, liveness — the ones a wide user base hits
   first and the two that already took the daemon down — now *exercised* live by
   the ingest×inference lane (real generative primary), not merely present.
