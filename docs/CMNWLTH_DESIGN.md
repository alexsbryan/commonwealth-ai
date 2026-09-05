# cmnwlth — a general compute rail, extracted from the mesh

Status: DRAFT (exploration, 2026-08-16). Nothing here is implemented; this
document exists to pressure-test an ontology before any code moves.

## Thesis

The Commonwealth mesh as shipped today is a general-purpose compute-sharing
substrate wearing two specific costumes: OpenAI inference requests and
corpus-shard ingest. Roughly 85k LOC of transport, discovery, gossip, leasing,
grants, capability scoring, pinned-pod bootstrap, and advisory coordination is
already work-agnostic; only the *work vocabulary* is LLM-shaped. This document
defines the general layer — **cmnwlth** — as an independent library with zero
LLM/corpus dependencies, and treats the existing system as its first consumer
and its conformance evidence.

One sentence: **a ring of mutually-trusting members that advertise
capabilities and interact through three shapes — calls, jobs, and sessions.**

## The ontology — seven nouns, three shapes

### Membership plane

| Noun | Contract | Existing instance |
|---|---|---|
| **Ring** | A founded trust group: join key (BLAKE3, shared out of band) + Ed25519 proof-of-possession; membership converges by gossip; social trust, not cryptographic attestation. | `sovereign/crates/sovereign-mesh/src/join.rs`, `src/gossip.rs`, `commonwealth/crates/commonwealth-discovery/` |
| **Member** | A node in the ring. Durable (a machine you own) or **ephemeral** (a rented machine with a TTL and a Provider that minted it). Ephemeral members bootstrap with seed-derived keys and a cert thumbprint known to the owner *before* boot — no TOFU window. | `sovereign-mesh/src/worker_pod.rs` (`BootstrapBlob`), `worker_controller.rs` (`WorkerProvider`) |
| **Grant** | TTL'd, allowlisted authorization for a member to participate in a named scope. Dual-enforced (at enrollment and at lease), dual-teardown, never a standing share. | `commonwealth/crates/commonwealth-knowledge/src/ingest_grant.rs` (`EphemeralGrantStore`, corpus-typed today) |

### Capability plane

| Noun | Contract | Existing instance |
|---|---|---|
| **Claim** | What a member advertises it can do: a typed capability descriptor plus live load/health observations. Additive; absence of a claim is a veto, never a default. | OICP manifests (`/oicp/v1/capabilities`), hardcoded to inference vocabulary |
| **Selector** | Scored choice among claimants for a given need, with a glassbox decision log. Local wins ties; fallback to local on any error. | `oicp-types/src/scoring.rs` + `sovereign-mesh/src/peer_inference.rs` (`MeshInferenceProvider`), keyed on `InferenceRequirements` |

### Work plane — three interaction shapes, not one

| Shape | Contract | Existing instances |
|---|---|---|
| **Call** | Synchronous, budgeted, possibly streaming request/response. Not leased, not verified by the rail — the caller judges the response and degrades gracefully on peer failure. | Chat/embed routing; federated knowledge search (3s/peer budget, scatter-gather); model blob fetch |
| **Job** | Asynchronous leased work: submit → lease → heartbeat → complete, with a reaper, max-attempts, and requeue on lease expiry. Verification is a consumer-supplied policy, run by the submitter. Envelope: `{ unit_id, kind, payload }`. | Peer-assisted ingest (`commonwealth-knowledge/src/work_queue.rs`); worker-pod dispatch (`sovereign-mesh/src/worker_http.rs` — the envelope already exists here, generic, with only `kind = "chat"` wired) |
| **Session** | A long-lived coupled circuit between members with latency/affinity requirements the lease model cannot express. **Named in the contract, deferred in v1.** | Tensor-split RPC (raw TCP, `:50052`) is the motivating edge case; the iroh `HttpBridge` per-(peer, ALPN) tunnel is session-shaped plumbing that already exists |

Cross-cutting, already general, adopted as-is: the transport seam
(`commonwealth-transport/src/lib.rs` — `PeerTransport` + `TrafficClass`
routing every peer conversation), replicated state
(`commonwealth-state::MeshStore`), and advisory coordination
(`sovereign-work-atlas` claims / `resource_may_i`).

## What the library deliberately does NOT own

- **Verification implementations.** The rail defines the seam
  (`verify: fn(&Job, &Artifact) -> Verdict`, submitter-side); consumers supply
  it. Ingest's cosine re-embed sample check
  (`commonwealth-knowledge/src/shard_manager.rs::verify_merge_sample`) stays
  in ingest as its impl.
- **Sandboxing.** The rail defines an `Executor` trait keyed on job `kind`.
  Isolation tiers (WASI, jailed native, GPU) are executor impls, shipped
  separately and opted into per-node — mirroring the corpus custody flags:
  a node that has not declared an executor for a kind structurally cannot be
  selected for it.
- **All LLM/corpus vocabulary**: models, slots, recipes, grounding, OICP's
  inference-specific claim fields.
- **Session's implementation** (v1 names the shape only).
- **Cryptographic result attestation.** Trust remains social + policy-verified,
  as the mesh's threat model already states plainly.

## Use cases

Each use case is annotated with the nouns it exercises and — the point of the
exercise — what it *stresses* in the ontology. UC1–UC4 exist today and are
conformance evidence. UC5–UC6 are planned consumers. UC7–UC10 are deliberately
non-LLM hypotheticals: their job is to keep the abstraction honest, and none
of them may require a new noun to work.

### UC1 — Chat inference routing (exists)
Shape: **Call**, chosen by **Selector**. A member's envelope allows offload;
the scorer ranks local + online peers' claims; highest wins, local wins ties;
streaming response; fall back to local on any error.
Stresses: the Selector's need/claim vocabulary must generalize beyond
`InferenceRequirements` without losing the decision log or the
downgrade-vs-declined-upgrade distinction (`sovereign-mesh/src/tier.rs`).

### UC2 — Federated knowledge search (exists)
Shape: **Call**, scatter-gather with a per-peer budget. Peer answers only if
its claim permits (`query_sharing`); peer offline degrades to local, never
breaks.
Stresses: Calls need a *fan-out* form with partial-result semantics, not just
point-to-point. Budget is a first-class Call parameter.

### UC3 — Peer-assisted ingest (exists)
Shape: **Job** under a **Grant**. Coordinator gossips a handoff pointer;
peers lease shard-index units, heartbeat, complete; coordinator pulls,
merges, and verifies by re-executing a sample locally; eviction broadcast +
peer self-eviction on grant expiry.
Stresses: (a) the queue must stay coordinator-local (pull-based) because
gossip-replicated state cannot give linearizable leases — the rail must not
pretend otherwise; (b) verification-by-sample-replay is submitter-side and
consumer-defined; (c) Grants bound *participation*, Jobs bound *work* — two
nouns, not one.

### UC4 — Cloud burst via rented GPU pod (exists, operator-driven)
Shape: ephemeral **Member** minted by a Provider. Owner mints a bootstrap
blob (seed, signed token, upload manifest, owner key); pod self-signs a cert
the owner can pin before boot; pod joins as a first-class peer and serves
Calls; destroyed on TTL/idle.
Stresses: burst is a *membership* property plus a *selection* policy (a score
floor or queue-depth trigger that mints a member), not a work-plane concept.
The rail must treat a 40-minute-old Vast.ai pod and a five-year-old desktop
identically once both are members.

### UC5 — Distributed test sharding (planned; first LLM-free consumer)
Shape: **Job**. Shard a large test suite (`cargo nextest` partitions) across
ring members that claim the right toolchain; collect JUnit shards; merge.
Verification: rerun a failed shard locally before believing it (failure
replay), trust green shards.
Stresses: (a) Claims need a platform/toolchain vocabulary (arch, OS,
toolchain hash) with nothing LLM-ish in it; (b) job inputs are *references*
(a git rev both sides can materialize), not payload bytes — the envelope must
carry content-addressed refs, not just inline JSON; (c) asymmetric
verification policy (verify failures, trust successes) must be expressible.

### UC6 — Distributed solve: sandboxed TDD candidate evaluation (planned)
Shape: **Job**, requiring an isolating **Executor**. `sovereign-tdd` today
is purely local; the expensive inner step (apply candidate edits, run the
test command) becomes a job whose `kind` demands a sandboxed executor,
because the code being run is model-generated and untrusted by construction.
Stresses: this is the use case that forces the executor-tier story — a
member that advertises no sandbox tier is structurally ineligible; silent
downgrade to unsandboxed execution must be impossible by type, not by
discipline.

### UC7 — Home render/transcode farm (hypothetical)
Shape: **Job**. Blender frames or HandBrake segments across the ring;
GPU-claiming members preferred; per-frame artifacts pulled back by the
submitter.
Stresses: (a) GPU capability claims outside any inference vocabulary;
(b) large artifact return paths (the resumable ranged-fetch machinery built
for 30GB GGUF pulls generalizes here); (c) progress as a first-class job
event stream, not just lease heartbeats.

### UC8 — Nightly ETL / batch pipeline (hypothetical)
Shape: **Job** with dependencies between kinds (extract → transform → load).
Stresses: whether the rail needs a DAG. Position: **no** — dependency
ordering stays consumer-side (the submitter submits stage N+1 when stage N
verifies); the rail carries jobs, not workflows. If a real consumer proves
this wrong, that is an ontology revision, recorded here.

### UC9 — Game server / collaborative desktop tunnel (hypothetical)
Shape: **Session**. A member hosts a stateful low-latency service; another
member holds a pinned circuit to it for hours.
Stresses: validates that Session is a real third shape and not tensor-split
special pleading — two independent use cases (UC9 and tensor-split) with the
same contract needs (affinity, lifetime coupling, latency floor, no
lease/retry semantics) justify the noun. Still deferred in v1.

### UC10 — Ring-wide advisory locking for a shared NAS (hypothetical)
Shape: none — **coordination plane only**. Members declare scopes over file
paths on a shared volume; `resource_may_i`-style verdicts (held / expired /
free) with tombstones distinguishing "abandoned" from "never taken".
Stresses: proves the coordination plane is independent of the work plane —
a consumer can use cmnwlth for coordination alone, with no Calls, Jobs, or
Sessions at all.

## Trait sketch (v1 surface, ~terse)

```rust
// cmnwlth-core — types and seams only; no transport, no executors, no verifiers.

pub struct MemberId(pub [u8; 32]);            // Ed25519 verifying key
pub struct ProgramRef(pub blake3::Hash);       // content-addressed, kind-scoped

pub struct Job {
    pub unit_id: u64,
    pub kind: String,                          // open set: registry, not enum (ARCH_PRINCIPLES §2/§4)
    pub payload: serde_json::Value,            // inline data or CAS refs
    pub needs: Needs,                          // cpu/mem/gpu/platform/deadline
}

pub trait Claimant {                           // capability plane
    fn claims(&self) -> Vec<Claim>;            // additive; absence is a veto
}

pub trait Selector {
    fn choose(&self, need: &Needs, claimants: &[(MemberId, Claim)]) -> Decision;
    // Decision carries the full scored trace — glassbox, always.
}

pub trait CallRail {                           // sync work
    async fn call(&self, to: MemberId, class: TrafficClass, req: Request, budget: Budget)
        -> Result<Response>;
    async fn scatter(&self, to: &[MemberId], req: Request, budget: Budget)
        -> Vec<(MemberId, Result<Response>)>;  // partial results are the contract
}

pub trait JobRail {                            // async leased work
    async fn submit(&self, jobs: Vec<Job>, grant: GrantId) -> HandoffId;
    async fn lease(&self, handoff: HandoffId) -> Option<Lease>;      // pull-based, coordinator-local
    async fn heartbeat(&self, lease: &Lease) -> Result<()>;
    async fn complete(&self, lease: Lease, artifact: Artifact) -> Result<()>;
}

pub trait Executor {                           // member-side; per-node opt-in per kind
    fn kinds(&self) -> &[String];
    fn tier(&self) -> ExecTier;                // Wasi | JailedNative | TrustedNative | Gpu
    async fn run(&self, job: &Job, cap: ResourceCap) -> Result<Artifact>;
}

pub type Verifier = dyn Fn(&Job, &Artifact) -> Verdict + Send + Sync;  // submitter-side, consumer-supplied

pub trait Provider {                           // mints ephemeral members
    async fn mint(&self, spec: &MemberSpec, bootstrap: BootstrapBlob) -> Result<PendingMember>;
    async fn destroy(&self, member: MemberId) -> Result<()>;
}
```

Session is intentionally absent from the v1 trait surface.

## Boundary and enforcement

The separation is structural, not aspirational:

1. **Layer rule** in `quality/ARCH_LAYERS.toml`, enforced by the existing
   `cargo xtask quality` gate: no `cmnwlth-*` crate may depend on any
   sovereign/LLM/corpus crate. This lands in the same commit as the first
   crate.
2. **Conformance by consumption**: the existing mesh adopts the library's
   types; the instantiation table above is regenerated against real impls as
   phases land.
3. **A second consumer that isn't us** (Phase 4) keeps the first honest: an
   example binary using only `cmnwlth-*` crates, zero sovereign imports.

## Phases

| Phase | What lands | What gets DELETED |
|---|---|---|
| 1 | `cmnwlth-core`: the trait surface above + the promoted Job envelope + layer rule + this doc moved in as the crate's spec | — (additive; smallest possible) |
| 2 | Already-general crates (`commonwealth-transport`, `-discovery`, `-state`, `sovereign-work-atlas`) implement core traits; worker-pod path switches to the promoted envelope | duplicated envelope/manifest types in `sovereign-mesh` |
| 3 | `LeaseQueue<J>` extracted from `work_queue.rs`; `GrantStore<S>` from `ingest_grant.rs`; `Need`/`Claim` traits with `InferenceRequirements` as first impl | corpus-specific lease lifecycle, grant plumbing; net-simplification is the gate — if the sovereign-side diff does not shrink, the abstraction is wrong and we stop |
| 4 | UC5 (test sharding) or a UC7-class demo as the LLM-free consumer | — (proof, not product) |

Physical layout: crates under `cmnwlth/` (this directory), separable into an
independent repo later. `commonwealth/crates/{transport,discovery,state}` are
not physically moved until the Phase 3 API settles — boundary-by-layer-map
first, `git mv` as a mechanical final step.

## Hard problems, and the positions this design takes

These are commitments, not caveats (2026-08-16). The social trust ring does
the heaviest lifting: it buys us out of Byzantine members, incentive design,
and Sybil resistance entirely — those problems return only if the ring opens.
What remains is physics and OS reality, at n=6 peers as at n=10,000:

- **Result trust.** Every verification scheme has a cost structure that fails
  somewhere (re-execution doubles work, quorum multiplies it and needs
  determinism, SNARKs/TEEs cost or trust too much). Position: social ring +
  consumer-supplied verifiers — which means the rail is only as general as
  the set of workloads with cheap verifiers or trusted members. Named ceiling,
  accepted.
- **Sandbox–capability tension.** The most valuable jobs (GPU) are the least
  sandboxable, and `JailedNative` may be unimplementable on macOS members.
  Position: tier eligibility is a structural selection fact; silent downgrade
  to a weaker tier is impossible by type. The WASI tier ships CPU-only and
  says so.
- **Determinism.** Cross-arch floats (ARM vs x86) and GPU reduction order
  break exact replay everywhere except pinned-engine WASI CPU jobs. Position:
  for all other tiers the correct verification form is tolerance-banded
  comparison — `verify_merge_sample`'s cosine check is the general pattern,
  not an embedding-specific hack.
- **Stale self-reported claims.** Manifests cache 60s, gossip converges in
  ~10s, load changes in milliseconds; push-scheduling on that picture herds.
  Position: the Job rail is pull-based (workers take work when ready) — a
  load-bearing property of UC3, kept deliberately. Calls stay exposed via the
  Selector; mitigation is EWMA observation feedback plus fall-back-to-local.
- **At-least-once is the truth.** A reaped lease plus a slow-not-dead worker
  runs work twice; distributed leases cannot give exactly-once. Position: Job
  consumers must be idempotent, verified-then-deduplicated, or keep side
  effects off the rail (why UC8's DAG stays consumer-side).
- **Data gravity.** Inputs outweigh programs; mature systems move jobs to
  data, which re-imports the custody questions the corpus flags answer. The
  CAS open question below is this problem.
- **Burst economics.** Pod boot is minutes, so bursting pays only if the
  queue stays deep that long — and the orphaned pod is the one failure that
  costs money silently. Position: destruction is enforced pod-side
  (self-destruct on TTL), never owner-remembered.
- **Version skew.** A ring upgrades unevenly; every wire type and every job
  `kind` is a compatibility contract forever — same serde
  defaults/aliases/deprecation discipline the recipes already carry.

## Open questions (revisions land in this section, dated)

- **Quorum verification**: deliberately omitted. On a 6-peer social-trust
  ring, sample-replay + consumer checks likely suffice forever; quorum is a
  low-trust primitive on a high-trust substrate. Revisit only if a consumer
  with an actual adversary shows up.
- **CAS for job inputs/artifacts**: UC5/UC7 want content-addressed refs. Does
  the existing model-blob transfer path generalize, or does the rail need its
  own small CAS seam?
- **Call/Job unification**: a Call could be modeled as a degenerate
  zero-lease Job. Rejected for v1 — the failure semantics differ (degrade vs
  retry), and collapsing them buys one fewer trait at the cost of both
  contracts getting vaguer.
- **Session**: two use cases now justify the noun (tensor-split, UC9). What
  is the minimal v2 contract — pinned circuit + lifetime + latency class?
