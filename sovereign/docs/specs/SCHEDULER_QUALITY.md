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

> **Read this before trusting any number below (2026-07-27).** A Phase-0
> pass to put this programme in contact with reality found that the
> `mesh-peer` placeholder bug (F8's neighbourhood) was not an isolated
> defect but an instance of a pattern: **load-bearing signals that nothing
> writes, and gates that nothing can turn red.** Three findings change how
> the rest of this document should be read:
>
> - **F9** — the scorer's local-load input is never written
>   (`record_dispatch(None)` has zero callers), so on a homogeneous fleet
>   the ranked path is *structurally* incapable of preferring a peer. This
>   undercuts F1's premise and much of §4.1's motivation.
>   **Priced and half-fixed the same day (§4.4).** The local half landed in
>   production: −71% mean / −76% p95 under sustained contention, ±1%
>   everywhere else. The peer half turned out to be *protective* and was
>   deliberately left alone. The larger consequence is in §4.4's first
>   paragraph: **arm 0 was never the shipped system**, so every earlier
>   number in this document compares policies on the mesh as *designed*.
> - **F10** — `RankObjective::Product` and `TierFloor::None` are hardcoded
>   in production, so §4.1.1 and §4.1.2 measure policies that **cannot
>   currently fire outside the simulator**. The calibration contract's
>   input data (`BenchmarkResult`) is not collected either.
> - **F11** — what the dispatcher omits from the wire (no `X-Node-Id`, no
>   bearer) blinds serving-side attribution and admission control, drops
>   attribution entirely on the streaming path, and makes plaintext-LAN
>   peer dispatch return 403 — plausibly the reason cross-node inference
>   has never been observed working here.
>
> What landed in the same pass: a cross-node **serviceability** gate, in
> two places. In-process, a mock peer that *resolves* `model` rather than
> accepting any body (`sovereign-mesh/tests/chat_completion_e2e.rs`) —
> verified red against the pre-fix code. In the multi-process soak, a
> `--workload offload` lane that forces a real cross-node serve and
> attributes it (`scripts/mesh-soak.sh`). Also repaired: two soak SLIs that
> were structurally incapable of going non-zero (see §5).

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
| D1 / D2 / D3 split — an inbound peer request is served by `SovereignInferenceAdapter` picking a **local** slot, never re-forwarded | ⚠️ **Verified for the DESKTOP only** — see the correction below |
| Ranked failover — a 503 on the best peer tries the next peer, not straight to local (`peer_inference.rs:1522`) | ✅ Holds |
| RAII in-flight guards, saturating decrements, drop-order documented (`peer_inference.rs:1597`) | ✅ Holds |
| DST harness for gossip convergence, seeded faults, quiesce-then-assert (`sovereign-mesh/src/dst.rs`) | ✅ The pattern Tier 1 below should copy |

**Correction to the re-forwarding row (2026-07-27).** This doc previously
recorded that row as "✅ Verified: no request-ping-pong hazard at N=12".
That is true of the **desktop** wiring, which puts the raw engine inbound
(`state.rs:941-953`) and so structurally cannot re-forward. It is
**unestablished for the CLI daemon**, which puts the `MeshInferenceProvider`
inbound (`bootstrap.rs:1396-1401` → `daemon.rs:2311-2327`, reached at
Priority 0 by `routes_inference.rs:171-240`). No hop, TTL or origin field
exists anywhere in `InferenceRequirements` (`requirements.rs:16-38`), and
the single-hop guard was **retired on 2026-06-27**
(`inference_adapter.rs:352-359`) in favour of an envelope-driven gate that
is hop-blind. The desktop avoids the loop by wiring; the daemon avoids it,
as far as anyone has checked, by luck. Treat this as an open question, not
a verified property.

The fragility is not in the code. It is in the **control loop** — and, as
F9 records, in signals that nothing writes.

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
>
> **Remedy update (§4.2.1):** the finding stands; the proposed fix does
> not. Piggybacking load onto responses reaches only the peers a
> decider is already talking to, and F1's cost lives in the peers it is
> **not** — measured at 4–7% coverage and ±3% latency on three fleets.
> F1 is a property of the channel that reaches everybody, so its repair
> has to be one too.

### F2 — The shared "busy" signal is not a busy signal (CRITICAL)

`inference_availability` multiplies into the score
(`scoring.rs:553`, clamp `[0.2, 1.0]`). It is set from exactly one
place — `POST /internal/node/activity`
(`commonwealth-api/src/routes_internal/mesh_admin.rs:38`) — mapping
**human coding activity**: `hot=0.20 warm=0.65 cool=0.85 idle=1.00`.

Solving `load_penalty(n) = 1/(1 + 0.05n) = 0.20` gives **n = 80**. In
the scorer's own units, *one human at the keyboard equals eighty queued
inference requests*. A hub grinding through twenty peer requests with
nobody sitting at it advertises `1.00` and looks maximally idle.

In fairness: a **peer's** `current_in_flight` **is** gossiped and **does**
override the load term (`scheduler_core.rs:512-521`), so a peer's real load is
visible — it is F1-stale, not absent. It is the availability term that is
mis-specified, and it is weighted to dominate.

**Sharpened by F9 (2026-07-27), then closed the same day.** That fairness
applied only to *peers*. The **local** candidate had no equivalent: its load
input (`local_observations.in_flight`) was never written at all, so the
comparison was between a peer whose load is stale and a local side whose load
was permanently zero. The local half of F9 is now fixed — the scorer reads
`in_flight_publisher`, which is the **same quantity** a peer gossips — so the
comparison is finally like-for-like: stale-but-real against fresh-and-real.
That makes F1 (dead time) the remaining asymmetry rather than one asymmetry
among two. Read F9 and §4.4 before drawing conclusions from this finding.

> **Caveat on "real load is visible", raised 2026-07-26 and not yet
> resolved.** That sentence assumes the gossiped counter is a *total*.
> `MESH_LOAD_AWARENESS.md` and `AppState::current_local_in_flight`
> state that intent, but every bump site for the counter
> (`peer_inference.rs::enter_local_total`, four call sites — `:1735`,
> `:1822`, `:1864`, `:2309`; this doc previously said six) sits in the
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

**Worse than filed, measured 2026-07-27 (§4.5, F10).** The above
describes the term with a rate card. Production has none — and no
observed EWMA for peers either — so `throughput_factor` takes its
`(None, None)` branch and returns neutral 1.0 for **every peer on every
fleet**. It is not weakly discriminating, it is a constant, and
`blind-rate-card` costs nothing on five of six scenarios for exactly
that reason. The "only live discrimination is below 20 tok/s" sentence
is the load-bearing one and it survives: the one fleet where the rate
card is worth anything (`mixed-hubs`, −32%) is the only one in the suite
containing a sub-reference node.

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

### F8 — A soft named target bypassed the scheduler entirely (HIGH, FIXED 2026-07-27)

Every finding above is about how the scorer *ranks*. This one is about
traffic the scorer never saw.

`select_route` short-circuits on a named target **before** ranking
(`peer_inference.rs`, `DecisionPath::NamedModel`), and that is correct
for a **hard** target — an explicit `model_id` from the caller is a
constraint, and silent substitution was the original bug on that path.
But the same branch also carried the **soft** target: a node configured
with a shared-model primary (`shared_model_id`), which is a
*preference*. When that model resolved to nobody — the cluster is
forming, the host is down — the fallback chain was
`[named] -> LocalFallback` on the streaming path, and a bare
`return self.local.complete()` on the non-streaming one.

The household consequence is the sharpest in this document, because it
fails hardest exactly where the household invested most. A 4B laptop
configured to send its primary turn into a shared 122B, on a mesh that
also has a 35B hub: while the cluster forms, that laptop answers from
its **own 4B** and the hub sits idle. Nothing is bought. No latency is
saved (local is not faster than a free LAN peer at that size gap), no
privacy is honoured (the envelope already said `MeshAllowed` — it had
to, or `shared_primary_id` would not have fired), and the user gets a
markedly worse answer for no reason. It is not a quality/latency
trade-off the scorer got wrong; it is a decision the scorer was never
asked to make.

Scale note, and the reason this is HIGH rather than CRITICAL: the
persistent form is the `Unknown` case above. The transient form — a
named host that resolves and then fails every address mid-cascade —
self-heals, because `PeerHealthTracker` quarantines it after a
threshold and subsequent requests take the `Unknown` path. See §4.3 for
what shipped and what deliberately did not.

### F9 — The scorer reads a local-load counter that nothing writes (CRITICAL, structural — local half FIXED 2026-07-27)

Found 2026-07-27 while building the Phase-0 cross-node liveness gate. It
undercuts the premise of F1 and much of §4.1's motivation, so it is recorded
before the measurement sections rather than appended after them.

> **Status after measurement (2026-07-27, §4.4).** F9 has two halves and they
> point in **opposite directions**. The local half is a defect and is now fixed
> in production: wiring the real local in-flight count into the scorer is worth
> **−71% mean / −76% p95** under sustained contention and is a **no-op (±1%) on
> every other fleet measured**. The peer half — peer `samples` never leaving 0,
> pinning `cold_start_weight` at 0.7 — is *protective*: freezing it is worth up
> to −33% mean latency, and "completing the wiring" would be a regression on
> four of five fleets. **Do not fix the peer half.** See §4.4 for the table and
> the arms.

`load_penalty` (`scoring.rs:274-278`) reads `in_flight` from the local
candidate's observations — `MeshInferenceProvider::local_observations`
(`peer_inference.rs:407`). That field is bumped only by `record_dispatch(None)`
→ `observe_dispatch` (`peer_inference.rs:848-856`, `scheduler_core.rs:166-169`).

**`record_dispatch(None)` has zero callers in the repository.** So do
`record_success(None)` and `record_failure(None)` (`:861-889`). The only call
site is `record_dispatch(Some(&peer.name))` at `:2173`, the non-streaming
named-peer arm. `samples` is seeded once to `COLD_START_SAMPLES * 2 = 40` at
construction (`:557-562`) and never moves again.

There are three local in-flight counters, and the scorer reads the one nobody
writes:

| counter | written by | read by |
|---|---|---|
| `local_inflight_by_model` (`:442`) | `enter_local_inflight` (`:1863`) | **only** `locate_named_model` (`:1493-1499`) |
| `in_flight_publisher` (`:480`) | `enter_local_total` (`:1888`) | gossip only |
| `local_observations` (`:407`) | *nothing on the dispatch path* | **the scorer** |

Consequences, all measured from code. On the ranked path the local candidate is
scored **permanently idle and permanently healthy**: `load_penalty ≡ 1.0`,
`observation_mult ≡ 1.0`, `cold_start_weight ≡ 1.0`. The design comment at
`peer_inference.rs:1198-1203` — "so a hot local slot can lose to an idle peer on
load" — describes behaviour this code cannot exhibit. And `beats_local`
(`scheduler_core.rs:117-123`) awards every tie to local, because `pick_better`
returns the incumbent on a full tie (`scoring.rs:458-470`) and
`candidates_equal` suppresses zero-delta hops (`oicp_select.rs:41-45`).

The arithmetic on a homogeneous, both-idle fleet:

```
local = claim × 1.15 (Local locality) × 1.0 (cold, seeded 40) × T
peer  = claim × 1.05 (Near locality)  × 0.7 (cold, samples 0) × 1.0
        where T = clamp(local_tg_tok_s_ewma / 20, 0.3, 1.0)
```

A peer wins iff `1.15·T < 1.05 × 0.7 = 0.735`, i.e. `T < 0.639` — **the origin's
own observed throughput below ~12.8 tok/s**, and `throughput_factor` needs ≥5
completed local streams to move off neutral at all (`scoring.rs:231`). At
`T = 1.0` no peer can ever win. The peer's 0.7 cold-start penalty is likewise
effectively permanent on the streaming path, since nothing calls
`observe_dispatch` for peers there either.

**So on a homogeneous fleet the ranked path is not mis-tuned — it is
structurally incapable of preferring a peer.** F1's "dead time exceeds service
time" and every load-awareness argument in §4 rest on a signal that is never
written. Tier 1 does not surface this because it constructs observations
directly instead of letting the dispatch path produce them; that is a gap in the
sim's fidelity, and the calibration contract's "predict decisions, not seconds"
gate would not catch it either, because both sides agree — wrongly.

What this does **not** claim: named dispatch is unaffected. `locate_named_model`
reads `local_inflight_by_model` (`:1493-1499`), which **is** written
(`enter_local_inflight`, `:1863`) and keyed identically at both ends. That is
the one path where local load genuinely moves a decision, and it is what the
Phase-0 soak probe uses to force a real cross-node serve (§5 Tier 2).

**The local half, fixed 2026-07-27** after §4.4 priced it. The gather point
(`peer_inference.rs`, the `local_obs` binding) now overrides `in_flight` with
`in_flight_publisher` — the RAII-maintained total this node already gossips.
Two reasons that beats teaching the dispatch path to call
`record_dispatch(None)`: it cannot drift out of pairing with the guards that
already maintain the counter, and it makes both sides of the comparison the
**same quantity**. A peer's in-flight number is *its* published total
(`scheduler_core.rs:512` prefers the gossiped count over the self-observed one),
so scoring local on a private counter was comparing two numbers that only shared
a name. Pinned by
`the_local_candidate_is_scored_on_this_nodes_real_in_flight_count`
(`tests/scheduler_decision_records.rs`), verified red against the pre-fix code
with the recorded `in_flight` at 0 where 8 was true.

### F10 — The scorer has no speed signal, so §4.1's policies cannot fire (CRITICAL — PRICED 2026-07-27, §4.5)

Filed as "a hardcoded switch." That was the smaller half, and the ordering
between the two halves turned out to be the finding.

**Half A — the switch.** `RankObjective::Product` (`peer_inference.rs:1288`)
and `TierFloor::None` (`:1293`) are **hardcoded** at the production call site,
so the tier floor (`scheduler_core.rs:596-653`) and the predicted-time
objective (`:681-726`) are unreachable outside Tier 1. §4.1.1 and §4.1.2 are
labelled *measured*, and they were — in the simulator. Neither has shipped.
Read them as evidence about a policy that **could** be adopted, not as a
description of how this mesh routes today.

**Half B — the inputs, and it is upstream of half A.** The data those policies
consume is never collected. `run_baseline_benchmark`
(`sovereign-inference/src/benchmark.rs:59`) and
`MeshInferenceProvider::set_local_benchmark` (`peer_inference.rs:808`) have no
callers, and `NodeCapabilities.benchmark` is hardcoded `None` in the gossip
builder (`capabilities.rs:194`) — whose comment promises a `with_benchmark`
setter that does not exist, as does `benchmark.rs`'s module header. Every
candidate is scored with `benchmark: None`.

Three consequences, measured in §4.5:

1. **`throughput_factor` is a constant in production**, not a weak signal. Both
   its sources are shut — no rate card, and the observed EWMA is gated behind
   the `samples >= 5` the ranked path never reaches (F9's peer half) — so it
   returns neutral 1.0 for every peer on every fleet. F3 is worse than
   catalogued.
2. **Unhardcoding half A today would expose a policy that cannot execute.**
   `PredictInputs::from_candidate` reads the advertised benchmark and nothing
   else, so `predict` would return `Err(Unpredictable::NoThroughput)` for every
   candidate on every request. Half A is not the blocker; half B is.
3. **The obvious repair is the wrong one.** Calling `run_baseline_benchmark` at
   startup probes the `Speed::Fast` slot and leaves `throughput_factor`
   extrapolating from a ~2.5 GB model to a 21 GB one on a linear size law that
   is known to be false. §4.5 prices that: at realistic sub-linearity it buys
   −56% latency and doubles declined upgrades. Probe the model being *scored*,
   or do not probe.

This is also why §5's calibration contract cannot be honoured: it says the
service-time model is "fit from data the fleet already collects," and the fleet
collects none of it.

§4.3's soft-named fallthrough (F8) and F9's local half remain the only items in
this document that landed in production.

### F11 — What the dispatcher omits from the wire blinds four surfaces (HIGH)

`provider_for_peer` (`peer_inference.rs:2046-2065`) builds a plain
`RemoteApiProvider` with no `X-Node-Id` and no bearer. Four consequences, none
of them local to one module:

1. **Serving-side attribution never fires.** `admission.rs:125` decides
   `is_peer` by the presence of `X-Node-Id`, so a mesh-dispatched chat is
   indistinguishable from a local one on the serving node.
   `peer_inflight_current` stays 0 and `InferenceServed{for_node}`
   (`routes_inference.rs:872-887`) never emits. This is also why the soak's
   `admission_safety` invariant is inert for inference.
2. **Serving-side admission control does not apply to mesh chat at all.**
   `daemon.max_peer_inflight` (default 1) and `yield_to_foreground_secs` both
   gate on the same `is_peer` flag (`admission.rs:51-70`), so neither fires for
   an offloaded turn.
3. **Streaming loses the origin-side attribution too.** It is computed
   (`peer_inference.rs:2604-2605`) and then discarded by
   `complete_stream_with_finish` (`:2755-2760`), which is what the adapter calls
   (`inference_adapter.rs:1325`); SSE chunks echo the client's own model string
   (`routes_inference.rs:952`). Only the **non-streaming** response carries
   `"<model> @ peer <name>"` via `annotate` (`:1386-1389`) — which is precisely
   why the Phase-0 probe is non-streaming.
4. **Plaintext LAN peer dispatch cannot work.** `client_auth_layer` admits
   loopback unconditionally but requires the bearer from any non-loopback caller
   (`client_auth.rs:143-145,170-176`), and `mesh create`/`join` promotes the bind
   to `0.0.0.0` and forces a token. A bearer-less dispatch from another machine
   gets 403. The two paths that work are loopback (the netns soak) and **iroh**,
   whose acceptor forwards `CLIENT_ALPN` to `127.0.0.1:<client_port>` so it
   arrives as loopback (`iroh_access.rs:236-239`). This may be the whole reason
   cross-node inference has never been observed working on this mesh: a
   plaintext LAN mesh cannot offload regardless of the F8/`mesh-peer` fixes.

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

> **Read `as-implemented` as as-*designed* (correction, 2026-07-27).** Every
> table in §3.1 and §4.1.x uses arm 0 as its baseline, and arm 0 gives the
> decider an exact local queue depth and an accumulating peer history —
> neither of which the shipped dispatch path produces (F9). The *policy*
> comparisons below are unaffected: each holds the belief model fixed and
> varies one policy, which is the comparison they were built to make. What
> they do not describe is this mesh's behaviour on a given evening. §4.4
> measures that gap directly, and the arms that model the shipped beliefs
> are `blind-local-load` / `blind-peer-ramp` / `blind-observations`.

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
  — **measured**: −8% at constant quality on a fleet whose top band
  spans 34/25/11 tok/s, and the win is the two hubs the clamp cannot
  tell apart, not the slow one it can (§4.1.2)
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
>
> **5. F2 × §4.1, the composition that was missing at first landing**
> (`Arm::PredictedTimeOutboundOnly`). `published_load()` returned `Total`
> for every arm but one, so the objective had never seen a gossiped count
> that misses inbound peer work. The structural prior was that it should
> hurt *more* than the product — the product passes `in_flight` through
> `load_penalty`, a bounded multiplier, while predicted-time multiplies
> it by a service time. The prior is **conditionally right, and the
> condition is the finding**:
>
> | fleet | product damage | predicted damage | predicted offload share |
> |---|---|---|---|
> | household-evening-12 | +126% | **+0.4%** | 30% |
> | twin-hubs | +20% | **+0.2%** | 12% |
> | isolation | +584% | **+627%** | 70% |
>
> Exposure tracks how much the objective actually hops: a corrupted
> peer-queue count cannot damage a decision that stayed local. So
> predicted-time looks near-immune on the fleets where it declines most
> offloads and is *worse than the product* on the one fleet where it
> offloads 70% of traffic. **Do not read the first two rows as
> robustness.** The regime that concentrates the risk is sustained
> contention — which is also the only regime where F1 still mattered
> (+43.8%). Both of §4.1's information dependencies bite in the same
> place, and that place is where the two-daemon audit stops being
> "earned" and becomes a prerequisite.
>
> **6. Model-load time, the term whose absence would have cost most**
> (`SimConfig::model_load_sec_per_gb`, `predicted_time::LoadDebt`).
> `ProviderModel.status.estimated_load_time_sec` existed and the
> objective ignored it, while the sim charged nothing for paging a model
> in — so the arm could not have found its own blind spot, exactly as
> with the exact rate card. Load is now charged **additively, not
> multiplied by the queue**, because one load warms the slot for
> everything behind it. The objective absorbs it:
>
> | load s/GB (21GB hub) | arm 0 eff | predicted eff |
> |---|---|---|
> | 0.0 (preloaded) | 0.33–0.42 | 0.95–0.98 |
> | 1.0 (~21s) | 0.38–0.42 | 0.90–0.93 |
> | 3.0 (~63s) | 0.37–0.45 | 0.91–0.97 |
>
> Two consequences. The record grew `model_loaded` / `estimated_load_ms`
> on `CandidateInputs`, under the rule this arm makes explicit: **an
> objective may not read a signal the record does not carry**, or the
> decision stops being replayable from a capture. And a cold model with
> no advertised estimate is charged **zero** — a documented under-charge,
> because inventing a load time is the same fabrication
> `Unpredictable::NoThroughput` refuses for decode rates. Manifests
> should advertise the field; until they do, this objective is
> optimistic about cold peers in exactly one measurable way.

### 4.1.1 The tier floor, and what it costs — MEASURED 2026-07-26

> **Scope (F10): simulator-only.** `TierFloor::None` is hardcoded at the
> production call site (`peer_inference.rs:1293`), so nothing below has ever
> influenced a real dispatch. These are results about a policy that could be
> adopted, not a description of current behaviour.

> `Arm::TierFloor`, `Arm::PredictedTimeTierFloor`,
> `sovereign-mesh/src/tier.rs`. **This is the blocker above, priced —
> and it does not clear §4.1 for landing. It reverses part of its
> claim.**

**The mechanism, stated first.** Nothing structural separated a 4B from
a 35B for a synthesis request. `latency_match_score` is symmetric
(`abs_diff`, `scoring.rs:82`), so a *downgrade* and an *upgrade* score
identically; and a small model advertises a `Normal` claim alongside its
`Fast` one (`routes_oicp.rs:80`), so a 4B is a class-matched 1.0
candidate for a Normal turn. The only separation was `claim_affinity` —
self-reported, and a ranking multiplier rather than a gate. §4.1 drops
the multiplier, so the separation goes to zero. **The affinity term was
an accidental quality brake of the same family as `cold_start_weight`'s
0.7 floor and a mis-rated rate card**, which is now three sightings of
one pathology.

The floor makes capability a **filter**: candidates are partitioned into
bands (relative, computed per decision from the sizes visible then — no
absolute GB threshold, no table of model names), a `Normal`/`Extended`
request must be served from band 0, and predicted time ranks whatever
survives. It is the policy the *local* slot picker has always enforced
via `latency_to_speed`, finally applied to peers — a node will not
answer its own synthesis turn from its 4B, then ship it to someone
else's.

**Two hazards that were being counted as one.** `TierMetrics` splits
them, and the split matters:

| arm | fleet | downgrade (served below the origin's OWN local model) | declined upgrade (a stronger node was feasible) |
|---|---|---|---|
| arm 0 | household | 0% | 54% |
| predicted-time | household | **31%** | 69% |
| predicted-time | twin-hubs | 10% | 90% |
| predicted-time | heterogeneous | 4% | 96% |

Predicted-time serves *every* turn below the best available. Most of
that is declining an upgrade — the origins are 4B laptops, so those
users get what they'd have got at home, faster. But 4–31% is a true
regression, and the two need separate names because only the first
makes anyone worse off than not offloading.

**What the floor costs — and the answer is a capacity fact, not a
scheduling one.**

| fleet | arm 0 | arm0+floor | predicted | predicted+floor | top band |
|---|---|---|---|---|---|
| household-evening-12 | 25.7s | 559.5s | 11.4s | **559.5s** | 1 hub |
| heterogeneous-fleet | 40.2s | 120.4s | 11.5s | **120.4s** | 1 hub |
| twin-hubs | 33.3s | 31.0s | 11.1s | **32.6s** | 3 hubs |

Read the middle rows and the mechanism is invisible; read
`queue_wait_ms` by dispatch quartile and it is not. Service time is
**flat** while the queue climbs:

| fleet / arm | queue wait Q1 → Q4 | service | verdict |
|---|---|---|---|
| household, predicted+floor | 241s → **1020s** | 26.8s flat | queue unbounded |
| heterogeneous, predicted+floor | 45s → **182s** | 27.5s flat | queue unbounded |
| heterogeneous, **arm 0** | 13s → **42s** | 23.5s flat | already unbounded |
| twin-hubs, predicted+floor | 11.2s → 8.6s | 26.8s flat | **stable** |

A single 35B cannot serve a twelve-node household's knowledge turns.
That was always true; arm 0 concealed it by letting 62% of turns stay on
4B laptops, and predicted-time concealed it harder. The floor does not
cause the saturation — it *reveals* it, and on `heterogeneous-fleet`
arm 0's own queue is already growing without any floor at all.

**Three consequences, in descending order of how much they change the
plan.**

1. **§4.1's headline is not a quality-constant number.** "126–250% wrong
   objective" compares a policy that answers hard turns from a 35B with
   one that answers them from a 4B. On `twin-hubs` — the one fleet whose
   top band has capacity, so the only place the comparison is honest —
   arm0+floor is **31.0s** and predicted+floor is **32.6s**. At constant
   quality the predicted-time objective is **~5% worse than the
   product** on that fleet, not 200% better. One fleet is one data
   point, and the suite has no second fleet with a capable, unsaturated
   top band; building one is now the highest-value scenario work.
   *(Built — see §4.1.2, which qualifies this consequence: the sign of
   the constant-quality result turns out to depend on whether the top
   band's members differ in speed.)*
2. **The floor is free where the top band has capacity.** twin-hubs:
   −2% versus arm 0 *and* every quality loss eliminated (declined
   upgrades 76 → 0). Strictly better on both axes. So the floor is not
   the thing that is expensive; a one-hub fleet is.
3. **Predicted-time herds harder than the product once candidates are
   homogeneous.** With the floor on twin-hubs, arm0+floor spreads
   31/27/18 across three identical hubs while predicted+floor spreads
   40/28/10. Identical hubs differ only by a *stale* gossiped queue
   count, so the argmax is shared for a whole anti-entropy window —
   F5 and F1 compounding, in the regime the floor creates. §4.2 step 2
   (break the herd) is therefore a prerequisite for the floor, not a
   follow-on.

**Does a lying peer defeat it?** `size_gb` is self-reported, and it is
the only input to a *quality* gate that a node states about itself — so
`SimConfig::advertised_size_error` prices the flattery the way
`advertised_rate_error` prices the rate card. Two-sided, and the
adversarial direction is a small model over-selling into band 0:

| size error | seeds (of 5) where a non-hub reached band 0 | downgrades | mean |
|---|---|---|---|
| ±0% | 0 | 0 | 506.3s |
| ±25% | 0 | 0 | 506.3s |
| ±50% | 0 | 0 | 506.3s |
| ±100% | **1** | 0 | 442.1s |

Robust up to ±50%, and the reason is arithmetic rather than luck: the
hub is 3.5× the next model against a 2.0× band edge, so a lie has to
move the *ratio* by 1.75× before it crosses. At ±100% it does, on one
seed in five. Note the mean *falls* when it happens — an extra band-0
node relieves the saturated hub, which is a second sighting of the
capacity finding rather than a benefit of dishonesty. Downgrades stay
**0** at every level: a mis-advertisement changes which band a node is
in, and does not defeat the filter itself.

**What this does not settle.** These are all *proxies*: the sim counts
where capability went, never whether an answer was good. §5's landing
gate remains a Tier-2 measurement the sim cannot supply. The proxies are
worth something the latency table was not — `TierMetrics` is computed
from `RoutingDecision` records alone, so **the identical function scores
a production capture**, which is what lets a Tier-2 run be judged by the
same ruler.

### 4.1.2 What the objective is worth, on a second unsaturated fleet — MEASURED 2026-07-27

> **Scope (F10): simulator-only.** `RankObjective::Product` is hardcoded at the
> production call site (`peer_inference.rs:1288`), so the predicted-time
> objective cannot fire outside Tier 1. Additionally, per F9, the local
> candidate's load input is never written — so any conclusion here that depends
> on local load being observable is a statement about the sim, not the fleet.

> `scenario::mixed_hubs`, `Arm::PredictedTimeTierFloorTwoChoices`,
> `tests/mesh_sim_scoreboard.rs::does_predicted_time_beat_the_product_where_the_top_band_has_capacity`
> and `::does_breaking_the_herd_recover_what_the_floor_costs`. **§4.1.1
> consequence 1 was n=1 in two ways at once — one fleet, one seed. Both
> are now five seeds and two fleets, and the answer is conditional
> rather than either headline.**

`mixed-hubs` is the second fleet with a capable, unsaturated top band,
and it is deliberately the *opposite bracket* to `twin-hubs`. Band 0 on
`twin-hubs` is three **identical** hubs, so a predicted-time objective
has nothing to discriminate on but a stale gossiped queue count — the
condition most hostile to it. Band 0 on `mixed-hubs` is the same 35B on
three different machines, **34 / 25 / 11 tok/s**: real speed variance,
which is what predicting a completion time is *for*. Neither fleet
settles the question alone; together they bracket it.

Capacity is arithmetic, not hope — mean service ≈20s / 27s / 62s across
the three, an aggregate ~0.10 turns/s against an offered knowledge load
of ~0.035 turns/s, so even a policy that herds every turn onto the
*fastest* hub runs it at ~70%. Both fleets end their runs less than one
turn deep in queue, against 6.6 turns for `heterogeneous-fleet` and 38
for `household` (§4.1.1). That ratio — final-quartile wait over service
time — is the gate; the Q1→Q4 ratio §4.1.1 printed turns out to be only
a screen, since it fires on any fleet loaded enough to build a queue at
all.

**The result, five seeds per fleet, both arms wearing the floor:**

| fleet | band 0 | arm0+floor | predicted+floor | Δ | seeds predicted wins |
|---|---|---|---|---|---|
| twin-hubs | 3 × identical | 31.8s | 32.9s | **+3%** | 1/5 |
| mixed-hubs | 34/25/11 tok/s | 28.1s | **25.7s** | **−8%** | **5/5** |

So the objective's value is a function of whether the top band has
speed variance, and §4.1.1's −5% was a property of the fleet that
produced it. Five seeds also shrink that number: +3%, not +5%.

**The mechanism is F3, and it is not the obvious one.** The natural
reading of `mixed-hubs` is "predicted time avoids the 11 tok/s hub".
It does not need to — *the product already avoids it*. Served-turn
counts on band 0, one seed:

| arm | hub-fast (34) | hub-mid (25) | hub-slow (11) |
|---|---|---|---|
| arm0+floor | 34 | 36 | **0** |
| predicted+floor | **45** | 24 | 1 |

`throughput_factor` scores the slow hub 11/20 = 0.55 and that is
decisive, so none of the −8% comes from the visible gap. The loss is
the product splitting ~50/50 between two hubs that differ by 36% in
decode rate, because the clamp at 20 tok/s renders both exactly 1.0.
Deleting the slow hub entirely (band 0 = 34/25/25, same seed, same
arrivals, only that node's hardware changed) leaves predicted time
ahead by 3% — the win survives deleting the gap the scorer can see.
**F3 was catalogued as "does not discriminate under heterogeneity";
this is F3 with a price attached, at constant quality.**

**It is not the simulator flattering the objective.** The concern is
real and specific: `predicted_time` consumes `pp_tok_s` / `tg_tok_s`,
and this sim's service-time model is computed from those same two
fields, so on a fleet built out of speed variance the objective starts
with a perfect world model. `advertised_rate_error` prices it — nodes
serve at their true rate and advertise a perturbed one:

| rate error | arm0+floor | predicted+floor | Δ | seeds won |
|---|---|---|---|---|
| ±0% | 28.1s | 25.7s | −8% | 5/5 |
| ±25% | 28.1s | 26.1s | −7% | 4/5 |
| ±50% | 31.2s | 27.7s | −11% | 5/5 |
| ±100% | 34.1s | 29.6s | −13% | 5/5 |

The win widens, because the perturbation is two-sided and the product
degrades faster. One asymmetry could have made that unfair — the
product has an error-correcting path predicted time does not
(`throughput_factor` prefers the observed decode EWMA past five
samples; `PredictInputs::from_candidate` reads the advertised benchmark
and nothing else) — so it is counted rather than argued: the observed
path carries about **5%** of candidate scorings here, because most
peers never reach five samples in half an hour. That is F7's ramp
wearing a different hat, and it means both objectives read the same
perturbed number in ~95% of decisions.

**§4.2 step 2 is a prerequisite, and its qualifier is the load-bearing
part.** §4.1.1 inferred the herding consequence from a distribution;
`Arm::PredictedTimeTierFloorTwoChoices` measures it. The two fleets
disagree, and the disagreement is the finding:

| fleet | arm0+floor | predicted+floor | +two-choices | top-server share |
|---|---|---|---|---|
| twin-hubs | 31.8s | 32.9s (+3%) | **30.5s (−4%)** | 0.38 → 0.31 |
| mixed-hubs | 28.1s | 25.7s (−8%) | **28.8s (+3%)** | 0.54 → 0.40 |

Sampling recovers the whole herding loss where candidates are
interchangeable — predicted time goes from worst arm to best — and
destroys the whole win where they are not. A **blunt** power-of-two
sampler is therefore a fleet-dependent coin flip, and §4.2 step 2's
*"among candidates whose predictions are within noise"* is not a
refinement of that sentence but the whole of it. Note also what makes
the qualifier expressible at all: predicted times are in milliseconds,
so "within noise" has units, where a dimensionless product has no scale
on which two scores can be called close. §4.2 step 2 wants §4.1
underneath it.

**One instrument note.** `herding_cov` sat at 1.36–1.39 across every
arm on both fleets while `top_server_share` moved 0.31 → 0.54. The
policy change these tests exist to detect is invisible to the herding
metric and obvious in the concentration one — a fourth entry for §6's
list of scoreboard denominators that need interrogating before they are
believed.

### 4.1.3 The within-noise sampler, built — and the band's own blind spot

§4.1.2 left the blunt sampler reading two fleets in opposite
directions and named §4.2 step 2's qualifier — *"among candidates whose
predictions are within noise"* — as the load-bearing clause. That
clause is now implemented (`predicted_time::tie_band`,
`Arm::PredictedTimeTierFloorWithinNoise`) and it does what the clause
promised on both fleets at once.

**The noise is named, not chosen.** Of `predict`'s four terms exactly
one is built from a gossiped count that can be seconds stale — `queue =
in_flight × service`, which is F1 — and `Prediction::uncontended_ms` is
the other three. Two candidates are within noise of each other when
their *uncontended* predictions do not separate them: whatever order
the queue term then imposes is an order on the one signal we know is
wrong, together, for every decider at once. No constant appears
anywhere in this, which keeps invariant 1 intact.

| fleet | arm0+floor | predicted+floor | +blunt two-choices | **+within-noise** | band ≥2 | mean band |
|---|---|---|---|---|---|---|
| twin-hubs | 31.8s | 32.9s (+3%) | 30.5s (−4%) | **30.5s (−4%)** | 97% | 2.92 |
| mixed-hubs | 28.1s | 25.7s (−8%) | 28.8s (+3%) | **25.9s (−8%)** | 29% | 1.30 |

Both readings survive, and the two right-hand columns say why rather
than leaving it inferred. On `twin-hubs` the band is essentially the
whole band-0 set (mean width 2.92 of 3), which retroactively confirms
§4.1.2's guess that the blunt draw there *was* a near-tie draw — the two
arms agree to the tenth of a second because they are running the same
policy. On `mixed-hubs` the band collapses toward the leader (1.30) and
the sampler moves off the argmax on only 8% of decisions, so the
objective keeps its win instead of handing it back.

**The counter-finding, which is the part worth carrying forward.** The
band asks "are these the same machine?" and answers from the
*advertised* rate card — a number the candidate states about itself, and
one `predicted_time` deliberately never corrects (invariant 3 forbids
substituting a rate; the product objective is the one with an
error-correcting path, `throughput_factor`'s observed EWMA past five
samples). So the band recognises identical hubs only while they
*advertise* identically, and `advertised_rate_error` — the instrument
note 963a8d88's method rule already required for this arm's inputs —
prices that assumption:

| twin-hubs, rate-card error | arm0+floor | argmax | blunt | within-noise | mean band |
|---|---|---|---|---|---|
| ±0% | 31.8s | 32.9s | 30.5s | **30.5s** (−7% vs argmax) | 2.92 |
| ±10% | 31.8s | 32.1s | 30.2s | **33.0s** (+3%) | 1.45 |
| ±25% | 31.8s | 32.1s | 30.2s | **33.0s** (+3%) | 1.45 |
| ±50% | 34.3s | 32.1s | 30.2s | **32.7s** (+2%) | 1.46 |
| ±100% | 75.3s | 33.7s | 32.1s | **33.6s** (−0%) | 1.39 |

A cliff, not a decay: ±10% is enough to take the band from 2.92 to 1.45
and invert the recovery. And it is **not** the safe failure the first
draft of `tie_band`'s doc comment claimed. A collapsed band still opens
on ~25% of decisions — on whichever pairs happen to have near-equal
*perturbed* times — so the sampler goes on firing on noise after it has
stopped firing on the real ties. The blunt sampler, which never
consults the rate card, holds its whole recovery across the sweep.

**So neither sampler dominates, and the honest rule is a fleet
question, not a policy question.** Where the top band is genuinely
heterogeneous, only the within-noise draw is safe — blunt gives back the
objective's entire −8%. Where the top band is genuinely homogeneous,
blunt is both simpler and robust, and within-noise matches it only when
the rate card is exact. The repair for the gap is not a tolerance
constant (which would forfeit invariant 1 and merely move the cliff):
it is giving `tie_band` an observed rate where one exists, which makes
it §4.2 step 1's problem — and note 963a8d88 already counted the
observed path carrying only ~5% of scorings in a 30-minute run, so that
repair is gated on backpressure landing first.

**One instrument note, and it is a fourth entry for §6's
scoreboard-denominator list.** `SamplerTrace` exists because latency
alone cannot distinguish "the sampler never fired" from "it fired
constantly and its picks were a wash" — the two rows would look
identical and mean opposite things. The band-width and moved-off-argmax
counters are what turn the `mixed-hubs` row from a coincidence into a
mechanism, and they are computable from a production capture the moment
the objective ships, because `tie_band` rides on the ranking rather
than on the simulation.

### 4.2 Three contained follow-ons, in order

1. **Fresh backpressure.** Piggyback the serving node's true queue
   depth and estimated wait on every response, including the 503 body.
   No extra round-trips; instantly fresh for any peer recently spoken
   to. Collapses F1's dead time for exactly the peers that matter.
   **Measured 2026-07-27 (§4.2.1) and it does not — the last sentence
   is the part that fails. Do not build it as written.** The mechanism
   works; its coverage is set by traffic density, and at household
   density there is nothing for it to reach.
2. **Break the herd.** Sample two among candidates whose predictions
   are within noise, take the less loaded. Small change to the ranked
   selector; largest tail improvement in §3. **Promoted to a
   prerequisite for §4.1's landing, not a follow-on** (§4.1.1
   consequence 3, measured in §4.1.2). **Built and measured in §4.1.3**
   — read that section before choosing a sampler, because the answer is
   a fleet question rather than a policy question. Short form: the blunt
   uniform draw is unusable on a heterogeneous top band (it gives back
   the objective's entire −8% on `mixed-hubs`), and the within-noise
   draw fixes exactly that, but its band is computed from the
   *advertised* rate card and a ±10% error collapses it — so on a
   homogeneous top band the blunt draw is the robust one. Neither is
   ready to be the single shipped policy; the thing that would make the
   within-noise band trustworthy is an observed rate, which is step 1's
   problem, not step 2's.
3. **Split congestion from failure.** One enum at the record site —
   `Congested { retry_after }` vs `Failed`. Congestion drives a
   short-half-life per-peer backoff; only failure touches
   `PeerHealthTracker`. Honour the `Retry-After` header the server
   already sends (`admission.rs:150`) and the client currently
   discards. *Then* a finite ceiling becomes safe to enable and the hub
   gets a real degradation mode.

Priority classes ([`MESH_INFERENCE.md`](MESH_INFERENCE.md) Increment 4)
land after (3), never before — shedding without (3) manufactures F4.

### 4.2.1 Step 1 measured before it was built — and it does not pay

`fresh-signals` is the arm half this document leans on, and it is not a
policy anyone can ship: it hands every decider the truth about every
peer at every instant. Step 1 proposes to collect that win by
piggybacking the serving node's load onto responses it already sends.
Those are not the same thing, and §6's arm-first rule is what caught
the difference: `Arm::ResponseBackpressure` is the mechanism with its
real reach — fresh for a peer this decider has served a request
through, stale gossip for everyone else, newest measurement winning
where both exist.

Five seeds per fleet, `tests/mesh_sim_scoreboard.rs`
(`what_does_piggybacked_backpressure_recover_of_fresh_signals`):

| fleet | arm 0 mean / p95 | backpressure | fresh-signals | coverage | median true signal age |
|---|---|---|---|---|---|
| household-evening-12 | 24.9 / 74.0 s | 25.3 / 76.7 s (**+1.6% / +3.7%**) | 22.6 / 61.8 s (−9.0% / −16.5%) | **6%** | 14.1 → 13.0 s |
| twin-hubs | 31.7 / 68.7 s | 30.9 / 65.3 s (−2.6% / −4.9%) | 28.3 / 56.5 s (−10.7% / −17.7%) | **7%** | 14.3 → 13.2 s |
| mixed-hubs | 22.9 / 52.3 s | 22.9 / 53.1 s (+0.1% / +1.5%) | 20.5 / 40.9 s (−10.5% / −21.9%) | **4%** | 14.8 → 14.4 s |
| isolation | 86.7 / 257.8 s | 84.5 / 265.3 s (−2.5% / +2.9%) | 85.2 / 260.3 s (−1.8% / +0.9%) | **46%** | 15.0 → **10.4 s** |

**The mechanism is not broken — it is unreached.** `isolation` is the
control: a background actor dispatching every ~8s against a
household's ~4 min. Coverage moves 4% → 46% and the median true signal
age drops 15.0 → 10.4 s, which is the piggyback doing exactly what step
1 says it does. So low coverage on the other three fleets is a fact
about **traffic**, not about the wiring.

**Why coverage is 4–7% at household density, arithmetically.** A
response is fresher than gossip only in the window between it landing
and the next gossip round — at most one 10 s interval. A decider makes
~41 peer-dispatches per 30-minute run while gossip delivers ~180
measurements per peer over the same span. The response reading is the
newest one for a few percent of decisions because that is the share of
decisions that fall inside those windows. Nothing about the rule is
conservative: it is the ratio of the two rates.

**And where coverage is high, the freshness is worth nothing.** On
`isolation`, `fresh-signals` itself buys −1.8% mean and +0.9% p95. That
fleet's problem is capacity, not information — no scheduler fixes an
oversubscribed queue, which is the reading `print_saturation` exists to
make. Step 1 therefore lands in a scissor: **at the densities where it
fires, information is not the constraint; at the densities where
information is the constraint, it does not fire.**

**The deeper reason, and it is the part that generalises.** F1's win
lives in gossip's staleness about peers a decider is *not* currently
talking to — a herd forms because everyone's picture of the peer they
are all about to choose is 10–30 s old. A response can only carry news
about a peer you already chose. The channel is structurally blind to
the population the finding is about. That is not fixable by making the
response carry more; it is fixable only by shortening the interval on
the channel that reaches everybody.

**Objective sensitivity, checked and negative.** §4.2 predicted step 1
would matter *more* to §4.1's objective, because the product bounds
`in_flight` through `load_penalty` while predicted time multiplies it
by a service time — the same asymmetry
`predicted-time+outbound-only` prices for attribution. Measured
(`is_fresh_backpressure_worth_more_to_predicted_time_than_to_the_product`),
the two deltas are indistinguishable: on `twin-hubs`, −2.6% mean under
the product against −2.5% under predicted-time+floor; on `mixed-hubs`,
+0.1% against −0.1%. The asymmetry is real in the arithmetic and
invisible at 4–7% coverage, because a first-order error on 6% of
decisions is smaller than a second-order error on all of them.

**Two caveats, one of which is a genuine unmeasured upside.**

- **The 503 half is untestable here**, exactly as F4 is: no admission
  gate runs in the sim, so no request is ever shed. A shed is the one
  case where the response arrives about a peer you were *about to keep
  hammering*, and it is also the case §4.2 step 3 needs. Nothing above
  prices it. It is the reason step 1 is *deferred* rather than
  *retired*.
- **The sim flatters the mechanism twice.** The reading is delivered at
  completion rather than one RTT later, and `load_belief` compares
  **true** measurement times — which a real client cannot do, since
  `gossip_last_seen_unix` is receipt time and understates gossip's age
  (§3.1). A production implementation choosing between the two
  channels would over-value gossip relative to this simulation. Every
  number above is an upper bound on the real mechanism.

**Consequence for the ordering.** §4.1.3's conclusion — that the
within-noise band is untrustworthy until the rate is observed, and that
the repair is "gated on backpressure landing first" — has lost its
carrier. Step 1 will not deliver an observed rate to the peers whose
banding is in question, for the same coverage reason. Either that
repair finds a different channel, or the band stays advertised-rate
based with the ±10% cliff §4.1.3 measured. Step 3 (splitting congestion
from failure) is now the first item in §4.2 with an unfalsified claim,
and it is the one that carries the 503 body step 1 wanted anyway — so
the piggybacked reading should ride along **there**, on the shed path,
where its coverage is by construction 100% of the decisions that matter.

### 4.3 The soft-named fallthrough — LANDED 2026-07-27

F8's fix, and the first item in this document to land **in production**
rather than as a Tier-1 arm. §6's arm-first rule does not gate it: it
changes a fallback *chain*, not a scheduling *policy*. The scorer's
objective, weights and gates are untouched — production still ranks on
`RankObjective::Product` with `TierFloor::None`, exactly as §4.1
requires until the Tier-2 gate exists. What changed is which requests
reach the scorer at all.

**The rule.** A soft named target that resolves to nobody now falls
THROUGH to ranked mesh selection, with local as the last rung rather
than the second. Both surfaces: `select_route`'s
`NamedModelLocation::Unknown` + `soft` arm (streaming), and `complete`'s
shared-primary preamble (non-streaming). On the non-streaming path the
fix is structural rather than additive — *not* rewriting `model_id`
leaves the request on the ranked path that already ran a few lines
below, so the early `return` simply goes away.

**The carve-out that keeps it honest.** A hard target — an explicit
caller-supplied `model_id` — still fails loudly. It is a constraint, not
a preference, and it must never be silently served by whatever the
scorer happens to like. Pinned by
`hard_named_target_still_fails_loudly_rather_than_falling_through`.

**Why it is recordable.** A fallthrough emits a second decision record
on a new `DecisionPath::NamedFallthrough`, sharing the request's
`oicp_request_id` with the `NamedModel` record that named the missing
model. Two records, one story: *the target resolved to nobody, and then
the scorer ran.* Neither alone can answer "why did my 122B request get
answered by the 35B?". The plan joins its **outcome** to the
fallthrough record, because the ranked scorer is what picked the
server — joining it to the named record would attribute a serve to a
decision that scored nothing. The path is kept distinct from
`RankedOicp` (rather than reusing it) so a scoreboard can count
"how often is the shared cluster unavailable?" without that traffic
disappearing into the ordinary ranked population; `decision_replay`
admits both, since a fallthrough's candidates carry real breakdowns.

**Deliberately NOT done — the `Peer` + soft case.** When the named host
*resolves* and then fails every address mid-cascade, the plan still ends
at `LocalFallback`. Landing the fallthrough there needs something this
architecture does not have: per-**step** decision attribution. A plan
carries one `decision_id`, chosen up front, and a cascade whose named
step and ranked steps came from two different decisions cannot say which
one to credit without lying about one of them. The gap is also the
transient one (see F8's scale note) — `PeerHealthTracker` quarantines
the failing host, after which the persistent `Unknown` path applies.
Per-step attribution is the prerequisite; it is not on this critical
path.

**Gates.** `scripts/sovereign-lint.sh` 0 fail / 175 pre-existing warns
(0 in new code); `scripts/sovereign-test.sh` **8223 pass / 0 fail** (was
8219 — the four new e2e tests in
`sovereign-mesh/tests/scheduler_decision_records.rs` §9). The e2e proves
the headline claim end-to-end against a real `MeshInferenceProvider`:
shared primary configured, nobody advertising it, a free peer on the
mesh ⇒ **the peer serves and local does not**, with both records
emitted and the outcome joined to the right one. Its companion pins the
degrade path — no worthy peer ⇒ still local, still recorded as a scored
stay-local rather than a gate.

### 4.4 F9 priced — and the baseline was not the system — MEASURED 2026-07-27

The finding to read first is not about latency. It is that **arm 0 was
not the shipped system**, and had not been since S0.

`Sim::local_view_observations` handed `rank` an exact local queue depth
and let peer observations accumulate samples through `observe_dispatch`.
Production does neither on the ranked path. So every number recorded
against arm 0 — §3.1, §4.1.1, §4.1.2, §4.1.3, §4.2.1 — is a comparison
against the mesh as **designed**, not as shipped. Those numbers are not
wrong, and they are not retracted: "is this policy better than the
product objective?" is a question about the designed system and arm 0 is
the right denominator for it. But "what does this mesh do tonight?" was
never being asked, and F9 is why the answer would have been different.

Four arms now separate the two, because F9's halves bias in **opposite
directions** and a single total would have netted them out:

| arm | local `in_flight` | peer `samples` | is |
|---|---|---|---|
| `as-implemented` | exact | accumulates | the mesh as **designed** |
| `blind-local-load` | **0** | accumulates | F9's local half alone |
| `blind-peer-ramp` | exact | **frozen 0** | F9's peer half alone — **and the fix candidate** |
| `blind-observations` | **0** | **frozen 0** | the mesh as **shipped** |

Mean of 5 seeds, against `as-implemented`:

| scenario | `blind-local-load` | `blind-peer-ramp` | `blind-observations` |
|---|---|---|---|
| `household-evening-12` | +0% / +0% | −24% / −32% | −24% / −32% |
| `pair` | +0% / +0% | −11% / −6% | −11% / −6% |
| `twin-hubs` | +0% / +1% | +3% / +4% | +3% / +4% |
| `heterogeneous-fleet` | +1% / +1% | −33% / −34% | −33% / −33% |
| `isolation` | **+288% / +205%** | +13% / −27% | **+288% / +205%** |

*(mean / p95. `isolation` offloads: 47.4 → 11.6 under a blind local slot.)*

**The local half costs nothing on four fleets and 3.9× on the fifth.**
That is not a weak finding, it is a *sharp* one: `load_penalty` is
`1/(1+0.05n)`, so one or two in flight is a 5% nudge that the locality
bonus (1.15) swamps. Only where the local slot backs up **deeply** does
the term grow teeth — and that is `isolation`, the sustained-contention
fleet, which is the case a household hub exists for. F9 is CRITICAL as
filed, and its blast radius is precisely the case that matters.

**The landing case is unusually clean.** The fix is `blind-peer-ramp`
(the shipped mesh, plus local load wired) against `blind-observations`
(the shipped mesh): **−71% mean / −76% p95 on `isolation`, and ±1%
everywhere else.** A large win where it fires and a no-op where it does
not — so it landed in production the same day, per §6's arm-first rule.

**The peer half is a brake, not a bug — do not "complete" it.** Freezing
peer `samples` at 0 pins `cold_start_weight` at 0.7 forever, and that is
worth −11% to −33% mean latency on three of five fleets (`pair`,
`household-evening-12`, `heterogeneous-fleet`), against a +3% cost on
`twin-hubs`. The mechanism is monotone and has three points on it, read
on `heterogeneous-fleet` p50 at the reference seed: `warm-start` (peers
always 1.0) 120.7s, `as-implemented` (peers ramp) 33.7s,
`blind-peer-ramp` (peers always 0.7) 16.9s. The more attractive a peer
looks, the more this fleet over-offloads and the worse it does.

This is the **second** time this document's obvious fix has been a
regression — F7's warm-start was the first, at +235%. Both times the
mechanism that looked like a bug was load-bearing as a *brake*. The
pattern is worth naming: on a fleet where the peer is not decisively
better, anything that raises peer scores uniformly buys queueing, not
throughput. §6's arm-first rule has now paid for itself three times
(F7, §4.2.1, F9-peer).

**One honest limit.** `throughput_factor` gates its source-of-truth on
`samples >= THROUGHPUT_OBSERVATION_THRESHOLD` (`scoring.rs:231`), so a
frozen `samples` also means the mesh **measures peer throughput on every
stream and then declines to read it** (`ThroughputTarget::Peer` keeps
`tg_tok_s_ewma` current regardless). The 0.7 brake and the discarded
throughput signal are entangled in this arm and were not separated. If
the brake is ever replaced by something deliberate, that separation is
the first experiment to run — a peer scored on its *observed* rate is a
different policy from a peer scored at a flat 0.7, and only one of them
is defensible.

**Gates.** `scripts/sovereign-lint.sh` 0 fail / 173 pre-existing warns (0
in new code); `scripts/sovereign-test.sh` **8257 pass / 0 fail** (was
8256 — the one new production test; the sim's own test is behind
`mesh-sim` and does not run in the default workspace set).
`mesh_sim_scoreboard` 23 pass / 0 fail (was 22 — new:
`what_the_scorer_loses_by_never_seeing_its_own_load`, which asserts the
arm wiring and the one directional claim that is arithmetic rather than
fleet-specific: zeroing the local count can only *reduce* offload,
because `load_penalty` is monotonically decreasing in `in_flight`).
`scheduler_decision_records` 15 pass / 0 fail (was 14), including the
red-first production pin described in F9. `chat_completion_e2e` 17,
`load_awareness_e2e` 5, both unchanged — the fix moved no existing
routing assertion, which is itself the point: no test in the repository
could distinguish the pre- and post-fix scorer until one was written
for it.

### 4.5 F10 priced — the scorer has no speed signal at all — MEASURED 2026-07-27

§4.4 found that arm 0 was not the shipped system in two ways. There is a
third, it was never listed, and it is the larger one: **arm 0 supplies
every candidate with a rate card, and no node on this mesh has ever
advertised one.**

The chain is dead end to end. `run_baseline_benchmark`
(`sovereign-inference/src/benchmark.rs:59`) has zero callers.
`MeshInferenceProvider::set_local_benchmark` (`peer_inference.rs:808`)
has zero callers, so `local_benchmark` stays at the `None` it is
constructed with (`:573`). The gossip builder hardcodes `benchmark: None`
(`capabilities.rs:194`). Two doc comments describe the mechanism as
though it runs — `benchmark.rs`'s module header ("Runs once at daemon
startup … persisted to disk") and `capabilities.rs`'s ("stamped in by
the daemon's startup probe via a separate `with_benchmark` setter"). The
setter does not exist and the probe has no caller.

**What that does to the score, and it is not a degradation — it is a
deletion.** `throughput_factor` (`scoring.rs:362`) has exactly two
sources and production supplies neither: the observed EWMA is gated on
`samples >= 5`, which the ranked path never reaches (F9's peer half),
and the benchmark estimate needs the card that does not exist. Its
`(None, None)` branch returns **neutral 1.0**. So F3's term is not
"failing to discriminate under heterogeneity" — it is a **constant**,
and every peer on every fleet is scored as though it ran at the
reference rate.

One asymmetry survives, and it runs the other way. The *local* candidate
does reach the observed gate: production seeds local `samples` above the
cold-start threshold at construction (`peer_inference.rs:559`) and keeps
`tg_tok_s_ewma` current via `ThroughputTarget::Local`. Since
`throughput_factor` clamps to `[FLOOR, 1.0]`, the local node is the only
candidate that can be scored *below* the constant every peer enjoys — a
bias toward offload, opposite in direction to F9's local half. Measured
inert in this suite (every local rate sits above the 20 tok/s reference,
so the clamp never bites), but it is in the code and it is why F9's and
F10's halves must not be netted into one number.

**Two new arms** (`ALL_ARMS` 19 → 21): `blind-rate-card` — the card
removed, everything else as-implemented; and `blind-shipped` —
`blind-peer-ramp` plus `blind-rate-card`, which is what this mesh does
tonight and the successor to `blind-observations` as the as-shipped
denominator.

| scenario | `blind-rate-card` | `blind-shipped` |
|---|---|---|
| `household-evening-12` | +0% / +0% | −24% / −32% |
| `pair` | +0% / +0% | −11% / −6% |
| `twin-hubs` | +0% / +0% | +3% / +4% |
| `heterogeneous-fleet` | +0% / +0% | −33% / −34% |
| `mixed-hubs` | **+29% / +88%** | **+48% / +117%** |
| `isolation` | +0% / +0% | +13% / −27% |

*(mean / p95, 5 seeds, against `as-implemented`. The `blind-shipped`
column is `blind-peer-ramp` plus the rate card, and outside `mixed-hubs`
it is `blind-peer-ramp` to the decimal — the two effects do not
interact.)*

**The rate card is worth nothing on five of six fleets, including the
one called `heterogeneous-fleet`.** That is the sharp part.
`throughput_factor` divides by a 20 tok/s reference and clamps to 1.0,
so a rate card only carries information about a node *slower* than the
reference. `mixed-hubs` is the only fleet in the suite with one (an 11
tok/s hub). Everywhere else every node clamps to 1.0 with or without a
card, and removing it changes not one decision. F3 catalogued the clamp
as failing under heterogeneity; the clamp is better described as
**a detector for sub-reference nodes and nothing else**.

**The landing case, and then the reason not to take it.** Wiring the
probe moves the mesh from `blind-shipped` to `blind-peer-ramp` (the peer
ramp stays frozen either way — §4.4 measured it protective): −32% mean on
`mixed-hubs`, 0% on all five others. The same large-where-it-fires,
no-op-elsewhere shape that cleared F9 to land the same day. It also
survives a mis-measured probe, and *widens* — −32% at ±0/±25% rate
error, −37% at ±50%, −40% at ±100%, with the `blind-shipped` control
flat at 28.0s throughout. A worse rate card helps, which is the fourth
sighting of "a broken signal becomes an accidental brake."

**But the mechanism the sim priced is not the one production would
ship.** In this sim each node advertises a card measured on the model it
serves, so `throughput_factor`'s size-ratio extrapolation
(`scoring.rs:384`) runs at ratio 1.0 and is inert — asserted in the test
rather than left to be noticed. `run_baseline_benchmark` probes the
**`Speed::Fast` slot**: a ~2.5 GB model standing in for a 21 GB hub, with
the estimate scaled linearly on the size ratio. `SimConfig`'s new
`probe_baseline_size_gb` / `probe_sublinearity` model that, with real
scaling as `rate ∝ size^-β`. β = 1 is the assumption the code already
makes, and it reproduces the un-probed rows exactly (asserted, not
eyeballed):

| β | `blind-shipped` | `+rate-card` | Δ | downgrades | declined upgrades |
|---|---|---|---|---|---|
| 1.0 (the code's assumption) | 28.0s | 19.0s | −32% | 0.0 | 31.2 |
| 0.9 | 28.0s | 19.0s | −32% | 0.0 | 31.2 |
| 0.7 (bandwidth-bound) | 28.0s | 12.3s | **−56%** | 0.0 | **67.0** |
| 0.5 | 28.0s | 12.1s | **−57%** | **3.0** | **67.0** |

**Read the right-hand columns or the table lies to you.** The extra 24
points of "win" at realistic β is bought with capability, not earned:
declined upgrades more than double, and by β = 0.5 the mesh starts
serving turns below the origin's own local model. β < 1 does not give
the scorer better information — it gives it a *systematic under-estimate
of large models*, and the clamp is one-sided, so that error can only
push big candidates down. This is §4.1.1's hazard arriving through a
completely different door: anything that makes small fast models look
better reads as a large latency win on a scoreboard that cannot see
answer quality.

**Verdict: do not simply un-dead the existing probe.** The −32% row is
real and is the only one that describes an honest rate card, and it is
reachable — but only by probing *the model being scored* rather than the
Fast slot. Eliminate the extrapolation; do not tune β. That is a
different piece of work from calling `run_baseline_benchmark` at
startup, and it is the one F10 actually asks for.

**And this settles F10's other half.** `RankObjective::Product` is
hardcoded at `peer_inference.rs:1288`, but unhardcoding it today would
expose a policy that cannot execute: `PredictInputs::from_candidate`
reads the advertised benchmark and nothing else, so `predict` would
return `Err(Unpredictable::NoThroughput)` (`predicted_time.rs:342`) for
**every candidate on every request**. The hardcoded objective is not the
blocker. The missing rate card is, and the tier floor —
`TierFloor::from_requirements` needs only advertised `size_gb` — is the
one §4.1 policy that is actually reachable today. §4.1.1's numbers say
it must not be enabled on a one-hub fleet regardless.

**Gates.** `mesh_sim_scoreboard` 24 pass / 0 fail (was 23 — new:
`what_the_scorer_loses_by_never_measuring_anyone`). `sovereign-mesh`
lib 20 pass, including the `ALL_ARMS`-iterating invariants at 21 arms.

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
| **capability** | `TierMetrics` (§4.1.1): **downgrade rate** — served in a weaker band than the origin's own local model, a real regression — and **declined-upgrade rate** — a stronger node was feasible and passed over. Everything else on this table is a cost; this is what was traded for it, and without it a policy that answers hard turns from a 4B reads as an improvement |
| **saturation** | `queue_wait_ms` by dispatch quartile against a flat service time. Separates "this policy queues" from "this fleet is oversubscribed" — the distinction that decided §4.1.1, where the floor's 559s turned out to be a capacity fact no scheduler can fix |
| **hard invariants** | assertions, not scores: `LocalOnly` never crossed the wire; `Fast` never offloaded; no request served by a node lacking the claimed capability (the third had no implementation until capability became a banded, recorded property — it is now `TierMetrics::downgrades == 0` under a binding floor, guarded by an `unbanded_decisions == 0` check so it can never pass vacuously) |

### Tier 2 — `household-bench` (real hardware, plateaus)

The 3-actor scenario [`MESH_INFERENCE.md`](MESH_INFERENCE.md) §0
specified as Increment 0 — Alice enriching, Bob on complex knowledge
queries, Carol casual — reporting per-actor TTFT p50/p95, inter-token
rate, stall count and hub-vs-local turn share. Run at plateaus, not per
change.

### Tier 3 — soak (nightly)

Extend `mesh-soak-nightly` with load. It currently soaks membership and
gossip convergence, not scheduling.

#### Tier 2.5 — the cross-node serviceability lane (LANDED 2026-07-27)

`household-bench` was never built, and the gap that mattered turned out not to
need it. `scripts/mesh-soak.sh --workload offload` boots N **real** daemons on
one machine inside a rootless netns and asserts the one property nothing else
did: that a request can actually be **served by another node**.

- **Forcing function, no new knob.** `locate_named_model:1501` prefers a peer as
  soon as `local_inflight > peer_inflight`, so N concurrent `{"model":"primary"}`
  turns against one node push at least one onto an idle peer. `primary` is a
  mesh-advertised alias, so the peer genuinely resolves it.
- **Attribution.** The non-streaming response's own `model` field, which
  `annotate` (`peer_inference.rs:1386-1389`) rewrites to `"<model> @ peer <name>"`.
  No new HTTP surface, no trace capture. (Streaming cannot be used here — F11.)
- **First observed cross-node serve on this mesh, 2026-07-27:**
  `turn 2 → HTTP 200 served-by: primary @ peer node1`, 1/3 turns offloaded, two
  real daemons, real gossip.
- **Proven not inert.** With `SOVEREIGN_DISABLE_PEER_INFERENCE=1` the same lane
  goes red (0/3 offloaded, `invariant_violation_rate` 0.0 → 0.5, gate FAIL).

Preconditions the lane handles explicitly: the peer's Slow slot must be warm
(else it advertises no `primary` row) and its gossiped in-flight must have
settled to 0 (else the tie goes local). Both are why the probe warms and waits
rather than firing cold.

The **ranked-anonymous** class deliberately is not probed here — per F9 it
cannot be forced on a homogeneous fleet — and is gated in-process instead, in
`sovereign-mesh/tests/chat_completion_e2e.rs`, against a mock peer that
*resolves* `model` rather than accepting any body. That distinction is the whole
lesson: the previous mock certified transport, so twelve green e2e tests
coexisted with a total outage of anonymous offload.

#### Three defects repaired in the gate itself (2026-07-27)

Adding an assertion to a gate that cannot fail would have reproduced the very
defect this programme exists to eliminate. Found and fixed:

1. **`invariant_violation_rate` could never be non-zero.** `soak_slis` counts a
   checkpoint only when it carries a `violations` key (`mesh_soak.rs:307`), but
   the harness emitted that key **only on the passing branch** — a failing
   checkpoint was silently dropped. `check()` now emits the CLI's `--json`
   record verbatim, which always carries it. Pinned by
   `a_failing_checkpoint_is_only_counted_when_it_carries_violations`.
2. **`founder_degraded_rate` could never be non-zero** either, for the same
   reason: `founder_degraded` is only in the `--json` branch. The same one-flag
   change revives it — which matters because that SLI is the *only* assertion
   `--reachability-chaos` makes.
3. **Multi-node soaks were secretly single-node.** The daemon takes a per-HOME
   run lock (`lifecycle.rs:569-591`) and `SOVEREIGN_ALLOW_MULTIPLE_DAEMONS`
   appeared nowhere in `scripts/` or `.github/`, so node0 took the lock and
   every other node exited immediately. The surviving one-node mesh still passed
   the whole invariant pack, because convergence and pairwise liveness are
   vacuous over a single reachable node. A failed bring-up is now a red run.

Still outstanding, and deliberately not changed: CI remains advisory
(`continue-on-error: true`, the gate step ends `|| true`, and
`.github/mesh-slo-baseline.json` does not exist, so `gate_slis` reports
`first_run` and exits 0 regardless). Enforcement is a separate decision once
this lane has a track record.

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
| `PredictedTime` | how much of the oracle gap is a **wrong objective** rather than **imperfect information**? | Overwhelmingly the objective: +126%/+200%/+250% against +4.7%/+1.8%/−0.0%. Survives a ±2× mis-rated fleet and up to ~63s of model-load time. **But it routes knowledge turns to 4B laptops**, so it cannot land without a tier floor. Full result in §4.1 — and read §4.1.1 before quoting those percentages, which are not quality-constant. |
| `TierFloor` | what does requiring the top capability band cost, on its own, before any objective change? | Nothing where the top band has capacity (twin-hubs 33.3s → **31.0s**); everything where it does not (household 25.7s → **559.5s**, one hub). §4.1.1. |
| `PredictedTimeTierFloor` | how much of §4.1's win survives being made to respect capability? | On the one fleet whose top band is not saturated, **none of it** — arm0+floor 31.0s vs predicted+floor 32.6s, i.e. the objective is ~5% *worse* at constant quality. Every quality loss is eliminated (76 declined upgrades → 0). This is the arm that stopped §4.1 landing on a number that was not measuring what it claimed. |
| `PredictedTimeOutboundOnly` | does F2's mis-attributed load hurt the new objective more than the product? | **Conditionally, and the condition is the finding**: +0.4%/+0.2% where it offloads 30%/12% of traffic, but **+627% vs the product's +584%** where it offloads 70%. Exposure tracks offload share — the near-immunity is not robustness. §4.1 point 5. |

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
| 1 | §4.1 predicted-time ranking — **arm done, tier floor done, landing NOT cleared** | ~~efficiency ratio improves~~ and ~~tier floor as a separate explicit input~~ (both done — §4.1.1). What the floor found re-scopes the rest: at constant quality the objective is **~5% worse than the product** on the only fleet whose top band is not saturated, so the landing case has to be rebuilt rather than finished. Prerequisites now, in order: ~~**(a)** a scenario with a capable, *unsaturated* top band~~ (done — `mixed-hubs`, §4.1.2); ~~**(b)** §4.2 step 2 (break the herd)~~ (**built and measured — §4.1.3**, but it did NOT close the way (a) did: the within-noise band is the only sampler safe on a heterogeneous top band, and its band collapses under a ±10% rate-card error, so the sampler choice is still fleet-dependent and the real repair — an observed rate for `tie_band` — is gated behind §4.2 step 1); **(c)** the Tier-2 answer-quality gate the sim still cannot supply; **(d)** an **objective tag on `RoutingDecision`** so replay can pick the right policy |
| 2 | Fresh backpressure — **demoted** | Against the product objective F1 looked like the median's main cost. Against a *correct* objective it is worth 1.8–4.7% on three fleets and +43.8% on `isolation` alone, so this is now scoped to sustained contention. Exit unchanged: the median gap between the `as-is` and `fresh` arms closes |
| 3 | Two-choices sampling | herding CoV and p95 improve; the completed-count trade-off is reported, not hidden |
| 4 | Congestion ≠ failure + honour `Retry-After` | quarantines-of-healthy-nodes goes to zero under a shed scenario |
| 5 | F6 floors: deficit-ordered queue, contribution demoted to surplus | worst-served-origin share and dependency-weighted shed hold under sustained contention |
| 6 | Enable a finite ceiling; priority classes | hub degradation is shedding-with-recovery, not unbounded queueing — and step 5's floors hold while it sheds |
| 7 | **F9 local half — LANDED 2026-07-27** | Wire the real local in-flight count into the scorer. Armed first (`blind-local-load` / `blind-peer-ramp` / `blind-observations`), measured at −71% mean / −76% p95 on `isolation` and ±1% on four other fleets, then landed. §4.4 |
| — | **F9 peer half — DO NOT BUILD** | Priced as *protective*: freezing peer `samples` is worth −11%..−33% mean on three fleets. Completing the wiring is a regression. Second instance of F7's trap. §4.4 |
| 8 | **F10 — the rate card, and it must be per-model** | Blocks step 1 entirely (a predicted-time objective with no rate card returns `NoThroughput` for every candidate). An honest card is worth −32% mean on `mixed-hubs` and 0% on five other fleets. Exit criterion is **not** "the probe has a caller": it is that the card describes the model being scored, so `throughput_factor`'s size-ratio extrapolation stays inert. Wiring the existing `Speed::Fast` probe instead fails this — §4.5 |
| — | **F10 via the Fast-slot probe — DO NOT BUILD** | Un-deading `run_baseline_benchmark` as written reads as −56% and is a quality regression: declined upgrades double and downgrades appear. Fourth instance of the trap. §4.5 |

~~F3's clamp is the one plausible exception to "arm before landing": a
term that evaluates to a constant carries zero information, so no
metric can degrade by fixing it.~~ **Retracted 2026-07-27 by F10's arm,
and it is worth keeping the strikethrough rather than deleting the
sentence, because the reasoning was seductive and wrong in an
instructive way.** The premise is true — `throughput_factor` really does
evaluate to a constant in production (§4.5) — and the conclusion still
does not follow. A constant carries no information, but the thing that
replaces it is not information either unless it is *unbiased*. The
shipped repair would substitute a systematic under-estimate of large
models, and §4.5 measures that degrading two quality proxies while
latency improves. "No metric can degrade by fixing it" quietly assumed
the fix is a measurement; it is an extrapolation. Arm first — including,
and especially, the changes that look like they cannot lose.

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
| gossiped in-flight overrides self-observed (peers only — see F9) | — | `scheduler_core.rs:512-521` |
| local in-flight, read from the gossip publisher (F9 fix, 2026-07-27) | — | `peer_inference.rs`, the `local_obs` binding |
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
