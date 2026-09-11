# The daemon's core — four things, and a placement test for everything else

Drafted 2026-09-11. This is the SURFACE half of the daemon design. The Runtime
half (core-only Runtime, `Scope`/`Capabilities`/`Lane` built per turn, the
35 → 0 reach-through bar) is `quality/TOPOLOGY.md` §3.5 and is not restated.
The client half (desktop attaches as a pure client, one turn driver) is the
`sv-surface` campaign and is not restated either. This document answers the
question those two leave open: of the 242 paths the serving hosts register
today, which belong to the daemon at all, and how each family migrates.

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
kill bar in §5 is there to catch it.

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

## 4. The consequence for sovereign-cli-llm

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

The 18,319 lines of `*_http.rs` in sovereign-mesh are the daemon's handlers
living in a crate the desktop used to link. Once the desktop no longer names
sovereign-mesh, those modules belong in the host crate, and sovereign-mesh
becomes what its name says: the mesh.

## 5. Sequencing

Standing bar from TOPOLOGY §10: every phase names the state it makes
unrepresentable, or is labelled scaffolding.

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

**Kill bars.** If a local job's submit-to-leased latency measures above
100 ms p50 on this host, the jobs family is wrong for anything a UI waits on,
and any verb that turned out to be interactive moves back to turn or resource
before Phase 3 continues. If the progress carrier cannot be decided per kind
without a second channel for the same unit, the rail gets a `Progress` act
before Phase 2's executor is written, not a workaround beside it. If Phase 1's
launch census finds a shipped path for sovereign-server, it becomes a proxy
and nothing else, and its 8 duplicate paths still go.

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
