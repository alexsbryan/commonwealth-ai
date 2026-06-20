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
- Driver + invariant pack: `sovereign-mesh/src/dst.rs` (`DstMesh`).
- Clock injection: `commonwealth-core::{Clock, SystemClock, TestClock}` — drives
  per-node skew deterministically (and is the seam the offline-decay fix uses).

A failing scenario prints its `seed`; re-run with that seed to reproduce.

## Layer 2 — Multi-process soak (real bytes)

Real `sovereign daemon` processes under real OS-level faults (SIGKILL, network
partition, clock skew, memory pressure) — the failure modes only visible across
actual process + network boundaries. The assertions are Rust; the orchestration
is a shell.

```
# local subprocess backend (runnable on a dev box / toolbox; real multi-process)
scripts/mesh-soak.sh --nodes 3 --minutes 30 --seed 42

# podman backend (nightly/CI target — adds real partitions + cgroup OOM)
MESH_SOAK_BACKEND=podman scripts/mesh-soak.sh --nodes 5 --minutes 60 --seed 42
```

- Assertion engine: `sovereign mesh check-invariants --nodes <a:port,...> [--expect-live <id,...>] [--json]`
  polls `GET /v1/mesh/status` and evaluates convergence / no-ghost / liveness;
  exits non-zero on violation. Pure eval is unit-tested in
  `sovereign-cli-llm/src/mesh_soak.rs`.
- Findings stream to `mesh-soak-findings.jsonl` (one line per tick, seed-stamped).
- The podman backend needs an image carrying `sovereign-cli`; see the script
  header. The local backend needs only a `cargo build --bins`.

This is the layer that reproduces the real OOM path and the clock-skew flap.

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
| Admission safety | peer in-flight ≤ ceiling, → 0 at quiescence | **B — headless ceiling** |
| Bounded fan-out | a search opens ≤ MAX_FANOUT_CORPORA, skips oversized | **C — fan-out bound** |
| Liveness | every up node seen live by every peer (skew-immune) | **A — offline-decay** |

Fault catalog (Layer 1 mechanism / Layer 2 mechanism): partition (empty
endpoints / `iptables`), crash (shutdown / `kill -9`), wire faults (FaultProxy /
`tc`), clock skew (TestClock / `libfaketime`), overload (LoadDriver + set
ceiling), giant unsealed corpus (mock 1M-row / real corpus + cgroup mem).

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

Remaining operational step: graceful self-`leave` gossiping a self-tombstone
(today a left node is removed via `revoke_member` or decays to offline).

## Go / no-go before going wide

Tie the decision to widen to a concrete bar, not a vibe:

1. DST invariant pack green across the fixed-seed set (CI, blocking once promoted).
2. A clean overnight Layer-2 soak under the full fault schedule.
3. The four **must-pass** invariants hold throughout: admission-safety,
   bounded-fan-out, no-double-emit, liveness — the ones a wide user base hits
   first and the two that already took the daemon down.
