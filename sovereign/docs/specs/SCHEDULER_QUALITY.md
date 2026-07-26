# Scheduler quality — a measurement loop for OICP delegation

Status: **Phase 1 S0 landed (2026-07-26) — the Tier-1 simulator runs
the real scorer, and §3's transcribed numbers are superseded by §3.1.
S1's *instrument* landed the same day and is self-tested at 1.000
agreement against a simulated capture; the hardware capture it points
at has not been taken. No Phase-2 behavioural change has landed in
production.** Phase 0 (P1–P4) instrumentation landed earlier the same
day. Companion to
[`MESH_INFERENCE.md`](MESH_INFERENCE.md) (which specified
`household-bench` as Increment 0 and was never built) and
[`OICP_RATIONALIZATION.md`](OICP_RATIONALIZATION.md) (which unified the
scorer but deliberately preserved its shape).

Method: code audit of every decision point from request to dispatch,
plus two simulation probes that drove a faithful *transcription* of
the real scoring arithmetic (§3) — and, since S0, a Tier-1 simulator
that drives the **real** decision function itself (§3.1). Every
finding below carries a `file:line`. Every number is labelled either
*measured from code* or *modelled*. Where §3 and §3.1 disagree, §3.1
wins: it ran the code.

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

> **S0 update (§3.1):** reproduces against the real scorer, but it
> costs the **tail**, not the median — the opposite of what §3's
> transcription reported. And the recorded gossip age understates the
> true age, because `gossip_last_seen_unix` is receipt time.

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

> **Caveat on "real load is visible", raised 2026-07-26 and not yet
> resolved.** That sentence assumes the gossiped counter is a *total*.
> `MESH_LOAD_AWARENESS.md` and `AppState::current_local_in_flight`
> state that intent, but every bump site for the counter
> (`peer_inference.rs::enter_local_total`, six call sites) sits in the
> joiner-side provider — the **outbound** path. Whether a request
> arriving *from* a peer also passes through it is the open question,
> and it is not answerable by reading: it needs two daemons with
> `SOVEREIGN_DECISION_LOG` set, driving A→B and reading B's
> `FleetSnapshot.local.in_flight_published`.
>
> **Priced first (`Arm::OutboundOnlyLoad`, 2026-07-26): if the bug is
> real it is expensive, so the audit is earned.** Publishing only
> locally-originated work under-reports the true signal by 67–93% and
> costs:
>
> | scenario | total (intended) | outbound-only | Δ mean | top-server share |
> |---|---|---|---|---|
> | household-evening-12 | 25.7s | 58.0s | **+126%** | 0.41 → 0.55 |
> | twin-hubs | 33.3s | 40.0s | +20% | 0.41 → 0.50 |
> | isolation | 81.4s | 556.9s | **+584%** | 0.55 → 0.62 |
>
> The loop is the one F2 already names, closed the other way round: a
> node saturated by peer work advertises near-zero load, reads as
> idle, and wins more of it. That is the bounded-cost case for
> spending two daemons on the audit — an experiment that was ranked
> "worth confirming" is now ranked "confirm before Phase 2 touches
> the load term at all".

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

> **S0 update (§3.1):** the mechanism reproduces, but the *remedy*
> mostly does not apply. In a fleet with a unique capability winner
> the eligible set is a singleton — 49 of 49 offloads — so sampling
> has nothing to sample and two-choices is a literal no-op. It bites
> only when several peers tie, and even then it is dominated by
> fixing staleness.

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

### F7 — The cold-start ramp is self-locking (HIGH, found by Tier 1)

`cold_start_weight` (`scoring.rs:297`) starts a peer at
`COLD_START_MIN_WEIGHT` and ramps to 1.0 over `COLD_START_SAMPLES = 20`
observations. Its doc comment states the intent as a normative claim:

> "so new peers still receive routable traffic (otherwise they'd never
> accumulate history)"

**That claim is false, and the mechanism is the reason.** Samples
accumulate only on dispatch (`record_dispatch`), and the 0.7 floor is
multiplied against a locality bonus that already favours local
(1.15 vs 1.05 for a LAN peer). A cold peer therefore needs roughly a
1.6× claim-score advantage merely to break even with the local model —
and if it does not get it, it is never dispatched to, so it never
accumulates the samples that would lift the penalty.

Measured in Tier 1 (§3.1): on a 12-node household fleet, **one node
ever received a peer request**, and across two 30-minute fleets **no
(decider, peer) pair ever completed the ramp**. At household request
volumes the ramp is not a ramp; it is a permanent flat penalty applied
to every peer and to no local slot.

This compounds F6's concentration loop from the other direction: F6 is
about who gets *served* under contention, F7 is about who ever gets
*tried*. Both push work toward the incumbent.

> **Priced 2026-07-26 (`Arm::WarmStart`) — and the obvious remedy is
> wrong.** The finding above is confirmed; the fix it implies is not.
> Warm-starting every decider's peer observations at
> `COLD_START_SAMPLES` — the counterfactual in which the ramp has
> already finished — makes the mesh **much worse**, not better:
>
> | scenario | arm 0 mean | warm-start mean | Δ | Δ offloads |
> |---|---|---|---|---|
> | household-evening-12 | 25.7s | 86.1s | **+235%** | 49 → 77 |
> | heterogeneous-fleet | 40.2s | 114.5s | **+185%** | 33 → 48 |
> | twin-hubs | 33.3s | 32.3s | −3% | 74 → 76 |
>
> **The mechanism is *not* F1 — that was checked, not assumed, and
> the check refuted it.** The obvious reading is that lifting the
> floor unlocks offloads the decider cannot aim, because it cannot see
> a peer's queue for 10–30s. `Arm::FreshWarmStart` tests exactly that
> by warm-starting *and* telling the scorer the truth. If staleness
> were the cause the penalty would shrink. It does not — it grows:
>
> | scenario | warm-start cost, stale signal | warm-start cost, **fresh** signal |
> |---|---|---|
> | household-evening-12 | +235% | **+264%** |
> | heterogeneous-fleet | +185% | **+246%** |
> | twin-hubs | −3% | +7% |
>
> So the extra offloads lose **on their own merits**, with or without
> F1. Which makes F7 a symptom of something larger than F7: the
> scoring function is systematically **over-eager to offload**, and
> `cold_start_weight`'s 0.7 floor is the only thing compensating —
> accidentally, via a term that was written to mean something else
> entirely.
>
> That is a direct argument for §4.1. A product of dimensionless
> multipliers cannot represent "this hop costs more than it buys",
> so it cannot decline a bad offload on the merits; ranking on
> **predicted time-to-answer** can, and would make the floor
> unnecessary rather than load-bearing.
>
> Consequences for Phase 2. First, **do not remove the cold-start
> floor until the objective is fixed** — it is the only brake, and the
> arms above price its removal at +235% even under perfect
> information. Second, the doc comment is still false and should still
> be corrected (arm 0 dispatches to exactly **1 of 12** peers over a
> household evening) — but the correction is to the *documentation*,
> not to the constant.
>
> Recorded as a method note because it nearly went the other way: the
> first write-up of this block asserted "the ramp masks F1" from the
> latency table alone. The table was equally consistent with two
> mechanisms and one extra arm separated them.
>
> Isolation caveat, since `samples` feeds three terms: the arm also
> flips some peers from `benchmark_estimate` to `observed`
> throughput. Measured, not assumed — 1137 of 1232 peer candidates
> (≈92%) still score from the benchmark estimate under warm-start, so
> the source flip is a minority effect and the latency delta is
> dominated by `cold_start_weight` itself.

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

### 3.1 What Tier 1 found when it ran the real scorer (2026-07-26)

Phase 1 S0 landed and re-ran the same questions through
`scheduler_core::rank` — the production decision function — instead of
a transcription of it (`sovereign-mesh/src/mesh_sim/`, feature
`mesh-sim`; `tests/mesh_sim_scoreboard.rs`). Seed 20260726. **The
table above is superseded on magnitude and on remedy; two of its three
readings do not survive.**

`household-evening-12` (1 hub, 3 desktops, 8 laptops, 30 min, 128
decisions):

| arm | p50 s | p95 s | eff vs oracle | top-server share | waste | tail spread s |
|---|---|---|---|---|---|---|
| as-implemented | 17.7 | 71.3 | 0.42 | 0.38 | 0% | 84.4 |
| fresh signals | 17.3 | 63.1 | 0.45 | 0.38 | 0% | 70.2 |
| two-choices | 17.7 | 71.3 | 0.42 | 0.38 | 0% | 84.4 |
| fresh + two-choices | 17.3 | 63.1 | 0.45 | 0.38 | 0% | 70.2 |
| oracle | 11.1 | 17.3 | — | 0.23 | 0% | 4.6 |

`twin-hubs` (3 *identical* hubs, 8 laptops — the only fleet where a
sampling remedy has anything to sample):

| arm | p50 s | p95 s | eff vs oracle | tail spread s |
|---|---|---|---|---|
| as-implemented | 29.6 | 85.5 | 0.33 | 84.0 |
| fresh signals | 25.4 | **41.6** | 0.44 | 16.9 |
| two-choices | 26.6 | 60.3 | 0.40 | 47.6 |
| fresh + two-choices | 26.5 | 42.4 | 0.43 | 17.6 |
| oracle | 11.3 | 17.4 | — | 8.6 |

**F1 — reproduces, but it costs the TAIL, not the median.** §3 read it
the other way round. Removing staleness moves p50 by 2% on
`household-evening-12` and 14% on `twin-hubs`, while p95 improves 11%
and **51%** respectively. The median barely moves because a stale
signal mostly changes *whether* you offload; the tail moves because it
changes *which* peer you pick, and only a fleet with more than one
eligible peer can express that.

**F3 — reproduces exactly.** `throughput_factor` is 1.000 for every
node in a 25→120 tok/s fleet, as predicted. The term intended to
handle heterogeneity is constant across heterogeneity.

**F5 — the mechanism reproduces; the proposed remedy is inert in the
common case.** On `household-evening-12` the eligible set is a
**singleton in 49 of 49 offloads** — exactly one peer strictly beats
local — so two-choices sampling is a literal no-op and the table shows
it byte-for-byte identical to arm 0. Sampling only bites when the
capability winner is not unique (`twin-hubs`: p95 85.5 → 60.3), and
even there it is dominated by fixing staleness and adds nothing on top
of it. §3's claim that two-choices cuts p95 2.5× and completes fewer
requests does **not** survive: it completes exactly as many, and its
effect is smaller and conditional on fleet composition.

**Two findings the transcription could not have produced**, both
consequences of running the real scorer:

- **F7 — the cold-start ramp is self-locking.** `cold_start_weight`'s
  own doc comment says the ramp exists "so new peers still receive
  routable traffic (otherwise they'd never accumulate history)".
  Measured: on `household-evening-12`, **1 of 12 nodes ever received a
  single peer request**, and across both fleets **no (decider, peer)
  pair ever reached the 20 samples that complete the ramp**. Samples
  accumulate only on dispatch, and the 0.7 cold-start floor multiplied
  against local's 1.15 locality bonus means a cold peer needs roughly
  a 1.6× claim advantage just to break even. Never chosen → never
  sampled → never un-penalised. At household traffic volumes the
  "ramp" is a permanent flat penalty on every peer, and the code's own
  normative claim about it is false.
- **The recorded gossip age understates the real one.**
  `gossip_last_seen_unix` is receipt time, not measurement time, so
  the P2 provenance on every candidate record is optimistic by the
  propagation delay — median 12.4s true vs 8.5s recorded here, 21.0s
  vs 8.9s on the pair scenario. Every F1 number derived from records
  alone is a lower bound.

**Waste needs two numbers, not one.** §5's "offloads where round-trip
exceeded local service" is 100% on `household-evening-12` — and that
is not a defect. A laptop sending a knowledge turn to a bigger, slower
model buys a better answer with latency, deliberately. The scoreboard
therefore reports `slower` (any offload slower than local) separately
from `waste` (offloads that were slower *because the peer was backed
up* — it would have won had it been idle). On the isolation scenario
waste is 80%, which is real scheduler error; on the household scenario
it is 0%.

**Unchanged from §3:** the F4 arm remains untestable here because no
admission gate is enabled, exactly as its caveat says.

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

> **PRICED as a Tier-1 arm 2026-07-26 (`Arm::PredictedTime`,
> `sovereign-mesh/src/predicted_time.rs`). The objective works, and the
> arm also found the thing that would break if it shipped as-is.**
>
> The arm replaces only the ranking: same hard gates, same candidate
> records, same scores recorded, so a delta against arm 0 is a delta of
> *objective* and of nothing else. It introduces **no constant** — no
> floor, no weight, no tie-margin — because `rtt` sits inside the
> number and a hop that buys less than it costs loses arithmetically.
>
> **1. The decomposition, which is what the arm was built for.** Arm 0
> and `Oracle` bracketed the problem; this is the missing middle term —
> the oracle's objective computed from what a decider can actually see.
>
> | scenario | arm 0 | predicted | oracle | wrong objective costs | imperfect info costs |
> |---|---|---|---|---|---|
> | household-evening-12 | 25.7s | 11.4s | 10.8s | **+126%** | +4.7% |
> | twin-hubs | 33.3s | 11.1s | 10.9s | **+200%** | +1.8% |
> | heterogeneous-fleet | 40.2s | 11.5s | 11.5s | **+250%** | −0.0% |
> | isolation | 81.4s | 16.9s | 11.7s | **+382%** | +43.8% |
>
> The objective error dominates the information error by 5–100× on
> three of four fleets. **That reorders Phase 2**: F1 (fresh
> backpressure) was queued as step 2 on the strength of §3's "staleness
> costs the median", but against a *correct* objective staleness is
> worth 1.8–4.7% on the fleets where offloading is rare. `isolation` is
> the exception at +43.8%, and it is the one fleet with sustained
> contention — which is the honest scope for F1's remaining value.
>
> Corroborates F7 independently: arm 0 offloads 49/74/33 times where
> predicted-time offloads 38/11/7. The scorer *was* over-eager to
> offload, measured now by a second mechanism.
>
> **2. What the harness cannot see, stated before anyone quotes the
> table.** A node's advertised `BenchmarkResult` is built from the same
> `Hardware` the service-time model consumes, so the rate card is exact
> truth by construction and the predictor has **zero model error** — its
> only error is the queue substitution (gossip carries an in-flight
> *count*, so each job ahead is assumed to look like the job in hand).
> `SimConfig::advertised_rate_error` exists to price that flattery, and
> the win survives it:
>
> | rate-card error | arm 0 eff | predicted eff |
> |---|---|---|
> | ±0% (exact, flattering) | 0.29–0.42 | 0.95–1.00 |
> | ±10% | 0.29–0.42 | 0.86–0.90 |
> | ±100% | 0.36–0.91 | 0.85–0.90 |
>
> Predicted-time loses ~10 points to a mis-rated fleet and then goes
> flat; it does not unravel. Read arm 0's *rise* in that right-hand
> column as a third sighting of the same pathology, not as arm 0
> improving: at ±50% some advertised rates fall under
> `throughput_factor`'s 20 tok/s reference, the clamp finally
> discriminates, peers score lower, and fewer bad offloads happen. A
> broken rate card becomes an accidental brake — exactly what
> `cold_start_weight`'s floor was.
>
> **3. THE BLOCKER, and it is not latency.** Ranking on time alone
> prefers whichever node answers soonest, which on every fleet here is a
> *small fast model*. Arm 0 sent all 49 household offloads to the 35B
> hub; predicted-time sent 37 of 38 to 4B laptops. In `twin-hubs` it
> never chose a hub at all.
>
> That is the objective doing precisely what it was asked, and it is why
> the paragraph above this block is load-bearing rather than a caveat:
> **§4.1 cannot land without the tier floor as a separate explicit
> input.** Worse, no metric on this scoreboard can see the cost — §5's
> metrics are latency, fairness and waste, and answer quality is not
> among them. So the landing gate for Phase 2 step 1 is not an
> efficiency-ratio number; it is a quality gate the sim cannot supply.
>
> **4. A missing record field, found from replay's side.** A
> `RoutingDecision` carries inputs, scores and a verdict, but not *which
> objective* mapped scores to verdict, so `decision_replay` re-runs the
> product policy over any capture: a predicted-time capture reports
> scorer agreement **1.000** and policy agreement **0.009**. The fix is
> an objective tag, which is smaller than the `predicted_ms` column one
> would reach for first — `CandidateInputs` already carries `in_flight`,
> `rtt_ms` and both `bench_*` rates, so **the objective is already
> scoreable against a production capture with no new instrumentation.**

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

**S0: landed 2026-07-26.** Results and what they overturned are in
§3.1. Structure:

| where | what |
|---|---|
| `src/scheduler_core.rs` | The decision, extracted from `select_peers_ranked` as a pure total function over a belief snapshot. Production gathers then calls it; the sim builds the snapshot and calls the same function. Decision-preserving: the ten Phase-0 e2e record tests pass unchanged. |
| `src/mesh_sim/` (feature `mesh-sim`) | Virtual clock, event queue, gossip propagation, manifest-cache ageing, queueing, the five arms, the perfect-information oracle. Beside `dst.rs`, same rationale, no extra dependencies. |
| `src/mesh_sim/scoreboard.rs` | Split in two on purpose: `RecordMetrics` is computable from a **production capture** as well as a sim run — that is the precondition for S1 — while `TruthMetrics` needs simulator ground truth and is therefore never allowed to define a calibration gate. |
| `tests/mesh_sim_scoreboard.rs` | The scoreboard run. §5's hard invariants are assertions; everything else is printed, because a metric with an unagreed threshold is a flaky test. |

Arm 0 is not a model of the scheduler: `rank` is the function the
daemon calls. What the sim models is the environment — service time,
gossip delay, cache ageing, queueing — and those are exactly what S1's
calibration contract exists to check.

**Three measurement bugs were found and fixed during S0**, all of
which had made the first run's numbers look better than they were.
Recording them because each is a trap the next scoreboard metric can
fall into: (1) herding CoV computed over *chosen* targets makes
maximal herding score as zero, since the CoV of a one-element vector
is zero — the denominator must be the *eligible* set; (2) pooling all
local service into one `<local>` bucket makes a policy that spreads
work across twelve nodes read as more concentrated than one that
funnels everything to a hub; (3) `{:>6.2}` applied to a `&str` in
Rust's formatter is a *truncation*, which silently rendered `3.11` as
`3.`.

**S1: the instrument landed 2026-07-26; the capture has not been
taken.** The step is split because its two halves have very different
costs, and only one of them needs hardware.

The decomposition that made the first half cheap: a decision record
carries no manifests, so it cannot re-run `rank` — and it does not
need to. Agreement factors into two independent questions, both
answerable from a production capture as the schema already stands:

| half | question | inputs |
|---|---|---|
| **scorer agreement** | does the record carry every input the *score* depended on? | recorded `CandidateInputs` + `claim_score` + `locality` + `size_gb`, pushed back through the real `score_with_adjustments` |
| **policy agreement** | does the record carry every input the *ranking* depended on? | recorded `final_score`s, pushed back through the real strictly-beats-local filter and best-first sort |

The two deliberately run off different inputs — the policy half reads
recorded scores, never recomputed ones. Chaining them would let one
scorer bug cascade into a policy failure and you would learn strictly
less from the same run.

| where | what |
|---|---|
| `src/decision_replay.rs` | The replay itself. Calls production's scorer and production's ranking policy; reports `scorer_agreement` / `policy_agreement`, per-factor disagreements, and named `ReplayGap`s. Both ratios return `0.0` on an empty denominator, never a vacuous `1.0`. |
| `src/scheduler_core.rs` | The ranking half of the decision extracted as `winners_over_local` + `beats_local` + `local_sentinel`, so replay re-runs the policy rather than a copy of it — the same discipline that makes arm 0 *be* the scheduler. |
| `tests/scheduler_replay_agreement.rs` | The fixture with a known answer: sim → `TracingDecisionSink::to_path` → JSONL → `SchedulerTrace::from_jsonl_path` → replay. Every stage but the record *content* is the code a real capture goes through. |

**Result: 1.000 / 1.000, bit-exact, across all five scenarios × four
decision-making arms** (4,573 candidates, 413 decisions). On a
simulated capture that is the only admissible answer — the sim wrote
those records by running the same code moments earlier — so the value
of the run is that it makes a *wrong* answer unambiguous. The suite
proves the instrument can fail: corrupting one recorded
`inputs.samples` drops agreement to 0.958 and names `cold_start_weight`
as the factor that no longer follows from the record.

**What S1 settled without spending daemon time.** The open question
going in was whether `claim_affinity` — an argument the scorer takes
and the record does not carry — forced another Phase-0 schema field.
It does not, and the reason is structural rather than lucky:

```text
observation_mult = effective_affinity(a, obs) / a
                 = (clamp(a) · (1 − w·f)) / a     [samples > 0]
                 = (1 − w·f)                       for a ∈ (0, 1]
```

and `a ∈ [0, 1]` **by construction** — `ScoredClaim::claim_affinity` is
always `CapabilityClaim::effective_affinity()`, which clamps (NaN → 0).
The multiplier is therefore independent of `a` across the whole legal
domain except `a == 0`, which is exactly the `claim_score == 0` case
(both are that same clamped number upstream). The replay probes with
`1.0` or `0.0` accordingly and reproduces the breakdown exactly.
Finding this *before* the capture is the whole reason S1's replay was
built ahead of S1's data.

**What still needs hardware.** The exit criterion above has two
clauses and only the first is now instrumented. Decision-agreement is
ready to run against a real capture (`SOVEREIGN_DECISION_LOG=<path>`
on two daemons, drive traffic, replay). Ordinal p95 agreement needs
the Tier-2 side to exist, and is unblocked but unstarted.

**Arms landed alongside S1 (2026-07-26).** The first three are
diagnostics rather than candidate policies: they price a question before
it costs hardware time, which is the cheapest thing the simulator does.
The fourth is the §4.1 candidate itself.

| arm | question | answer |
|---|---|---|
| `WarmStart` | does F7's self-locking ramp *cost* anything? | Yes, and with the opposite sign to the one the finding implies: removing the penalty is **+235% mean latency**. See F7. |
| `FreshWarmStart` | is that damage F1's fault? | **No** — the penalty is *larger* under fresh signals (+264%). The offloads lose on their own merits, so the 0.7 floor is compensating for an over-eager objective, not for staleness. This is the arm that stopped a wrong causal claim reaching this doc. |
| `OutboundOnlyLoad` | would it matter if the gossiped in-flight counter missed inbound peer work? | Yes — **+126% to +584%** mean latency, under-reporting the signal by 67–93%. The two-daemon audit is earned. See F2. |
| `PredictedTime` | how much of the oracle gap is a **wrong objective** rather than **imperfect information**? | Overwhelmingly the objective: +126%/+200%/+250% against +4.7%/+1.8%/−0.0%. Survives a ±2× mis-rated fleet. **But it routes knowledge turns to 4B laptops**, so it cannot land without a tier floor. Full result in §4.1. |

They all follow one pattern worth naming, because it generalises: **a
null result is only informative if the knob is proven connected.** Each
arm asserts its own wiring before printing an outcome — `WarmStart` that
arm 0 applies a cold-start penalty and warm-start removes it;
`OutboundOnlyLoad` that the published sum actually shrinks;
`PredictedTime` that its verdicts differ from arm 0's *and* that
predictions exist at all (a request with no token shape is
unpredictable for every candidate including local, which would collapse
the arm into stay-local-always and print as a strong result). Without
that, "nothing moved" and "nothing was flipped" render identically.

`PredictedTime` extends the pattern one step, and the extension is the
reusable part: **an arm must also price the harness assumption that
most flatters it.** Here the sim builds each node's advertised benchmark
from the same `Hardware` its service-time model consumes, so the
predictor was being graded against its own rate card.
`SimConfig::advertised_rate_error` is the knob that discloses it. The
generalisation: when an arm consumes a signal the harness *generates*,
the arm cannot measure that signal's error, and the report has to say
so before anyone quotes the number.

### Phase 2 — behavioural changes, each as an arm then a landing

| # | Step | Exit criterion |
|---|---|---|
| 1 | §4.1 predicted-time ranking — **arm done, landing blocked on the tier floor** | ~~efficiency ratio improves~~ (done: 0.29–0.42 → 0.85–0.90 on a mis-rated fleet, 0.95–1.00 on a perfect one). Remaining, and re-scoped by what the arm found: a **tier floor as a separate explicit input**, plus a quality gate — the arm routes knowledge turns to 4B laptops and no §5 metric can see that. Then `ci-bench` core flat; glassbox trace states a predicted time per candidate; an **objective tag on `RoutingDecision`** so replay can pick the right policy |
| 2 | Fresh backpressure — **demoted** | Against the product objective F1 looked like the median's main cost. Against a *correct* objective it is worth 1.8–4.7% on three fleets and +43.8% on `isolation` alone, so this is now scoped to sustained contention. Exit unchanged: the median gap between the `as-is` and `fresh` arms closes |
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
