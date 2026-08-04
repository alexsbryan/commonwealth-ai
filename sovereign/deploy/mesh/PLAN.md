# Mesh compute — the plan

**Status:** DRAFT, nothing landed. Written 2026-08-04, rewritten same day after the ambition was
corrected upward.
**Predecessor:** `sovereign/deploy/onprem/PLAN.md` (single box, landed 2026-08-03, clean-VM
rehearsal still owed).
**Compass:** §7 (make it structural), §10.6 (one decider one name), §18.1 (a gate you have not
watched fail is not a gate), §18.3 (never silently substitute), §18.5 (one run is not a
measurement), §9.1 (no production branch without a tracing event).

---

## Mission

> **Your documents never leave the machines that hold them, and processing them scales with
> hardware you already own.**

Every workstream below is funded only insofar as it delivers that sentence. A capability that does
not make one of the two halves more true is out of scope, however interesting.

---

## North star — cross-org consortium compute

The multi-year destination is **compute across organizations that do not trust each other**, where
every participant is *confident* rather than *reassured*.

That word does the work. Confidence is not a security posture; it is a property of evidence. So the
north star has a one-line test, and every feature in this document is measured against it:

> **Every claim the mesh makes about itself should be checkable by the party who would be harmed if
> it were false.**

This is the moat. Hyperscalers cannot offer it — a consortium on AWS trusts AWS, and AWS is a
single point of both failure and subpoena. Kubernetes cannot offer it — k8s assumes one cluster
admin and one datacenter, and every node in a cluster is mutually trusted by construction.
Commonwealth already has the substrate that neither has: identity by public key, NAT-traversing QUIC
dialed by that key, gossip membership, a replicated store, and a contribution ledger.

### The trust ladder — what "confident" decomposes into

Five claims a consortium member needs to verify. **Today all five are real and all five are
self-attested by the party who would benefit from lying.** The multi-year arc is converting each
self-attestation into a counter-signed, reconcilable receipt.

| # | The claim | Today | What confidence requires |
|---|---|---|---|
| T1 | *My data never left my machine.* | `mesh_sharing = false` returns scored chunks + provenance, never index bytes; a custody ledger entry is recorded (`docs/TWO_NODE_QUICKSTART.md:8`, `:76`) | Two-sided receipts: the querent's node holds a matching record, and the two reconcile |
| T2 | *Only entitled parties could reach my data.* | Nothing. `/internal/*` is unauthenticated (see appendix) | Entitlement is a **capability I issued**, not a label you assert about yourself |
| T3 | *The answer cites sources that actually say that.* | Grounding gate, citations, `cannot_know_from_here` as a first-class verdict | Citations verifiable against the serving corpus **without re-disclosing it** |
| T4 | *Computation on my data ran only where I permitted.* | `decision_log` records every candidate exclusion with a typed reason, replayably | The decision record is **signed by the decider** and checkable by the data owner |
| T5 | *You did not keep a copy.* | `partition_evict` wipes the peer's working partition and logs it (`corpus_queue.rs:694-722`) | The eviction receipt is signed and bound to the grant that authorized the work |
| T6 | *The rules did not change without my seeing it.* | Charter sections are parsed, versioned and amended (`sovereign-cli-dev/src/amend.rs:59,128,231`); decision records are written through a `DecisionRecorder` trait to the NoteStore (`found.rs:305,338`), which gossips mesh-wide; **charter drift is detected by recomputing the on-disk hash against a recorded one** (`project_cmd/audit/mod.rs:110-130`) | The same three, scoped to a consortium rather than a software project, with the recorded hash counter-held by members |

The encouraging half: **the events already exist.** The ledger, the decision log, the grant
lifecycle, the custody records, and the charter/decision-record machinery are all implemented — as
the unsigned, single-sided, or differently-scoped version. The multi-year work is signing,
reconciliation, and re-scoping, not inventing the vocabulary. T3 is the genuinely hard one and the
only one that needs new cryptography.

### Who mints consortium membership — RESOLVED 2026-08-04 (operator)

**The founder node is the initial point of trust, and is accountable for its own governance —
transparently and enforceably, by the system.**

This is the answer to the objection that a founder is a single point of trust and therefore
reproduces the thing H3 exists to remove. It does not, provided the founder's authority is
*legible*: joining a consortium means accepting a charter you can read, whose amendment history you
hold, whose decisions are replicated to your node, and whose current text you can verify against a
recorded hash. The founder can still act unilaterally; it cannot act *silently*. That is precisely
the north star test applied to governance itself — the claim "the rules are what I told you they
are" becomes checkable by the member who would be harmed if it were false.

Three properties make it enforceable rather than aspirational, and all three have working
implementations today:

1. **The charter is a hashed artifact.** `sovereign project audit` recomputes the on-disk
   `CHARTER.md` hash against the recorded one and reports `none` / `differs` / `unknown` / `n/a`
   (`project_cmd/audit/mod.rs:110-130`) — four verdicts, §18.1's shape, not a boolean.
2. **Amendments are structured and versioned.** `parse_charter_sections` / `changed_sections` /
   `serialize_charter_sections` (`amend.rs:75,128,451`) diff a charter *by section*, and an
   amendment bumps `charter_version` (`amend.rs:231`). A member can see exactly which clause moved.
3. **Decisions are recorded through a seam, not a file.** `DecisionRecorder` is a trait with a
   `NoteStoreDecisionWriter` implementation (`found.rs:305,338`), and non-private notes already
   gossip mesh-wide keyed by `content_hash` — so a decision record replicates to every member's node
   without a new transport.

**Honest scope caveat.** These primitives live in `sovereign-cli-dev` and govern *software
projects* — founding, phases, charter amendment. Pointing them at a consortium is a genuine
re-scoping, and `sovereign-cli-dev` is a high-layer CLI crate, so enforcement by the daemon means
lifting the primitives down the stack. What transfers for free is the **mechanism** (hash-recorded
tamper evidence, section-level amendment diffs, a recorder seam over a replicating store) and the
fact that it is proven in use. What does not transfer for free is the schema or the crate location.
Do not represent consortium governance as shipping.

Related and worth reading before H3 design: `sovereign/docs/GOVERN_A_CORPUS.md` — "surface tensions,
resolve them into common law" — which already models adjudication over a governed baseline, and is
the closest existing analogue to inter-member dispute resolution.

### Three horizons

Each horizon's trust requirement is what forces the next piece of architecture. This is why the
order is not negotiable.

**H1 — one organization, many machines.** *(now → ~2 quarters)*
One legal entity owns every node, so self-attestation is acceptable: there is no adversary inside
the boundary. Deliver the mission sentence literally. This is the two PoCs below.

**H2 — one organization, walled groups.** *(~1 year)*
Practice groups, departments, ethical walls. Self-attestation stops being acceptable *between*
groups — a wall that one side can silently step over is not a wall. This is where T2 and T4 start,
and where receipts first appear.

**H3 — many organizations, one consortium.** *(multi-year)*
Mutually distrusting participants. All five claims counter-signed and reconcilable. The moat.

---

## Bottom line — where we actually are

**H1's compute half is built and measured; H1's residency half is a working quickstart; H2's trust
model does not exist at all.**

- Two boxes run a 122B model at 17.3 / 17.9 / 17.8 tok/s against a 14.8 tok/s solo baseline
  (`docs/DISTRIBUTED_PILOT_READINESS.md:543-544`). Authenticated QUIC between nodes is
  throughput-free on a LAN (`:533`).
- `docs/TWO_NODE_QUICKSTART.md` is titled "a cited answer from a corpus that never leaves its
  machine" and walks through it end to end, custody ledger included.
- Peer-assisted ingest is a real distributed job system: a pull-based lease/heartbeat/complete
  queue (`corpus_queue.rs`), bounded and revocable ephemeral grants whose revoke fails concurrent
  work closed (`corpus_grant.rs:13-17`), and `partition_evict` for no-retention teardown.
- Against that: the mesh has **no org, group, tenant, or role** (`commonwealth-core/src/mesh.rs:12-31`),
  and its entire auth surface is one equality check on one route (`routes_internal/gossip.rs:28-45`)
  while `:9742` binds `0.0.0.0` by default (`setup_config.rs:1043-1046`).

So the work is not "add clustering". It is: **give the mesh an identity spine, and make residency
policy a feasibility gate in the ranker that already exists** — then, over years, make every
resulting claim checkable by the party it protects.

---

## The scaling insight that sets the order

**Tensor-sharded inference gets worse as you add nodes. Document ingest gets better.**

Sharded decode adds a layer-boundary round trip per node, and the LAN envelope is 10.9–13.3 ms per
16 KB activation hop (`DISTRIBUTED_PILOT_READINESS.md:507`). Peer-assisted ingest is a pull-based
work queue — adding a node adds a worker.

So *"processing them scales with hardware you already own"* is **probably true for ingest and
probably false past a small N for inference.** Measure the axis where the mission sentence is most
likely to hold, and measure it first. An earlier draft of this plan proposed measuring the decode
curve; that would have tested the weaker claim and could have killed the plan on the wrong evidence.

---

## Architecture

H1 target: **N boxes on one campus network, one of which is the front door.**

```
                        nginx :443  (TLS, route allowlist)
                             │
                    sovereign-server :8080          ← tenancy, grounding gate, citations
                             │  remote backend
                    ┌────────┴────────┐
                    │  daemon :9741   │             ← front door; owns the primary slot
                    │  daemon :9742   │
                    └────────┬────────┘
             gossip (10s)    │   ggml-RPC tensor stream · ingest work queue
        ┌────────────────────┼────────────────────┐
   daemon :9742         daemon :9742         daemon :9742
   node B                node C               node D
   corpus: litigation    corpus: —            corpus: finance
   [clearance: lit]      [clearance: any]     [clearance: fin]
```

Three planes, deliberately different, all three already implemented:

- **Control** — anti-entropy gossip of the whole `Mesh` snapshot, fanout 2, every 10s, plus
  `MeshStore` KV replication namespaced by `app_id`.
- **Retrieval** — `/internal/knowledge/search` fan-out. Peers open the shards they host, return
  typed scored chunks with provenance, and the caller merges. **The index never moves.** Behind a
  peer-admission gate so a busy node sheds peer searches rather than starving its own user.
- **Work** — either the ingest queue (leases) or the tensor stream (raw TCP over iroh). Only the
  `primary` slot is ever distributed; `fast`/`embed`/`code` stay local because distributing them
  crashed the worker under concurrent multi-slot load (`docs/RPC_DISTRIBUTED_INFERENCE.md:125-126`).

"Front door" is not a mesh role. Membership is flat and leaderless; where election is needed it is
`min(NodeId)` over the online set — a pure function requiring no consensus.

---

## Workstream 0 — the ingest scaling curve (gates everything)

**A measurement, not a build. Nothing else starts until it reports.**

Every distributed number in the repository is two-node (`DISTRIBUTED_PILOT_READINESS.md:4`, `:273`).
The mission's second half — *scales with hardware you already own* — rests on a curve nobody has
plotted, on the axis that has never been measured at all.

**Deliverable.** Pages/hour of document ingest at 1, 2, and 4 nodes, same corpus, same hardware
class, recorded alongside the existing decode measurements. Secondary: the decode curve at the same
node counts, so the two axes can be compared and the product claim scoped honestly.

**Reporting rules (§18.5).** Three trials minimum per configuration; medians with spread. One run is
not a measurement. Guard-tripping runs are recorded as invalid, never discarded.

**Prior art on getting this wrong, in this repo.** `DISTRIBUTED_PILOT_READINESS.md:570-586` is a
self-correction retracting a previously-published throughput figure: a log re-audit found the
distributed slot lived 17 seconds and generated no token. That retraction is the standard this
workstream is held to.

**Kill criterion.** If 4-node ingest throughput does not exceed 2-node by more than the trial
spread, the mission's second half is false at fleet scale. Stop, and rescope to "the on-prem box,
plus one helper."

**Deletes:** none.

---

## Workstream 1 — the trust domain

**The keystone.** H2 begins here, and H3 is unreachable without it.

### The design call

Do **not** invent a third identity concept. Two exist and do not talk: `TenantId` in
`sovereign-server`, and `NodeId`/`MemberRecord` in the mesh. Adding `OrgId` beside them is §10.6
outright — three deciders where there should be one.

**RESOLVED 2026-08-04 (operator): org and tenant are the same spine.** A firm is a tenant; a
practice group is a tenant; a consortium member is a tenant. There is no second noun and no `OrgId`.
A *matter* is not an identity at all — it maps to a corpus owned by the right tenant (see open
question 2, now derived).

**One identity spine, and residency policy becomes one more feasibility gate in `rank()`.**

### Where `TenantId` lives — forced by the layer map, not chosen

Today `TenantId(pub String)` sits in `sovereign-server/src/auth.rs:92`. As the mesh's identity spine
it must be visible to both families. The layer contract (`quality/ARCH_LAYERS.toml`) narrows this to
exactly one home:

- `commonwealth-core → sovereign-*` is **forbidden with no exception** — *"mesh foundation must stay
  consumable without the agent runtime"* (`ARCH_LAYERS.toml:125-128`). Note the contrast:
  `commonwealth-api` and `commonwealth-daemon` each carry `except = ["sovereign-contracts"]`
  (`:160-173`); `commonwealth-core` deliberately does not. **So `sovereign-contracts` is not
  available**, even though `CorpusVisibility::Private { owner }` already lives there.
- `sovereign-contracts` sits in the bottom `contract` layer, so it cannot reach *up* into
  `commonwealth-core` (`mesh-foundation`) either. The two crates that need the type cannot see each
  other in either direction.
- A new leaf crate would work but must **not** be named `sovereign-*` — the forbid rule is a name
  glob, which is why `commonwealth-core` cannot depend on `sovereign-time` despite it being a
  zero-dep contract-layer leaf.

**Decision: `TenantId` goes in `oicp-types`.** It is in the `contract` layer, it is a serde-only leaf
(`oicp-types/Cargo.toml:9-11`), and it is **already a dependency of both** `sovereign-contracts`
(`:14`) and `commonwealth-core` (`:9`). Zero new dependency edges, no `ARCH_LAYERS.toml` change, no
`[[exception]]` entry.

Naming tension, stated so it is a conscious call: `oicp-types` is named for the inference capability
protocol, and a tenancy type is a stretch there. Accepted for one small type. **Split trigger:** if a
third shared identity type appears, lift them into a neutral contract-layer leaf (`identity-types`)
rather than growing `oicp-types` into a junk drawer.

### What the spine buys immediately

Because `CorpusVisibility::Private { owner }` (`sovereign-contracts/src/types/mod.rs:904`) and a
node's clearance now carry the *same type*, residency becomes one predicate over two existing
fields:

> **A corpus owned by tenant T may only be hosted on a node whose clearance set contains T.**

That is checkable continuously, enforceable at corpus registration, and impossible to express before
this decision. It is the §7 "structural, not remembered" win the whole workstream exists for.

### Three fields, three different merge rules — get this right

Naively "make the policy monotonic" is wrong for a *set*: a monotonically-growing clearance set is
privilege escalation, not restriction. The correct split:

| Field | Where | Merge | Why |
|---|---|---|---|
| `Mesh.require_clearance: bool` | `commonwealth-core::Mesh` | **Monotonic, stricter-wins** (only ever turns ON) | Downgrade protection, exactly `require_encryption`'s contract (`mesh.rs:20-28`) |
| `NodeCapabilities.clearance: BTreeSet<TenantId>` | `commonwealth-core` | Plain LWW, like the rest of the capability struct | It is **self-descriptive** — only node D writes node D's clearance, so there is no conflicting writer to merge |
| The *authority* for an assignment | H1: operator config · H3: a capability issued by the tenant | — | See below |

The clearance set being self-declared means the risk is **forgery, not merge**: node D asserting a
clearance it was never granted. That is precisely what WS2's signed request auth closes, which is
why WS1 is advisory until WS2 lands. At H1 the assignment is operator config on each node; at H3 the
node must **present a capability the tenant issued** rather than assert a label about itself — that
is trust-ladder rung T2, and ephemeral grants (`corpus_grant.rs`) are already the right shape for
it.

**1a. Mesh-wide policy, monotonic under merge.** Add a policy block to `Mesh` beside
`require_encryption`, which already solved the hardest problem here and documents why
(`commonwealth-core/src/mesh.rs:20-28`):

> Founder-set and **monotonic**: `merge_from` only ever turns this ON (stricter-wins), so no peer —
> stale or hostile — can demote an encrypted mesh to plaintext.

Under last-writer-wins gossip, security policy is a downgrade attack: a stale peer wins by being
slow. Every policy field added here merges stricter-wins or it does not ship.

**1b. Node clearance labels.** `NodeCapabilities` is already rebuilt and gossiped every 10s
(`sovereign-mesh/src/capabilities.rs:64`). Add a clearance label set. A field on an existing struct
on an existing schedule — not a subsystem.

**1c. The gate.** `rank()` already runs an ordered feasibility filter before scoring, and records
every rejection with a typed reason rather than dropping it silently:

| Existing gate | Reason variant | Paired test |
|---|---|---|
| quarantined peer | `ExclusionReason::Quarantined` (`scheduler_core.rs:431`) | `:901` |
| no manifest | `ManifestUnavailable` (`:439`) | `:924` |
| lacks `x:forced_choice` | `NoForcedChoice` (`:464`) | `:1020` |
| no OICP claim match | `NoClaimMatch` (`:474`) | — |

Add `ClearanceMismatch` as a fifth, ahead of manifest fetch so an uncleared node is never even
asked. One variant, one branch, one test.

### Why this shape

- **The policy is enforced by the pure function that is already simulated.** `mesh_sim` replays
  production's real `rank()` at thousands of scenarios/sec. A clearance violation becomes a
  build-failing test, not a review promise (§18.1).
- **T4 gets its evidence for free.** "Why did this run on node D?" already has a recorded,
  replayable answer. H3 adds a signature to a record that already exists.
- **It is structural, not remembered** (§7). No operator can forget to configure the wall, because
  the node's own gossiped labels are what exclude it.

### Deletes (net-simplification ratchet)

- `commonwealth-inference::{InferencePlan, ShardPlan, MeshPlan, SchedulingStrategy, Orchestrator,
  tier_router}` — a second, richer scheduler vocabulary with **zero production callers**,
  constructed only in `commonwealth-test-harness`. Two scheduler concepts collapse to one.
- `POST /internal/scheduling/plan` — stores plans nothing in the tree produces.
- `POST /internal/scheduling/intent` — a stub returning `granted: true` unconditionally with an
  empty leader (`routes_internal/gossip.rs:112-114`). A gate that always passes is worse than no
  gate (§18.1).
- `NodeCapabilities.active_processes` — always empty by design.
- `MeshPeering` / `PeerTrustLevel` (`mesh.rs:31`) — carried through gossip, never populated.
  **Decide here:** the org model populates them, or they go. Carrying an unpopulated trust type into
  a security feature is the §10.6 trap wearing a disguise.

**Ratchet:** −2 concepts, −2 routes, +1 config block.

---

## Workstream 2 — an authenticated internal plane

### The hole

`:9742` binds `0.0.0.0` by default (`setup_config.rs:1043-1046`) and carries gossip, join, model
load/unload, **raw GGUF bytes**, corpus mutation, atlas state, and a fleet-wide pipeline pause with
one-hop fanout. Its only auth is a `mesh_id` + `join_key_hash` equality check on the gossip route
(`routes_internal/gossip.rs:28-45`).

This is a documented, deliberate trust-the-network posture, stated plainly at
`sovereign/docs/ENTERPRISE_FLEET_DEPLOY.md:90-95` rather than hidden. It is defensible for a
household. It is why enterprise IT says no, and it makes T2 unsatisfiable at any horizon.

### The fix needs no new PKI

Every node already holds an Ed25519 key at `<data_dir>/node_key`, gossips `node_pubkey`, and
**already signs its dial info** (`MemberRecord.dial_info_sig`, `mesh.rs:61-83`). Signed request auth
on `/internal/*` with the gossiped member directory as trust root reuses all of it — no CA to
operate, which matters because the on-prem kit already makes TLS a manual step with no ACME.

**This is also the first rung of the H3 ladder.** Once requests are signed by node key, a signed
*receipt* is the same machinery pointed the other way.

Ordering is not negotiable: WS1's labels are **self-asserted** and therefore advisory until the
asserting peer is authenticated. WS2 must land before any second organization — or any second walled
group — touches the fabric.

### Deletes

- Two stale mTLS claims asserting a control that does not exist, both sitting exactly where an
  engineer checks before trusting the port: `routes_app_internal.rs:44` ("mTLS proves the caller is
  in the mesh") and `setup_config.rs:797` ("(`:9742`, mTLS) always binds `0.0.0.0`"). A 2026-07-27
  cleanup caught this and left a marker (`routes_internal/mod.rs:8`) but fixed that file only.
  Delete ahead of the rest of the workstream; it is a two-line change and currently the most
  misleading text in the mesh.
- `SOVEREIGN_LOCAL_FIT_CHECK_SKIP` — deprecated word-order twin, already slated to collapse.

**Ratchet:** −1 knob, −2 false claims, +0 concepts.

---

## Workstream 3 — tenancy that holds

The seam exists, is tested, and is **inert in the shipped build**. Three defects, severity order.

**Re-graded after the org == tenant decision.** These stopped being "`sovereign-server` bugs" the
moment `TenantId` became the mesh's identity spine — they are now defects *in the spine*. Two
consequences: **3a moves onto PoC 2's critical path** (that demo needs a corpus actually owned by a
tenant, and the only writer of `Private { owner }` is compiled out), and **3c is a hole in the
security root**, not a server-local privilege slip. WS3 is therefore no longer freely parallel: 3a
gates PoC 2.

**3a. The `Private{owner}` writer is compiled out.** `TenantRuntime::forbidden_corpora()`
(`sovereign-server/src/tenant.rs:44`) and the retrieval ceiling (`sovereign-core/src/context.rs:70`)
both filter on `CorpusVisibility::Private { owner }`. The only production writer of that visibility
is `corpus_upload.rs:220`, behind `#[cfg(feature = "dev-routes")]` and therefore absent from the
hardened build. Every other path writes `Org`. **In the shipped configuration the deny-set is always
empty and every corpus is visible to every key.** The isolation is real in the tests and absent in
the product.

**3b. Two filters run after the SQL `LIMIT`.** `list_conversations` pages globally and then filters
by tenant prefix in Rust (`routes.rs:343`, `:347`); `POST /v1/search` does `.filter(...).take(50)`
over an unbounded query (`:420-421`). A busy second group blanks the first group's list. The on-prem
plan calls this a hard blocker for practice group #2 and is right to.

**3c. `approve_task` takes no tenant at all.** Extractors are `Extension<Arc<ServerApprovalChannel>>`,
`Path(task_id)`, `Json(body)` (`routes.rs:375-384`), and the approval key is
`format!("{task_id}:{}", body.step_id)`. Any key approves any pending task by id. Privilege bug,
smallest fix on the list.

**Do not add a workspace/matter/namespace noun here.** `tenant` is the only scoping noun in the tree
and now permanently the only one — org collapsed into it, and a matter is a corpus, not an identity.
A fourth noun proposed later is a decision to reverse this, and should be argued as one.

**Ratchet:** +0 concepts, +0 knobs, 3 defects closed, −1 noun permanently foreclosed.

---

## Workstream 4 — survivability and the operator surface

**4a. Containment must become the default.** A dying ggml-RPC worker is an uncatchable `GGML_ABORT`
that SIGABRTs the host; in-process rescue was evaluated and rejected because the asserts sit in void
paths. The containment is `[compute] distributed_primary` (`setup_config.rs:553`), which runs the
distributed primary in a supervised child.

It ships **default-off** with **no row in `sovereign/DEFAULTS_LEDGER.md`**, against that ledger's own
same-commit contract. Meanwhile: under containment, `kill -9` of a worker mid-decode at 122B
recovered in 2m35s, one respawn, zero re-warms (`DISTRIBUTED_PILOT_READINESS.md:52-66`) — and **the
same kill against the shipped default has never been run**, with the docs expecting it to be fatal.
Run the never-run test and report what actually happens (§18.1 — "never-ran" is a verdict). Then
flip the default and write the ledger row that should already exist.

**4b. Heterogeneity is a constant in the ranker.** `NodeCapabilities.benchmark` is permanently `None`
(`capabilities.rs:223`) with a guard test forbidding it (`:450-454`), because the obvious filler —
a linear size-ratio extrapolation — measured −56% and is filed DO-NOT-BUILD. So `throughput_factor`
returns a neutral 1.0 for every peer and a fleet of 4090s and laptops routes as though every box is
identical. That is exactly the "hardware you already own" case.

The honest fix is half-built: `svrn mesh bench` writes *measured* throughput keyed by (model,
placement, hardware, ctx, probe version, link) into `mesh_measurements`, which **never interpolates**
— a missing record returns "not measured" plus the command that would produce it. Feed `rank()`
measured points only. Do not arm the extrapolator. Depends on WS0 producing points.

**4c. Operator surface.** Today an admin gets `GET /health` returning the literal string `ok`,
journald text logs with no JSON and no tenant field, and an nginx access log that deliberately omits
bodies. No `/metrics`, no structured logging, no admin console, no audit trail correlating a key to a
question, no upgrade or migration path. At N boxes "which node is degraded" has no answer.

Minimum for a multi-box pilot: a fleet status view (the per-node data already gossips), structured
logs with a tenant field, and an append-only audit ledger. The ledger shape exists — `contributions`
replicates over `MeshStore` under its own `app_id`. **This is also T1/T5's substrate**: the audit
record H1 needs for operations is the same record H3 needs to counter-sign.

---

## What we will NOT build — the app platform

**Decision: `commonwealth-app` is not the path to mesh compute. Do not finish it.**

This is written down because the seam is inviting and a future session will otherwise rediscover it
and try. `SYSTEM_OVERVIEW.md:2800` calls it a "mesh app platform"; every noun in that sentence
exists and none of the wiring does:

- `POST /v1/apps/{id}/install` inserts a manifest into an in-memory HashMap. No fetch, no verify,
  no spawn (`routes_apps.rs:62-67`). The registry has no persistence — every install is lost on
  restart — and `merge` compares versions lexicographically (`registry.rs:53`), so `"2.0.0"` beats
  `"10.0.0"`.
- `AppProcess::start` — the only code that would run an app — has **zero callers**. `AppStatus::Failed`
  is never constructed; `health_check` is never called; the `Child` handle is never awaited.
- `AppPortMap::set` is never invoked, so the `/app/{id}/*` reverse proxy returns 503 unconditionally
  (`routes_apps.rs:115-124`). Even populated, it is buffer-in/buffer-out with an 8 MiB cap — no
  streaming, no SSE, no WebSocket.
- `POST /internal/app/registry` has a receiver and **no sender**; peers never learn an app exists.
- The per-app mDNS advertise/withdraw/browse functions are dead code.
- `MeshAppManifest` has six fields and cannot declare ports, resources, env, volumes, secrets,
  replicas, or a restart policy. `RequiredCapabilities` has zero readers. `AppPermissions` has zero
  enforcement points.
- `MeshStore` deletes everything older than **7 days**, mangles non-UTF-8 through `from_utf8_lossy`
  on replication, and permanently diverges on same-second cross-node writes to one key — stated in
  the test's own doc comment (`store.rs:329-337`).

Last substantive commit to the crate: 2026-06-09.

**Why not finish it.** The gap between that and running an ordinary internal service is a container
runtime, an image registry, a scheduler, a CSI, an ingress controller, secrets, and metering. That
is k3s, which is free, mature, and does all of it. Competing there spends years to reach parity on a
commodity.

**What to build instead.** The path to general mesh compute runs **through the ingest queue, not
through `commonwealth-app`**. The queue is already a working distributed job system — leases,
heartbeats, completion, revocable grants, eviction — carrying a domain-specific payload. Generalizing
that payload is a far shorter path than resurrecting code that has never executed anything. And what
Commonwealth has that k3s structurally cannot is exactly the north star: compute across machines that
do not trust each other and are not in one datacenter.

Note also that two unrelated things are called "MeshApp": the shipping one is a sandboxed static
HTML corpus explorer in a Tauri webview (`MESHAPP_AUTHORING.md:4-5`, seven of them in
`sovereign-desktop/public/meshapp/`), with a manifest schema incompatible with
`commonwealth_app::MeshAppManifest`. Nothing converts between them. Keep the names straight in any
future discussion.

**Deletes:** on accepting this decision, `commonwealth-app`'s dead half — `AppProcess`,
`AppPortMap`, the `/app/*` proxy, the app mDNS trio, and the senderless registry route — should be
removed rather than left as a trap. The manifest and registry may survive if the desktop explorer
path wants them.

---

## Risk register

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| 1 | **Ingest does not scale linearly either.** The mission's second half is false at fleet scale. | Kills the premise | WS0 runs first with a written kill criterion. |
| 2 | Customer machines are not on one campus network. Relay floor 5.5–7.1 tok/s vs 17.3–17.9 direct. | Scopes the market | State the LAN requirement up front, as a feature — the office is the fabric. Do not sell to distributed workforces at H1. |
| 3 | WS1 clearance labels are self-asserted; an attacker node claims any label. | Critical if WS2 slips | Sequencing enforces WS2 before any second group or org. Do not reorder. |
| 4 | Machines are workstations people are using; pooling steals the GPU mid-workday. | Adoption blocker | Cluster load awareness exists — a peer's gossiped self-reported in-flight overrides the decider's local observation, so a workstation serving its own user is not seen as phantom-idle. Verify under WS0; needs a stated foreground-yield policy. |
| 5 | Host and worker must run the same llama.cpp build (ggml-RPC is wire-version-sensitive). | High, operational | Wire-version handshake is a known-open P1. Ship one binary set per fleet; make skew a startup refusal, not a crash. |
| 6 | Adding org/clearance to gossip breaks wire compat with existing meshes. | Medium | `#[serde(default)]` + monotonic merge, exactly as `require_encryption` did it. |
| 7 | A fourth scoping noun appears (org vs tenant vs workspace vs matter). | Medium, architectural | WS1 decides org-vs-tenant before WS3 touches scoping. Open question #1. |
| 8 | Single-box on-prem rehearsal still owed; building N-box on an unrehearsed foundation. | High, process | The clean-VM rehearsal is a hard prerequisite to WS1. |
| 9 | **H3's T3 needs cryptography we do not have.** Verifying a citation without re-disclosing the corpus is a research problem. | Deferred, not ignored | H3 is multi-year. T1/T2/T4/T5 are engineering; T3 is the one that may need a commitment scheme. Do not promise T3 in H1 or H2 materials. |
| 10 | **`TenantId` is an unvalidated free-text `String` and is now the security root.** `"Firm"`, `"firm "` and `"firm"` are three tenants; a clearance check passes or fails on whitespace. | Medium, but structural | Canonical form + validator land with the move to `oicp-types`, not after. Open question 4. |
| 11 | **The governance primitives are project-scoped and live in a high-layer CLI crate.** Re-scoping them to a consortium, and lifting them somewhere the daemon can enforce them, is real work. | Medium, H3-only | Do not represent consortium governance as shipping. The mechanism transfers; the schema and crate location do not. |

---

## Sequencing

```
[onprem clean-VM rehearsal]          ← PREREQUISITE, already owed

H1 ── PoC 2a  residency, positive     ← ZERO BUILD. TWO_NODE_QUICKSTART already does this.
       │                                Demo it now; it is mission half 1, already true.
       │
      WS0  ingest scaling curve       ← measurement only; can kill the plan
       │
       ├── linear? ──no──►  rescope to "box plus one helper"; stop
       │
      yes
       │
      PoC 1  bulk ETL burst           ← mission half 2. Doubles as WS0's deliverable.
       │
H2 ── WS3a  Private{owner} writer     ← gates PoC 2b; smallest item on the critical path
       │
      WS1  trust domain               ← TenantId→oicp-types, clearance set, ClearanceMismatch
       │
      PoC 2b  residency, negative     ← the refusal demo. First customer of the keystone.
       │
      WS2  authenticated :9742        ← MUST precede any second group or org
       │
      WS3b/c  tenancy defects         ← parallel with WS1/WS2; different crate
       │
      WS4  survivability + ops        ← 4a anytime; 4b needs WS0's points
       │
H3 ── receipts: T1, T5 → T4 → T2      ← sign the events that already exist
      T6 governance re-scope          ← charter/decision records, founder accountability
      T3 last, and only if the research lands
```

**PoC 2a is available today and should be demoed before anything is built** — it is the mission's
first half, already true, and it costs a walkthrough rather than a workstream. Everything else waits
on WS0's verdict.

WS3b/c still parallelize — they live in `sovereign-server`, not the mesh. WS3a no longer does: it is
the smallest item on the critical path and it gates the refusal demo. WS4a (the never-run kill test)
is independent and cheap.

---

## The two PoCs

Both are H1. Together they are the mission sentence, demonstrated rather than asserted.

### PoC 1 — bulk document ETL across machines you own

**Displaces:** Textract / Comprehend / Glue / EMR line items, which price per page and recur.

**Built already:** the pull-based lease/heartbeat/complete queue (`corpus_queue.rs`), ephemeral
grants that are bounded, renewable, and revoke-fails-closed with the `grantable` marker enforced in
exactly one place (`corpus_grant.rs:8-17`), and `partition_evict` — peers wipe their working
partition on teardown and log it (`corpus_queue.rs:694-722`).

**The demo:** point N machines at one document set; watch pages/hour rise with N; revoke the grant
mid-run and watch peers exit their pull loops and wipe their partitions.

**The number it produces:** pages/hour against node count at 1/2/4. This is WS0's deliverable and
the PoC at once.

### PoC 2 — federated retrieval with custody

**Displaces:** managed vector search, *and* the "our data went to a third party" objection.

This splits in two, and the split matters: the positive half needs no build at all, and the negative
half is where the keystone earns its keep.

**PoC 2a — residency, positive. Zero build; demo it now.**
`docs/TWO_NODE_QUICKSTART.md` already does this end to end: node B asks, node A's corpus answers
with cited snippets and provenance, index bytes never move because `mesh_sharing = false`, and a
custody ledger entry is recorded. The doc carries its own glassbox verification section. This is
mission half one, already true, and nothing in this plan is a prerequisite for showing it.

**PoC 2b — residency, negative. The refusal demo.**
Today `mesh_sharing = false` means "don't replicate my index, but *do* answer anyone." The
enterprise needs "don't answer this querent at all." That is exactly WS1's `ClearanceMismatch` gate,
so 2b is the keystone's first customer — and it also needs WS3a, because the demo requires a corpus
genuinely owned by a tenant and the only writer of `Private { owner }` is currently compiled out.

**The demo:** the same question from two principals. One returns a cited answer; the other returns
`cannot_know_from_here` **plus a decision-log entry naming the excluded node and the reason.** That
is the product claim on one screen — and it is T4's evidence, unsigned.

---

## Resolved (2026-08-04, operator)

- **Org and tenant are the same spine.** No `OrgId`. `TenantId` moves to `oicp-types` — the only
  crate in the bottom layer that both families already depend on and that the forbid rules permit.
  A matter is not an identity; it is a corpus owned by a tenant. WS1 is unblocked. *(WS1 "The design
  call")*
- **The unit of clearance is the node, over tenant ids** — derived from the above, since the node's
  clearance set and a corpus's `Private { owner }` now carry the same type. Coarse, and to be stated
  plainly as coarse in customer material: a matter is isolated by living in its own corpus owned by
  the right tenant, not by a per-matter access rule.
- **The founder node mints consortium membership** and is accountable for its own governance through
  the charter / amendment / decision-record machinery, verifiable by every member. *(North star,
  "Who mints consortium membership")*

## Open questions

1. **Does the front door need to fail over,** or is manual failover an acceptable H1 answer? Manual
   is fine — but it must be written in the customer brief, not discovered.
2. **Do we pool workstations or only dedicated boxes?** The interesting answer is workstations; the
   sellable one is probably "a few dedicated boxes plus opt-in workstations."
3. **What is the upgrade story for N machines** when there is not one for a single machine yet?
4. **What is a `TenantId`'s canonical form?** It is a bare `String` today
   (`sovereign-server/src/auth.rs:92`) and `install.sh` issues one key mapped to the literal
   `"firm"`. As the security spine it needs a canonical form and a validator — otherwise `"Firm"`,
   `"firm "` and `"firm"` are three tenants, and a clearance check silently passes or fails on
   whitespace. Small, but it is now a §7 invariant rather than a nicety.
5. **At H3, what happens when the founder is wrong?** Governance makes the founder's actions
   *visible*; it does not make them *reversible*. Whether a consortium needs an exit path (fork the
   mesh, elect a new founder, or dissolve) is a governance-design question, not an engineering one,
   and it should be answered in the charter template rather than in code.

---

## Appendix — verified ground truth

Checked against source on 2026-08-04 (§11.1). Items marked *(doc)* cite repository markdown whose
underlying runs were not re-executed here.

### The mesh has no trust model

- Auth boundary is one equality check, gossip route only — `routes_internal/gossip.rs:28-45`. The doc
  comment names itself "the auth boundary".
- `/internal/scheduling/intent` returns `granted: true` unconditionally — `:112-114`.
- `Mesh` is flat: `id`, `name`, `join_key_hash`, `require_encryption`, `members`, `peers` —
  `commonwealth-core/src/mesh.rs:12-31`. No org, group, tenant, or role.
- `require_encryption` is **monotonic under merge**, stricter-wins, explicitly so no stale or hostile
  peer can downgrade — `mesh.rs:20-28`. **The template for WS1.**
- `internal_bind` defaults to `"0.0.0.0"` — `setup_config.rs:1043-1046`. The client port defaults to
  `127.0.0.1` (`:975`); do not conflate them.
- Two surviving stale mTLS claims: `routes_app_internal.rs:44`, `setup_config.rs:797`. The 2026-07-27
  correction is recorded at `routes_internal/mod.rs:8,17` and fixed that file only.

### The ranker is where policy belongs

- `rank()` is pure — no I/O, no clock, no interior mutability — `scheduler_core.rs:317`.
- Typed exclusion reasons: `Quarantined` `:431`, `ManifestUnavailable` `:439`, `NoForcedChoice` `:464`,
  `NoClaimMatch` `:474`; paired tests `:901`, `:924`, `:1020`.
- `NodeCapabilities` rebuilt and gossiped every 10s — `capabilities.rs:64`.
- `benchmark` permanently `None` (`:223`) with guard test `gossip_never_advertises_a_benchmark`
  (`:450-454`). Heterogeneity is a constant in scoring today.

### Residency and custody are real

- `docs/TWO_NODE_QUICKSTART.md:1` — "a cited answer from a corpus that never leaves its machine";
  custody ledger at `:76`, glassbox verification at `:89`.
- Ephemeral grants: `grantable` enforced in exactly one place; revoke revokes the capability, fails a
  concurrent collaborate closed, retires the queue, and drops the gossiped handoff blob; idempotent;
  never mutates on-disk `CorpusMeta` — `corpus_grant.rs:1-21`, `:139-174`.
- Pull-mode queue is `corpus_next_unit` / `corpus_heartbeat` / `corpus_complete_unit` —
  `corpus_queue.rs:1-16`.
- `partition_evict` wipes the peer's working dir and logs the teardown — `corpus_queue.rs:694-722`.

### Tenancy exists and is inert

- Readers of `CorpusVisibility::Private { owner }`: `tenant.rs:44`, `context.rs:70`,
  `sovereign-contracts/src/types/mod.rs:904`.
- Only production writer: `sovereign-server/src/corpus_upload.rs:220`, behind `dev-routes`.
- Filter-after-LIMIT: `routes.rs:343` + `:347`, and `:420-421`.
- `approve_task` carries no `TenantId` — `routes.rs:375-384`.

### The app platform is a seam

- `AppProcess` zero callers; `AppPortMap::set` never called ⇒ proxy always 503 (`routes_apps.rs:115-124`);
  `/internal/app/registry` receiver with no sender; app mDNS trio dead; `install` is a HashMap insert
  (`routes_apps.rs:62-67`); registry unpersisted with lexicographic version compare (`registry.rs:53`).
- `MeshStore`: 7-day TTL, UTF-8 mangling on replication, same-second cross-node divergence
  (`store.rs:329-337`).
- Worker pods: single-tenant, one job; the only production runner POSTs HTTP to a child Sovereign
  daemon; the only `WorkerProvider` impls are Vast.ai — a rent-a-GPU controller, not on-prem.

### Measured performance *(doc)*

- 122B, 2 nodes, 36/12 blocks, head home: **17.3 / 17.9 / 17.8 tok/s vs 14.8 solo, ~20% better** —
  `DISTRIBUTED_PILOT_READINESS.md:543-544`.
- 4B: tunnel 39.6–41.0 vs direct 40.0–41.0 tok/s — tunnel tax invisible — `:533`.
- Relay floor **5.5–7.1 tok/s** (`QWEN122B_DISTRIBUTED_HANDOFF.md:33`); restated 5.8–7.1 from bench
  (`DISTRIBUTED_PILOT_READINESS.md:482`, `:536`). Ranges disagree slightly; treat 5.5 as the floor.
- LAN round trip 10.9–13.3 ms per 16 KB — `:507`. 600 KB logits return caps decode at ~12 tok/s even
  on LAN, which is why the output head stays home — `:494`.
- Worker `kill -9` under containment: 2m35s, one respawn, zero re-warms — `:52-66`.
- **Everything is two-node** — `:4`, `:273`. Retracted figure kept as this plan's reporting standard —
  `:570-586`.

### The layer contract that decides where `TenantId` lives

- `TenantId(pub String)` is currently at `sovereign-server/src/auth.rs:92`.
- `[[forbid]] from = "commonwealth-core", to = "sovereign-*"` — **no `except`** —
  `quality/ARCH_LAYERS.toml:125-128`, reason *"mesh foundation must stay consumable without the agent
  runtime"*. `commonwealth-api` (`:169-173`) and `commonwealth-daemon` (`:160-164`) each carry
  `except = ["sovereign-contracts"]`; `commonwealth-core` does not.
- The forbid is a **name glob**, so it also rules out `sovereign-time` — the zero-dep contract-layer
  leaf that would otherwise be the obvious precedent for a new shared primitive crate.
- Layer order: `contract` (`oicp-types`, `sovereign-contracts`, `oicp-client`, `arch-layers`,
  `sovereign-time`) is below `mesh-foundation` (`commonwealth-core`, …), so `sovereign-contracts`
  cannot reach up either. The two crates that need the type cannot see each other in either
  direction.
- `oicp-types` is a serde-only leaf (`oicp-types/Cargo.toml:9-11`) already depended on by
  `sovereign-contracts` (`:14`) **and** `commonwealth-core` (`:9`). Zero new edges.
- `sovereign-server` depends on both `sovereign-contracts` (`:66`) and `commonwealth-core` (`:78`),
  so it can bridge whatever the lower layers cannot.

### Governance primitives (for H3 / T6)

- `CharterSections`, `parse_charter_sections`, `changed_sections`, `serialize_charter_sections` —
  `sovereign-cli-dev/src/amend.rs:59, 75, 128, 451`. Amendments bump `charter_version` (`:231`).
- `DecisionRecorder` trait with `NoteStoreDecisionWriter` impl — `sovereign-cli-dev/src/found.rs:305,
  338`. Non-private notes gossip mesh-wide keyed by `content_hash`.
- **Charter drift detection** — recompute the on-disk `CHARTER.md` hash against the recorded one,
  four outcomes (`none` / `differs` / `unknown` / `n/a`) — `sovereign-cli-dev/src/project_cmd/audit/mod.rs:110-130`.
- Charter skeleton + amendment flow — `sovereign-cli-dev/src/project_cmd/charter_amend.rs:36, 180`.
- Adjudication analogue — `sovereign/docs/GOVERN_A_CORPUS.md` ("surface tensions, resolve them into
  common law"): establish a governed baseline, see tensions, adjudicate, ask what the law is.
- **Scope caveat:** all of the above governs *software projects* and lives in `sovereign-cli-dev`, a
  high-layer CLI crate. The mechanism transfers to consortium governance; the schema and crate
  location do not.

### Related docs

`sovereign/docs/ENTERPRISE_FLEET_DEPLOY.md` is the closest existing document to this framing and
states the unauthenticated-internal-port posture plainly at `:90-95`. `sovereign/docs/MESH_NETOPS.md`
is written for a security team approving a deployment and carries a §5 "open validation (not yet
proven — do not represent as tested)". `sovereign/docs/specs/SCHEDULER_QUALITY.md` is the scheduler
design reference. `docs/THREAT_MODEL.md` carries the known-gaps list this plan updates on landing.
