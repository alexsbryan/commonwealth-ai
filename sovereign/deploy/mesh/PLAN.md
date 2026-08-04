# The mesh cloud — plan

**Status:** DRAFT, nothing landed. Written 2026-08-04; consolidated to one voice the same day after
three drafting passes (ambition correction → staff verification → foundation-first re-cut).
**Vision (operator, 2026-08-04):** *the mesh is a cloud. Full stop.*
**Evidence:** every load-bearing claim about current code is verified at file:line in
[`GROUND_TRUTH.md`](GROUND_TRUTH.md) (same directory), cited below as **GT**. Claims marked *(doc)*
rest on repository markdown whose runs were not re-executed.
**Predecessor:** `sovereign/deploy/onprem/PLAN.md` — single box, landed 2026-08-03; its clean-VM
rehearsal is still owed and is a prerequisite here.
**Compass:** §7 structural-not-remembered · §10.6 one-decider-one-name · §18.1 watch the gate fail ·
§18.3 never silently substitute · §18.5 one run is not a measurement.

---

## Mission

> **The machines you already own are your cloud.**

Work runs where your data lives. Capacity scales with the hardware you plug in. Every placement
decision is replayable evidence, not a provider's promise. The line items an enterprise pays a
hyperscaler for — document pipelines, vector search, inference APIs, CI minutes, batch queues,
backup — become workloads on a fabric the enterprise controls end to end.

Two test sentences; every funded line of work must make one of them more true:

1. **Residency** — data is answered from the machines that hold it, and never moves.
2. **Elasticity** — throughput rises with machine count. The office is the datacenter.

## North star — the consortium

The multi-year destination is **compute across organizations that do not trust each other**, where
every participant is *confident* rather than *reassured*. Confidence is a property of evidence, so
the north star has a one-line test every feature is measured against:

> **Every claim the mesh makes about itself is checkable by the party who would be harmed if it
> were false.**

This is the moat. A consortium on AWS trusts AWS — a single point of failure and subpoena. k8s
assumes one cluster admin and mutually-trusted nodes by construction. Commonwealth already has what
neither has: identity by public key, NAT-traversing QUIC dialed by that key, gossip membership, a
replicated store, a contribution ledger (GT).

**The trust ladder** — six claims a member needs verified. All six exist today as the self-attested
version; the arc to H3 is converting each into a counter-signed, reconcilable receipt:

| # | Claim | Today (GT) | Confidence requires |
|---|---|---|---|
| T1 | My data never left my machine | scored chunks + provenance only; custody event, serving-side, suppressible by omitting `X-Node-Id` | two-sided receipts that reconcile (`KnowledgeQueryReceived` twin of the existing inference pair) |
| T2 | Only entitled parties could reach my data | nothing — internal plane unauthenticated | entitlement is a capability I issued, not a label you assert |
| T3 | The answer cites sources that say that | grounding gate, citations, `cannot_know_from_here` | citations verifiable **without re-disclosing the corpus** — research |
| T4 | Compute ran only where I permitted | inference decision log, typed + replayable | the record signed by the decider, checkable by the owner |
| T5 | You kept no copy | `partition_evict` wipes and logs | eviction receipt signed, bound to the authorizing grant |
| T6 | The rules didn't change silently | charter hash-drift, section diffs, gossiped decision records (project-scoped) | same machinery scoped to a consortium, hash counter-held by members |

**Membership is minted by the founder node, accountable through its own governance** (operator,
2026-08-04): the charter is a hashed artifact, amendments are versioned section diffs, decisions
replicate to every member (GT: governance primitives). The founder can act unilaterally; it cannot
act silently. Scope caveat: proven for software projects, in a high-layer CLI crate — re-scoping to
a consortium is real work. Do not represent it as shipping.

## The ten workloads — what "cloud" means here

The ambition, made concrete. Each rung rides the same substrate; each displaces a bill.

| # | Workload | Replaces | Today |
|---|---|---|---|
| 1 | **Document intelligence** — cited answers from corpora that never leave their machines | managed RAG / vector search | **Working** — `docs/TWO_NODE_QUICKSTART.md`, custody-logged (GT) |
| 2 | **Document refinery** — the firm's PDF/OCR backlog processed by every idle machine; revoke mid-run wipes peers | Textract / Comprehend / Glue | Queue works end to end for re-downloadable sources; **cannot yet carry local files** (GT) — the named gap |
| 3 | **Frontier inference on pooled GPUs** — a 122B model no single box holds, served across office machines | hosted inference APIs | **Measured**: 17.3–17.9 tok/s vs 14.8 solo, 2 nodes *(doc)* |
| 4 | **Enrichment farm** — overnight embedding, NER, graph enrichment of corpora as fleet jobs | embedding APIs, managed pipelines | Pipeline exists per-node; rides the queue once payloads generalize |
| 5 | **Your CI/CD on your metal** — build/test/release as leased jobs on idle machines | GitHub Actions minutes | We are our own first customer: hosted CI silently died on a spending limit, 4,369 min/mo audited, releases already local at $0 (GT: CI economy) |
| 6 | **Batch AI fleets** — agent sweeps, evals, bench runs, code migrations, scheduled and replayable | cloud batch / step functions | Harnesses exist per-node (`svrn bench`, solve); mesh scheduling absent |
| 7 | **Overnight harvest** — workstations opt in on a schedule; foreground work always wins; contribution ledger credits the donors | spot / preemptible capacity | Admission gate + gossiped in-flight exist; yield policy unstated |
| 8 | **Durable storage & backup** — corpora and artifacts replicated across N machines with custody receipts | S3 tier + backup vendors | Seed only: `MeshStore` has a 7-day TTL and lossy replication (GT) — hardening is a real workstream, not a flag |
| 9 | **Long-running services** — internal tools, model endpoints, dashboards: supervised, health-checked, failover | k3s / ECS | Deliberately last; enters via the work plane grown up (long leases + health + restart policy), **not** via `commonwealth-app` (below) |
| 10 | **Consortium compute exchange** — burst into a partner org's idle GPUs under signed, counter-signed, charter-governed grants | impossible on a hyperscaler | The moat. H3 |

Rungs 1–3 are H1's proofs — two exist, one is a measurement away. Rungs 4–7 are one move: **the
work queue's payload generalizes** (Phase 3 below). Rungs 8–9 complete the cloud. Rung 10 is the
north star, and every rung below it feeds the evidence plane it needs.

**Horizons.** **H1** (now → ~2 quarters): one org, many machines — self-attestation acceptable,
rungs 1–3 demonstrated, foundation landed. **H2** (~1 year): walled groups inside one org — ethical
walls, signed fabric, per-key tenancy; rungs 4–7 productize. **H3** (multi-year): the consortium —
all receipts counter-signed; rungs 8–10. Each horizon's trust requirement forces the next piece of
architecture, which is why the order is not negotiable.

---

## Where we actually are

**The compute half is built and measured (two nodes); the residency half is a working quickstart;
the trust model does not exist; the cloud generalization has not begun.** Specifically (all GT):

- The mesh has no org, tenant, group, or role. Its entire auth surface is one equality check on the
  gossip route; `:9742` binds `0.0.0.0`, and the ggml-RPC tensor port `:50052` is plaintext raw TCP
  on the same default.
- The retrieval serving handler checks **nothing** — it discards the sharing flags it is handed;
  `query_sharing=false` only stops advertisement. The only wall today is that peers don't know what
  to ask for.
- The work queue is a real distributed job system — leases, heartbeats, retries, reclaim, dedup,
  revocable grants, eviction — but its payload is ingest-only, its sources must be re-downloadable
  by the peer, its embed-model gate is declared-and-dead, and failed units drop chunks silently.
- `TenantId` exists only inside `sovereign-server` and dies at the server→daemon hop. The scheduler
  sees nothing corpus- or tenant-shaped. `CorpusVisibility::Private{owner}`'s only writer is
  compiled out of the hardened build — shipped isolation is zero.
- Custody and decision evidence exist on one plane each: custody serving-side only (suppressible),
  decision records inference-only.

So the work is not "add clustering." It is: **give the mesh an identity spine, put one residency
predicate behind three gates, authenticate the fabric, generalize the queue's payload — and make
every resulting claim checkable by the party it protects.**

---

## The substrate — six services

The cloud is six services. Every rung of the ladder is a composition of them; nothing on the ladder
requires a seventh.

**1. Identity & tenancy.** One noun: `TenantId` — a firm, a practice group, a consortium member
(org == tenant, operator-resolved; a *matter* is a corpus, not an identity). Home is `oicp-types`,
forced by the layer map — the only contract-layer leaf both families already depend on (GT: layer
contract). Moves with `#[serde(transparent)]` and a validator in the same PR (canonical lowercase
`[a-z0-9][a-z0-9-]{0,62}`, constructed only via `TenantId::parse()`). Nodes carry a clearance set;
corpora carry an owner. Merge rules differ by field and getting this wrong is privilege escalation:

| Field | Merge | Why |
|---|---|---|
| `Mesh.require_clearance: bool` | monotonic, stricter-wins | downgrade protection — `require_encryption`'s exact contract (GT) |
| `NodeCapabilities.clearance: BTreeSet<TenantId>` | plain LWW | self-descriptive — only node D writes node D's clearance; the risk is forgery (→ fabric), not merge |
| authority for an assignment | H1: operator config · H3: capability issued by the tenant | trust-ladder rung T2; grants are the proven shape |

**2. Placement.** One predicate — *work or data belonging to tenant T touches only nodes cleared
for T* — enforced at **three gates**, because the three planes place work through three separate
deciders that cannot see each other (GT: three planes):

| Plane | Decider today | The gate |
|---|---|---|
| Ingest / jobs | `allowed_peers` at dispatch, enforced at lease (403) | intersect candidates with cleared-for-owner — enforcement point already refuses |
| Inference offload | pure `rank()` with four typed exclusions | `ClearanceMismatch` fifth; needs `data_owners` threaded into the OICP envelope via the existing `PrincipalResolver` seam — `rank()` today sees nothing tenant-shaped |
| Retrieval | **none** | serving-side wall on the machine that owns the data + fan-out courtesy filter |

Every gate records a typed, replayable rejection; the retrieval plane additionally gains the
decision record it entirely lacks (per-peer served / refused / unreachable — today transport
failure and "no results" are indistinguishable, a §18.3 violation sitting in the demo path).

**3. Work.** The queue is the compute primitive of the whole cloud: pull-based leases, heartbeats,
3-attempt retries, reclaim-with-cancel, content-hash dedup at merge, grant-scoped, revocable,
evicting (GT: ingest queue). Two gaps close in H1 (local-document units + a grant-scoped
source-fetch route, so it can carry the customer's own files; integrity — embed gate made live,
dropped units named). Then the strategic move: **the payload generalizes** — `WorkUnit` grows from
ingest indices to a job-kind abstraction, and rung 5 (CI) is the pilot payload: this repo's own
test suite as leased jobs on an idle peer. Services (rung 9) are this same machinery grown up:
long leases, health checks, restart policy.

**4. Data.** Corpora that never move, searched in place, merged by the caller — working today.
Ownership becomes real (the `Private{owner}` writer un-gated, owner forced to the caller's tenant,
owner surfaced on `IndexInfo` where the serving wall reads). Storage-as-a-lane (rung 8) is
deferred to H2+ and honestly labeled: `MeshStore` as it stands is a gossip cache, not a store.

**5. Fabric.** iroh QUIC dialed by node key is already authenticated transport. The two open
surfaces are the HTTP internal plane (`:9742`) and the raw tensor port (`:50052`, where the iroh
tunnel is *additional* reachability, not containment — GT). The fix: Ed25519
signed-request auth on `/internal/*` (detached signature over method/path/body-hash/timestamp/nonce,
verified against the gossiped directory; join and loopback exempt — join *is* the bootstrap), rolled
out **two-phase** (sign + accept-and-log fleet-wide, then require — a gossiping fleet cannot take a
flag day); and the tensor port goes loopback + iroh-only, gated on one 122B tunnel-tax bench
(invisible at 4B *(doc)*, never measured at 122B). Once requests are signed, a signed *receipt* is
the same machinery pointed the other way — the fabric is the first rung of the H3 ladder.

**6. Evidence.** The contribution ledger, the inference decision log, and the charter/decision
machinery exist (GT). H1 makes evidence exist on every plane (retrieval record, two-sided custody);
H2 signs it; H3 makes it reconcile across orgs. T3 — verifying a citation without re-disclosing the
corpus — is the one research item; it is fenced out of every H1/H2 commitment.

## What we will NOT do

- **Resurrect `commonwealth-app`.** The "mesh app platform" is a named seam with zero executed
  code, an unpersisted registry, a proxy that 503s unconditionally, and a store that mangles bytes
  (GT: app platform). Its dead half gets deleted. Rung 9 arrives through the work plane's
  grant/lease machinery when the ladder reaches it — that revises the earlier "services never"
  posture (operator, 2026-08-04) while keeping its reasoning: we do not race k3s to commodity
  parity; we arrive at services with the one property k3s structurally cannot offer — placement
  across trust boundaries, provable to the party at risk.
- **Arm the throughput extrapolator.** `benchmark` is permanently `None` with a guard test; the
  linear-size filler measured −56% and is filed DO-NOT-BUILD. Heterogeneity gets *measured* points
  (`svrn mesh bench` → `mesh_measurements`, which never interpolates) or a neutral constant.
- **Promise T3** in any H1/H2 material.
- **Add a fourth scoping noun.** Tenant is the only identity; a workspace/matter/namespace proposed
  later is a decision to reverse the spine, argued as one.

---

## The plan — foundation first, demo every phase

Three independent tracks converge on the demos. **Rule: every phase ends with something on a
screen, and a phase without its demo does not merge.** Each demo is the phase's own §18.1 evidence
— the gate watched failing in front of someone who can cancel the work.

```
Phase 0 ─► Phase 1 (F1→F3→F2→F4) ─► Phase 2 (G3 · G1→G2 · G4) ─► Phase 3 (generalize work)
  │
  ├─ Track M:  M0 name boxes ─► M1 ingest curve (kill criterion) ─► M2 local docs
  │
  └─ Track A:  A1 sign+log ─► A2 tensor bench+bind ─► A1' require        ─► [H2 gate]
```

**Phase 0 — clear the ground** *(days; parallel-safe).* Deletes land before new concepts: the two
stale mTLS claims; the always-granted `/internal/scheduling/intent` stub; the dead scheduler
vocabulary (`InferencePlan` et al., zero production constructors); the `MeshPeering`/`PeerTrustLevel`
decision (populate or delete — an unpopulated trust type inside a security feature is the §10.6
trap); `SOVEREIGN_LOCAL_FIT_CHECK_SKIP`. Integrity: embed-model gate live at `next_unit`, merge
refuses empty stamps, dropped units named in the merge report, fan-out client keeps per-peer
outcomes. WS4a: run the never-run containment kill test, flip `distributed_primary`, write the
overdue `DEFAULTS_LEDGER` row. The on-prem clean-VM rehearsal (owed) runs here.
*Demo: rung 1 walkthrough (zero build); a worker `kill -9`'d mid-decode recovering; a wrong-embed
peer refused at lease time; a dropped unit named instead of silent.*

**Phase 1 — the identity spine** *(strictly ordered, one PR each; F3 ahead of F2 because it
depends only on F1 and carries the first demo).*
- **F1** `TenantId` → `oicp-types` + validator, same PR.
- **F3** Ownership real: `Private{owner}` writer un-gated, owner = caller's tenant, owner on
  `IndexInfo`. *Demo: two API keys, one box — the corpus visible to its owner, invisible to the
  other, in the hardened build.*
- **F2** Clearance state: monotonic policy block + LWW clearance set, `#[serde(default)]`.
- **F4** Registration-time predicate + the server tenancy defects (filter-after-LIMIT,
  `approve_task` takes no tenant). *Demo: an uncleared node refused at registration; a stale peer
  cannot demote policy.*

**Phase 2 — the gates** *(consume the spine; three planes in parallel).*
- **G3** ingest allowlist ∩ clearance. *Demo: the uncleared peer's 403, with reason.*
- **G1** retrieval serving wall + typed refusal → **G2** fan-out record into the epistemic ledger
  event. *Demo — the headline: the same question from two nodes; one gets the cited answer, the
  other gets `cannot_know_from_here` **plus the record naming the refusing node and
  `ClearanceMismatch`**. Residency, positive and negative, on one screen.*
- **G4** `data_owners` through the envelope; `ClearanceMismatch` in `rank()`; sim + paired tests.
  *Demo: decision-log replay showing the exclusion — no prompt carrying T's text reaches an
  uncleared box.*

**Phase 3 — generalize the work plane** *(the cloud move; design-gated after Phase 2 starts).*
`WorkUnit` grows a job-kind seam; the CI runner is the pilot payload — leased build/test jobs with
streamed results, this repo as the customer. Rungs 4, 6, 7 follow the same seam; the foreground-
yield policy (rung 7) gets stated and tested here. *Demo: this repository's own test suite green,
run as mesh jobs on an idle peer — Actions minutes at $0.*

**Track M — measurement** *(hardware-gated, parallel).* **M0** the operator names four same-class
boxes on one switch — nothing moves first. **M1** the ingest scaling curve at 1/2/4 nodes: finish
line = corpus queryable (owner-side tail included); mandatory phase breakdown (unit wall-clock /
shard fetch / merge+index) so a bad curve is diagnosable — shard fetch is serial per peer and index
build is owner-only, the Amdahl tail lives there; fresh corpus identity per trial (dedup fakes
repeats); per-node unit counts (stragglers visible); two baselines (coordinator self-pull = scaling
baseline; bare ingest = the fabric's price); three trials, medians with spread, guard-tripped runs
recorded invalid. Corpus is LAN-served JSONL, stated as such. **M2** local-document units + the
grant-scoped, ranged, evictable source-fetch route. *Demos: the curve itself; then rung 2 on the
customer's own files with mid-run revoke → peers exit and wipe.*

**Track A — authenticated fabric** *(start anytime; must complete before any second group or
org).* **A1** signed `/internal/*`, accept-and-log — *demo: the ledger naming every unsigned
caller.* **A2** the 122B tunnel-tax bench, then the tensor bind. **A1'** require — *demo: an
unsigned request refused, typed.* Note the deliberate flip: today an absent `X-Node-Id` is served
with zero ledger rows; under A1' absent identity refuses, and the e2e pin that enshrines today's
behavior flips with it.

**Kill criterion (Track M).** If 4-node ingest does not beat 2-node by more than the trial spread,
the elasticity claim is false at fleet scale — with one honest branch: if the phase breakdown
blames the serial merge tail, that is a named fixable bottleneck (fix, re-run) rather than a
structural verdict. Either way the claim stays unproven until a curve passes. A structural kill
rescopes the initiative to "the box plus trusted helpers" — the residency half and rungs 1, 5, 6
survive that verdict intact.

## Feasibility — the standing verdict

**H1 is assembly and gating of machinery that exists, not invention** — roughly **7–10
engineer-weeks** of build (estimates, not measurements), plus Phase 3's pilot (~2–3 weeks). Every
new piece copies a proven neighbor: the monotonic merge copies `require_encryption`; the fifth
exclusion copies the existing four; the ingest gate reuses an enforced 403; H3's receipts are A1's
signing reversed; wire compat is the recipe that already shipped once. The critical path runs
through the operator twice (boxes, rehearsal), through the streaming runtime once (the fan-out
record — the hairiest file this plan touches), and through no research.

Named nerves: the serial merge tail (the one fixable place the curve can die — hence the two-branch
kill); the two-phase fabric rollout (a gossiping fleet cannot take a signing flag day); one worker
per node strands big boxes until measured points exist; ops are pilot-grade (manual front-door
failover, no upgrade story) — right for design partners, said plainly, not GA.

## Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | Ingest doesn't scale either — elasticity false at fleet scale | M1 first in its track; two-branch kill criterion; residency half survives |
| 2 | Customer machines aren't on one campus LAN (relay floor 5.5–7.1 vs 17+ tok/s) | state the LAN requirement as a feature; don't sell distributed-workforce at H1 |
| 3 | Clearance labels are self-asserted until the fabric is signed | Track A ordering is a hard gate before any second group; do not reorder |
| 4 | Pooling steals a workstation's GPU mid-workday | gossiped in-flight + admission exist; rung 7's yield policy stated and tested in Phase 3 |
| 5 | llama.cpp wire-version skew between host and workers | one binary set per fleet; make skew a startup refusal, not a crash (open P1) |
| 6 | New gossip fields break older meshes | `#[serde(default)]` + stricter-wins, as shipped before |
| 7 | A drifted embed model poisons a merged index (gate currently dead) | Phase 0 makes the gate live at lease + merge |
| 8 | The queue can't carry local files — rung 2's product gap | M2; the measurement is not blocked, the customer demo is |
| 9 | `TenantId` free-text as security root ("Firm" vs "firm ") | validator lands with F1, not after |
| 10 | Governance primitives are project-scoped, high-layer | mechanism transfers, schema doesn't; never represent consortium governance as shipping |

## Resolved (operator, 2026-08-04)

- **The mesh is a cloud — full stop.** Ten workloads above; the plan is the substrate, the lanes
  ride it.
- **Org == tenant.** One identity noun; a matter is a corpus. Clearance unit is the node — coarse,
  and stated as coarse in customer material.
- **`TenantId` lives in `oicp-types`** — forced by the layer map (GT), not preference.
- **The founder mints consortium membership**, accountable via charter/amendment/decision records.
- **Services are on the roadmap** (rung 9) via the work plane — revising the earlier "not the app
  platform, ever" posture while keeping its k3s reasoning. `commonwealth-app` stays dead.
- **Measure ingest, not decode.** Sharded decode degrades per node; the queue is where elasticity
  is likeliest true.

## Open questions

1. Front-door failover: manual acceptable at H1? (Say so in the customer brief either way.)
2. Pool workstations or dedicated boxes? Sellable answer is probably "dedicated plus opt-in."
3. Upgrade story for N machines, when one machine has none yet.
4. At H3, what happens when the founder is wrong — exit path (fork, re-elect, dissolve) belongs in
   the charter template, not code.
5. **Which four machines run M1?** Operator action; gates the measurement track's schedule.
6. How does the tenant ride the server→daemon hop at H2 — header plus a `#[serde(default)]`
   request field, designed when H2 starts.
7. Phase 3's job-kind seam: extend `WorkUnit` in place or introduce a `JobKind` envelope above it?
   Design question opened by the CI pilot; decide with code in front of us, not here.

---

*Ground truth: [`GROUND_TRUTH.md`](GROUND_TRUTH.md). Update both files in the same commit as the
code they describe (§1.1).*
