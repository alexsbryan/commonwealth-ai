# The serving package boundary

Declared 2026-09-14 by rung `domains-4` (the adjudication that fixed the words), ahead of
the work: rung `domains-9` registers the `[[package]]` row and watches this boundary FAIL on
the fused tree; rung `domains-10` turns it green. The ranked record is a **`Venue`**
(`quality/DOMAINS.md` §10.1); its interfaces below use that name.

Serving answers *which engine answers this, and can I prove why* — 44,485 lines (38,665
without `frontdoor.rs`, which is Host) with no crate, no name and no doc, invisible to the
route census because all eight core modules register zero routes and are reached by
function call from the turn path.

**Which crates it holds is not written here.** The set lives in one place,
`[[package]] name = "serving"` in `quality/ARCH_LAYERS.toml`, for the reason
`commonwealth/BOUNDARY.md` gives at its head: a second copy of a list drifts while the gate
stays green. `cargo xtask boundary-gate` enforces the property (blocking).

## Why this is declared before the work, not after

Every gate here goes green on the half of Phase C that moves files, and stays green on
a `commonwealth-*` dep acquired the day after; `layer-gate` cannot see it. So the
declaration lands first and is **watched failing on the fused tree** (ARCH §18.1).

Unlike `commonwealth`, this package is **red the day it is declared**, and that is the
point. The scheduler half (the eight modules in the tier table below, 6,356 lines)
imports zero `commonwealth_*`, zero `sovereign-inference`, zero `axum`, and carries one
non-Serving `crate::` reference between them (`decision_replay.rs:75`, feature-gated, in
a doc comment). **That half is already a crate; it has not been given a manifest.** The
knot is `peer_inference.rs` (5,399 lines, 38% of Serving's mesh lines) and, api-side,
`admission` + `routes_inference` + `routes_responses`.

The anticorruption layer `DOMAINS.md` §5 asks for **already exists and is already the
only door**: `PeerEndpointSource` (`sovereign-mesh/src/peer_inference.rs:337`) plus the
`MemberRecord → PeerInferenceEndpoint` translation at `sovereign-mesh/src/daemon.rs:2286-2345`
— nine fields, three predicates, one transport call, sixty lines. Phase C does not invent
that seam; it renames it and moves it to the Fabric side.

## The two tiers

**Package crates** (`[[package]] name = "serving"`, `doc = "sovereign/SERVING_BOUNDARY.md"`):

| Crate | Lines | Role |
|---|---:|---|
| `sovereign-scheduler` | ~6,800 | **Arithmetic over the published language.** `scheduler_core`, `oicp_select`, `predicted_time`, `tier`, `decision_log`, `decision_replay`, `decision_trace`, `throughput_tracking`, `slot_aliases`, `yield_backoff`. No I/O, no clock read, no interior mutability. |
| `sovereign-serving-host` | ~11,000 | **The ports and the knot.** `peer_inference`, `inference_adapter`, `oicp_synthesis`, `guest_lender`, `pinned_worker_source`, `entry_endpoint`, plus `sovereign-api`'s `admission`. Opens connections, holds the HTTP surface, receives every candidate through a port. |
| `serving-policy` | 1,241 | Already exists, already tier-0, ZERO in-repo deps. `fair_sched` left `commonwealth-core` 2026-09-03; two `[[forbid]]` rows pin it both ways (`quality/ARCH_LAYERS.toml:381-389`). **The precedent Phase C copies.** |
| `sovereign-serving` | 720 → 0 | The peg: eleven exported types with zero external references, plus 771 lines of shard assignment that drag `corpus-engine`. Emptied by rung 9. |

**Shared leaves the package may reach — `oicp-types`, `kernel-types`,
`sovereign-contracts`, `serving-policy`. Nothing else.** `oicp-types/src/scoring.rs`
already holds the scorer (`ScoredClaim` :438, `pick_better` :458,
`best_claim_for_request` :481, `SCORING_EPSILON` :431); `oicp_select.rs` is a shim over it.

**Grandfathered `[[exception]]` rows — exactly two, each with a `tracking` burn-down.** A
third means the boundary is drawn in the wrong place (K4).
`sovereign-serving-host → sovereign-inference` (`RemoteApiProvider`, `peer_inference.rs:64`)
clears when the remote provider is reached through `oicp-client`;
`sovereign-serving-host → commonwealth-core` (`PeerHealthTracker`, `ids::NodeId`) clears
when quarantine state is the host's own and identity is `kernel_types::NodeId`.
`sovereign-scheduler → sovereign-core` is **zero from day one**: its only non-`oicp` uses
are `traits::InferenceProvider` and `types::Speed` (`oicp_select.rs:25-26`), and the
former is already a leaf at `sovereign-contracts/src/traits.rs:281`.

## The rules

1. **The scheduler may name `oicp-types` and nothing else in this repo** — a ranker that
   names the mesh foundation cannot rank a candidate that is not a mesh member.
2. **Not `sovereign-mesh`** — the edge Phase C exists to delete; it is why a CLI wanting a
   roster inherits llama-cpp (`DAEMON_CORE.md` §4).
3. **Not `sovereign-inference`** — ranking is not executing; only the host connects.
4. **Not `sovereign-api`** — `prompt_compactor.rs:48` has the one such import today, and
   wire types belong to `oicp-types` or the host, never the ranker.
5. **The host may name inference; it may NOT name `sovereign-mesh`** — it receives
   candidates through a port, never an `EmbeddedDaemon`; a dep here is the kill clause.
6. **No published noun wears a local alias** — `oicp_select.rs:32-34` renames three
   `oicp-types` symbols with `as`, so `grep SCORING_EPSILON` misses the scheduler entirely
   (ARCH §8, defeated by `as`); deleted, not relocated.
7. **The rules count dev- and build-dependencies too** — whoever lifts the package carries
   its tests.

### The five entries the package publishes

**(a) Two supplier ports, not one** — the roster port loses two Fabric leaks; the guest
lookup is a different question, so it stays a different port.

```rust
// sovereign-scheduler — was PeerEndpointSource, peer_inference.rs:337-371.
#[async_trait]
pub trait VenueSource: Send + Sync {
    /// Everything routable right now. No filtering, ranking or ordering
    /// guarantee: the scheduler does all three.
    async fn candidates(&self) -> Vec<Venue>;
}
// OFF the port: local_node_id() (:348) is a process constant -> a constructor arg
// typed kernel_types::NodeId, burning exception #2 down. ledger_emission_for() (:365,
// #[doc(hidden)] — the tell) -> the host mints emissions from RoutingOutcome instead.
// Serving emits facts; Fabric prices them.
// sovereign-serving-host — a PIN, not a candidate. NamedModelLocation (:3104) has
// separate Peer(..)/Guest(..) variants; :3116 says "Not a scoring outcome ... a PIN".
#[async_trait]
pub trait GuestLenderSource: Send + Sync + std::fmt::Debug {
    async fn lender_for(&self, model_id: &str) -> Option<GuestLender>;
    async fn posture(&self) -> GrantPosture;   // Unusable is NOT NoLink
    async fn invalidate(&self);                // the lender returned 401
}
```

**(b) The scheduler entry** — free-standing over an assembled snapshot, not the
`route(&self, req, candidates)` the order sketched.
```rust
// sovereign-mesh/src/scheduler_core.rs:339 — pub(crate) today, becomes pub.
pub fn rank(rec: RoutingDecisionBuilder, inputs: RankInputs<'_>) -> RankResult;
pub struct RankInputs<'a> {          // :267
    pub now_unix: u64,               // PASSED, so a sim runs on virtual time
    pub oicp_request_id: &'a str,
    pub req: &'a InferenceRequirements,
    pub needs_forced_choice: bool,
    pub objective: RankObjective,    // Product (prod) | PredictedTime (§4.1)
    pub tier_floor: TierFloor,       // PASSED, not derived — :293-296
    pub local: SelfView<'a>,
    pub candidates: &'a [VenueView],
}
pub struct RankResult {              // :322
    pub ranked: Vec<RankedVenue>,   // best-first
    pub tie_band: Option<usize>,     // None under Product: no scale, no ties
    pub decision: RoutingDecision,
}
// Three properties survive the move verbatim or lift step 7 is worthless: purity
// (:336-338), now_unix passed not read, and objective/tier_floor passed not derived
// — :293-296 refused the derivation because a derived floor "would apply everywhere
// the moment it compiled, and no before/after would exist to price it".
// local_sentinel :99, beats_local :117, winners_over_local :154 stay pub(crate):
// exposing them invites a second copy of the strictly-beats-local filter.
```

**(c) The admission entry** — the decision separated from the middleware, over a decider
that already exists in a tier-0 leaf and is not re-minted (ARCH §11).
```rust
// sovereign-serving-host, over serving-policy/src/fair_sched.rs :244 SchedCore<K> ·
// :448 try_grant(key,weight,cap) -> :198 TryGrant{Granted, WouldQueue{position}, Shed}.
// Principal::Local (no X-Node-Id) is ALWAYS admitted: the user's own chat must never 503.
// `Principal` is Admission's word (DOMAINS.md §10.1) and the key of DAEMON_CORE §1's table.
pub enum Principal { Local, Member { node: NodeId, raw_header: Option<String> } }
pub trait Admission: Send + Sync {
    /// Pure over the snapshot: no axum, no AppState, no clock read.
    fn admit(&self, who: &Principal, now_unix_ms: u64) -> AdmissionVerdict;
    fn posture(&self) -> AdmissionPosture;  // Paused|ForegroundYield|Ceiling|Open
}
pub fn shed_response(r: AdmissionRejection) -> Response;               // :211
pub fn local_queue_shed_response(pos: u32, wait_ms: u64, retry: u64);  // :199
// peer_admission_layer (:551) and client_fairness_layer (:457) stay in the HOST
// as ~20-line axum adapters and are NOT package surface.
```

**(d) The decision record and the replay entry.**
```rust
// sovereign-scheduler::routing_decisions (was sovereign-mesh/src/decision_log.rs)
pub const DECISION_LOG_SCHEMA: &str = "oicp-decision/v1";        // :64
pub const DECISION_LOG_ENV:    &str = "SOVEREIGN_DECISION_LOG";  // :68
pub struct RoutingDecision { /* :216 — candidates, excluded, verdict */ }
pub struct RoutingOutcome  { /* :577 — joins on decision_id */ }
pub trait  RoutingDecisionSink: Send + Sync + std::fmt::Debug { /* :617 */ }
pub fn emit_decision(sink: &Arc<dyn RoutingDecisionSink>, d: RoutingDecision); // :1068
pub fn emit_outcome (sink: &Arc<dyn RoutingDecisionSink>, o: RoutingOutcome);  // :1073
// sovereign-scheduler::replay (was sovereign-mesh/src/decision_replay.rs)
pub fn replay_decision(d: &RoutingDecision) -> Result<DecisionReplay, SkipReason>; // :342
pub fn replay_decisions<'a, I>(decisions: I) -> ReplayReport;                      // :458
pub fn replay_trace(t: &SchedulerTrace) -> ReplayReport;                           // :517
// Three strings do NOT move with the code: the two constants above (every runbook
// arms the env var; the tag is on jsonl already written) and DECISION_TRACE_TARGET
// = "mesh.decision" (:73), matched by RUST_LOG filters. The last stops describing
// where the code lives — a comment to add, not a constant to change.
```

**(e) What `sovereign-cli-daemon`'s eight sites call** — eight production sites in one
crate; `sovereign-core` has zero, its seam being `InferenceProvider`, already a leaf at
`sovereign-contracts/src/traits.rs:281`.
```rust
// provider.rs:111              dyn sovereign_mesh::peer_inference::PeerEndpointSource
//                          ->  dyn sovereign_scheduler::VenueSource
// provider.rs:117,:128,:136    with_peer_source{,_and_publisher}(raw, src[, publisher])
//                          ->  InferenceRouter::builder(raw).candidates(src)
//                                  .in_flight(publisher).node_id(id).build()
// bootstrap.rs:1151,:1203,:2202  Arc<MeshInferenceProvider>
//                          ->  Arc<sovereign_serving_host::InferenceRouter>
// bootstrap.rs:2301-2308       CompositeEndpointSource::new(daemon as Arc<dyn ..>, ..)
//                          ->  CompositeVenueSource::new(daemon_port, pinned_port)
// build/inference.rs:130       EntryNodeEndpoint::parse(mesh as Arc<dyn ..>, &hex)
//                          ->  EntryNodeEndpoint::parse(mesh_port, &hex)
```

The constructor pair exists only because a reload must not mint a fresh publisher
(`provider.rs:95-106`); the builder collapses it, and the `node_id` removed from the port
in (a) enters here. `daemon_port` is an adapter the **host** writes: the `EmbeddedDaemon`
cast is the knot, and if it survives the rung has failed.

### The words, and who keeps each

Adjudicated 2026-09-14; rows, prices and evidence are the `[[collision]]` rows with
`context = "serving"` in `quality/DOMAINS.toml`, argued in `quality/DOMAINS.md` §10.4.

| Family | Verdict | Survivor | Rung |
|---|---|---|---|
| `Tier` ×3 + `TierFloor` | split | `TierFloor @ sovereign-scheduler` | domains-9 |
| `Routing*`/`LoadPolicy`/… | delete (13 types, 0 external refs) | `RoutingDecision` + `RankObjective` | domains-9 |
| eight "candidate" nouns | converge the word, split the role | `Venue` + `oicp_types::ScoredClaim` | domains-5 |
| four "admission" deciders | split | `Admission* @ sovereign-serving-host` | domains-10 |
| two `decision_log`s | split | `decision_log` (tool id) @ recipe-author | domains-10 |
| `GuestLender*`/`GrantPosture` | split | `GuestLender @ sovereign-serving-host` | domains-10 |
| `frontdoor.rs` | Host, confirmed (two callers, 23 sites) | stays in `sovereign-api` | domains-4 |
| the public interface | converge | the five entries above | domains-10 |

## What a green gate does not prove

A clean dependency closure is not a clean lift — the caveat `studio/BOUNDARY.md` earned
by performing one: its gate was green while `sovereign-contracts` embedded a file from
outside its crate root and the sandbox had to preserve the monorepo's directory shape to
compile. The way to know Serving carries no such embed is to lift it. Nor does the gate
prove the package **routes**: `boundary-gate` reads manifests and cannot tell a scheduler
that ranks from one that returns the first candidate. Until `serving-lift.sh`'s later
steps stop abstaining, green means "the edges are legal".

**And it proves nothing about answer quality** — neither gate runs a model against a
question bank. `svrn quality check --lane routing` is that instrument; rung 10 owes it.

## What is enforced, and what is not

**Tier 1 — the declaration, blocking on every push.** `boundary-gate` reads the manifest
graph (normal + dev + build edges, `build.rs`, `include_str!` escapes); `layer-gate` reads
the `[[forbid]]` table, which since 2026-09-09 outranks package membership and the
shared-leaf allowance alike. This stops an edge being acquired BETWEEN lifts.

**Tier 2 — the physical lift, enforced by a bar or by nobody.**
`scripts/serving-lift.sh --sandbox` copies the closure outside this repository, synthesises
a workspace, and builds, tests and RUNS it against N stub OpenAI endpoints: 429 with
`Retry-After` on the K+1th, replay reproducing every decision, positive and negative
controls, and a decider guard. `commonwealth` learned the lesson this section records —
both its lift scripts lost their only caller when a campaign closed and nothing went red.
**Name the bar that calls this one, or it is inventory the day it is written.** One
honesty note it must carry: `replay_decision` assumes `RankObjective::Product`
(`scheduler_core.rs:72-76`), so the replay step abstains by name rather than pass.

**Tier 3 — the remainders, enforced by nothing and therefore named here.**

- **The `MeshPlan` cascade.** Deleting `RequestRouter`, `SchedulingStrategy`, `NodeRole`
  and `PlanTrigger` removes 4 of `MeshPlan`'s 7 fields (`sovereign-serving/src/plan.rs:22-33`).
  the registry's `[[peg]]` row listed `MeshPlan` live with 6 refs (corrected 2026-09-14); those are one import plus
  three helpers (`simulated_mesh.rs:195`, `:202`, `:209`) which — with
  `store_adapter.rs:141/:149/:160/:169` — **have zero callers repo-wide.** Taken literally,
  the order leaves a four-field husk behind.
- **The `GateReason` salvage.** `Verdict::Gated { gate: String }` (`decision_log.rs:509`)
  is a closed set living as free text inside a serialized schema (ARCH §9) — the shape
  `UnavailableReason` had and the live model lost. Mint it with
  `#[serde(rename_all = "snake_case")]` so known values stay byte-identical and an
  `Other(String)` arm so old jsonl still parses. **Rung 11, not 9** — a wire-schema change
  must not ride a deletion commit (ARCH §2); sweep every construction site first.

## When the gate fails

Declare the edge or delete it. `[[exception]]` rows carry `package = "serving"` and a
`tracking` burn-down condition; they are a counted ledger, and a stale one — the edge is
gone — fails the gate until it is deleted. Exactly two are grandfathered. A third means the
boundary is in the wrong place: move the line, do not widen the ledger. Removals are the
celebration.
