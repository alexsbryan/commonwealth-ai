# The work plane — design

**Status:** DRAFT design, nothing landed. Written 2026-09-04 from an operator-directed
exploration ("commonwealth as the rails for a general distributed compute mesh — for
scientific researchers losing HPC access, and for arbitrary workloads"); **re-cut onto the
ring rail 2026-09-09** and landed as cw-lift Phase 5 (`quality/campaigns/cw-lift.toml`,
rungs 5a–5h). This is the design deliverable [`PLAN.md`](PLAN.md) Phase 3 was gated on
("design-gated after Phase 2 starts"), and it answers PLAN.md open question 7.

**Read the audit first.** The 2026-09-04 cut rested on machinery that mostly does not exist
as described. §"What is true in code" is that audit, sixteen rows, checked at file:line on
2026-09-09; it overrides every claim in this document that predates it, and it is the reason
for the re-cut. Four things changed: the substrate is the **ring journal**, not a `JobKind`
envelope grown out of `WorkUnit`; the pilot kind is **`process:v1`** — one command run from a
source checkout at a pinned git rev — not OCI; the **inference plane is not the first
customer** and no unit in v0 touches a model; and six nouns became four, because `Grant` and
`Placement` were both mechanisms this repository already has under other names.

**Evidence discipline:** as PLAN.md — every load-bearing claim about current code is verified
at file:line and cited inline. Design assertions are marked *(design)*. Where the 2026-09-04
cut cited a path that does not exist, the audit gives the real one rather than quietly fixing
it: a wrong crate name is how a design gets built against the wrong seam.

**Compass:** §2 closed sets are enums, open sets are registries · §7.5 identity from
essence · §10.6 one decider, one name · §11 cite, don't recall · §18.3 never silently
substitute · §19 the inventory outranks the plan.

**Companions.** [`docs/CMNWLTH_DESIGN.md`](../../../docs/CMNWLTH_DESIGN.md) (2026-08-16)
defines the same ontology under other names — `Job`, `Executor`, `Claim`, `Selector`,
`JobRail` — and those nouns map **one-to-one** onto the ones below; that document now carries
a superseded-by header pointing here, with the mapping table in it. It stays as the ontology
and the use-case set; it is not a live design and it is not deleted.
[`sovereign/docs/WORK_ATLAS.md`](../../docs/WORK_ATLAS.md) owns the word "work" for
**agent-coordination** claims on this same rail — advisory, deliberately not a lock manager —
and nothing here extends it; a `Lease` below is not an atlas claim and the two never share a
type.

---

## What is true in code (audit, 2026-09-09 — overrides this document's own history)

Checked at file:line on 2026-09-09 against `5c98898c7`. Each row is a claim this document
made and the fact that replaces it. This table is the rung's first deliverable (§11.1: the
corrections come before the mechanism).

| Claim in this doc, before 2026-09-09 | Fact today |
|---|---|
| Donor aborts when the coordinator vanishes (§Resolved 1, "already on the wire") | The server returns **404** with `Reclaimed { reason: "handoff not found" }` (`corpus_queue.rs:378-381`). The peer heartbeat loop acts only on **410** (`auto_ingest.rs:1175`) and drops everything else into two debug catch-alls (`:1187`, `:1195`), so the donor keeps ingesting into a lease nobody holds. There is also no miss counter: a coordinator that goes silent forever is never noticed. **Live bug, fixed at 5a.** |
| `TenantId` lives in `oicp-types` (the "precedent" the JobSpec promotion leans on) | Only `sovereign-server/src/auth.rs:92` — 29 refs, 7 files, one crate. **No validator exists**; `auth.rs:35` reads `/// Extract tenant_id from a valid API key.`, which claims extraction, not checking. The leaf precedent that does exist is `ToolDescriptor` (`oicp-types/src/tool.rs:112`). **Moved at 5a (PLAN.md F1).** |
| The work queue is settled | The **pull** path is already the default; the gate is `SOVEREIGN_USE_LEGACY_PARTITION` (`commonwealth-api/src/routes_internal/corpus_collaborate.rs:41`, `use_pull_queue()` `:43-46`). `SOVEREIGN_USE_WORK_QUEUE` survives in exactly two comments (`corpus_collaborate.rs:259`, `server.rs:380`) and no code reads it. |
| `EmbedModelMismatch` is the refusal pattern to generalize | **Zero constructors** — only the variant's definition (`work_queue.rs:73`). The refusal pattern that actually works is `PeerNotAllowed`: constructed once (`work_queue.rs:241`), enforced as 403 at the route (`corpus_queue.rs:310-314`). |
| `WorkerProvider`: Vast, RunPod | Trait at `worker_controller.rs:73`. **Two** non-test impls, both Vast (`worker_pod_provider.rs:110`, `:218`); the other five are test mocks. RunPod is four doc comments (`worker_controller.rs:22,72`; `worker_pod.rs:6,187`) and no code. |
| `JobSpec` generalizes additively | `worker_controller.rs:99`, nine fields. Four (`image`, `disk_gb`, `gpu_name`, `max_price_per_hour`) are Vast knobs, cloned only by `derive_pod_spec` (`multi_pod_coordinator.rs:418-421`). Only `image` is asserted by a test (`:497`); the other three are asserted by nothing. Both real providers take the spec as `_spec` and never read it. |
| `SchedCore`'s deficit ordering is a neighbour to copy | The rule is **weight-then-FIFO** (`serving-policy/src/fair_sched.rs:349-356`) plus a per-origin equal-share cap (`:163`, enforced `:345-347`). The word "deficit" appears nowhere in the crate — and the crate is `serving-policy`, not `sovereign-mesh`. |
| `rank()`'s typed exclusions are reusable from `peer_inference` | `rank()` is `pub(crate)` (`scheduler_core.rs:323`) and `ExclusionReason` is a closed five-variant enum (`decision_log.rs:479`). Nothing outside that crate can call either. |
| Grants are "reused with one extension" | `EphemeralIngestGrant` (`ingest_grant.rs:52`) is keyed by `corpus_id` with `allowed_peers: Vec<NodeId>` — corpus-typed, not kind-typed, so "per-kind grants" is a new type, not an extension. Four grant types exist (`GuestGrant`, `EphemeralIngestGrant`, `ConsentGrant`, `TryGrant`); seventeen type names contain the word. |
| The activity signal is a subscribable hot/idle bus | Four levels (hot 0.20 / warm 0.65 / cool 0.85 / idle 1.00, `mesh_admin.rs:38-46`) delivered by a **fire-and-forget POST** (`sovereign-server/src/activity.rs:132-160`) into an `RwLock<f32>` (`state.rs:880`). Nothing subscribes. What *is* a usable seam is the pollable `AppState::should_yield_to_foreground()` (`commonwealth-api/src/state.rs:2115`). |
| `sovereign-compute` supervision is shared machinery a job executor can sit in | **Two** dependents — `sovereign-cli-daemon` and `sovereign-desktop` — both supervising the inference child, health checked by HTTP GET poll (`supervisor.rs:960`). It is a long-lived-child supervisor with its own restart decider, not a general executor host. |
| The `compute-distribution/` plan artifact already crosses the wire | `DistributionHandoff` (`sovereign-compute/src/distribution.rs:35`) is a JSON file written with `std::fs` (`manager.rs:885`) and read back **on the same host** (`child_main.rs:363`). Loopback, never a wire format. |
| The three `WorkUnit`s are one type wearing costumes | Three types, three lifecycles, one name: `commonwealth-core/src/knowledge.rs:319` (a closed three-variant ingest enum), `worker_http.rs:81` (`kind: String`, read only by the `echo` stub runner `worker_daemon.rs:88` and a test runner `:1142`, never by the production `SubprocessRunner`; plus `JobManifest :92`, `CompletedUnit :105`), and `sovereign-pipeline/src/worklist.rs:47` (a sqlite row). |
| `principal` is a new noun the job plane must mint | On the rail the actor **is** the signing key, and the roster binds it to a person. Nothing to mint. |
| "the ledger credits the donor" | `LedgerEventKind` (`commonwealth-core/src/contributions.rs:59`) is a closed five-variant set with no compute variant. There is nothing to write today; 5h adds one and reuses the existing emitter (`commonwealth-state/src/contributions.rs:73`). |
| Sandboxed script execution exists to build on | `bwrap`, `firejail`, `nsjail` and `--network=none` return **zero hits** workspace-wide; the only `podman` builds release artifacts and runs CI. `sovereign-tools/src/compute.rs:8` says "Sandboxed Python code execution tool" over an implementation (`:30-59`) that spawns `python3` with a 30-second timeout and no isolation of any kind. The word is aspirational. |

Two consequences the table forces, rather than leaves as taste. **`Placement` cannot copy the
two neighbours it named** — one is `pub(crate)` behind a closed enum, the other implements a
different rule under the cited name — so v0 ships no scorer at all. And the OCI payload
contract has **no floor under it**: this repository contains no sandbox mechanism, so a design
that leans on "rootless, digest-pinned, network-isolated" is leaning on prose.

## Mission

> **Any member's LLM-free work runs anywhere a member has consented to run it — carried by
> signed acts on the rail the mesh already replicates.**

Three test sentences, after PLAN.md's pattern:

1. **Consent is mechanical.** A unit runs on a node only if the submitter's act names that
   node and the node's own `Offer` act names that kind; anything else is a typed refusal in
   the fold, never a silent best-effort.
2. **Self-hosting, without inference.** This repository's own test suite and ratchets run as
   units on the plane, split across two nodes, merged counts identical to a local run, Actions
   minutes at $0 — PLAN.md's Phase 3 demo verbatim, and its rung 5.
3. **Heterogeneous by construction.** The same kind carries a cargo test shard, a Python
   simulation, and a shell one-liner submitted by a program with zero sovereign in it.

The user story this serves: researchers who already trust each other pool the machines they
have — laptops, lab boxes, a rented GPU or two — and run sweeps, simulations, and batch work
overnight instead of losing it to a dead allocation. The north star remains PLAN.md rung 10
(consortium compute exchange); this design is the rungs 4–7 machinery with rung 9 (services)
arriving as a consequence rather than an extension.

## The axiom: two tiers, never one

The single load-bearing decision, which a naive "everything is a job" gets wrong:

| | Control plane (jobs) | Data plane (never jobs) |
|---|---|---|
| Question | *where does something stand, for how long, on whose consent* | *how does traffic reach the standing thing* |
| Today | ingest handoffs; model warm + slot lifecycle; rented-pod workloads; bench trials | decode turns; retrieval fan-out; artifact fetch |
| Noun here | lease + kind + requirements + provenance | request/response over the fabric |

A decode turn is not a job and must never become one — it is a millisecond-class routing
decision against a live table, and the job plane leases in seconds-to-minutes. Squashing the
tiers contaminates both (TTFT dies in a queue sweep; the envelope bloats with latency
concerns). *(design)* The inference plane's eventual mapping is two units — `model-warm` and
`model-serve` (a long lease with health and restart policy, PLAN.md rung 9's exact words) —
plus traffic to that lease on the data plane. **That mapping is H2 and is not built in v0**;
see §"No inference in v0".

## The contract — four nouns on the rail

The 2026-09-04 cut had six nouns and a deliberately-not-noun. Two of the six are gone: `Grant`
became an intersection of two fields the other acts already carry, and `Placement` had no
neighbour it could actually copy (audit rows 7 and 8). What is left is four.

**One namespace, `work`.** *(design)* Acts are `Payload`s under `RailAct::Record`
(`commonwealth-rail-core/src/lib.rs:200`) with a `kind` discriminator readable without
decoding — the codec and fold shape `measurements_rail.rs:152` already uses for the one
non-KV fold in the tree. The fold never sees `Seal`; corrections void as the rail already
does. `work` deliberately does **not** join `DAEMON_OWN_NAMESPACES`
(`ring_roster.rs:251`): that would flip its roster to derived and orphan the
operator-written `roster.json`. In v0 the roster is the on-disk app-ring roster, written by
hand on each node.

**1. `WorkAct` — the closed set of things that can happen** *(design)*, in a new
`commonwealth-work` package crate: `Submit { handoff, kind, units, allowed, ttl_secs }`,
`Offer(WorkOffer)`, `Lease`, `Renew`, `Complete { handoff, unit_hash, outcome, result,
provenance }`, `Fail`, `Revoke`. `Submit.allowed` keeps the tri-state
`HandoffQueue.allowed_peers` already has (`work_queue.rs:130`: `None` open, `Some(∅)`
self-only). **There is no `Grant` noun**: `Submit.allowed ∩ Offer.accept_from` *is* the grant,
and it is two fields on acts that had to exist anyway.

**2. `WorkOffer` — the node-side sharing declaration** *(design)*. Named apart from
`sovereign-pipeline`'s `Offer` (`pod.rs`, an unrelated pipeline noun; the rename-apart is
deliberate and is priced at 5b). Config-as-data, published as an `Offer` act, latest per actor
wins: kinds offered, concurrency budget, `yield_to_foreground`, isolation level, os, arch,
local repo checkouts, and `accept_from` — the donor's own consent list. This is the HTCondor
classad half and the concrete form of PLAN.md rung 7's "yield policy unstated": the offer IS
the statement, and it reads the pollable `should_yield_to_foreground()` that already exists
(`commonwealth-api/src/state.rs:2115`) rather than the fire-and-forget activity POST the
2026-09-04 cut mistook for a bus.

**3. `JobExecutor` — the seam on the donating node** *(design)*. Named apart from
`sovereign-core`'s `Executor`. A trait plus registry — `descriptor()`, `validate()`,
`execute()` — in the shape `StepRegistry` and `ToolRegistry` already prove, copied rather than
depended on because both are outside the package. It does **not** sit inside the
`sovereign-compute` supervisor: audit row 11 says that is a long-lived-child supervisor with
its own restart decider and two inference dependents, and putting a second restart decider
behind it would be §10.6 at crate scale. The descriptor carries an `isolation` claim; v0 names
one level and says which.

**4. `JobUnit` — the unit of work, in the leaf** *(design)*, `oicp-types/src/job.rs`,
String/u64/`Value` only, zero new dependencies: `{ envelope, kind, unit_hash, payload,
requirements, tenant }`, alongside `JobKind{id, version}` spelled `id:vN` — the spelling
`manifest::features` already uses — and `JobRequirements{repo_rev, os, arch, preconditions}`,
whose `Precondition` is `kernel_types::quality::Precondition`, which already spells the
toolbox as `Container` and `python3` as `Binary`. **`unit_hash` is a content hash over the
rail's own canonical form** (`Payload::new`, `payload.rs:98` — recursive sorted keys,
fractional numbers refused, >64 KiB refused): one canonical writer in the tree, and identity
from essence rather than a counter (§7.5). This is where the 2026-09-04 cut's "promote
`JobSpec`" goes — and it is a **new type, not a promotion**, because four of `JobSpec`'s nine
fields are Vast knobs (audit row 6).

**The one predicate.** *(design)* `may_take(&proj, self_key, &offer, unit, now) -> Result<(),
WorkRefusal>`, closed: `KindNotOffered`, `VersionSkew`, `NotAllowed`, `IsolationBelow`,
`RequirementUnmet(Precondition)`, `AlreadyLeased`, `Abandoned`, `PayloadNotCanonical`,
`Concurrency`, `Yielding`. Run donor-side before appending, and again by `svrn job status` —
the validate/writable split the expenses ring template already ships. **This replaces
`Placement`.** v0 refuses; it does not rank. A fair-share ordering needs a scorer, the two
scorers the first cut named cannot be reached (audit rows 7, 8), and a second construction of
a score is the `mesh bench`/`mesh plan` trap this workspace has already paid for once.

**The fold.** *(design)* `WorkProjection::fold(&Admission)` — pure, order-independent given
admission's total order. Unit status is `commonwealth_core::knowledge::UnitStatus`
(`knowledge.rs:348`) with `peer: NodeId` generalized to the actor key; it already carries
`Queued{prior_attempts}`, `Leased{expires_at_ms, attempts}`, `Complete`, `Failed`.
`HandoffPhase` (`:388`) is taken whole. Expiry re-queues through `LeasedUnit::is_live_at`
(`:452`, "the one place the comparison is written") with `LEASE_MS` (`:423`) and
`MAX_UNIT_ATTEMPTS` (`:419`) — **no new constants**. A second lease on a held unit is a
reported `lost_leases` row; an act for a unit the actor never leased is `unreadable`. Both are
`rail_kv::project`'s existing discipline (`rail_kv.rs:286`): report, never drop (§18.3).

**The deliberately-not-noun: results.** A `Complete` act carries a `Value` capped at the
rail's own 64 KiB and a `kernel_types::Judgement` — **one outcome vocabulary for the whole
workspace**, so exit 4 and exit 5 become `CouldNotJudge` and a unit the cohort refuses becomes
`NeverRan`, rather than a third verdict set. Results over the cap are refused, never
truncated. `MeshStore` stays what it honestly is — a cache (PLAN.md rung 8, deferred).

## The payload contract: a command, not an image

The 2026-09-04 cut adopted OCI as the payload contract. The audit removed its floor: this
repository ships **no sandbox mechanism at all** (row 16), so "rootless, digest-pinned,
network-isolated" described intent, not a mechanism a donor could enforce.

- **v0 is `process:v1`, trusted-native, and the descriptor says so.** A unit is one command
  run from a source checkout at a pinned git rev: `{argv, cwd, stdin?, env, timeout_secs,
  result}`. Trust comes from the ring being a social-trust group whose members already run
  each other's code by joining — which is the honest statement of what is true today, rather
  than a sandbox claim with nothing behind it.
- **The body is copied, not re-derived.** `run_shell`'s spawn-with-timeout — `current_dir`,
  stdin null, `kill_on_drop`, `process_group(0)`, PGID `kill -KILL` on timeout, tail cap —
  plus the workdir resolver that rejects absolute paths and `..`, plus the
  colour-normalization env block that the 2026-08-25 `0p/0f` incident earned. It is a seventh
  spawn-with-timeout in the workspace **only because the package cannot depend on the six that
  exist**, and that deviation is named in the ladder rather than left for a reviewer to find.
- **Donors keep one reused worktree per repo**, checked forward to the unit's `repo_rev`, so
  `target/` stays warm — `ensure_worktree`'s recipe, which `evidence-verdict.py` already
  proves. No per-unit worktrees.
- **OCI is H2, and it is named**: `oci:v1` as a second kind — podman rootless, digest-pinned,
  a host-side egress proxy holding the job token. It arrives as an executor registration, not
  as a change to any seam above.

## No inference in v0

The 2026-09-04 cut made the inference plane the first customer. It is not one. **No unit in v0
touches a model**: `JobRequirements` carries no model field, `JobContext` carries no model, and
no executor in the package names one. There is no judge, no bench lane, and no fingerprint on
this plane.

The reason is the pilot, not squeamishness. The in-house customer is this repository's CI —
per-crate test shards, evidence verdicts, lint scopes, xtask ratchets — and every one of those
is LLM-free, which makes the *whole* judge-heterogeneity risk (below) inapplicable to v0
rather than merely mitigated. Adding a model to the first payload would have imported that risk
for nothing.

What the inference plane's machinery still teaches, for when the H2 adapter is built: worker
eligibility (settle / flap / quarantine) is a donor trust state machine every kind wants, and
the `MeasurementKey` discipline is the placement-scorer pattern *if* a scorer is ever earned.
Neither is reachable from here today (audit rows 8 and 11), so neither is a v0 dependency.

## Pressure-test verdicts (2026-09-04, re-judged 2026-09-09)

| Risk | Severity | Verdict | Answer |
|---|---|---|---|
| Judge heterogeneity | High — silent pilot killer | **out of scope in v0** | No unit carries a model, so there is no judge to mix. The risk returns with the H2 inference payload and the answer is unchanged: `requirements` pins an exact model identity, the cohort forms homogeneously or refuses, §18.3 |
| Coordinator SPOF overnight | High — loud pilot killer | **NOT resolved on 2026-09-04; resolved differently now** | The 2026-09-04 row claimed donor abort-on-coordinator-loss was "already on the wire". It is not — the donor drops the 404 into a debug catch-all (audit row 1), which is the live bug 5a fixes. The structural answer is the rail: the journal is the record, every node folds it, and there is no single queue to lose |
| ACE via any executable payload | High beyond cohort 1 | **stated, with no mechanical floor, and said plainly** | v0 is trusted-native on a social-trust ring. The 2026-09-04 row claimed "OCI rootless + digest-pinned + host-proxy egress" as a floor; no such mechanism exists in this repository (audit row 16). Cohort 1 is two machines with one owner |
| Environment bootstrap / PyPI / wheels | Medium | **carried by the checkout, not an image** | A unit runs in a repo checkout at a pinned rev on a donor that offered that repo. Anything the donor cannot satisfy is `RequirementUnmet`, named |
| Private-repo / local-file staging | Medium | inherited mechanism | Small payloads by-value; large via grant-scoped fetch — PLAN.md Track M2's design, two customers one mechanism |
| Preemption × spend | Low | sized away | Units are small; tier 1 (stop offering) is free in a pull model; `yield_to_foreground` reads a seam that exists |
| Arch/deps skew (torch-class) | Low for pilot | the predicate's job | `JobRequirements{os, arch, preconditions}` is checked by `may_take` before a `Lease` is appended, and a refusal is typed |

Provenance is cheap and mandatory day one: a `Complete` carries `ComputeAttribution
{repo_rev, os, arch, toolchain, host}` with a `comparable_to()` check, so "a verdict that is
not yours" is a typed question rather than a footnote. A mesh-run result must survive review,
which is the point of running work on it.

## The pilot: this repository's own CI

The 2026-09-04 cut named an external sweep library. The operator's 2026-09-09 decision replaced
it: a first cut anchored on inference was "anchored way too much on inference", and the pilot
is now **`svrn quality check --trigger ci:<job> --distribute`** — not a new runner, a flag on
the runner that exists.

It is chosen because it is not throwaway. Hosted CI died on a spending limit (PLAN.md rung 5;
4,369 billed minutes audited), pre-push moved the test run to CI, and every dev peer already
holds a warm `target/`. The selected instrument rows become `process:v1` units: `argv` from
the row, `preconditions` from the row, `could_not_judge_exits` and `verdict` from the row. The
results merge through the same four-verdict roll-up and the same table renderer the local run
uses, with one `node` column added, so a distributed verdict is diffable against a local one
at the same rev.

The acceptance gate is that diff (§18.1, and one of them is watched failing): merged pass/fail
counts equal a local `sovereign-test.sh --human` at the same rev; a shard pinned to a rev the
donor cannot resolve shows `RequirementUnmet` in the table rather than vanishing; and a run
with `repo_rev` unpinned against a donor one commit behind is **watched producing a
non-comparable attribution** before the pin is trusted.

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

### Distance to the capstone, read at the end of Phase 5 (2026-09-09)

The rails are closer than the product. Phase 5 paid for most of the substrate this
customer needs and none of the customer.

**What Phase 5 actually bought toward it.** The rail carries signed, converging state
with one declared sender and real retention. 5c-5e proved the shape a federated plane
would reuse verbatim: a plane is a fold over rail admission plus a pluggable executor
registry, with zero new HTTP routes (`2a3437399`). 5f proved the DEPLOYMENT shape this
section's adapter verdict asks for — a package-only third party holding a roster key and
participating without serving HTTP, built and run outside this monorepo (`b03cbad01`).
Grants exist and are per-run. iroh dial-by-key exists, and the peer-path decay that was
killing established tunnels after ~3 minutes was root-caused and fixed the same day
(`77a834f31`, notes `1ca75415` / `c903a9c1`) — which matters here because every number
below would have been measured through a transport that was quietly dying.

**THE RAIL IS NOT A BYTE PIPE — BUT THE BRIDGE ALREADY IS.** A `Complete` act is a
capped `Value` against a 64 KiB payload ceiling and file artifacts are H2 (see "What we
will NOT do"), so media does not ride the rail. It does not need to.
`commonwealth-transport::HttpBridge` is `tokio::io::copy` in both directions
(`iroh.rs:816,820`) — a raw splice between the loopback TCP socket and the QUIC stream
that never parses HTTP and never buffers a body. A Jellyfin stream is an HTTP GET with a
`Range` header, and Range passes through untouched. **The byte plane is built, streaming,
and key-authenticated today; what is missing is a measurement, not a mechanism.** (An
earlier revision of this section called it unbuilt. That was wrong, and it was wrong in
the expensive direction — it priced a demo as further away than it is.)

Not yet checked, and it decides the multi-viewer case rather than the single-stream one:
whether each bridge connection is a separate QUIC stream on one connection or a new
connection each time. (Answered 2026-09-09 in the reading below: a new connection each
time. The question stands as written because it was asked before the answer existed.)

**THE PRE-REGISTERED NO-GO, written before the data exists.** Gap 3 above is the only one
that can kill the demo rather than cost time, so it gets a bar now rather than a reading
later (ARCH §18.1):

> A single iroh stream between two members on different networks sustains **>= 25 Mbit/s
> for 10 minutes with no stall exceeding 2 s**, measured on the relayed path (the floor,
> not the hole-punched best case) and reported as a distribution over >= 3 runs rather
> than a peak (§18.5, and the bounds-over-point-measurements rule). Below that, direct
> playback of a remote 1080p title is not honest and the demo is LAN-only — which is a
> different product claim, and should be made as one.

Run it BEFORE any shim code. It is hours, not weeks, and it is the cheapest question on
this page. It shares its unknown with Track A2's tensor tunnel bench, so one measurement
answers both.

**What the arithmetic says to expect, so the reading has something to disagree with.**
The bottleneck stack is wifi airtime, then the SERVING side's uplink, then the
hole-punch outcome, then QUIC windows, then relay capacity. Wifi 6 at 5 GHz gives
200-600 Mbit/s real and is not binding. The uplink splits sharply — fiber symmetric
500-1000 up against cable 20-50 up — and 1080p H.265 wants 4-8 Mbit/s, a 1080p H.264
remux 8-15 with peaks near 40, and 4K HDR 25-80. So: fiber-to-fiber DIRECT is
link-limited and 4K direct-play is comfortable; cable-upstream DIRECT does 1080p and not
4K; RELAYED is the unknown, and n0's public relays are shared fallback signalling rather
than a CDN, so single-digit to low-tens with no SLA is the honest prior. This mesh runs
`relays=n0-default`, which is the path a failed hole-punch actually lands on.

TWO THINGS MATTER MORE THAN THE AVERAGE. The hole-punch OUTCOME is a bigger swing than
any tuning, and CGNAT on either side forces the bad case — so the bench must report the
path it measured, not just the number. And the failure mode on wifi is STALLS, not
slowness: airtime contention and bufferbloat produce jitter that ruins playback at an
average bitrate that reads fine. That is why the bar above carries a stall ceiling beside
the rate, and why a mean alone would pass a stream nobody can watch.

**THE READING (2026-09-09), taken before any shim code as instructed.** Four verdicts are
available and two are used: the MECHANISM is cleared, the BAR is could-not-judge. Nothing
below is a restatement of the bar, and the bar above is unedited.

*The multi-viewer question is answered, and the answer is "a new connection each time."*
`HttpBridge::spawn`'s accept loop (`iroh.rs:454`) spawns a task per accepted TCP
connection; that task calls `endpoint.connect(target, alpn)` at **`iroh.rs:505`** — which
in iroh 1.0.2 runs `connect_with_opts` → `noq`'s `connect_with`, incrementing
`outgoing_handshakes` on every call, so there is no pool — then opens exactly ONE
bi-stream (`iroh.rs:523`) and pumps it (`:535`). The `Connection` is a local binding in
that task, dropped when the pump ends. So N viewers are N QUIC connections, not N streams
on one. Watched rather than inferred: under `RUST_LOG=iroh=debug`, four simultaneous
viewers produced exactly four `Connection established.` events, and a fifth request
produced a fifth.

Three consequences, in the order they bite:

- **No head-of-line blocking between viewers** — and for a stronger reason than
  multiplexing would give. Separate connections share no congestion window, so loss on
  one viewer's stream cannot stall another's. Four concurrent 100 MiB viewers each held
  3.7-4.8 Gbit/s across two samples, aggregating 14.7 and 16.7 Gbit/s — no per-viewer
  collapse, and the aggregate rises rather than falls with the viewer count.
- **Every viewer pays a full QUIC handshake, and so does every SEEK**, because a Range
  request on a fresh HTTP connection is a fresh TCP connection to the bridge. This is
  already a MEASURED production cost on this mesh rather than a projection:
  `sovereign-mesh/src/gossip.rs:66-91` records RuggedFox→BeefyMac on an idle LAN at
  p50 189 ms / p90 1327 ms / 2 dial timeouts with a fresh connection per gossip round,
  against 38.8 ms / 391 ms / 0 when the connection is reused — which is why that client
  is now built once per process. On a relayed path the handshake is relay RTTs, not LAN
  ones. **So the shim's HTTP client must pool connections and its origin must speak
  keep-alive.** That is a requirement this measurement produced rather than a preference.
- **The responder half of stream-multiplexing already exists.** `IrohAcceptor` accepts a
  connection then loops `conn.accept_bi()` forever (`iroh.rs:991-1000`), spawning a pump
  per stream. Only the DIALER is single-stream. If per-seek handshake cost ever has to be
  removed at the transport rather than at the client, it is a change to `HttpBridge`
  alone and the far end needs nothing.

*Range survives the splice byte-exact — the correctness gate passes.* A minimal HTTP/1.1
origin with real Range support sat behind an `IrohAcceptor`; `curl -r` went through the
`HttpBridge` loopback port; `sha256sum` compared against `dd` on the source. `206 Partial
Content` with a correct `Content-Range: bytes a-b/2147483648`, `200` for the full GET, and
`416` with `Content-Range: bytes */2147483648` for an unsatisfiable range all crossed
unaltered. Digests matched at every offset tried: 2000 B at offset 1000
(`28b68ea0…`), 1 KiB straddling the 1 GiB mark (`ec351faa…`), the last 100 B
(`b05016c7…`), a 100 MiB range (`22e1f375…`), a suffix range `bytes=-1048576`
(`0d81e388…`), and the whole 2 GiB file (`0b4e5591…`, equal to `sha256sum` of the source).
A separate 1 GiB pull verified every byte against its expected value in flight: no
mismatch, and every check above was run twice, 40 minutes apart, with identical digests.
THE CHECKER WAS ITSELF PROVEN ABLE TO FAIL (§18.1): re-reading the source one
byte off the requested offset reported FAIL, and an earlier version of the comparator
reported four spurious FAILs from a shell-expansion bug in its own arithmetic — found and
fixed before any of the above was believed.

*The throughput reading is a SCREENING TEST and it does not meet the bar.* The bar is two
machines on different networks over the relayed path. What was measured is one machine,
both iroh endpoints in one process, n0 contact severed entirely (`presets::Minimal`,
relays disabled), `path=direct`. **A same-box number cannot meet that bar and is not
offered as meeting it.** What it is good for is exactly one thing: if the splice could not
sustain 25 Mbit/s with no network in the way, the plan would be dead for the price of an
afternoon. It is not dead. Release build, 1 GiB per run, pooled over two independent samples taken 40 minutes
apart (the second sample ran about 14% slower across the board, which is the run-to-run
spread and the reason the whole set is quoted rather than the better half):

| | Mbit/s | min | median |
|---|---|---|---|
| through `HttpBridge` (n=10) | 4583 4685 4816 4823 5081 5118 5214 5607 5771 6350 | **4583** | **5099** |
| straight to the origin, no bridge (n=6) | 25259 25612 28081 28996 30188 31355 | 25259 | 28539 |

So the splice costs about 5.6x against loopback TCP and still lands ~204x above the bar's
rate — 183x at the slowest of the ten runs. Max inter-arrival gap on the bridge runs was
27.8-29.2 ms; zero gaps over 500 ms in any of them. **The mechanism is cleared and the
remaining unknown is purely the network.**

*The soak says the splice does not add stalls to a paced stream.* The ceiling above answers
"can it go fast"; a stream that nobody can watch fails on JITTER at an average that reads
fine, which is why the bar carries a stall ceiling. So the origin was paced at exactly the
bar's 25 Mbit/s and the stream held for the bar's ten minutes, three times:

| run | secs | rate | max gap | >500 ms | >1 s | >2 s | worst whole second |
|---|---|---|---|---|---|---|---|
| 1 | 600.00 | 25.0 Mbit/s | 87.3 ms | 0 | 0 | 0 | 23.1 Mbit/s |
| 2 | 600.00 | 25.0 Mbit/s | 88.9 ms | 0 | 0 | 0 | 23.1 Mbit/s |
| 3 | 600.00 | 25.0 Mbit/s | 87.7 ms | 0 | 0 | 0 | 23.1 Mbit/s |

Read the gap column with the instrument in mind: the origin paces in 256 KiB chunks, which
at 25 Mbit/s is one chunk every 83.9 ms, so a p99 near 84 ms is the PACER's period and not
the transport's. The transport's contribution is the few ms above it. Likewise the "worst
whole second" sits a little under 25 Mbit/s because a one-second window holds 11 or 12
whole chunks, not because a second went short.

*How the instrument was proved able to see a bad path (§18.4).* A harness that reports the
same figure whatever you do to the link is measuring itself, and four of the previous
session's instruments were blind in a row. Two knobs were added to the origin and both were
watched moving the reading before any number above was believed:

- **Rate.** Pacing the origin at 5 Mbit/s made the harness report 5.0 Mbit/s against 5300
  Mbit/s unthrottled through the same bridge — a factor of ~1000, so the number is not a
  constant.
- **Stall.** Injecting ONE deliberate 3000 ms pause after 8 MiB of a 25 Mbit/s stream made
  the harness report `max=3002.0 ms, stalls>2s=1, >1s=1, >500ms=1` and drove the worst whole
  second to zero — the stall detector fires on a known stall, reports its true magnitude,
  and counts it exactly once.

Both knobs live in `commonwealth-transport/examples/media_bridge_bench.rs`, a sibling to
`tunnel_bench` in the same crate. `tunnel_bench` was checked first (ARCH §19) and its
public surface reused unchanged — `build_relayed_endpoint` / `build_relay_only_endpoint`,
`HttpBridge`, `IrohAcceptor`, the dial-string helpers, and the `path=` line read from
`remote_info`. What it cannot answer is anything about HTTP: it speaks a private
`[send_len][want_len]` framing and reports a rate without a stall distribution. Those two
gaps are the whole of the new file.

*The two-machine number: ABSTAINED, not substituted (§18.3).* Through the measurement
window `svrn mesh status` reported 1/8 online — this node alone, every Mac offline, none
answering on the LAN — and `svrn mesh transport` agreed at `watchdog: 0/7 peer paths
active`. Two Macs (Alexs-MacBook, BeefyMac) came up at 15:42, after the runs, and both
show `path=mixed relay=usw1-1 direct=1`; they still could not be measured, for a different
and more useful reason. **The blocker is not peer availability but the absence of any way
to START the harness on the peer**: there is no ssh to those hosts from here, and the mesh
exposes no remote-exec surface — `mesh bench` measures decode speed, and `mesh
fetch-model` moves bytes over the tailnet rather than the iroh bridge. So the next run
needs a person (or a launchd job) to start `media_bridge_bench serve` on the far side; that
is the whole of the remaining setup. The bridge-cache fix at `77a834f31` is in this tree
and the daemon that was running carries it (built 14:25, after that commit's 13:59), so
nothing about the transport is in the way. **No cross-machine number is reported, and the
relayed floor stays unmeasured.**

*The relay leg was attempted, and the attempt is CONTAMINATED — reported rather than
buried (§18.3).* Both endpoints were built with `build_relay_only_endpoint`
(`iroh.rs:266`) and seeded with the relay target only, which should have pinned the bytes
to `use1-1.relay.n0.iroh.link` and back. It did not hold. The `path=` line said
`mixed direct=[69.181.167.209:40262] relay=[…]` and the numbers say the same thing: run 1
took 107.2 s for 100 MiB (7.8 Mbit/s, ttfb 660 ms, one 1296 ms stall), runs 2 and 3 took
0.13 s (6328 and 6111 Mbit/s) — loopback speed. **Two of the three runs measured the
direct path while claiming to measure the relay, and only the `path=` line caught it.**

The cause is known and already written down. `RelayOnlySelector::select` returns an empty
selection when no relay path is open, and an empty selection KEEPS THE CURRENT PATH — so a
direct path that validates first survives the pin. `tunnel_bench`'s `dial` records exactly
this, observed 2026-07-19. On one host the race is unwinnable: the two endpoints learn each
other's public address through the relay and hairpin straight to it.

What that leaves is one number with a caveat rather than a measurement: run 1 STARTED
relayed, so 7.8 Mbit/s is an UPPER bound on the relay rate for that run, not an estimate
of it — a migration to direct partway through can only have raised it. That upper bound
sits BELOW the bar's 25 Mbit/s, and it is consistent with this page's own prior
("single-digit to low-tens with no SLA is the honest prior"). It is not evidence against
the bar; it is a reason to expect the bar to be the binding constraint, which is what the
bar was written to find out.

The operational lesson for the next run is concrete: **`--relay-only` is a request, not a
guarantee — gate every relayed reading on the `path=` line and discard any run that does
not read `relayed`.**

*Verdict against the pre-registered bar.* **Could-not-judge on the bar; the mechanism is
cleared.** The bar asks for two machines on different networks over the relayed path and
that measurement was not available today. What IS settled is everything the bar was
protecting against on THIS side of the wire: the splice is byte-exact under Range, it
sustains ~204x the bar's rate with no network in the way, it adds no stalls to a paced
ten-minute stream, and N viewers do not interfere. Gap 3 above ("sustained multi-Mbps over
iroh is unmeasured") is now half-measured: the local half says go, the WAN half is
untouched.

*Recommendation, with the reasoning, leaving the bar as written.* The bar is set at the
right THRESHOLD and is missing a term. 25 Mbit/s with no stall over 2 s is the correct
floor for a 1080p H.264 remux and should not move. What the reading exposes is that the bar
measures A SINGLE STREAM'S STEADY STATE, and this transport's characteristic cost is not
steady-state — it is CONNECTION SETUP, paid per viewer AND per seek, already measured at
p50 189 ms / p90 1327 ms on an idle LAN with a cold connection (`gossip.rs:66-91`). A
relayed path multiplies that. A demo that clears 25 Mbit/s sustained and still takes over a
second to answer a scrub is a demo nobody enjoys, and the bar as written would pass it. So:
keep the bar, and add a companion acceptance on the same run — **time-to-first-byte for a
Range request on a COLD connection over the relayed path, p90 under 1 s** — measured with
and without HTTP keep-alive, since the difference between those two IS the shim's client
requirement.

*The single next measurement.* Get a shell on one Mac (the only missing piece), run
`media_bridge_bench serve --origin …` on it and `bridge --iroh <dial> --relay-only` here
(both sides need `--features iroh,iroh-relay-only`, since path selection is per-side), and
take the bar's own reading: 25 Mbit/s, ten minutes, three runs, stalls reported, with the
`path=` line stating `relayed` rather than the number implying it. Everything else on this
page waits on that one number, and nothing else measured today can substitute for it.

**THE DEGRADED CASE IS ALREADY EXPRESSIBLE, which is the elegant half.**
`iroh_access::PeerTransportPath` reports `direct | relayed | mixed | idle` plus the
active direct-address count and the relay in use. So a shim can be honest about
degradation rather than silently stuttering — "this peer is on the relay, a 4K remux will
not hold" is a sentence this system can already produce (ARCH §18.3, applied to a product
surface instead of a log line). Pair it with rung 1's residency pattern — play from the
holder, transcode where the media lives — and the measured path quality becomes an INPUT
to the serving node's transcode decision. Jellyfin already transcodes; both facts already
exist; nothing new is needed to connect them. That, rather than a new transport, is what
"elegantly, with what is already built" means here.

**Then, in order.** Provider-ID identity is a genuine architectural decision and not a
port: cross-server item identity is TMDB/IMDB — external, mutable, third-party-controlled
— against a system whose essence is content hashes (§7.5). It has consequences well past
media and should be decided on its own, not inside a shim. The federated-query seam is the
`JobKind` move on the data side, and cw-lift 5g is the structurally identical change to
ingest, so the pattern gets proven adjacent before it is needed here. The Jellyfin API
emulation is last because it is irreducible, large, and blocked on nothing — it is the
only item that is purely work.

**The honest estimate.** A minimum demo — two machines, a shim beside each, a client on A
playing a title that lives on B, no VPN and no port-forward — is weeks rather than months
IF the throughput bar clears, and a different conversation if it does not. Two caveats
that belong with the number: nobody has yet priced the emulation layer against a real
client, and the GPL-2 / AGPL split keeps the shim a separate distribution, which is a
packaging constraint worth settling early rather than discovering at ship.

## What we will NOT do

- **Invent sandboxing.** No seatbelt-profile authorship, no firewall DSL. v0 says
  trusted-native and does not dress it up; the `isolation` claim is an ordered enum so a real
  runtime can be added later without a seam change.
- **Claim a sandbox we do not have.** The 2026-09-04 cut did, in the OCI section and in the
  ACE row; audit row 16 is why both are rewritten rather than softened.
- **Add HTTP routes.** The rail replicates signed state with one declared sender, and a
  second door would fork that census. `svrn job submit` and `svrn job status` go through the
  append and log paths that already exist.
- **Add a results store.** Results are capped `Value`s on `Complete` acts; rung 8 stays
  deferred and honestly labeled. File artifacts are H2.
- **Rank.** v0 refuses or admits; it does not score. A scorer arrives with a measurement that
  demands one, under one owner (§10.6).
- **Flat self-hosting.** Decode turns are never jobs; the data plane keeps its own latency
  class.
- **A fourth scoping noun** (inherits PLAN.md): the signing key + `allowed` + `accept_from` +
  kind cover the consent model; anything more is a decision to reverse the spine, argued as
  one.

## Resolved (2026-09-09 — on the rail; each reusing machinery that exists)

**1. Coordinator SPOF — the journal is the record, and there is no queue to lose.** The
2026-09-04 resolution built an append-only `JobRecord` on the submitting node and called
donor-side recovery "already on the wire". Both halves are superseded. The rail *is* the
append-only journal, replicated to every member, and the queue is a fold over admission —
so a submitter that dies mid-run costs nothing: donors keep leasing, completing and
appending, and `svrn job status` folds the same acts on any node. What the audit found is
that the donor-side half was never on the wire at all (row 1): the 404 was dropped. That is
fixed at 5a with a pure `heartbeat_verdict` decision — 410 aborts, 404 aborts, three
consecutive misses abort on silence — extracted so it can be tested without a live response.
Delivery semantics stated plainly: **at-least-once, idempotent per `unit_hash`**; double
deliveries are recorded rather than hidden.

**2. Lease machinery — one mechanism, and it is the fold's.** No fork of the decider
(§10.6). A `Lease` act sets `expires_at_ms`; `Renew` moves it; every node derives expiry from
the same floor via `is_live_at` and nobody publishes an expiry event. "Long lease" is not new
machinery — it is the same lease with a kind-declared interval carried in the descriptor.
The 2026-09-04 resolution routed heartbeats through the `sovereign-compute` supervisor's
health check; audit row 11 says that supervisor has two inference dependents and its own
restart decider, so reusing it would have been a second decider, not one.

**3. There is no placement scorer, and that is the resolution.** The 2026-09-04 resolution
made scorers a kind-keyed registry beside a placement decider, copying `rank()` and
`SchedCore`. Neither is copyable (audit rows 7, 8). v0 ships `may_take` — one predicate, one
implementation, refusals only — and the fair-share question is deferred until a measurement
asks for it. This is the smaller thing that works, and it removes the `mesh bench`/`mesh
plan` double-construction risk entirely rather than guarding against it.

**4. Payload identity — the rail's canonicalizer, no second writer.** The 2026-09-04
resolution keyed images by digest with an ordered source list. With no image, the question
becomes unit identity, and it has one answer: `unit_hash` is a content hash over the bytes of
`Payload::new(json!({"kind", "payload"}))`. That canonicalizer already refuses fractional
numbers and oversized payloads, and it is the only one in the tree — so a unit that cannot be
canonicalized is `PayloadNotCanonical`, named, before it is ever appended.

**5. Preemption — two tiers, both from signals that exist.** Tier 1, stop offering: the pull
model makes this free — a donor that stops appending `Lease` acts takes nothing new. Tier 2,
cancel in flight: `WorkOffer.yield_to_foreground` polls
`AppState::should_yield_to_foreground()` (`commonwealth-api/src/state.rs:2115`), which is a
real seam, unlike the activity bus the 2026-09-04 cut assumed (audit row 10). Only tier 1 is
built in v0. Checkpointing stays out of scope; unit-boundary sizing absorbs the cost.

**Remaining open at this layer:** whether the `work` roster on the two daemons stays
hand-written (v0) or the second lift earns the derived form. The durability question that
survives is PLAN.md's own open question 1 (front-door failover), and the rail weakens it
rather than depending on it.

---

*Companion to [`PLAN.md`](PLAN.md) (which this extends at Phase 3),
[`GROUND_TRUTH.md`](GROUND_TRUTH.md), and
[`docs/CMNWLTH_DESIGN.md`](../../../docs/CMNWLTH_DESIGN.md) (the same ontology, superseded as
a design). The ladder and the bars are `quality/campaigns/cw-lift.toml`, Phase 5. Update this
file in the same commit as the code it describes (§1.1).*
