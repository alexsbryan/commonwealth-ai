# OICP / Scheduler Rationalization — audit + plan

Status: **EXECUTED — all five phases landed 2026-06-10** (same day as
the audit). Every phase gated on the full workspace suite; final
state 6 655 tests / 0 failures. Per-finding outcomes inline below
(`✔ FIXED` markers). The three disclosed behavior changes:

1. **Joiner adopts gossiped `inference_availability`** (was dropped
   on the floor; busy peers were scored as idle). SSOT term, clamp
   [0.2, 1.0], `None` = neutral for self-scoring.
2. **`best_claim_for_request` skips `status.available == false`
   models on every path** (the mesh path previously scored
   unavailable models — latent gap; the selector already filtered).
3. **Commonwealth claim synthesis is multi-claim** (small models
   advertise Fast claims; manifests carry real `size_gb`).

ci-bench was deliberately not run for these: each change is
structurally invisible to single-node sovereign benches (availability
= multi-peer selection only; multi-claim = standalone-daemon manifest
fallback only — Sovereign-embedded daemons serve sovereign's own
manifest). Evidence = golden pins, 9 re-pinned behavioral scenarios,
unit tests, and the mesh e2e suites inside the workspace gate.

Original audit follows. Method: two independent code audits
(spec-vs-impl; decision-point census) + targeted liveness
verification of contested verdicts.

---

## 1. The map — every place that decides "who serves this request"

| # | Decision | Where | Formula | Glassbox (can you reconstruct from logs?) |
|---|---|---|---|---|
| D1 | Joiner picks peer-vs-local | `sovereign-mesh/peer_inference.rs:846` `select_peer` | claim × obs-mult × load × locality × cold-start × throughput (**no availability term**) | ✅ ~85% — well traced |
| D2 | Daemon picks local model for a peer request | `commonwealth-api/routes_inference.rs:337` `route_with_oicp` → `commonwealth-inference/scheduler/oicp_select.rs:139` | same product **plus availability** — an independent, already-divergent reimplementation | ❌ 0% — completely untraced |
| D3 | Serving peer picks Fast-vs-Slow slot | `sovereign-mesh/inference_adapter.rs:488` → `sovereign-mesh/oicp_select.rs:213` | latency_class→Speed map + hint veto (no scoring; correct shape) | ✅ ~90% |
| D4 | Synthesis tier (Fast vs Primary) | `sovereign-core/runtime/evidence.rs:441` `resolve_synthesis_route` | intent + atom-enum + evidence-shape heuristic | ✅ ~95% |
| D5 | Handler Speed assignments | ~60 hardcoded `Speed::` sites across `runtime/handlers/*` | none — hardcoded per handler, some bypass D4 | ❌ ~40% |
| D6 | Observation EWMAs feeding D1 | `throughput_tracking.rs` + `peer_inference.rs` record_* | per-peer keyed by **name string**; separate per-model in-flight map | ✅ good |

The healthy parts, on the record: **oicp-types is in good shape** — the
scoring helpers are documented, constant-justified (mostly), and pinned
by ~50 unit tests; D3 and D4 are clean single-purpose deciders with
good traces; the v0.3 spec itself is coherent.

## 2. Findings

**F1 — Duplicate scoring engine, already divergent (CRITICAL). ✔ FIXED (Phase B)** — SSOT `score_with_adjustments` in oicp-types; equivalence pinned by golden tests; availability decided once.
`commonwealth-inference/scheduler/oicp_select.rs` and
`sovereign-mesh`'s `adjust_for_observations` path implement the same
multiplicative formula independently. They have already drifted: the
commonwealth side multiplies `inference_availability`, the
sovereign-mesh Joiner side does not. No test enforces equivalence; a
constant change must be made twice and won't be. This is exactly the
class of bug as the router-classifier-parity incident (2026-06-09:
bench improved while desktop degraded because the stack was wired in
one place only).

**F2 — ✔ FIXED (Phase A): deleted.** A dead twin scheduler shadowing the live one (CRITICAL for the
mid-level reader).** In `commonwealth-inference/scheduler/`:
`ModelPortfolio` (+ SWAP_THRESHOLD), `adaptive.rs`'s
`InferenceScheduler`, `plan_builder`, `layer_assignment`, and the
orchestrator's llama-server spawning have **zero runtime callers**
(verified: nothing in commonwealth-daemon or commonwealth-api invokes
`build_shard_plan`/`build_inference_plan`/Orchestrator). The LIVE
distributed-inference path is `sovereign-inference/rpc_distribution`.
Meanwhile SYSTEM_OVERVIEW's scheduling table documents the dead one as
if live. A maintainer reading "Scheduling + orchestration" today is
reading about code that never runs. Mixed case: `knowledge_assignment`
is **live** for `plan_collaborative_ingestion*`
(corpus_collaborate.rs:619,669,718) but its `assign_knowledge_shards`
neighbor appears dead — same file, opposite fates, no signage.
`usage_predictor`: dead (but reserved by MESH_INFERENCE Inc 5).

**F3 — D2 is invisible (HIGH). ✔ FIXED (Phase B)** — `route_with_oicp` traces every candidate + winner; D1/selector emit full `ScoreBreakdown` events. `route_with_oicp` — the path every
peer-served request takes on the hub — emits no tracing. "Why did the
hub answer with model A" requires re-deriving synthesized claims by
hand (~30 min). The composed score breakdown is logged NOWHERE on any
path (D1 logs only the throughput term).

**F4 — Speed fragmentation (HIGH). ✔ FIXED (Phase C)** — `SynthesisRoute::to_speed()` + named `speed_for_retrieval_intent`; intentional pins documented in place. ~60 hardcoded `Speed::`
assignments across handlers; `resolve_synthesis_route` exists (role.rs
made it load-bearing 2026-06-09) but its output is manually re-mapped
at three sites in knowledge_query.rs and bypassed entirely by other
handlers. "Intent decides HOW" is the established principle; today the
HOW is scattered.

**F5 — Claim production is thinner than the spec implies (MEDIUM). ✔ FIXED (Phase D)** — multi-claim synthesis shared by advertiser+scheduler; affinity reality documented.
Commonwealth's `synthesize_default_claim` advertises only
`Normal/32k/2k` claims (no Fast claims even for small models — known
PR-E gap); affinity everywhere is a static startup constant derived
from profile config, while doc-comments say "self-assessed" — a
mid-level reader will expect a feedback loop that doesn't exist.
(Observation-blending at *scoring* time is real and spec-compliant;
the advertisement never changes.)

**F6 — Availability semantics undecided (MEDIUM). ✔ DECIDED (Phase B)** — adopted everywhere, None=neutral for self. The one-sided
`inference_availability` multiplier (F1) is also a design question:
should the Joiner trust gossiped availability when it has fresher
local observations? Today the answer differs by code path by accident.

**F7 — Observation keying fragility (LOW-MEDIUM). ⏳ OPEN (accepted)** — name-string keying stands; revisit if a rename-reset ever bites. Per-peer
observations keyed by peer **name string** (rename = history reset);
per-model in-flight lives in a second map with different semantics.

**F8 — Minor honesty gaps (LOW). ✔ FIXED (Phases B+D)** — constants documented; composed product integration-tested; spec §6a cross-references the scorer. `LATENCY_ADJACENT_SCORE=0.8` /
`TWO_CLASS=0.5` carry no rationale comment; integration of the full
product is untested (only the individual factors are pinned); the
spec's §6 example formula and the implemented product should be
cross-referenced.

## 3. Rationalization plan

Discipline per phase: `callers`/`blast` before every deletion or
signature change; workspace gates green per phase; one concern per PR.

### Phase A — truth in the map (delete/label the dead twin)
- Verify-then-delete in `commonwealth-inference`: `portfolio.rs`,
  `adaptive.rs`, `plan_builder.rs`, `layer_assignment.rs`, and the
  orchestrator llama-server path — OR, where deletion is premature
  (test-harness uses), move under a `#![doc = "BENCH-ONLY"]`-bannered
  module with no pub re-export from the crate root. The repo's
  established preference is delete-over-archive (recipes SOT, legacy
  atlas precedents).
- Split `knowledge_assignment.rs`: keep the live
  `plan_collaborative_ingestion*` family; delete dead neighbors.
- `usage_predictor`: delete now, rebuild against the real idle-hours
  signal in MESH_INFERENCE Inc 5 (it predicts the wrong thing anyway —
  demand type, not idleness).
- Fix SYSTEM_OVERVIEW's scheduling section to describe the LIVE
  topology: D1–D6 above + `rpc_distribution` as the only distributed
  path.

### Phase B — one scoring engine (the SSOT move)
- The composed scorer moves to **oicp-types** next to its factor
  helpers: `score_candidate(claim_ctx, obs_ctx) → Option<ScoredCandidate>`
  where `ScoredCandidate` carries the final score **and a
  `ScoreBreakdown`** (claim_score, obs_mult, load, locality,
  cold_start, throughput+source, availability) — the breakdown is the
  glassbox artifact.
- D1 and D2 both consume it. Availability semantics decided once
  (proposal: availability multiplies on BOTH paths; the Joiner's
  fresher local failure-rate already lives in obs_mult, so the terms
  are complementary, not redundant — but this is the one open design
  question for review).
- Golden tests: current-behavior vectors for both call sites pinned
  BEFORE the move (the IpTransport golden-URL pattern); an equivalence
  test that the two paths produce identical breakdowns for identical
  inputs, forever.
- One `tracing` event per decision, emitting the full breakdown for
  every considered candidate at debug and the winner at info. Exit:
  "why did this go to the 4B" answerable from logs in <5 min on EVERY
  path (D2's current answer is 30 min of hand-derivation).

### Phase C — one Speed decider
- `SynthesisRoute::to_speed()` (or extend role.rs's resolver) so D4's
  output maps mechanically; the three manual re-mappings in
  knowledge_query.rs collapse onto it.
- Census the ~60 hardcoded `Speed::` handler sites into named role
  intents (most are legitimately "this handler is Primary work" — the
  fix is routing them through one named function, not changing
  behavior). Bench gates: ci-bench core must be flat.

### Phase D — claim honesty
- Doc-comment reality onto `CapabilityClaim.affinity` ("static,
  config-derived at startup; observation blending happens scorer-side")
  and onto the latency-mismatch constants (rationale or "non-normative
  reference values, pinned for interop").
- Commonwealth Fast-claims gap: either implement multi-claim synthesis
  (small models advertise Fast) or explicitly close the PR-E note as
  wontfix-until-needed. Decide, don't leave the TODO ambiguous.
- Integration test of the full product (composed formula with all
  factors non-default) in oicp-types.

### Phase E — docs + drift
- oicp-v0.3.md cross-references the implemented scorer; SYSTEM_OVERVIEW
  updated (Phase A did the topology, this does the scoring story);
  re-run drift so the narrative claims re-anchor.

## 4. Acceptance — the mid-level-engineer bar, made concrete

1. **One formula, one file**: grep `load_penalty\|cold_start_weight`
   consumers → exactly one composed scorer; equivalence enforced by
   test, not vigilance.
2. **Every routing decision reconstructible from logs alone in <5
   minutes** — D1 through D5, including the hub's D2.
3. **No dead module without a banner**: everything in scheduler/ is
   either runtime-live, deleted, or explicitly marked bench-only at
   the module head.
4. **The docs describe the running system**: a new engineer reading
   SYSTEM_OVERVIEW §Scheduling finds rpc_distribution and D1–D6, not
   ModelPortfolio.
5. Gates: full workspace lint+test green; ci-bench core flat
   (routing behavior unchanged — this whole plan is supposed to be
   behavior-preserving except where divergence F1/F6 forces a decided
   unification, disclosed in the PR).

## 5. Relationship to MESH_INFERENCE

This plan is the foundation Inc 1–4 stand on: the cache-affinity and
verify-compat terms become one more factor in the Phase-B SSOT scorer
(with breakdown logging for free); the cascade (Inc 2) lands on
Phase C's single Speed decider; household-bench's per-decision
attribution comes from Phase B's breakdown events. LatencyMatrix
wiring (Inc 1) waits until Phase B exists so it's added once, not
twice.
