# The daemon's core — four things, and a placement test for everything else

Drafted 2026-09-11. This is the SURFACE half of the daemon design. The Runtime
half (core-only Runtime, `Scope`/`Capabilities`/`Lane` built per turn, the
35 → 0 reach-through bar) is `quality/TOPOLOGY.md` §3.5 and is not restated.
The client half (desktop attaches as a pure client, one turn driver) is the
`sv-surface` campaign and is not restated either. This document answers the
question those two leave open: of the 242 paths the serving hosts register
today, which belong to the daemon at all, and how each family migrates.
Since 2026-09-14 it also holds the crate half — what crate the daemon is, what
that crate may hold, and how the node's state stops being one object every
context reaches through (§4; the `domains` campaign's design rungs D1–D3, D9).

Every number below is produced by `scripts/daemon-route-census.py`. Re-run it
before trusting a count; the script is the source, this file is the argument.

## 1. The claim

WireGuard is small for one structural reason: its entire state is one table,
`public key → allowed IPs`, and one verb, send a packet. Identity is the key,
the ACL and the routing table are the same rows, nothing is negotiated, and
everything that is not that table belongs to the kernel or to the userspace
tool. The daemon has the same shape available and has not stated it.

**The daemon is four things.**

1. **A node.** An identity (the ed25519 node key) and one table, `principal →
   Scope`, where `Scope` is the airtight corpus ceiling plus capabilities. A
   peer's row and a local caller's row have the same shape. The forgeable
   `enabled_corpora` selection sits under the ceiling the way a peer's chosen
   route sits under WireGuard's allowed-IPs.
2. **One verb: the turn.** Tokens, narration, interpretation, clarification,
   metadata, streamed against a `Scope`. Search is the retrieval half of a
   turn, solve is several turns, embed is a turn primitive.
3. **A job queue.** Ingest, enrich, reindex, atlas build, compaction, model
   load, transfer: "run this and tell me how it goes." One submission shape,
   one status vocabulary, one cancel, on top of `commonwealth-work`, whose
   unit + lease + `WorkProjection::fold` already exist.
4. **A short list of single-owner resources.** Loaded weights, the corpus
   index writer, the SCIP graph, the node key, the port. Things that would be
   wrong if two processes held them.

**The placement test for a route**, in the order to ask it:

- Would it be wrong for two processes to do this at once? If no, it is a client
  or a job's *submitter*, not the daemon.
- Is it a turn, a job submission or observation, a row in the node table, or a
  read of a single-owner resource? If none, it does not belong in the daemon.

Agent-state CRUD (conversations, notes, memories, projects, skills) is a fifth
class recorded here for the count only. `quality/TOPOLOGY.toml` deliberately
declares that store non-exclusive (SQLite WAL, concurrent writers wanted), and
`sv-surface` owns which surfaces reach it through the daemon. What this
document asks of that class is one thing: ONE accessor crate owns the file
name and schema. The 2026-08-04 rename of `sovereign.db` to `svrnmesh.db` stranded
every map built before it because the daemon and the desktop each derived the
name; that is two accessors for one path, ARCH §8, not a topology question.

## 2. Measured today (2026-09-11)

| measure | value | source |
|---|---|---|
| route registrations, serving hosts | 277 | census |
| unique paths | 242 | census |
| paths registered by more than one host crate | 13 | `--dupes` |
| host crates | sovereign-mesh 163 · commonwealth-api 89 · sovereign-server 22 · sovereign-cli-daemon 3 | census |
| `*_http.rs` in sovereign-mesh | 22 modules, 18,319 lines | `wc -l` |
| status/progress vocabularies across job-shaped surfaces | 22 `*Status`/`*Phase`/`*Progress` enums, 18 `*Progress` carriers | `grep pub enum` |
| `WorkActKind` today | Submit Offer Lease Renew Complete Fail Revoke — no Progress | `commonwealth-work/src` |
| grant vocabularies on the node | `GuestGrant` store, `Roster`/`Person` ring, corpus `Grant*` DTOs — three | `grep pub struct` |
| CLI call sites writing corpus-index or enrichment in-process | 28 (enrich_cmd 15, corpus_cmd 3, bench 3, atlas 2, five singles) | grep over cli crates |
| `TOPOLOGY.toml` invariants failing | 4 of 7, all four about writers and cancellation | `quality/TOPOLOGY.toml` |
| `sovereign-cli-llm` | 1,264 crates, 611 MB debug; the daemon is 1,259 and 614 MB | `cargo tree`, `ls` |
| `sovereign-server` | 22 routes, 6,940 lines, `today = UNRESOLVED — appears in no release script` | `TOPOLOGY.toml` |

The census sorted by class:

| class | unique paths | what migrates |
|---|---|---|
| turn | 33 | one host; the 2 sovereign-server duplicates collapse, the 4 worker-proxy ones stay |
| job | 76 | → `/v1/jobs` (4 paths) over `commonwealth-work` |
| node | 33 | one `principal → Scope` table under three grant vocabularies |
| resource | 58 | one read family per resource; the atlas has three today |
| store | 38 | sv-surface's; one accessor crate |
| out | 4 | app proxy and admission test shims; leave |

Seventy-six job-shaped paths is the finding. Nearly a third of the surface is
"run a thing and ask how it is going", spelled 76 ways with 22 status enums,
and the four failing topology invariants (no lease on corpus-index, write
projection cyclic, desktop writes what it is not granted, cancel does not cross
the process boundary) are all consequences of that spelling: each job family
owns its own runner, so nothing holds the lease and nothing can be cancelled
from outside its process.

## 3. Migration by class

### 3.1 Turn — 33 paths, already designed

TOPOLOGY §3.5 and the Phase 6 one-driver bar own the shape. This document
adds only the duplicate-host finding. Six turn paths have two
implementations. Two are a second host: `/v1/conversations/{id}/messages` and
`/stream` in both sovereign-mesh and sovereign-server, and go with §3.6. Four
are `/v1/chat/completions`, `/v1/models`, `/v1/embeddings` and
`/oicp/v1/capabilities` in both commonwealth-api and sovereign-mesh; the
sovereign-mesh copies are the worker-mode inference proxy
(`worker_inference_proxy.rs`), a different process, and stay. One non-turn
duplicate is inside one process: `/internal/corpus/status` is registered by
both commonwealth-api and sovereign-mesh in the same daemon, and Phase 1
removes one of them.

### 3.2 Job — 76 paths become 4

The target family, on top of what `commonwealth-work` already has:

```
POST   /v1/jobs                 Submission{kind, payload}  → UnitRef
GET    /v1/jobs/{id}            ProjectedUnit (WorkUnitStatus + per-kind progress)
GET    /v1/jobs/{id}/events     SSE — solve_http.rs already has this pattern
DELETE /v1/jobs/{id}            Revoke
GET    /v1/jobs?kind=&state=    the projection, filtered
```

`kind` is a closed set, so an enum (ARCH §9): ingest, enrich, reindex,
atlas-build, compaction, model-load, model-transfer, index-transfer,
skeleton-rebuild, project-rebuild, solve, workflow, process. Each kind is a
`JobExecutor` impl registered in the daemon's `JobExecutorRegistry`; a local
job is a unit whose only eligible donor is this node. The executor holding the
lease IS the store lease, which is the one structural change `TOPOLOGY.toml`
asks for and flips `every-exclusive-store-has-a-lease` and `write-is-acyclic`
together. Cancel is a `Revoke` act on the rail rather than a call into
`corpus-engine::CancellationRegistry`, which flips
`cancellation-reaches-the-writer`: a desktop cancel of a CLI-submitted ingest
is the same act on the same unit.

**The named gap.** `WorkActKind` has no progress act, and today's ingest and
enrichment emit progress through 18 per-kind carrier types. Two honest
options, and the choice is the first thing to decide in Phase 2: a `Progress`
act on the rail (visible to peers, costs a rail append per tick, needs
coalescing), or a process-local progress channel keyed by `UnitRef` that the
SSE route reads (cheap, invisible off-node). The rail act is right for units
peers can lease; the local channel is right for compaction. Both is two
implementations of one thing, so pick per kind by whether the unit is
offerable, and say so in the kind's descriptor.

**What this deletes.** The 76 paths and their handlers; the 22 status enums
in favour of `WorkUnitStatus` plus one progress payload per kind; the 28 CLI
sites that write corpus-index or enrichment in-process, each becoming a
submission plus an events follow; `CancellationRegistry`'s cross-process
claim. `corpus_watch_http.rs` (1,617 lines, 20 paths) is a standing job with
pause/resume/sync-now, and becomes one kind with a long lease.

**What this does not touch.** Interactive verbs that look like jobs but are
turns: `preview`, `search` on a local corpus. The census already classes them
as turn. A job family that a UI polls for a preview is the wrong tool, and the
kill bar in §6 is there to catch it.

**Corrected 2026-09-14, by the inventory.** Three claims above were written without it. A job
kind is not a closed set: `oicp_types::JobKind` is an open, versioned id, and
`commonwealth-work` already dispatches it through `JobExecutorRegistry`, which registers,
resolves and filters kinds by isolation — ARCH 9's open set, so the registry that exists is the
shape and no enum is minted. The progress gap is half closed: `JobContext` carries a
process-local progress sink per unit, as a string; what is open is a typed payload per kind and
the rail `Progress` act for offerable units. And the family has begun: the daemon's donor loop
already runs `process:v1` and `ingest:v1` units off the rail, while the other fourteen runners
keep their own tables. The donor binds Compute's fold
to Fabric's rail and Ingest's engine, so by §4.3's rule it is the daemon's, in `jobs`, with
`ingest_executor` beside it; what it needs from the node — the rail and its nudge, the
foreground signal, a status slot — is three of §4.2's capabilities. Compute's other placements
are `quality/DOMAINS.md` §11. One more decider converges here: `auto_ingest`'s pull loops over
`sovereign-grants`' `WorkQueueManager` are a second lease for collaborative ingest beside
`ingest:v1`, whose own header says it replaces them. That is a behaviour rung, and when it lands
most of §4.2's collaborative-ingest fields have no reader.

### 3.3 Node — 33 paths, one table under three vocabularies

`mesh_http.rs` (create, join, leave, rotate, switch, forget-member, status,
relay-candidates, measurements), the commonwealth-api internals (join, gossip,
ring sync, quiesce, activity, latency), and the grant routes (`/internal/guest/
grant/*`, `/internal/corpus/grant`, `/v1/conversations/{id}/enabled-corpora`)
are already the node. What is not yet one thing is the table. Three grant
vocabularies exist: the `GuestGrantStore`, the ring `Roster` of `Person`s, and
the corpus `Grant*` DTOs. Whether they resolve to one `principal → Scope` row
at request time or three lookups in three handlers is not measured here; the
measurement is `callers("corpus_ceiling")` and `callers("GuestGrantStore")`
and is Phase 5's first step. The bar is one resolver, one row shape, and the
ceiling never defaulting to permissive (TOPOLOGY §3.5 already names
`sensitive_corpora: None` meaning "all eligible" as the inverted invariant).

**Measured 2026-09-14: neither.** Five resolvers answer "who is asking" over four identities and
no two share a type, and the ceiling is not wired on the daemon or the desktop at all:
`corpus_principal` is set only in `sovereign-server`, and `sensitive_corpora` nowhere. The one
resolver is designed below. The five: `client_auth_layer`; the client fairness gate, whose
own documentation says its key is for fairness and must never carry access control; peer
admission on `X-Node-Id`; the turn's tenant-prefix `PrincipalResolver`; the sensitive-corpus
oracle. Which callers other than the owner can start a turn on the node was not measured, so the
exposure is not sized here. The absence is a defect, not a design choice, and no code move fixes
it.

The resolver, decided:

- **One resolution at the edge, one value.** The daemon's `edge` authenticates a request once and
  attaches a `Principal`. Admission derives its fairness and peer keys from it, this table
  derives the turn's `Scope`, and a guest's grant bounds its routes.
- **The type is published language; the resolver is the daemon's.** `Principal` lives in
  `sovereign-contracts`, because Serving's package and Answering both key on it and neither may
  name the daemon. The resolver needs the client token, the grant store and the roster, so it
  lives in the daemon's `edge` and `node`, the only place that holds all three.
- **The shape is the union of what today's resolvers distinguish, never a narrowing:** the local
  owner with the declared sub-identity the fairness gate buckets on, a remote client by
  credential, a member by verified node id, a guest by grant, anonymous. SERVING_BOUNDARY's
  `Local | Member` sketch is withdrawn: it folds the fairness buckets into one arm and has no
  guest.
- **`Scope` collides three times, and none of the three is the ceiling:** a guest grant's
  capabilities (`sovereign-grants`), a tool's effect locus (`oicp-types`, published and exempt),
  a workflow template context (studio). Answering keeps the word, which TOPOLOGY §3.5 designed
  the value under; the two first-party homonyms rename apart in the rung that mints it. In the
  registry, `Principal` leaves Serving's owned words for the published language.

### 3.4 Resource — 58 paths, three read families over one atlas

Reads of what the daemon exclusively owns stay. The consolidation is that the
atlas is read through three families: `/internal/atlas/*` (13 paths),
`/internal/meshapp/{corpus}/*` (13), and `/internal/corpus/{corpus}/atoms|
chunks|coverage-card` (8). Three read surfaces over one store is three places
a schema change lands. One read family, with the meshapp and reading views as
query shapes on it, is the target; the turn-client already wraps all three
(`atlas_*`, `corpus_*`, `conv_*` methods), so the client side of the collapse
is a rename.

### 3.5 Store — 38 paths, sv-surface's

Recorded for the count. The one requirement this document adds: the file
name, schema and migrations of every `<root>/*.db` live in one crate, and
every process, daemon or shell, opens through it. Whether a given surface
opens the file or asks the daemon is sv-surface's call per surface.

### 3.6 sovereign-server — resolve, do not grant

`TOPOLOGY.toml` says it: "appears in no release script; either mobile access
is dead on a shipped build or there is a packaging step nobody has found.
Resolve before granting it anything." Of its 22 paths, 8 duplicate
sovereign-mesh (conversations ×4, documents ×2, mcp ×2 — counting
`/mcp/message`) and the rest (`tasks/approve`, `tools`, `corpora/upload`,
`corpora/{id}/chunks`, `documents/ask|state|upload`, `cycle/bdd`, `solve`,
`search`) are a turn, three jobs, three resource reads and a store write that
the daemon either already serves or should. The resolution is a census of
who launches it (`grep -r sovereign-server scripts/ .github/`, today: one
dev-onboarding script that kills a stale one), then either a release step
that ships it as a proxy in front of the daemon's client port, or deletion.
Deletion is 6,940 lines and one host fewer, and is the default unless the
launch census finds a shipped path.

## 4. The host crate

The daemon owns its construction (which parts exist in which variant, built in which order), its
data root, its listeners, its background tasks and their shutdown, and §1's node table. It owns
the translations between contexts, because only the composition root sees both sides of one. It
decides nothing a context owns — no threshold, score, ranking, admission rule, fold or enrichment
pass — and it does not own the operating-system process: argv, signals, exit codes, the panic
hook, the memory watchdog, log files, service installation and the `setup` and `doctor` verbs
stay with the binary, `sovereign-cli-daemon`, which calls the library.

§1's placement test says which routes belong to the daemon. Its companion says which modules
belong to the daemon's crate. Each is **assembly** (constructs parts in dependency order), a
**surface** (a route shell: parse, call one context, shape the reply), **edge** (who may call,
and translating a caller's dialect into the node's protocol — `client_auth`, `loopback_guard`,
`local_only`, `headers`, `frontdoor`) or an **adapter** (implements one context's port with
another's capability, §4.3). A module that is none of these holds a decision, and the decision
belongs to the context that owns its words.

### 4.1 `sovereign-daemon`

Today the node is assembled three times over. `sovereign-cli-daemon`'s `run_daemon` builds
engines and providers; `EmbeddedDaemon` in `sovereign-mesh` runs membership, translates members
into serving candidates and hands out the runtime, notes store and insight service through 53
public methods; `sovereign-api`'s `AppState` carries the rest (§4.2).

The host crate is a new library, `sovereign-daemon`, at tier 5 in the `mesh-api` layer beside
`sovereign-mesh`. Not `sovereign-cli-daemon`: at tier 6, `sovereign-cli-llm` and
`sovereign-cli-dev` would depend on a sibling host, which the layer map forbids, and
`sovereign-mesh`'s own tests could not reach it.

| lands in it, measured 2026-09-14 (`wc -l`) | lines |
|---|---:|
| `sovereign-mesh` modules tagged host after the 2026-09-14 retags: `daemon`, `daemon_services`, `supervised_task`, `job_registry`, every `*_http` route shell (`mesh_http`, `governance_http`, `publish_http` and `rpc_warm_http` were tagged fabric or serving), `mcp_router`, the edge leaves, the job surface and adapters (§3.2, §4.3) | 35,827 |
| `sovereign-api`'s host cluster less `state` and the middleware seam: `frontdoor`, `client_auth`, `headers`, `reshaping`, `server`, `routes_status`, `routes_internal`, the foreground-signal implementation | 14,942 |
| `sovereign-api`'s route shells tagged by domain: serving, fabric, `routes_knowledge`, `routes_oicp_ingest` | 11,446 |
| the composition half of `sovereign-cli-daemon`'s `daemon_cmd`: `bootstrap`, `run_daemon`, `build`, `discovery_policy`, `tool_registry`, `vram_plan`, `solve_http` | ≈ 9,000 |

About 69,000 lines, before `daemon`'s membership half returns to Fabric. That is large, and it is
100% host by tag, so the `domains` loop never enqueues it — the one way that loop passes a god
crate. Three things answer for it: the placement test above is a review rule on every module that
lands; the shrink is this document's phases (76 job paths become 4, three atlas read families
become one, eight duplicate routes go, 28 in-process CLI writers become submitters); and
`size-gate` baselines the crate the day it exists. Splitting it by route class now was priced and
refused: that cuts crates along lines Phases 2–4 redraw, so everything would move twice.

Its modules follow §3's classes — `assemble`, `edge`, `turn`, `jobs`, `node`, `resources`,
`store`, `adapters` — so each phase lands inside one module family. Its consumers are
`sovereign-cli-daemon`, `sovereign-cli-llm` for the `MeshAdmin` variant and worker mode until
Phase 6, and its own tests. `sovereign-server` stays out of its tree (§3.6), and
`sovereign-desktop` already names none of `sovereign-mesh`, `sovereign-api` or
`sovereign-cli-daemon`.

`EmbeddedDaemon` splits by owner, not size. Its membership operations — create, join, leave,
switch, forget, rotate the invite, report reach — are Fabric adopting, persisting and gossiping
its own roster and identity. Once Fabric owns its state they are Fabric's methods, and the
daemon observes membership through readers instead of orchestrating it (ARCH 12). The candidate
translation, RPC worker discovery and model-transfer endpoints are the daemon's Fabric-to-Serving
adapter. The accessors are a locator and disappear with typed parts; construction, resume,
listener and shutdown are assembly.

Two `[[forbid]]` rows land with the crate: `sovereign-mesh -> sovereign-daemon` and
`sovereign-mesh-test-harness -> sovereign-daemon`. Lower tiers cannot reach tier 5 already. No
gate sees two things, so they are named here: a struct in `sovereign-daemon` that carries every
part and is handed to every handler passes all of them while restoring `AppState` under a new
name, and a decider can land in the crate green.

### 4.2 The node's state — `AppState` dissolves

`AppState` (63 public fields, 88 public methods) is passed whole to every handler and into five
non-host clusters. It is built before most of what it holds exists: its identity key, dial
signer, ring rail, transport, client token, convergence recorder and in-flight gauge arrive
afterwards through `install_*` and `with_*` calls, so a reader of any of them handles a slot the
daemon may not have filled. The design item framed it as one port trait per consuming context.
That is seven mirrors of one god object, and it leaves every field where it is. Assigned instead
by what each field *is*, the 63 fall to six owners:

| owner | n | what the fields are | home |
|---|---:|---|---|
| Fabric | 20 | node id with its public key, dial-info provider and dial signer; the roster; the ring rail and its write nudge; the replicated KV; transport; clock; gossip's liveness maps; the mutation persistence hook; the convergence recorder; the fan-out gauge; the mesh-app registry and port map; the contribution emitter; the RPC-over-iroh flag | `sovereign-mesh` |
| Serving | 20 | model, pipeline and slot aliases; servable model files; the local inference handle and RPC shard warmer; the inference store; peer and client admission schedulers with their caps, switch, tallies, rejected-header record and reciprocity weights; the contribution pause and yield-peers switch; the availability composite; the in-flight gauge; venue preferences | `sovereign-serving-host` |
| collaborative ingest | 9 | active ingests, progress, pull loops, verify reports, the work queue, ingest grants, the quiesce and throttle dials, the newsworthy tick handle | `jobs` (§3.2) |
| Answering | 3 | the ATOS middleware registry, session store and repo root | `sovereign-core`'s pipeline, after the ATOS inversion |
| Workbench | 1 | the next-edit model slot | next-edit's crate |
| node | 10 | client token, guest grants, start instant, the corpus-engine handle, the foreground signal (last active, window, in-flight), storage budget and used, the activity emitter | `sovereign-daemon` |

**What crosses owners once the fields go home.** Measured 2026-09-14 by matching every field and
method name against each `.rs` file that names `AppState`, with inline test modules cut and
same-named fields on unrelated structs (desktop, rails daemon, harness, tools) removed by hand:
about 115 production sites read a field another context owns. 62 are route shells reading the
node's parts, which is the design; `sovereign-api`'s `mesh_admin` alone is 25 of them, a
node-admin surface tagged `fabric` and misfiled. The other 53 sit in seven modules, and each read
is one of eight capabilities:

| capability | read by | exists as |
|---|---|---|
| the roster, as reach | collaborative ingest, the newsworthy leader check; the Venue translation through `EmbeddedDaemon` | decided: `MemberReach` in `sovereign-contracts` (DOMAINS.md §10.2) |
| node identity | collaborative ingest, shard recovery, the newsworthy host | **a reader** — below |
| replicated namespaces | work-atlas claims, the donor, collaborative ingest, the newsworthy host | `commonwealth_rail::RingRail`, `commonwealth_state::MeshStore` |
| the foreground signal | written by the turn, admission and next-edit; read by ingest workers and the donor | `corpus_engine_yield::{ForegroundSignal, ForegroundLease, YieldHook}` |
| the corpus engine | gossip, collaborative ingest, shard recovery, the newsworthy host | `Arc<CorpusEngine>` |
| transport | collaborative ingest | `commonwealth_transport::PeerTransport` |
| facts for the ledger | shard recovery, the newsworthy host, the donor | the facts rule — below |
| what this node claims about itself | gossip | **new: one port** |

Six of the eight exist. Measurement forced the two new ones.

*Identity is a reader, not a value.* `EmbeddedDaemon::join_mesh` adopts the founder's roster and
swaps the node id inside a running daemon, rebuilding nothing, and the comment at the swap
records what a cached placeholder did (`local node not found in mesh` 500s, gossip log spam every
ten seconds). Fabric publishes identity as a watch over `NodeId`, and a consumer holds the watch.
This corrects SERVING_BOUNDARY entry (a), which called `local_node_id` a process constant.

*Gossip asks the node what to claim.* `capabilities::build_local_capabilities` takes an optional
`AppState` for the in-flight count and the storage remaining, and gossip reads the inference
store and recomputes availability on its own — Fabric reaching into Serving and the node for its
own advertisement, the `fabric -> host` backflow in the cluster graph. It inverts into one port
Fabric declares and the daemon implements, answering what this node claims right now:
availability, in-flight, storage remaining, loaded models, hosted corpora. Working name
`SelfClaims`, which `converge noun` finds undefined. Fabric publishes the claims and does not know
who computed them.

*The facts rule.* No context outside Fabric names `ContributionEmitter` or `ActivityEmitter`. Each
context emits its own outcome — Serving's `RoutingOutcome`, a unit's completion, an ingest's
result — and a daemon adapter prices it into the ledger, as SERVING_BOUNDARY already states for
Serving.

**Construction is staged, and parts are total.** A part is constructed after everything it holds
exists, and nothing is installed into it afterwards. The order is the dependency direction the
layer map already enforces: data root and identity; Fabric; the engine; Serving, handed the
roster reader, the identity reader and the engine; `jobs`; Answering's runtime; each surface's
router over its own part; the listeners. A construction variant decides which parts exist, and
an absent part is absent from the variant's type — `DaemonServices` already does this for
services (TOPOLOGY §3.5), and this extends it to state. What legitimately changes during life
stays inside its owner and is published as a reader: the roster and node id at adoption, the slot
aliases and servable files on a models reload, the provider on `replace_models_and_reload`.

Where an install slot breaks a cycle today, the cycle is the finding. The in-flight gauge is the
worked case: `MeshInferenceProvider` creates it, the bootstrap installs it into `AppState` so
gossip can read it, and a reload hands the same `Arc` back to the new provider so the count
survives. The gauge wants to exist before the provider, so the node creates it and gives it to
both — a signal object created first, never a slot filled later.

Handlers take a part, never the node: each surface builds its `axum::Router` over the one part it
serves and the daemon merges them, as `ServingCapability`'s pre-built routers already do. Tests
build the node the way production does: the 63 test sites that construct `AppState` directly (45
under `sovereign-mesh`, 18 under `sovereign-api`) move to a test node produced by the same
assembly with fakes at the ports.

**Risks carried.** A reader copied into a field reintroduces the placeholder-id bug silently. A
part holding Serving's provider rather than its reader serves the old model after a reload — the
shape of the wrong-slot incident ARCH 8 records. And `sovereign-api`'s middleware seam
(`Middleware`, `PipelineContext`) is Answering's port, named by Workspace's decision extractor
and the ATOS middlewares; it lifts with Answering and is not host code.

### 4.3 Adapters, and the forbid that stays

`[[forbid]] corpus-engine* -> sovereign-*` is right and does not change. The clusters it appeared
to block — `sovereign-mesh`'s ingest (3,410), understanding (2,316) and workspace (233), and
`sovereign-api`'s ingest (1,152) — were tagged by the port they serve, not by what they are. Read
module by module, almost none of them is engine code: they implement a trait that `corpus-engine`,
`sovereign-work-atlas` or `sovereign-core` declares, using the roster, the rail, the KV or the
engine handle.

**The rule.** An adapter implements context A's port with context B's capability. It lives with A
when it needs nothing but A's language and the published kernel. It lives with B only when B's
target dependencies already include A — the plugin case, as `sovereign-gliner` implements the
tiered pipeline's `ChunkEntityExtractor`, or Understanding's host implements the engine's
enrichment-pass port (`corpus-engine/DECOMPOSITION.md`, Step 7 redrawn). Otherwise it lives in the
composition root, and never in a third context's crate for tier convenience, whose own-context
share would fall.

| module | what it is | home |
|---|---|---|
| `newsworthy_host` | `corpus-engine`'s `NewsworthyHost` over roster, identity, KV and engine | `adapters` |
| `work_atlas_broadcaster` | `sovereign-work-atlas`'s `ClaimBroadcaster` over the rail | `adapters` |
| `auto_ingest`, `auto_resume`, `watched_folder_setup`, `watched_folder_runtime`, `ingest_executor` | ingest run as background work | `jobs` (§3.2) |
| `turn_approval` | `sovereign-core`'s `ApprovalChannel` for one socket; needs only the approval desk | `sovereign-core` |
| `knowledge_client`, `landscape_digest_client` | `sovereign-core` provider traits implemented as HTTP clients of the daemon's own surface | the client family beside `sovereign-turn-client`; inside the daemon the loopback dissolves (TOPOLOGY §3.5) |
| `reading_formatters` | a pure projection over `AtomEnvelope` | Understanding's read model |
| `research_run_dir` | reads the research loop's own run artifacts | `research`, in `sovereign-core` |
| `source_content_validator` | checks model-emitted tool-call arguments for the inference adapter | `sovereign-serving-host` |
| `auto_recover` (api) | the shard manager recovering its own stranded partitions | `sovereign-grants` |
| `routes_knowledge`, `routes_oicp_ingest` (api) | route shells | surfaces |

Nothing goes to `corpus-engine`, nothing goes to `sovereign-enrichment-build`, and no
`[[exception]]` row is added. `sovereign-enrichment-build`, which the cluster graph proposed
because its tier fit, is refused by the rule's last clause: it is the atlas build orchestrator,
and holding ingest pull loops would lower its Understanding share.

### 4.4 The consequence for sovereign-cli-llm

`sovereign-cli-llm` is 611 MB and 1,264 crates because it is the daemon's
closure with a terminal in front. Its 143k lines split by what each module
does:

| group | modules | lines | destination |
|---|---|---|---|
| harness | bench, eval, inner_chaos, quality_lane, search_gym, knowledge_gym, voice_eval, mesh_bench, router_fit, reading_diag, recipe_agent_live_trial | ~68k | a back-of-house `sovereign-cli-bench` that keeps sovereign-eval, gliner and llama |
| state-owning verbs | enrich_cmd (26k), atlas_cmd, corpus_cmd, pipeline_cmd, corpus_snapshot, corpus_watch | ~38k | become job submitters; the engine code they call in-process moves behind a `JobExecutor` |
| clients | chat, ring, job, claim, workflow, govern, backlog, meshapp, mesh_member, portfolio, notes | ~20k | the dispatcher, or a client sibling with no inference dep |
| in-process model loads outside bench | `corpus extract-entities` (GlinerExtractor), `mesh` warm-cache and `LlamaBackend::init` | 3 sites | a job kind each |
| awareness | awareness_cmd (7.8k, behind a feature already) | | stays feature-gated with the harness |

The test that the split is real is a dependency, not a line count:
`cargo tree -p sovereign-cli --features dev-tools -i llama-cpp-4` returns
nothing, and the same for `ort` and `iroh`. Today the edge that makes this
impossible is `sovereign-mesh → sovereign-inference`: a client that imports
mesh types for a roster inherits llama-cpp. The cut is either a feature on
sovereign-mesh or moving the client-facing types (roster, peer, join
protocol) to a leaf crate, and the sv-surface predicate (desktop names no
sovereign-mesh) already pushes toward the leaf.

The `*_http` handlers in sovereign-mesh are the daemon's. The desktop no longer
names sovereign-mesh, so they move to the host crate §4.1 names, and
sovereign-mesh becomes what its name says: the mesh.

## 5. Sequencing

Standing bar from TOPOLOGY §10: every phase names the state it makes
unrepresentable, or is labelled scaffolding.

**Relation to the `domains` campaign.** Its moves relocate code into §4.1's crate
and change no behaviour; these phases change the wire surface and job semantics.
They stay separate campaigns — fusing them is the refactor that also changes
error semantics (ARCH 2) — and share only the crate, whose layout is §3's
classes. Phases 0 and 1 do not depend on the move and can land on either side of
it.

**Phase 0 — census as a ratchet.** `scripts/daemon-route-census.py` runs in
`cargo xtask quality` with two counts baselined: unique paths, and paths
registered by more than one host. Neither may rise. Scaffolding by the bar,
and honest about it. Deletes nothing.

**Phase 1 — one host per path.** Resolve sovereign-server (§3.6). State made
unrepresentable: `/v1/conversations` with two implementations, and one
daemon registering `/internal/corpus/status` twice. Bar: `--dupes` lists only
the worker-proxy paths (4). Deletes: up to 6,940 lines.

**Phase 2 — `/v1/jobs` with one kind.** Local ingest first, because it is the
kind with cancel and progress today and the one the four failing invariants
name. Decide the progress carrier (§3.2) before writing the executor. State
made unrepresentable: an ingest without a lease; a cancel that cannot reach
the writer. Bar: `every-exclusive-store-has-a-lease` and
`cancellation-reaches-the-writer` in `TOPOLOGY.toml` flip to `holds = true`
for corpus-index, and a desktop cancel stops a CLI-submitted ingest in a test
that runs both. Deletes: `lc_http.rs` ingest/progress/cancel handlers,
`IngestProgress`, `LocalCorpusProgress`, the CLI's corpus_cmd in-process
ingest.

**Phase 3 — the remaining kinds.** enrich, reindex, atlas-build, compaction,
model-load, transfers, skeleton, project-rebuild, watch. The CLI's 28 direct
writes become submissions. State made unrepresentable: a shell process
writing corpus-index or enrichment. Bar: `write-is-acyclic` and
`a-process-writes-only-what-it-is-granted` flip; `SOVEREIGN_ENRICH_SKIP_INDEX`
is deleted from `env-flags.toml` because the state it guarded cannot occur.
Deletes: ~70 paths, 22 status enums, `corpus_watch_http.rs`,
`CancellationRegistry`'s cross-process claim, and the engine-side ingest
entry points the CLI called.

**Phase 4 — one read family per resource.** Atlas 3 → 1 (§3.4). State made
unrepresentable: a schema change that lands in two read surfaces. Bar: the
turn-client's `atlas_*`, `conv_*`, `corpus_atoms*` methods call one route
family. Deletes: `meshapp_http.rs` and the reading half of `reading_http.rs`
as separate routers.

**Phase 5 — one scope resolver.** Measure first (§3.3), then one
`principal → Scope` resolver. State made unrepresentable: a request whose
ceiling was never resolved. Bar: `sensitive_corpora` has no permissive
default; three grant vocabularies resolve through one function.

**Phase 6 — the CLI split.** §4. State made unrepresentable: a client verb
that links inference. Bar: the three `cargo tree -i` checks return nothing
for the dispatcher. Deletes: `sovereign-cli-llm` as a crate; its harness half
is renamed, its client half moves, its state-owning half became Phase 3's
submitters.

Phases 1, 4 and 5 are independent of 2 and 3 and can run alongside. Phase 6
depends on 3. Cargo-touching work is one worker at a time.

## 6. Pre-registered predictions and kill bars

Bars must exist before the data or the verdict is not honest.

| prediction | number | falsified if |
|---|---|---|
| unique paths after Phase 3 | 242 → ≤ 170 (−76 job paths, +4) | the census reads above 180 |
| status/progress vocabularies after Phase 3 | 22 enums → 1 + one payload per kind | a second `*Status` enum survives on a job surface |
| topology invariants failing after Phase 3 | 4 → 0 | any of the four still `holds = false` |
| dispatcher inference deps after Phase 6 | llama-cpp-4, ort, iroh: 3 → 0 | any `cargo tree -i` returns a path |
| `sovereign-cli-llm` binary after Phase 6 | 611 MB → gone; bench binary ≤ 611 MB; dispatcher stays ≤ 420 MB | dispatcher grows past 420 MB |
| public fields on a node-wide state type (§4.2) | 63 → 0; the type is gone | a struct outside `assemble` holds parts of more than one owner |
| writes into a part after its construction | `install_*`/`with_*` calls → 0 | an install slot resolves a construction cycle |
| functions outside `assemble` taking the whole node | every handler → 0 | one exists |
| caller-identity resolvers (§3.3) | 5 → 1, at the edge | a context reads a header to learn who is asking |

**Kill bars.** If a local job's submit-to-leased latency measures above
100 ms p50 on this host, the jobs family is wrong for anything a UI waits on,
and any verb that turned out to be interactive moves back to turn or resource
before Phase 3 continues. If the progress carrier cannot be decided per kind
without a second channel for the same unit, the rail gets a `Progress` act
before Phase 2's executor is written, not a workaround beside it. If Phase 1's
launch census finds a shipped path for sovereign-server, it becomes a proxy
and nothing else, and its 8 duplicate paths still go. If staging the node's
construction finds two parts that need each other and no created-first signal
breaks the cycle, they are one part: merge them and say which. If `SelfClaims`
needs more than about five inputs from other contexts' internals, Fabric is
computing Serving's claims for it: redraw the port.

## 7. What was not verified

Named so the next reader does not take them as measured.

- Whether the three grant vocabularies already resolve through one function
  at request time. Phase 5's first step.
- Submit-to-leased latency for a self-donated unit. Phase 2's first
  measurement, and the kill bar depends on it.
- The current Runtime reach-through count against TOPOLOGY §3.5's 35 → 0 bar.
- Whether `sovereign-workflow-host`'s `/internal/workflows` surface registers
  through `.route(` at all; the census found none, so it is either nested
  differently or the note describing it is ahead of the code.
- The `out` class is 4 paths by the rules in the script; `/v1/apps` and the
  app proxy may be a resource (an installed bundle) rather than out. The
  rule is a claim, not a measurement.
- Which callers other than the owner can start a turn on the daemon (§3.3):
  the size of the unwired-ceiling exposure.
- `SelfClaims`' exact inputs. §4.2's five come from reading gossip and the
  capabilities builder, not from a port drafted against them.
- How `EmbeddedDaemon`'s lines split between Fabric and the daemon; §4.1's
  69,000 counts all of `daemon`.
