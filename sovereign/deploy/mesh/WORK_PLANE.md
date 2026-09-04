# The work plane — design

**Status:** DRAFT design, nothing landed. Written 2026-09-04 from an operator-directed
exploration ("commonwealth as the rails for a general distributed compute mesh — for
scientific researchers losing HPC access, and for arbitrary workloads"). This is the
design deliverable [`PLAN.md`](PLAN.md) Phase 3 was gated on ("design-gated after Phase 2
starts"), and it answers PLAN.md open question 7.

**Evidence discipline:** as PLAN.md — every load-bearing claim about current code was
verified at file:line during the design session and is cited inline. Design assertions
are marked *(design)*. The pilot library (separatrix) is external to this repo; its
properties are cited from its own manifest and study files, read 2026-09-04.

**Compass:** §2 closed sets are enums, open sets are registries · §7.5 identity from
essence · §10.6 one decider, one name · §11 cite, don't recall · §18.3 never silently
substitute · §19 the inventory outranks the plan.

---

## Mission

> **Any member's workload runs anywhere a member has consented to run it — and the
> mesh's own inference is the first such workload.**

Two test sentences, after PLAN.md's pattern:

1. **Consent is mechanical.** A job runs on a node only under a grant that node issued,
   for a kind that node offers; everything else is a typed refusal at lease time —
   never a silent best-effort.
2. **Self-hosting.** Standing up the fleet's shared model is itself a job on this plane.
   The mesh is its own first customer, provably, with receipts.

The user story this serves: researchers who already trust each other pool the machines
they have — laptops, lab boxes, a rented GPU or two — and run sweeps, simulations, and
batch work overnight instead of losing it to a dead allocation. The north star remains
PLAN.md rung 10 (consortium compute exchange); this design is the rungs 4–7 machinery
with rung 9 (services) arriving as a consequence rather than an extension.

## The axiom: two tiers, never one

The single load-bearing decision, which a naive "everything is a job" gets wrong:

| | Control plane (jobs) | Data plane (never jobs) |
|---|---|---|
| Question | *where does something stand, for how long, on whose grant* | *how does traffic reach the standing thing* |
| Today | ingest handoffs; model warm + slot lifecycle; rented-pod workloads; bench trials | decode turns; retrieval fan-out; artifact fetch |
| Noun here | lease + kind + requirements + placement + provenance | request/response over the fabric |

A decode turn is not a job and must never become one — it is a millisecond-class routing
decision against a live table, and the job plane leases in seconds-to-minutes. Squashing
the tiers contaminates both (TTFT dies in a queue sweep; the envelope bloats with latency
concerns). The correct mapping of the inference plane is two jobs — `model-warm` (a
placement-constrained task) and `model-serve` (a **long lease with health and restart
policy** — PLAN.md rung 9's exact words) — plus traffic to that lease on the data plane.

## The contract — six nouns, one deliberately-not-noun

**1. `JobSpec` — the unit of submission (REUSED, not minted).** The name and most of the
shape already exist at `sovereign-mesh/src/worker_controller.rs:98` — the rented-pod
controller's spec: container image ref, uploads validated by owner-signed sha256
(`UploadFile`/`UploadSource`, `worker_controller.rs:127` — "the URL is trusted transport,
not trusted source"), a work-queue manifest of `WorkUnit { unit_id, kind, payload }`
units (`worker_http.rs:81`), and opaque runner config. *(design)* The mesh job plane
generalizes THIS type — adding `principal`, `grant_ref`, `requirements`, and provenance
hooks — rather than minting a parallel noun; it is promoted to `oicp-types` with the fold,
on the `TenantId` precedent (layer map forces contract nouns to the shared leaf).

**2. `JobKind` — the contract between submitter and donor.** *(design)* A kind is **code
the executing node has**: an open registry across the ecosystem, closed per node — a
node runs only what it has registered a `JobExecutor` for, and an unknown or
wrong-version kind is a typed refusal at lease time. This is the `EmbedModelMismatch`
pattern (`commonwealth-knowledge/src/work_queue.rs`, `QueueError`) generalized from embed
models to all work. Kind ids carry versions. The first kinds: `ingest` (today's closed
`WorkUnit` enum at `commonwealth-core/src/knowledge.rs:328` becomes just the `ingest`
kind's payload — it stays closed, correctly, because it is closed in the world), and
`oci` (below). **This answers PLAN.md open question 7: a `JobKind` envelope above
`WorkUnit`, not an extension of `WorkUnit` in place.**

**3. `WorkOffer` — the node-side sharing declaration.** *(design)* Named apart from
`sovereign-pipeline`'s `Offer` (`pod.rs:54`, an unrelated pipeline noun — convergence
check 2026-09-04; the rename-apart is deliberate). Config-as-data, gossiped like other
capability state: schedule (hours / idle-only), concurrency budget, the set of kinds
offered, advertised runtime and hardware (arch, GPU), preemption consent. This is the
HTCondor classad half, and it is the concrete form of PLAN.md rung 7's "yield policy
unstated" — the WorkOffer IS the statement.

**4. `JobExecutor` — the seam on the donating node.** *(design)* Named apart from
`sovereign-core`'s `Executor` (`executor.rs:230`, the workflow engine). A trait +
registry mirroring `ToolRegistry` (descriptor + payload schema + execute — the proven
shape in this codebase). Runs inside a supervised child from the existing
`sovereign-compute` supervisor (crash isolation, kill -6 containment proven live);
streams progress; emits artifacts. Its descriptor carries an **`isolation` claim** — v0
it names the runtime (rootless container, etc.); later, VM-grade options — so a WorkOffer
can require isolation levels without seam changes.

**5. `Grant` — submitter↔donor consent (REUSED).** The ephemeral-grant machinery already
exists (allowlist + TTL + revoke, enforced at lease with 403 — `PeerNotAllowed`,
`work_queue.rs`; `grant_store` read in `corpus_queue.rs:770`). *(design)* One extension:
grants name principals, never groups, and become per-kind. The grant carries a scoped
**job-token** the executor presents on the researcher's behalf for inference calls —
principal forwarding solved with the same artifact (§10.6: one artifact, two duties,
one owner).

**6. `Placement` — matchmaking.** *(design)* `requirements ∩ capabilities ∩ WorkOffer ∩
grant`, ordered by fair share when contended. A new decider that copies two proven
neighbors: `rank()`'s typed exclusions (`peer_inference`) and `SchedCore`'s deficit
ordering for who is next. Requirements must be able to describe a **placement graph**,
not just one node — a distributed model is one job whose payload is a plan (block
ranges, host-last), and the plan artifact already exists (`compute-distribution/`
handoff files). Kinds own their placement scorer; the measurement discipline
(`MeasurementKey`, medians, near-misses travel with the median — `mesh_measurements`)
is the scorer pattern for any kind, not just tok/s.

**The deliberately-not-noun: results.** Artifacts stream back to the submitter over the
existing request/response fabric; the executor records a custody event; the ledger
credits the donor. `MeshStore` stays what it honestly is — a cache (PLAN.md rung 8,
deferred). Storage-as-a-lane is not smuggled in here.

## The payload contract: OCI, adopted not invented

Sandboxing and environment portability are solved problems with boring industry answers;
we adopt them and spend our design budget on the parts nobody's infra ships (§19).

- **Rootless, daemonless runtimes** — podman / Apptainer on Linux donors. No daemon, no
  root, no licensing; runs on a shared lab box without an admin. Apptainer's SIF is one
  content-addressable file: it drops into the grant-scoped fetch + digest-keyed warm
  cache pattern exactly like the existing RPC warm cache (`~/.svrnmesh/rpc-cache`
  pre-warming precedent), and images become mesh-distributable artifacts instead of
  registry round-trips over campus egress filters.
- **Donors run prebuilt, digest-pinned images — never build.** A Dockerfile build
  executes `RUN` steps before any sandbox exists. Submitters build (or bootstrap with a
  one-shot build job on their own node); donors only execute images referenced by
  manifest digest — the same trust shape as `UploadFile`'s owner-signed sha256.
- **Egress through a host-side proxy.** The container runs network-isolated with one
  mounted socket owned by the donor's executor, which presents the job-token to the
  judge/inference endpoint. Isolation and principal forwarding solved with one move;
  no firewall of our own authorship.
- **Runtime is a WorkOffer capability.** One `oci` job kind; donors advertise
  `podman` / `apptainer` / `colima-vm` / (later) `gvisor` / `firecracker`, arch, GPU.
  **Mac donors are second-class and we say so**: containers on macOS mean a Linux VM and
  no Metal — Mac donors are CPU-class executors, GPU donors are Linux. Today's fleet is
  exactly this shape.
- **The CI runner pilot (PLAN.md Phase 3) rides the same kind** — the `oci` payload
  answers both customers; no CI-specific machinery.

## The inference plane is the first customer

The unification is **contract-first, adapter-first, implementation convergence optional**:

- The contract (JobSpec / lease / WorkOffer / grant / scorer seam) is the one mechanism
  for "where does something stand." The job plane builds against it natively.
- The inference plane gets an **adapter that presents slot lifecycle as long-lease
  jobs** — internally it stays exactly as fast and specialized as it is today, possibly
  forever. Interface unification is where the value is; implementation unification is a
  refactor you earn later or never. (k8s precedent, both ways: its control plane runs on
  its own pods, but it got there incrementally — API contract first.)
- What the inference plane's machinery becomes: worker eligibility (settle/flap/
  quarantine) is a **donor trust state machine** every kind wants; the MeasurementKey
  discipline is the **placement-scorer pattern**; "strictly beat local" is a kind-owned
  rank function; the never-wedge guard and child supervision are already shared
  (`sovereign-compute`).
- A second convergence, already in the code: the rented-pod path (`WorkerProvider`:
  Vast, RunPod — `worker_controller.rs:73`) makes **mesh donors one more provider** —
  "the mesh is a cloud" is literal at that trait seam. *(design)* The pull model (donor
  pulls when its WorkOffer says it is available; consent lives donor-side) is the mesh
  shape; the push-to-rented-pod shape stays for burst capacity.

**The falsifiable fold test.** The unification is right iff it REDUCES total concepts —
one lease machinery, one fairness decider, one evidence format, one donor-trust state
machine — measured against two parallel stacks. If six months in there is a job plane
plus untouched inference machinery plus a translation layer with special cases, the fold
failed and should be cut back to contract-only. Guardrails: the JobSpec envelope stays
small and closed (identity, principal, grant ref, requirements, provenance hooks; shard
plans and sweep coordinates live in kind payloads under schemas kinds own); the job
plane's lease machinery never sits in the decode hot path.

## Pressure-test verdicts (2026-09-04)

| Risk | Severity | Verdict | Answer |
|---|---|---|---|
| Judge heterogeneity | High — silent pilot killer | **must design now** | `requirements` pins an exact mesh model identity, not a logical name; placement forms a homogeneous cohort or refuses; per-unit record stamps judge identity. §18.3 — mixing judges is substitution, refuse or name it |
| Coordinator SPOF overnight | High — loud pilot killer | **resolved** (§Resolved 1) | Append-only JobRecord journal on the submitting node; the queue is a projection (replay + re-register on crash). Donor abort-on-coordinator-loss is already on the wire (`corpus_queue.rs` heartbeat NotFound arm) |
| ACE via any executable payload | High beyond cohort 1 | stated + mechanical floor | OCI rootless + digest-pinned + host-proxy egress; grants name principals, never groups; `isolation` claim in the executor descriptor day one |
| Environment bootstrap / PyPI / wheels | Medium | answered by OCI | Image IS the environment; SIF warm cache keyed by digest |
| Private-repo / local-file staging | Medium | inherited mechanism | Small payloads by-value (the JobSpec `uploads` shape); large via grant-scoped fetch — PLAN.md Track M2's design, two customers one mechanism |
| Preemption × spend | Low | sized away | Budgeted, replicate-structured studies make units small; yield at unit boundary; kinds declare unit-cost hints |
| Arch/deps skew (torch-class) | Low for pilot | placement's job | Donor-advertised capability; placement refuses when the cohort cannot form |

Provenance is cheap and mandatory day one: per-unit `{git ref, image digest, judge model
id, node id, seeds, timestamps}` in the JobRecord — a mesh-run result must survive review,
which is the point of running science on it.

## The pilot: separatrix

An external separatrix sweep library is the first external customer, chosen because its
own design rules make the hard parts easy — core has **zero runtime dependencies as a
stated rule**; studies are small TOML + Python; runs are budgeted (`budget_runs`) and
replicate-structured (natural small units); every run journals and replays; and it
carries a **null control** designed to fire when a search manufactures boundaries out of
instrument drift.

The null control doubles as the mesh-heterogeneity acceptance gate (§18.1 — the gate
watched failing): deliberately scatter one sweep across mixed judges and watch the null
arm catch it. If it does not, the mixing gate moves into placement — either way the
question is answered by an experiment, not an argument. Its studies also pin judges by
logical name (`endpoint.model = "primary"`, resolving per machine — one already rides a
rented A6000 through a tunnel), which is exactly how the judge-heterogeneity risk was
found.

## The sibling customer: federated media libraries (Jellyswarrm-shaped)

A second external customer, deliberately **not** work-plane: a federated-library proxy
(Jellyswarrm — combines multiple Jellyfin servers into one; 880 stars on manual config)
exercises fabric, identity, data, routing, and evidence — five of six services — with no
jobs anywhere. That is the boundary check: the six-service split is not secretly
"everything is the work plane," and this customer proves the data-plane rails
independently. It is rung 1's residency pattern ("play from the holder, transcode where
the media lives") in consumer-visible form.

**The adapter verdict** (the question this customer forces): the adapter still exists —
it shrinks, and its deployment inverts. Commonwealth absorbs reachability (iroh by node
key replaces VPN/port-forwards), the server registry and shared API keys (grants), user
mapping and cross-server user sync (the mesh principal), and feed fan-out/merge (the
federation seam). What stays is irreducible substrate-side never: the Jellyfin API
emulation the client ecosystem demands (clients speak Jellyfin, and that is the product),
provider-ID item dedup, playback session semantics, and client-quirk maintenance. The
centralized always-up proxy someone must host becomes a **local shim beside each node**.
License note: GPL-2 vs this repo's AGPL keeps it a separate distribution, never absorbed
code.

Design gaps it exposes (added to the ledger, not yet scheduled):

1. **The federated-query seam is corpus-shaped** — the fan-out/merge/serving-wall
   machinery (`commonwealth-knowledge`) must become item-type-generic. Same move as
   `JobKind`, on the data side; this customer is its second proof.
2. **External-provider-ID identity (§7.5)** — cross-server item identity is TMDB/IMDB
   ids, not content hashes; provider-id must be a first-class identity form.
3. **The binary streaming plane** — sustained multi-Mbps over iroh is unmeasured; LAN
   fine, WAN gated on the same relay-floor unknown as Track A2's tensor tunnel bench.

## What we will NOT do

- **Invent sandboxing.** No seatbelt-profile authorship, no firewall DSL. OCI runtimes
  are the isolation story; the `isolation` claim field enumerates industry options.
- **Build untrusted images on donors.** Donors execute digest-pinned prebuilt images
  only; `RUN` steps are pre-sandbox code execution.
- **Add a results store.** Artifacts stream to the submitter; rung 8 stays deferred and
  honestly labeled.
- **Flat self-hosting.** Decode turns are never jobs; the data plane keeps its own
  latency class. The fold is contract-first and reversible by its own test.
- **A fourth scoping noun** (inherits PLAN.md): principal + grant + kind + WorkOffer
  cover the consent model; anything more is a decision to reverse the spine, argued as
  one.

## Resolved (2026-09-04 — cheap resolutions, each reusing machinery that exists)

**1. JobRecord — an append-only journal on the submitting node; the queue is its
projection.** The dichotomy (submitter-authoritative vs sqlite coordinator) was false: in
the pull model the queue already lives on the job's owner — coordinator == submitter,
exactly as ingest handoffs run today (the corpus owner runs the queue). So the only real
question is persistence, and it is not a new store: the JobRecord is an append-only
journal (units, outcomes, provenance) on the submitting node; the in-memory queue is a
projection; crash recovery is replay → re-register → re-offer unfinished units,
`prior_attempts` preserved. Donor-side recovery already exists on the wire: heartbeat
against a vanished queue returns 404 with a `Reclaimed` body and the peer aborts its unit
via the existing cancellation path (`corpus_queue.rs` heartbeat `NotFound` arm). Delivery
semantics stated plainly: **at-least-once, results idempotent per unit** — same seeds give
the same result; completion is a fold keyed by `unit_id`; the journal records double
deliveries rather than hiding them. Consequence now stated rather than implied: the
submitting node must be reachable for the job's duration — a closed laptop is out of
scope at H1; durable delegation is PLAN.md's front-door failover question (its open
question 1), not a new mechanism invented here.

**2. Lease machinery — one mechanism: heartbeats ARE the health checks.** No fork of the
decider (§10.6). The supervised child's health is the *source* of heartbeats — healthy
child → executor heartbeats → lease renews; dead child → heartbeats stop → the existing
reaper reclaims and re-offers. "Long lease" is not new machinery; it is the same lease
with a kind-declared interval (the descriptor carries it, beside the unit-cost hint).
Restart policy is the queue's re-offer behavior. `model-serve` "checkpoint" is the warm
handoff file that already exists (`compute-distribution/`). `oci` jobs: no checkpoint at
v0 — unit-boundary sizing absorbs the preemption cost.

**3. Placement scorer — data crosses the wire; the scorer stays planner-side.**
Requirements are data in `oicp-types` (with JobSpec); scorers are a kind-keyed registry
beside the placement decider — the same descriptor-registry shape as `JobExecutor`. An
enum would be §2's wrong turn (kinds are an open set); a trait in the contract layer puts
behavior where the layer map wants nouns. Invariant carried forward from the
MeasurementKey lesson: **the planner is the one scorer owner — a second construction of a
score (CLI preview vs daemon, the `mesh bench`/`mesh plan` trap) is a §10.6 violation,
not a convenience.** Serialized placement plans ride in the job payload.

**4. Image fetch — by digest, from any source; the digest is the contract.** Not an
either/or. The existing `UploadSource` shape already resolved this for files (owner
stream vs URL fetch, sha-validated — "trusted transport, not trusted source",
`worker_controller.rs:127`); images inherit it: a digest-pinned image with an ordered
source list — grant-fetch from the submitter first when LAN (the RPC warm-cache pattern),
registry pull as fallback (works when the submitter is unreachable mid-download), warm
cache keyed by digest regardless of source.

**5. Preemption — two tiers, both from existing signals.** Tier 1, stop-offering: a
WorkOffer schedule retraction; the pull model makes this free — a donor that stops
pulling takes nothing new. Tier 2, cancel in-flight: the WorkOffer's yield trigger
subscribes to the activity-level signal the daemon already publishes to the mesh (the
hot/idle transitions the activity mesh emits); on hot, the executor fires the queue's
existing revocation path locally (the 410/cancellation mechanism). Checkpointing stays
out of scope at v0; kinds declare unit-cost hints so smarter behavior later needs no seam
change.

**Remaining open at this layer: nothing.** The durability question that survives is
PLAN.md's own open question 1 (front-door failover), which §Resolved 1 now depends on for
the delegated case.

---

*Companion to [`PLAN.md`](PLAN.md) (which this extends at Phase 3) and
[`GROUND_TRUTH.md`](GROUND_TRUTH.md). Update this file in the same commit as the code it
describes (§1.1).*
