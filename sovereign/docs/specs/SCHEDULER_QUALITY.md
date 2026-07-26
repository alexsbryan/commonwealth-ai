# Scheduler quality — a measurement loop for OICP delegation

Status: **Phase 0 landed — instrumentation in place, simulator not
yet built** (2026-07-26). Diagnosis measured, build order proposed;
§6 Phase 0 (P1–P4) is implemented and gated, Phase 1 (the Tier-1
simulator) is the next step. Companion to
[`MESH_INFERENCE.md`](MESH_INFERENCE.md) (which specified
`household-bench` as Increment 0 and was never built) and
[`OICP_RATIONALIZATION.md`](OICP_RATIONALIZATION.md) (which unified the
scorer but deliberately preserved its shape).

Method: code audit of every decision point from request to dispatch,
plus two simulation probes that drive a faithful transcription of the
real scoring arithmetic. Every finding below carries a `file:line`.
Every number below is labelled either *measured from code* or *modelled*.

---

## 0. The question this doc exists to answer

Retrieval, grounding and synthesis each have a bench, a scoreboard and
a tight iteration loop. The scheduling and delegation layer has none —
it has unit tests on individual factors, golden pins on the composed
product, and mesh e2e suites that verify *plumbing*. Nothing measures
whether a routing decision was **good**.

The root cause is not neglect. It is that **the scorer ranks; it does
not predict**. `score_with_adjustments`
(`oicp-types/src/scoring.rs:529`) returns a product of six
dimensionless multipliers. Its output has no units, so there is no
statement of the form "the scheduler was wrong by X." You cannot build
a scoreboard for an undefined objective.

Everything in §2 follows from that, and §4's structural proposal
addresses it directly.

## 1. What is healthy — on the record

The June 10th rationalization did real work and this doc does not
relitigate it.

| | Verdict |
|---|---|
| One composed scorer, one file, consumers log the full `ScoreBreakdown` | ✅ Holds |
| `offload_eligible` as a single shared predicate for both joiner-side gates (`oicp_select.rs:68`) | ✅ Holds |
| D1 / D2 / D3 split — an inbound peer request is served by `SovereignInferenceAdapter` picking a **local** slot, never re-forwarded | ✅ Verified: no request-ping-pong hazard at N=12 |
| Ranked failover — a 503 on the best peer tries the next peer, not straight to local (`peer_inference.rs:1522`) | ✅ Holds |
| RAII in-flight guards, saturating decrements, drop-order documented (`peer_inference.rs:1597`) | ✅ Holds |
| DST harness for gossip convergence, seeded faults, quiesce-then-assert (`sovereign-mesh/src/dst.rs`) | ✅ The pattern Tier 1 below should copy |

The fragility is not in the code. It is in the **control loop**.

## 2. Findings

### F1 — Dead time exceeds service time (CRITICAL, structural)

Gossip anti-entropy runs every 10s (`gossip.rs:57`); full-mesh
propagation at N=12 takes several rounds. The peer-manifest cache is
60s (`peer_inference.rs:63`). A knowledge turn takes 10–20s.

A feedback controller whose delay exceeds its process time constant
oscillates. At N=2 this is invisible — the joiner's self-observed
in-flight count *is* the peer's true load. At N=12 a decider sees its
own share of the load exactly and everyone else's 10–30s late.

This is the reason the current stack "just works" solo and on a pair,
and why no existing test can see the problem: **every test has one
decider.**

### F2 — The shared "busy" signal is not a busy signal (CRITICAL)

`inference_availability` multiplies into the score
(`scoring.rs:547`, clamp `[0.2, 1.0]`). It is set from exactly one
place — `POST /internal/node/activity`
(`commonwealth-api/src/routes_internal/mesh_admin.rs:38`) — mapping
**human coding activity**: `hot=0.20 warm=0.65 cool=0.85 idle=1.00`.

Solving `load_penalty(n) = 1/(1 + 0.05n) = 0.20` gives **n = 80**. In
the scorer's own units, *one human at the keyboard equals eighty queued
inference requests*. A hub grinding through twenty peer requests with
nobody sitting at it advertises `1.00` and looks maximally idle.

In fairness: `current_in_flight` **is** gossiped and **does** override
the load term (`peer_inference.rs:1085`), so real load is visible — it
is F1-stale, not absent. It is the availability term that is
mis-specified, and it is weighted to dominate.

### F3 — The heterogeneity term does not discriminate under heterogeneity (HIGH)

`throughput_factor` (`scoring.rs:356`) divides by
`THROUGHPUT_REFERENCE_TG_TOK_S = 20.0` and clamps to `[0.3, 1.0]`.
Every node decoding faster than 20 tok/s scores 1.0. A 25 tok/s hub and
a 120 tok/s laptop on a 4B are indistinguishable. The
benchmark-estimate path scales a baseline by model-size ratio, which
overshoots the clamp in almost every real case.

The term exists specifically to handle a mixed fleet, and it is
constant across a mixed fleet. Its only live discrimination is *below*
20 tok/s — which is where the largest, highest-quality models sit.

### F4 — Congestion and failure share one channel (HIGH, latent)

A 503 from admission lands in the `Err` arm
(`peer_inference.rs:2036`) and calls `record_failure` — which bumps
both the failure-rate EMA (`peer_inference.rs:655`) and
`PeerHealthTracker`'s consecutive counter. Three consecutive failures
quarantine the peer for 60s, escalating by 60s per re-quarantine to a
600s cap (`peer_health.rs:47-56`).

Congestion is transient and self-clearing. The mechanism attached to it
is escalating exile.

**Live-ness caveat, and it matters.** All three admission gates are
**off by default**:

- `DEFAULT_PEER_INFLIGHT_CEILING = usize::MAX` (`state.rs:360`), and
  `effective_peer_cap` returns `u32::MAX` for that sentinel
  (`state.rs:377`) — the fair scheduler is a no-op
- `yield_window_secs` initialises to `0` (`state.rs:1186`) and nothing
  in production sets it — `should_yield_to_foreground` short-circuits
  false
- pause is operator-driven only

So today the hub **never sheds — it queues without bound**, and its
degradation mode is latency, not rejection. F4 is armed the moment
anyone enables backpressure. The first protection an operator would
reach for is the one that breaks the client.

Related latent trap, same area: `bump_foreground_active()` is called
unconditionally in the completions handler
(`routes_completions.rs:36`, `routes_inference.rs:43`), which runs
*after* `peer_admission_layer`. With a non-zero yield window, the first
admitted peer request self-arms the yield gate against every subsequent
peer request for the length of the window.

### F5 — Deterministic argmax over a shared signal is a herd generator (HIGH)

Twelve deciders computing the same `argmax` from the same gossiped
snapshot is the textbook condition for synchronized convergence. There
is no randomization, no hysteresis, and no per-decider jitter anywhere
in `select_peers_ranked`.

### F6 — Reciprocity implements proportionality where the intent is a floor (HIGH)

**The principle, stated by the project owner 2026-07-26 and normative
from here:**

> The mesh must not concentrate. A node that *cannot* afford to
> contribute — an old laptop, a metered link, an under-resourced region
> — must not be crowded out by nodes that already hold the resources
> and want to use all of them. Fairness here means **guaranteed
> floors**, not proportional reward.

This supersedes the framing in [`MESH_INFERENCE.md`](MESH_INFERENCE.md)
§1, which read the objection to contribution-weighting as an
anti-commodification/anti-ranking values point. The real objection is
narrower and sharper: **concentration**. That distinction changes the
design, because a floor and a cap are not the same mechanism and the
code currently implements the wrong one.

**What the code does today.** `refresh_reciprocity_weights`
(`state.rs:1596`) derives a per-node weight from the contribution
ledger and feeds `effective_peer_cap` (`state.rs:377`,
`PEER_RECIPROCITY_K = 0.5`, `PEER_BASE_CAP = 1`). Contribution buys
capacity in **two** places, both compounding:

1. **Cap size** — a neutral node's cap is 1 concurrent request; a top
   contributor's cap is the whole ceiling.
2. **Queue order** — `best_eligible` ranks waiters by
   `ranks_ahead(weight, seq)` (`fair_sched.rs:243`), FIFO only as a
   tiebreak *within* equal weight. So a low-weight waiter is passed by
   every high-weight waiter that arrives, indefinitely.

`PEER_BASE_CAP = 1` reads like the floor but is not one. It bounds
**your own** concurrency; it reserves **nothing**. Nothing in
`SchedCore` holds capacity back for a low-weight origin — when
`held == slots_total`, `try_grant` queues or sheds regardless of who
you are (`fair_sched.rs:329`). *A cap floor is not a service floor.*

**The inversion that makes it worse.** A node with a weak local model
is precisely the node for which peers most easily "strictly beat
local" (`peer_inference.rs:1129`) — so it depends on the mesh most. It
is also the node with the least to contribute, therefore the smallest
cap and the last place in line. **The mechanism sheds hardest exactly
where dependency is highest.** That is the concentration loop, stated
mechanically.

**Proposed rule — one sentence, and it is the design:**

> **Floors are equal; surplus may be earned.** Under contention,
> capacity is divided max-min fairly across origins regardless of
> contribution. Contribution can only influence access to capacity
> nobody else is currently asking for — and uncontended capacity, by
> definition, takes nothing from anyone.

Mechanically that is two changes, neither large:

- **Queue order stops ranking by contribution.** Replace `weight` in
  `ranks_ahead` with a *service deficit* — how little this origin has
  been served in a recent window relative to the others. Deficit round
  robin / max-min fair share; a well-understood policy with a
  well-defined optimum, which matters because it gives §5 an
  unambiguous fairness target.
- **Contribution moves to surplus only.** The reciprocity weight stops
  scaling `effective_peer_cap` under contention and instead governs
  who may exceed the equal share when slots are otherwise idle. This
  also resolves the charter question: the ledger never decides
  *whether* you are served, only who mops up slack, so no balance,
  exchange rate or ranking-that-gates-access is introduced.

**Bench obligations this creates** (added to §5's scoreboard):

- **worst-served-origin share** under sustained contention, and Jain's
  index across origins — the direct floor metric.
- **floor-violation events** — an origin whose grant rate fell below
  its equal share while another origin exceeded its own.
- **dependency-weighted shed rate** — shed rate conditioned on how
  much worse the origin's *local* option was. This is the metric that
  makes the inversion above visible; a healthy mesh drives it to zero,
  and a concentrating mesh drives it up.

Currently all of this is inert in production (the ceiling is
`usize::MAX`, so `effective_peer_cap` returns `u32::MAX` and no
rationing happens at all). That is the opportunity: **the fairness
policy can be designed and bench-proven before it ever gates a real
request.** It must not be switched on before the metrics above exist —
that ordering is the whole point of §6.

## 3. Measurement — what the probes showed

Two probes drive a faithful transcription of the real arithmetic:
`load_penalty`, `cold_start_weight`, `throughput_factor`,
`effective_affinity`, the composed product, the failure EMAs, the
`peer_health` state machine, and a 10s gossip broadcast. Constants and
their source lines are in Appendix A. **These are models, not the
system.** Absolute numbers are not predictive; the mechanism
attribution is the claim.

Scenario: 12 nodes (1 hub, 3 desktops, 8 laptops), 15 simulated
minutes, one knowledge query per node per ~90s, 1500 context / 250
output tokens, service rates in Appendix A. Arms differ only in what
the decider knows and how it picks.

| arm | completed | p50 s | p95 s | hub share | dispatch CoV |
|---|---|---|---|---|---|
| as-implemented | 113 | 23.8 | 77.5 | 53.7% | 1.83 |
| + fresh in-flight (no gossip lag) | 113 | 17.0 | 76.8 | 53.7% | 1.84 |
| + congestion ≠ failure | 113 | 23.8 | 77.5 | 53.7% | 1.83 |
| + fresh + two-choices sampling | 97 | 14.1 | **31.2** | 41.4% | 1.57 |

`throughput_factor` evaluated to 1.0 for hub, desktop and laptop alike
— F3, measured.

Three readings:

1. **Staleness costs the median (~29%); deterministic argmax costs the
   tail (2.5×).** Two different defects with two different fixes, and
   no current test can separate them.
2. The congestion arm is a no-op because no 503s fire — the gates are
   off (F4's caveat), confirmed from the other direction.
3. Two-choices completes *fewer* requests while cutting p95 by 2.5×,
   because it spreads onto slower laptops. That is a real trade-off the
   scoreboard must show rather than hide.

Second probe, the F4 composition: four turns from one spoke inside a
12s window during a yield window produces a 60s quarantine, **37s of
which lands after the human stops typing**. Three such windows in a day
ratchets the cooldown to 180s and climbing. Meanwhile the availability
term was *already* de-prioritizing that hub 5× — quarantine adds a hard
skip on top of a soft signal that was working correctly.

## 4. The proposal

### 4.1 The structural change — rank on predicted time, not on a product

Replace the dimensionless product with a two-stage decision:

- **feasibility filter** — claims, hints, context/output fits, privacy
  posture, availability of the model. Boolean. This is what
  `score_claim_for_request`'s hard gates already do; it stays.
- **cost ranking** — *predicted* time to completion:
  `queue_delay + ctx/pp_tok_s + out/tg_tok_s + rtt`.

Every input already exists: `BenchmarkResult` carries `pp_tok_s` /
`tg_tok_s` / `baseline_size_gb`; the OICP envelope carries
`context_tokens` / `max_output_tokens`; gossip carries
`current_in_flight`; the manifest probe already measures RTT
(`peer_inference.rs:695`).

What it buys:

- constants stop being tunable fudge and become measurable quantities
- the glassbox trace becomes legible to a mid-level engineer — "node B,
  predicted 4.2s; local 6.8s" instead of seven multipliers
- **the bench gets a well-defined oracle**, because the optimum
  minimizes the same quantity the scheduler estimates (this is what
  makes §5's efficiency ratio possible at all)
- heterogeneity is handled by construction rather than by a clamp (F3)
- `LatencyMatrix`, a dead wire since it was written, acquires an
  obvious consumer
- speed and quality stop sharing one scalar — capability filters,
  predicted cost ranks

Quality/tier preference remains a separate, explicit input (a tier
floor from `resolve_synthesis_route`), not a multiplier folded into the
same number as latency.

### 4.2 Three contained follow-ons, in order

1. **Fresh backpressure.** Piggyback the serving node's true queue
   depth and estimated wait on every response, including the 503 body.
   No extra round-trips; instantly fresh for any peer recently spoken
   to. Collapses F1's dead time for exactly the peers that matter.
2. **Break the herd.** Sample two among candidates whose predictions
   are within noise, take the less loaded. Small change to the ranked
   selector; largest tail improvement in §3.
3. **Split congestion from failure.** One enum at the record site —
   `Congested { retry_after }` vs `Failed`. Congestion drives a
   short-half-life per-peer backoff; only failure touches
   `PeerHealthTracker`. Honour the `Retry-After` header the server
   already sends (`admission.rs:150`) and the client currently
   discards. *Then* a finite ceiling becomes safe to enable and the hub
   gets a real degradation mode.

Priority classes ([`MESH_INFERENCE.md`](MESH_INFERENCE.md) Increment 4)
land after (3), never before — shedding without (3) manufactures F4.

## 5. The quality loop

The unlock: **the scheduling decision is a pure function, and the
expensive part — generating tokens — is exactly the part that does not
affect it.** The real decision code can be exercised at thousands of
scenarios per second with no GPU.

### Tier 1 — `mesh-sim` (seconds, seeded, deterministic)

A discrete-event mesh linking the **real** `oicp-types` scorer, the
**real** `PeerHealthTracker`, and a real model of gossip staleness and
manifest TTL. Only token generation is faked. Lives in `sovereign-mesh`
behind a feature flag, beside `dst.rs` — same crate, same rationale
(only this crate can name the internals), same quiesce-then-assert
discipline.

Scoreboard — this is what makes it a quality loop and not a test suite:

| metric | definition |
|---|---|
| **efficiency ratio** | achieved vs. a clairvoyant oracle assigning with full knowledge. One headline number in [0,1]. Only definable once §4.1 lands. |
| **tail fairness** | per-actor p95 spread (`MESH_INFERENCE.md` H2) |
| **floor guarantee** | worst-served-origin share + Jain's index across origins under sustained contention (F6) |
| **floor violations** | count of origins whose grant rate fell below their equal share while another exceeded its own (F6) |
| **dependency-weighted shed** | shed rate conditioned on how much worse the origin's local option was — the concentration-loop detector (F6) |
| **isolation** | Δp95 on an interactive actor when a background actor starts |
| **herding** | CoV of dispatch counts per gossip window; route-flip rate on identical consecutive requests |
| **waste** | offloads where round-trip exceeded local service; 503-then-retry; quarantines of healthy nodes |
| **hard invariants** | assertions, not scores: `LocalOnly` never crossed the wire; `Fast` never offloaded; no request served by a node lacking the claimed capability |

### Tier 2 — `household-bench` (real hardware, plateaus)

The 3-actor scenario [`MESH_INFERENCE.md`](MESH_INFERENCE.md) §0
specified as Increment 0 — Alice enriching, Bob on complex knowledge
queries, Carol casual — reporting per-actor TTFT p50/p95, inter-token
rate, stall count and hub-vs-local turn share. Run at plateaus, not per
change.

### Tier 3 — soak (nightly)

Extend `mesh-soak-nightly` with load. It currently soaks membership and
gossip convergence, not scheduling.

### The calibration contract

The part that usually gets skipped. Tier 1 does **not** need to predict
latency accurately; it needs to predict **decisions**.

- the service-time model is fit from data the fleet already collects
  (`BenchmarkResult`, `ThroughputObservedStream` EWMAs) — not
  hand-tuned
- a small fixed set of Tier-2 scenarios is replayed through Tier-1
- the gate is **decision-agreement rate** (did the sim route the same
  way the hardware did?) plus **ordinal agreement** on per-actor p95 —
  never absolute error
- below threshold, Tier-1 numbers stop being admissible evidence until
  re-calibrated

Without this, Tier 1 becomes another proxy nobody trusts. With it, the
sim is allowed to be wrong about seconds as long as it is right about
choices.

### Cadence

Tier 1 on every change to scoring or selection (seconds — belongs in
the workspace gate). Tier 2 at plateaus. Tier 3 nightly.

## 6. Build order

**The rule that sets the ordering.** Get to the sim early, but split
the work by whether it changes behaviour:

- **Non-behavioural work goes BEFORE the sim.** Instrumentation, input
  provenance, state export. These raise the signal of the sim's first
  run — they decide whether its numbers are evidence or a model
  artifact — and they cost nothing in baseline fidelity because they
  change no decision.
- **Behavioural work goes INTO the sim as arms, not into production
  first.** The sim *is* the baseline machine: arm 0 is as-implemented,
  each fix is an additional arm. Landing a behavioural fix in
  production before the sim exists destroys the only baseline we will
  ever get, and F1/F3/F5 become unfalsifiable claims about a system
  that no longer exists in that form.

That resolves the apparent tension between "fix what's obviously
broken" and "measure before you optimize." Nothing needs to stay
broken; production simply doesn't move until the sim can say by how
much.

### Phase 0 — instrumentation (before the sim; changes no decision)

**Status: landed 2026-07-26.** All four steps are implemented in
`sovereign-mesh`, behind no feature flag, changing no routing
decision. Where the implementation deviates from the plan below, the
deviation is noted inline.

| where | what |
|---|---|
| `src/decision_log.rs` | The record vocabulary — `RoutingDecision`, `CandidateRecord` + `CandidateInputs` (P2 provenance), `RoutingOutcome` + `FailoverAttempt`, `FleetSnapshot` (P3) — plus the `DecisionSink` seam and its production / capture / null implementations. |
| `src/decision_trace.rs` | The P4 replay loader: `SchedulerTrace::from_jsonl` groups records into `Episode`s **by `decision_id`, not adjacency**, and reports a `join_rate` a caller can gate on. |
| `src/peer_inference.rs` | Emission. `select_peers_ranked` builds one record per decision including gated exits; the cascade in `complete_stream_with_id{,_and_finish}` and `complete` closes the join. `observation_snapshot()` is the P3 export. |
| `src/throughput_tracking.rs` | The outcome half rides on `ThroughputObservedStream`, the one place that already measures TTFT / wall time / token count for every terminus and every exit path. |
| `tests/scheduler_decision_records.rs` | Ten e2e tests against a mock peer: the join closes on every path, provenance is real rather than defaulted, exclusions and gates are recorded, failover is visible, a capture round-trips, and the sink changes no route. |

**How to capture a trace.** One environment variable on the daemon:

```
SOVEREIGN_DECISION_LOG=~/.sovereign/traces/evening.jsonl
```

Records also always reach `tracing` under the `mesh.decision` target
(registered in `DAEMON_TRACING_FILTER` — a custom target is dark
without that). `SchedulerTrace::from_jsonl_path` reads the file back.

**Two deviations from the plan, both deliberate:**

- **P3 is emitted into the record stream, not exposed as a route.**
  A snapshot fetched separately — after the fact, from a different
  surface — describes a different mesh than the one the episodes ran
  on. Folding a rate-limited (60s) `FleetSnapshot` into the same JSONL
  makes a capture *self-contained*: one env var yields a file holding
  both the episodes and the fleets they ran against, and
  `SchedulerTrace::snapshot_for` hands each episode the fleet as of
  its own timestamp. `observation_snapshot()` remains public for a
  one-shot read.
- **The named-model path records a verdict but no scored candidates.**
  `locate_named_model` is name resolution plus a min-in-flight
  tiebreak, not the OICP scorer; there is no `ScoreBreakdown` to
  report and a synthetic one would pollute the §5 scoreboard with
  decisions the scorer never made. Every outcome still has a decision
  to join to on both surfaces.

**One incidental fix, surfaced by the records themselves.** The
per-peer manifest cache was keyed on `NodeId`'s `Display`, which is
the *truncated* human form (`node-` + the first 8 of 16 bytes, as its
own doc comment says). Two peers sharing a first-8-byte prefix would
have shared a cache entry and been served each other's manifests. Now
keyed on `to_hex()`. Astronomically unlikely for random ids — it
showed up because two test peers rendered identically — but a
human-facing rendering should not be a cache key.


| # | Step | Why it raises the sim's first-run signal |
|---|---|---|
| P1 | **Decision records.** One structured event per routing decision: `oicp_request_id` (already the joinable tag, `peer_inference.rs:885`), the full candidate set with each `ScoreBreakdown`, the winner, and a second event on completion carrying served-by / TTFT / total / tokens / shed-or-not. | Calibration is *defined* as decision-agreement between sim and hardware. Without a joinable decision→outcome record there is no calibration contract, only a promise of one. This is the single highest-signal item. |
| P2 | **Input provenance + staleness stamping.** Attach the gossip round / age to `current_in_flight` and `inference_availability` where the scorer consumes them; carry manifest age alongside. | F1's dead time is currently a *hypothesis* (modelled 10–30s). P2 measures the real staleness distribution in production. It is also the sim's most load-bearing parameter — guessing it wrong invalidates every latency number. |
| P3 | **Observation-state export.** Dump per-peer `NodeObservations`, `BenchmarkResult` and `PeerHealth` (CLI verb or internal route). | Lets the sim's service-time model and fleet composition be **fit from the real mesh** rather than hand-tuned. This is the difference between "p95 improves 2.5×" being evidence and being an artifact of my chosen constants. |
| P4 | **Trace-replay fixture format.** Serialise a captured P1/P2 episode into a sim input. | Turns a real household evening into a repeatable Tier-1 scenario, and is the replay substrate the calibration contract needs. |

Phase 0 is also the answer to the standing "glassbox" obligation: after
P1+P2, "why did this go to the hub, and was that right in hindsight" is
answerable from logs in production, not just in simulation.

**What Phase 0 does not yet give you.** It records; it does not
measure. Everything in §5's scoreboard — efficiency ratio, tail
fairness, herding CoV, the F6 floor metrics — needs Phase 1 to
compute, because they are properties of a *population* of decisions
against an oracle, not of any single record. What Phase 0 does
establish is that the population is now capturable, and that §3's
numbers can be re-derived against the real scorer instead of a
transcription of it.

### Phase 1 — the sim

| # | Step | Exit criterion |
|---|---|---|
| S0 | Tier-1 `mesh-sim` + scoreboard + oracle, arm 0 = as-implemented | F1/F3/F5 reproduce against the **real** scorer, or are retired as artifacts of my transcription |
| S1 | Calibrate against replayed P4 fixtures | decision-agreement + ordinal p95 agreement above threshold; date recorded |

### Phase 2 — behavioural changes, each as an arm then a landing

| # | Step | Exit criterion |
|---|---|---|
| 1 | §4.1 predicted-time ranking | efficiency ratio improves; `ci-bench` core flat; glassbox trace states a predicted time per candidate |
| 2 | Fresh backpressure | the median gap between the `as-is` and `fresh` arms closes |
| 3 | Two-choices sampling | herding CoV and p95 improve; the completed-count trade-off is reported, not hidden |
| 4 | Congestion ≠ failure + honour `Retry-After` | quarantines-of-healthy-nodes goes to zero under a shed scenario |
| 5 | F6 floors: deficit-ordered queue, contribution demoted to surplus | worst-served-origin share and dependency-weighted shed hold under sustained contention |
| 6 | Enable a finite ceiling; priority classes | hub degradation is shedding-with-recovery, not unbounded queueing — and step 5's floors hold while it sheds |

F3's clamp is the one plausible exception to "arm before landing": a
term that evaluates to a constant carries zero information, so no
metric can degrade by fixing it. Even so it lands as arm 1 rather than
ahead of S0 — the cost of waiting is one sim run, and the benefit is
that the baseline stays honest.

Phase 0 before Phase 1 before Phase 2 is the same "baseline before
optimization" discipline `MESH_INFERENCE.md` specified and that was
skipped. If the failures do not reproduce, the cheapest possible
outcome is retiring the concern.

## 7. Acceptance — the mid-level-engineer bar

1. A routing decision states a **predicted time** and the outcome is
   comparable against it, from logs alone.
2. "Why did this go to the hub" is answerable in under a minute without
   re-deriving multipliers.
3. Every scheduler change lands with a Tier-1 before→after scoreboard,
   and every Tier-1 claim carries a calibration date.
4. No signal in the score means something other than its name
   (`inference_availability` currently does).
5. Congestion and failure are distinguishable in the code, in the
   traces, and in the metrics.
6. **No node is crowded out by better-resourced nodes.** Under
   sustained contention the worst-served origin holds its equal share,
   and the dependency-weighted shed rate does not rise with how much
   the origin needs the mesh (F6). Contribution influences surplus
   only, and never gates access.

---

## Appendix A — probe transcription

Constants read 2026-07-26, `oicp-types/src/scoring.rs` unless noted:

| constant | value | line |
|---|---|---|
| `LOAD_COEFFICIENT` → `1/(1+kn)` | 0.05 | 134 / 268 |
| `LOCALITY_{LOCAL,NEAR,FAR}_BONUS` | 1.15 / 1.05 / 1.00 | 137 / 277 |
| `COLD_START_SAMPLES` / `_MIN_WEIGHT` | 20 / 0.7 | 117 / 291 |
| `CONFIDENCE_SAMPLES` | 50 | 112 |
| `THROUGHPUT_REFERENCE_TG_TOK_S` / `_FLOOR` | 20.0 / 0.3 | 150 / 356 |
| availability clamp | `[0.2, 1.0]` | 547 |
| composed product | `claim × obs × load × loc × cold × thr × avail` | 548 |
| failure EMA up / down | `0.9r + 0.1` / `0.9r` | `peer_inference.rs:654` / `:633` |
| gossiped in-flight overrides self-observed | — | `peer_inference.rs:1085` |
| `FAILURE_THRESHOLD` / cooldowns | 3 / 60s +60s → 600s | `peer_health.rs:47-56` |
| `DEFAULT_GOSSIP_INTERVAL` | 10s | `gossip.rs:57` |
| `MANIFEST_TTL` | 60s | `peer_inference.rs:63` |
| activity → availability | hot .20 / warm .65 / cool .85 / idle 1.00 | `mesh_admin.rs:38` |

Modelled fleet: hub 25 tok/s decode, 420 tok/s prefill, claim affinity
0.95; desktops 30 / 260 / 0.80; laptops 45 / 120 / 0.60. Request:
1500 context, 250 output. Arrival: one knowledge query per node per
~90s. Gossip modelled as perfect broadcast every 10s — **generous**;
real anti-entropy is slower, so the herding measured is a lower bound.

The prototype scripts that produced §3 are session-scratch artifacts,
not committed. They should be superseded by Tier 1 rather than
preserved — the point of Tier 1 is that it links the real scorer
instead of transcribing it, which removes the transcription-drift risk
this appendix exists to bound.
