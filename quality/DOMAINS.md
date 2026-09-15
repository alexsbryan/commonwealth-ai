# Domains — ten bounded contexts, and the test that says whether one is real

Drafted 2026-09-11. This is the THIRD leg of the daemon design and the one the
other two cite without having. `quality/TOPOLOGY.md` §3.5 is the Runtime's
internal shape; `quality/DAEMON_CORE.md` is the surface, its route census and
its placement test. Both answer "where does this code go" against a structure
neither declares. This file declares it.

Every number below is `wc -l` over `**/*.rs` unless the row names another
source. That is a DIFFERENT instrument from `cargo xtask size-gate`, which
excludes comments and blanks and reads 586,878 code lines across 76 crates —
do not mix the two in one sentence. The counts here are for proportion, not
for a ratchet, and nothing in this file is a gate.

## 1. What sovereign is

**Sovereign answers a question from what you already have, says where the
answer came from, and declines when it cannot.**

That sentence is the core domain. `sovereign/README.md` states it as the
product promise — answers "cited from sources you keep locally", nothing
leaving the device unless asked — and the bench apparatus measures exactly it.

Everything else in this repository serves that sentence in one of two ways: it
**widens what "already have" means** (a corpus, a codebase, a peer's library, a
trusted group's machines) or it **makes a bigger model reachable** (pods,
donation, distributed inference). Neither is the core. That distinction is the
knife this file cuts with, and it is why the mesh — the most technically
interesting thing here — is a SUPPORTING context.

Inference execution, storage and transport are generic subdomains: llama.cpp,
SQLite/LanceDB, iroh. Vendored, not invested in.

## 2. The test: a context is real when it is individually applicable

Operator direction, 2026-09-11. A bounded context is not an opinion about
naming. **It is real when someone outside this repository can take it alone and
get value from it**, and we have run that test four times already:

| Lift | What it proved | Status |
|---|---|---|
| `studio` (`studio/BOUNDARY.md`) | the workflow/recipe authoring closure builds outside the monorepo, 36 s cold, zero source edits | lifted 2026-07-21 |
| `corpus-mcp` (`corpus-mcp/README.md`) | a third party can search a corpus-engine index AND read what its enrichment produced against a plain `llama-server`, with nothing carrying llama.cpp, ort or iroh | declared, gated |
| `commonwealth-work` (`scripts/cw-work-lift.sh`) | a package-only peer builds outside the monorepo in 7.7 s and completes three heterogeneous units inside a container boundary | lifted, re-proven 2026-09-11 |
| `commonwealth-rails` (`scripts/cw-rails-lift.sh`) | the lifted daemon joins a REAL mesh by invite and serves another member's library | lifted 2026-09-11 |

This is not a new mechanism and must not become one. `[[package]]` in
`quality/ARCH_LAYERS.toml` and `cargo xtask boundary-gate` already exist, and
the lift scripts already exist. What this file adds is the OBSERVATION that the
four lifts are the same test applied to four contexts, and that the test
generalises: **for each context below, name the standalone thing someone could
use.** A context that cannot answer that is not a context — it is a layer, or a
module, or a folder.

The test has teeth in both directions. `corpus-mcp` is the sharper of the four
precisely because it made the ENRICHMENT usable by a bare `llama-server`: the
thing that proved the boundary was a customer who wanted one half and not the
other.

## 3. What is measured, and what it says

Three crates carry most of this repository and each one's name describes the
first thing built in it rather than what it holds:

| Crate | Its own description | Actual |
|---|---|---|
| `sovereign-mesh` | lib.rs: "Commonwealth mesh integration layer… embeds the daemon, parses deep links, translates mesh state" — a ~2,000-line adapter | 82,241 lines, 97 modules; membership is ~11% |
| `commonwealth-api` | the mesh's HTTP surface | 49,634 lines; four of its six domains are not mesh, and its only real consumer is `sovereign-mesh` |
| `corpus-engine` | the corpus engine | 196,677 lines; `enrichment/` alone is 93,427, plus agent memory and build watchers |

**The mechanical cause is shared words, and it is measurable.** `Peer*`
resolves to 34 distinct types across 11 crates, carrying seven unrelated
concepts: a dialable address (`PeerContact`, `PeerEndpoint`, `PeerPath`), a
roster member with trust (`PeerTrustLevel`), a serving candidate with latency
history (`PeerInferenceEndpoint`, `PeerObservationRecord`), a rate-limited
caller (`PeerInflightGuard`, `PeerTally`), an operator policy row
(`PeerPreference`, defined four times down the stack), a knowledge source
(`PeerAtlasView`), and a fan-out target (`PeerRow`, `PeerVerdict`). `Node*` is
24 across 12 crates. `Corpus*` is 60.

Seven concepts under one word means the types get shared by accident, which
means the crates do. That is why `sovereign-mesh` names 21 in-repo crates, and
it is the same disease `cw-lift 3a` already diagnosed at CRATE level on
2026-09-04 — "the prefix meant three different things at once and the domain
boundary could not be read off a manifest" — one level down and never named.

## 4. The ten contexts

Each is named by the question it answers and by the word it owns exclusively.
"Applicable as" is the §2 test: the standalone thing a stranger could use.

### Core — the product is these two

**Understanding** — *what is this material about?*
Owns **atom**, tension, gap, ontology, seed, domain, field.
~99,900 lines: `corpus-engine/src/enrichment/` (93,427, of which `atlas/`
39,740 and `pipeline/` 27,135), `meta_atlas/`, `atlas_traversal/`.
Eight design documents of its own under `corpus-engine/` (`ATLAS.md`,
`ENRICHMENT_V2.md`, `INCREMENTAL_ATLAS.md`, …). `Tension*` is 9 of its 10
definitions inside `enrichment/` — the strongest context signal in the repo.
*Applicable as:* point it at a folder, get back what the material is about —
its domains, its tensions, its gaps. Half-proven already: `corpus-mcp` reads
what enrichment produced against a bare `llama-server`.

**Answering** — *what is the answer, and where did it come from?*
Owns **turn**, `Scope`, `Capabilities`, `Lane`, citation, refusal.
`sovereign-core` is 129,372, of which `runtime/` is 67,481. `TOPOLOGY.md` §3.5
already designs this context's interior; its 35 → 0 reach-through bar is this
context asserting itself against the Runtime.
*Applicable as:* an endpoint that answers cited from an index you hand it, or
declines.

### Supporting

**Ingest** — *how do bytes become chunks?* Owns **source**, recipe, extractor,
chunk. ~29,000: `extractors/` 16,292, `recipe*.rs` ~5,000, `chunkers/` 2,746,
`acquirers/` 2,102, `filters/` 1,934, `pii.rs`.
*Applicable as:* a recipe-driven document pipeline.

**Retrieval store** — *given a query, which chunks?* Owns **index**, shard,
embedding, neighbor. ~17,000: `index/` 9,253, `sharding.rs`, `snapshot*`,
`registry.rs`.
*Applicable as:* a searchable local index. Closest of the ten to generic —
that is a reason to invest less, not a reason to fuse it.

**Serving** — *which engine answers this, and can I prove why?* Owns **slot**,
model, decision, admission, replay. ~37,000: 19,102 in `sovereign-mesh`
(`peer_inference` 5,399, `scheduler_core`, `oicp_select`, `predicted_time`,
`tier`, `decision_log`/`_replay`/`_trace`, `prompt_compactor`,
`throughput_tracking`, `model_fetch`), 16,450 in `commonwealth-api`
(`frontdoor` 5,820, `routes_inference`, `routes_responses`, `admission`),
plus `commonwealth-inference` 1,491.
**It has no crate, no name and no doc, and it is invisible to the route
census**: all eight of its core modules register ZERO routes (verified
2026-09-11), because it is reached by function call from the turn path. That is
why `DAEMON_CORE.md` classes the whole of it as "turn — already designed".
*Applicable as:* a router in front of N OpenAI-compatible endpoints with
admission control and replayable decisions. The most obviously saleable thing
in this repository and the least owned.

**Fabric** — *who is in this group, how do I reach them, how do we agree?*
Owns **member**, node identity, reach, ring. The `commonwealth` package: 9
crates, 37,363 lines, zero `[[exception]]` rows across four widenings
(`commonwealth/BOUNDARY.md`). Plus ~14,200 of sovereign-side adapter currently
inside `sovereign-mesh` (membership 8,734, rail 4,130, media 1,343).
*Applicable as:* already proven twice — `cw-rails` joined a real mesh.

**Compute** — *whose machine runs this, and under what boundary?* Owns
**unit**, offer, lease, isolation, donor. ~19,700: `commonwealth-work` 8,152 is
the vocabulary half and is in the package; `sovereign-mesh`'s eleven
worker/pod modules plus `work_donor` and the guest tunnel are the executor
half.
*Applicable as:* proven — `cw-work-lift.sh` runs three heterogeneous units
outside the monorepo inside a rootless container.

**Workbench** — *what is in this codebase?* Owns **symbol**, call graph, edit.
~23,000: `corpus-engine-scip` 12,273, `enrichment/code_intel/` 2,274,
`corpus-engine-sections`, and next-edit's 7,808 (`next_edit*` in
`commonwealth-api` plus `fim_adapter`/`lsp_tier` in `sovereign-mesh`).
The `code-intel` package is already declared (`docs/CODE_TOOLING_BOUNDARY.md`),
5 of a target 9 crates.
*Applicable as:* a code-intelligence MCP server. Next-edit is its clearest lost
child — an IDE completion service whose spec is `sovereign/docs/NEXT_EDIT.md`,
living in the mesh's HTTP crate.

**Workspace** — *what does the assistant remember about its own work?* Owns
**note**, feature, plan item, design signal. ~17,800: `corpus-engine-notes`
11,985, `corpus-engine-atos` 2,850, `corpus-engine-archaeology` 2,514.
`DECOMPOSITION.md` already labels this tier "agent state (independent of the
data plane)" — it saw that this is not a corpus and filed it as a tier of
`corpus-engine` anyway.
*Applicable as:* a notes/decisions store any agent harness could mount.

**Build feedback** — *did it compile, did the tests pass?* Owns **lint result**,
test result, watcher. `corpus-engine-watchers` 3,294. The smallest and the most
obviously misfiled; nothing about it is knowledge.
*Applicable as:* barely — this is the one context where "fold it into
Workbench or delete it" is a live answer, and the §2 test is what says so.

### Not contexts

The **published language** — `oicp-types`, `kernel-types`, `corpus-engine-vocab`
— is the shared kernel between contexts, not an eleventh.
The **Host** is the composition root: `DAEMON_CORE.md`'s four things plus the
app surfaces. "What belongs in the daemon" was hard to answer until that file
asked it as a placement test, and the reason it was hard is that the daemon is
not a domain.

## 5. The context map, and the two calls in it

Answering → Understanding, Answering → Retrieval store and Answering → Serving
are customer/supplier: the caller asks, the supplier serves, and the supplier
does not know who called.

**Serving → Fabric and Compute → Fabric each need an anticorruption layer, and
their absence is the defect.** Fabric's `member` must become Serving's
`candidate` and Compute's `donor` by explicit translation. Today all three say
`peer` and therefore share types, and that single fact is the edge
`DAEMON_CORE.md` §4 names as the blocker on the CLI split: a client that
imports mesh types for a roster inherits llama-cpp.

**Understanding needs a published read model, and it is already half-built.**
Its language leaks: `Atom*` is 23 definitions with only 10 inside
`enrichment/`, `Seed*` 6 of 12, `Cluster*` 4 of 13. Consumers re-derived its
nouns because the context published nothing. `corpus-engine-vocab` (5,161
lines) exists so a host can read `atlas/atoms.json` without linking
`corpus-engine` — that is the read model, and finishing it is what stops the
leak.

So the two calls:

1. **Retire `peer` as a type name.** Fabric says `Member`. Serving says
   `Candidate`. Compute says `Donor`. Admission says `Caller`. Knowledge says
   `Source`. This is structural rather than policing: once the words differ the
   accidental type sharing cannot be expressed, and the crate graph has to state
   what it means.
2. **Finish `corpus-engine-vocab` as Understanding's published language**, and
   make every consumer outside the context read through it.

## 6. Where layers and domains part company

`quality/ARCH_LAYERS.toml` is a LAYER map and it is good at what it does: it
answers *what may depend on what*. It cannot answer *where does one word mean
one thing*, and no amount of tiering will make it.

`corpus-engine/DECOMPOSITION.md` is the clearest case. Its ten-crate target is
a tier ordering — "dep arrows go up the tier numbers, never down" — and four of
its tiers are different CONTEXTS wearing tier numbers: Tier 3 is Workspace
(and its own parenthetical says so), Tier 4 is Workbench, Tier 5 is Build
feedback, Tier 7 is Understanding. The tiers are right about dependency
direction and wrong about what the pieces are, which is why the plan can be
followed to completion and still leave a crate named `corpus-engine` holding an
atlas, an agent's memory and a test watcher.

One correction the measurement forces: that plan estimates the enrichment carve
at **~40,000 lines**. `enrichment/` is **93,427** today, and ~40,000 is roughly
what `atlas/` alone has grown to.

## 7. Sequencing

Executed by the `domains` campaign — flight rules in
`quality/campaigns/domains.toml`, the registry the instruments read in
`quality/DOMAINS.toml` (contexts, owned words, module tags, the Peer
dispositions, the cross-context edges, the collision verdicts), and per-rung
orders under `.sovereign/features/`. Drafted 2026-09-13; the campaign's
Decisions section records where its inventory overrode this file (§8,
"Re-measured"; §10).

**Re-sequenced 2026-09-14, operator direction: god crates first.** The phase
order below was wrong in one way that matters: it sequenced by CONCERN (names,
then the read model, then a crate, then the rest), so the largest misnamed
crate would have been demolished last and by four different rungs. The
campaign now sequences by CRATE, biggest first, and the phases fold in. The
loop: take the largest crate whose modules are not all its own context; the
cluster that IS its context stays; every other cluster moves to its context's
home crate (the registry's `crates` list, creating the home when it does not
exist); leaf clusters first, then by size; an edge that crosses crates after
the move goes through a port already designed (§10.3,
`sovereign/SERVING_BOUNDARY.md`) or the cluster splits, never a new exception
row; a rename rides the move; each destination whose own-context share drops
is enqueued, which is the breadth-first half; stop when the queue is empty and
the predicate holds. Phase A's renames ride the moves; Phase C is wave 1's
serving cluster leaving `sovereign-mesh`; Phase B is wave 3's first move out
of `corpus-engine`; Phase D is the loop. Wave 1 is `sovereign-mesh`: Fabric
(16,028 lines) stays, ten clusters leave. The algorithm runs as
`scripts/domains-census.py plan` (rung 3), not as a list a person keeps.

No phase here moves a line until the one before it has named something, because
the reason this repository grew three misnamed crates is that code with no home
goes wherever it links.

**Phase 0 — empty `commonwealth/crates/`.** Operator direction, run ahead of
Phase A on branch `domains-1-empty-the-commonwealth-directory` and finished
2026-09-11. Six crates whose names described a family they were not in left the
directory in two commits, changing no logic: `commonwealth-{api,inference,
knowledge,app}` became `sovereign-{api,serving,grants,meshapp-registry}`
(domains-1), then `commonwealth-test-harness` became
`sovereign/crates/sovereign-mesh-test-harness` and `oicp-conformance` moved to
the repo root beside the two crates it certifies against (domains-2). What
remains under `commonwealth/` is the nine-crate package and nothing else, and
`scripts/cw-work-lift.sh --sandbox` reported verdict 1 after each move — that
reading is the invariant, not the crate count. The ledger of what went where is
`sovereign/SYSTEM_OVERVIEW.md` §5. Ordering was BIG-FIRST (operator, reversing
the builder): a small crate's destination is decided by where the big one
lands, so moving it first moves it twice.

**Phase A — name the ten, retire the shared words.** No code moves. The
deliverable is this file plus the rename of `Peer*` per context (§5, call 1).
State made unrepresentable: a type that is a mesh member and a serving candidate
at once.

**Phase B — Understanding publishes.** Finish `corpus-engine-vocab` as the read
model; every consumer outside the context reads through it. State made
unrepresentable: a crate re-deriving `Atom` because it could not read one.

**Phase C — Serving gets a crate.** The largest homeless context, and nothing
else can be cleanly cut while it is fused to the turn path. Its lift test is
§2's: a router in front of N endpoints, built outside this monorepo.

**Phase D — the rest, in `DAEMON_CORE.md`'s order.** Its Phases 0-1 are
independent of everything here and can start immediately; its job class (76
paths → 4) is Compute's vocabulary reaching the surfaces at last.

`corpus-engine`'s own tier plan proceeds underneath all of this, unchanged in
ordering and re-labelled by context.

## 8. Pre-registered predictions

Bars before data, or the verdict is not honest (ARCH §18.1).

| prediction | number | falsified if |
|---|---|---|
| `Peer*` type definitions after Phase A | 34 → ≤ 12, in ≤ 4 crates | any crate outside Fabric defines a `Peer*` type |
| `Atom*` definitions outside `enrichment/` after Phase B | 13 → 0 | any consumer still declares its own |
| contexts with a named standalone artifact | 4 of 10 today → 10 of 10 | a context cannot answer "applicable as" and is kept anyway |
| `sovereign-mesh` after Phase C + D | 82,241 → ≤ 20,000, and its name means the mesh | it still holds a scheduler |
| crates whose name describes < half their contents | 3 (`sovereign-mesh`, `commonwealth-api` — `sovereign-api` since Phase 0, same crate, and the rename does not move this bar — and `corpus-engine`) → 0 | any survives Phase D |

**Kill bar.** If a context cannot name the standalone thing someone would use
it for, it is not a context and this file is wrong about it — merge it into its
neighbour and say which. Build feedback is the first candidate and is written
down as such rather than defended.

**Re-measured 2026-09-13, before the campaign's first row.** Leading with what
was wrong, so the table above is read as the 09-11 reading and the toml's
floors as the baseline:

- Row 1's denominator was unstated. Prefix-`pub` reads **39 across 13 crates**
  (26 outside the commonwealth package), not 34 across 11. Sweeping for `Peer`
  anywhere in a type name finds 21 more definitions and a fourteenth crate
  (`commonwealth-discovery`): **50 pub-ish, 35 outside the package in 9
  crates**. A prefix bar is passable by renaming `PeerFoo` to `MeshPeerFoo`,
  so the campaign counts the word. Two carve-outs are registry rows:
  `PeerStore` (a replicated KV, renamed apart) and `PeerAnswer` in
  kernel-types (egress custody, kept). The seven concepts in §3 hold and are
  nine: replicated KV and egress custody were missing.
- Row 2 would read green today while the defect stands. All 20 `atoms.json`
  readers already use `corpus-engine-vocab`'s `AtomsFile`; none declares its
  own struct. The leak is the DOOR: `read_atlas_atoms` is at
  `corpus-engine/src/enrichment/atlas/writer.rs:595`, not in vocab, so
  `corpus-mcp/src/tools.rs:873` hand-rolls one and nine sites bypass it. The
  bar becomes "pub `Atom*` outside vocab and `enrichment/`, minus three named
  axum binders": **12 → 0**, and the door invariant is made structural rather
  than counted. `Cluster*` is never persisted under `atlas/` and leaves §5's
  leak sentence; `Seed*`'s outside count is 2 atlas nouns, not 6.
- Row 3 counted studio, which §4 calls not a context, and `corpus-mcp`, which
  is a host. By the strict rule (a `[[package]]` the gate passes) it is **3 of
  10**: Fabric and Compute via `commonwealth`, Workbench via `code-intel`.
- Row 4's floor is **88,255**, not 82,241 — `sv-surface` moved desktop
  surfaces onto daemon routes hosted in `sovereign-mesh` in the two days
  between. `*_http.rs` is 26 files / 22,017 lines against `DAEMON_CORE.md`'s
  22 / 18,319. By module tags the crate is host 25,432 / serving 20,488 /
  fabric 16,028 / compute 9,740 / back-of-house 4,841 / workbench 4,811 /
  ingest 3,410 / understanding 2,316 / answering 747 / workspace 233 /
  retrieval 209 — the bar is reachable at 16,775 only if every non-Fabric
  tag leaves, and the host's 25,432 is a daemon-core campaign's to move.
- §4 Serving measures **44,485 lines** module by module (38,665 if
  `frontdoor.rs`, which reshapes prompts for third-party harnesses, is Host).
  The scheduler half (6,559, seven modules — `throughput_tracking` is host-tier,
  not scheduler: it names `commonwealth-state`; SERVING_BOUNDARY.md "Corrected
  2026-09-14") already imports nothing foreign; the knot is
  `peer_inference.rs` (5,399). The anticorruption layer §5 asks for exists at
  `sovereign-mesh/src/daemon.rs:2286-2345` reading nine fields; it is misnamed
  and lives in the host's god object. `sovereign-serving`, the peg, carries
  eleven exported types with zero external references and 771 lines of
  knowledge-shard assignment that drag `corpus-engine`.
- §5's words collide. `Candidate` has five first-party definitions in five
  crates; `Member` two; `Caller` one; `Source` is already Ingest's by §4 and
  is in the duplicate-name baseline. `Donor` is clean. The words are decided
  in rung `domains-4` against `converge noun` evidence, not here.
- §9's `corpus-engine-vocab` figure of 5,161 is right; a non-recursive
  `wc -l src/*.rs` gives 2,769 and is the trap §2's preamble warns about.
- `scripts/daemon-route-census.py:25` still names
  `commonwealth/crates/commonwealth-api/src`, gone since `domains-1`; every
  route `sovereign-api` registers is uncounted and today's 213 unique paths
  is an under-count. Repaired by rung `domains-3`.

## 9. What was not verified

Named so the next reader does not take them as measured.

- Whether `deep_research/` (25,597 lines in `sovereign-core`) is inside
  Answering or is an eleventh context. It has its own campaign orders; it was
  not classified here.
- The Answering context's line count. `sovereign-core` is 129,372 total and
  `runtime/` is 67,481, but no module-by-module pass was done the way
  `sovereign-mesh` and `corpus-engine` got one.
- Whether the seven `Peer*` meanings are seven or fewer. They were read from
  type names and module paths, not from call sites.
- Whether Retrieval store is genuinely distinguishable from a generic vector
  store, or whether the sharding and snapshot behaviour makes it custom.
- Every "applicable as" in §4 except the four in §2's table. Those four were
  run; the other six are claims about what a lift WOULD show.

## 10. Adjudications — rung `domains-4`, 2026-09-14

Every collision the three inventories surfaced is now one interface or a named
split. The rows are data — `quality/DOMAINS.toml` `[[noun]]` (72, all
`decided:`), `[[edge]]` (13), `[[collision]]` (20) — and this section is the
argument beside them. Nothing here moved code; rungs 5-10 do.

### 10.1 The words

A context owns a NOUN, matched anywhere in a type name. Role suffixes
(`Candidate`, `Record`, `View`, `Kind`, `Result`) are not words: `Candidate`
has five definitions and 23 kin across four contexts, and owning it would
rename `TensionCandidate` and `RelayCandidate` to move no coupling. §5's list
is therefore revised, each against `converge noun` output pasted in the
registry rows:

| context | §5 said | decided | evidence |
|---|---|---|---|
| Fabric | Member | **`Member`** (type stays `MemberRecord`) | exclusive after `suggest_seams.rs:64,87` rename to `SeamSymbol`/`SharedSeamSymbol` (13 + 6 sites); `code_capability_graph.rs:91` is back-of-house and exempt |
| Serving | Candidate | **`Venue`** | 0 definitions; one kin `kernel_types::quality::VenueAction` (18 sites / 3 files) renames to `TriggerAction`. `Seat` refused as a synonym of `Slot`; `Lender` refused — `guest_lender.rs:17-27` already means the pinned grant and says "a lender is not a peer" |
| Compute | Donor | **`Donor`** | 0 definitions; sole kin `WorkDonorHandle` is Compute's |
| Admission | Caller | **`Principal`** | 0 definitions; three of four kin are the owner (`sovereign-api/src/principal.rs:93,125,147`); converges with `DAEMON_CORE.md` §1's `principal → Scope` — zero renames |
| Knowledge | Source | **no word — not a context** | every candidate failed the exclusivity test, which is the kill bar firing: the atlas half is Understanding's (`RemoteAtlasView`, `AtlasPullLead`), the ask-a-peer half is Retrieval's (`RemoteIndexTarget`, `RemoteIndexReply`). `Source` is Ingest's word and the kernel's type (`kernel_types::origin::Source`); `Library` is Fabric's media noun |

Two match modes follow. `owns` matches anywhere; `owns_exact` matches the bare
name — Ingest's `Source`, whose 72 kin mean "which producer". Definitions in
`kernel` and `back-of-house` tagged crates count against no owner and are
printed as exempt, never hidden.

### 10.2 The Peer rows — 72 decided: keep 11, rename 56, merge 5, delete 0

Named adjudications: `PeerRow` was two concepts (`MemberFanoutRow` in
commonwealth-transport, `TravelMeasurementRow` in cli-llm). The three
spellings of one live-path fact collapse: `PeerPathSnapshot` survives,
`PeerTransportPath` merges into it, `IrohPeerPath` becomes **`MemberReach`**
and moves down to `sovereign-contracts` — the Fabric-published view every
other context reads. `PeerPreference`, defined four times down the stack,
becomes one owner (`VenuePreferenceStore`) and one DTO. The `PeerStore` family
is a replicated KV and says so (`ReplicatedKv*`). `PeerAnswer` in kernel-types
is kept: egress custody, the one place the word is load-bearing.
`oicp_types::PeerDescriptor` renames (`FederatedMeshDescriptor`) with its
serde names unchanged — oicp-types is in the bar's population, and the wire is
not.

### 10.3 The edges — one translation each, and one that is already a defect

`MemberRecord` crosses into Serving today through
`EmbeddedDaemon::peer_inference_endpoints` (`sovereign-mesh/src/daemon.rs:2286-2345`),
which reads eight fields plus three filters — over the `dm-shared-edges`
ceiling of about four. The split that keeps the ceiling honest: four fields
are Fabric REACH (`MemberReach`: node id, name, endpoints, alive), two are
CAPABILITY and arrive through the published `oicp_types::Capability` claims,
not off the roster row, and three are Serving's own (latency history,
availability, quarantine). A `Venue` is `MemberReach` + claims + Serving's
three; the Fabric translation carries four. E1 crosses as two methods and zero
fields; E2 as one `f32`. E10 (`PeerAtlasView::from_member`,
`sovereign-tools/src/atlas_peer_advice.rs:67`) is the working pattern and is
renamed, not redesigned.

E7 is a live defect, not a smell. `PeerTrustLevel` carries
`#[serde(rename_all = "snake_case")]` (`commonwealth-core/src/mesh/mod.rs:399-405`),
so its published spelling is `model_and_knowledge_sharing`, while
`routes_oicp.rs:346`'s `format!("{:?}").to_lowercase()` emits
`modelandknowledgesharing`. Two spellings of one closed set, one produced by a
`Debug` derive. The fix is a closed wire enum in oicp-types, and the failing
input exists today.

### 10.4 Serving's collisions — eight families

| family | verdict | survivor / renames | rung |
|---|---|---|---|
| `Tier` ×3 + `TierFloor` | split, four concepts | `TierFloor` (scheduler); `role::Tier` → `PreferredSlot` (31 sites); `apps::Tier` → `Origin` (16, wire-visible, serde rename); peg's `Tier` deleted. Not `LatencyClass`: `tier.rs:105-111` pre-registers that seam | 9, 5 |
| routing model (13 dead types) | delete | zero external references each, by `callers`. Salvage: `UnavailableReason`'s shape → a closed `GateReason` replacing `Verdict::Gated{gate: String}`. Cascade: `MeshPlan` loses four of seven fields and dies with four `store_adapter` methods and three harness helpers, all zero-caller | 9 |
| eight "candidate" nouns | converge the noun, split the role | one `Venue`; `VenueView`/`SelfView`/`ManifestView`/`Ranked`; score type is `oicp_types::ScoredClaim`, kept under its published name; the three `as` aliases at `oicp_select.rs:32-34` deleted; `LocalCandidateView` stays — its asymmetry is finding F1 in the type system | 5, 10 |
| four "admission" deciders | split | Serving keeps `Admission`; rail → `Inclusion`; runtime → `TurnLease`; native grounding → `Answerability` (128 sites). The decider already exists in a tier-0 leaf, `serving_policy::fair_sched::SchedCore`; admission needs its axum/`AppState` coupling cut, not a new decider | 10 |
| two `decision_log`s | split | mesh module → `routing_decisions`, types `RoutingDecision*`; studio's keeps the tool id in four registries. `SOVEREIGN_DECISION_LOG`, `oicp-decision/v1`, `DECISION_TRACE_TARGET` do not move | 10 |
| `GuestLender*` | split by side of the link | four types move to the serving host unrenamed; `StoredGuestLink` is host wiring; `sovereign-grants` keeps `GuestGrant`/`Scope`. `GuestLenderSource` is NOT `PeerEndpointSource`: enumerate vs lookup-by-model-id, `Vec` vs `Option`, a 60 s TTL with `invalidate()` on 401, and a guest is a PIN that beats selection (`peer_inference.rs:3104`). Two ports | 10 |
| `frontdoor.rs` | Host, confirmed | two production callers, 23 sites (`routes_responses.rs` 14, `routes_inference.rs` 9), both above the routing decision; imports no scheduler, candidate, score or admission type | — |
| the public interface | five sketches | the two ports, the scheduler entry with its real signature, admission on `SchedCore`, the decision record + replay, and what `sovereign-cli-daemon`'s eight sites call — the basis of `sovereign/SERVING_BOUNDARY.md`'s rules | 9 |

### 10.5 Understanding's collisions — and one correction to Phase B

| family | verdict | survivor / renames |
|---|---|---|
| `Gap` ×3 | split two, converge two | `quality/CONCEPTS.toml:831-843` already decided it; the atlas detector's `Gap` becomes **`Lacuna`** (49 sites / 11 files; `atlas/gaps.json` and its key stay via serde rename), deep-research's becomes `GapRow` (19 / 8). kernel-types refused: either survivor drags `AtomId`/`ChunkRef` or `AcquisitionRoute` to layer 0 |
| `Domain` | split; Understanding does not take the word | the atlas noun has no type (`git grep domains.json -- '*.rs'` = 0); the plugin `Domain` → **`FieldModel`** (45 refs), `DomainRegistry` → `FieldModelRegistry`. `Pass` refused: `EnrichmentPassRegistry` is a step, a domain is a genre |
| `Seed` | split | six homonyms rename (37 refs / 11 files); `SeedError`/`SeedReport` are Understanding's; the outside count is 1 → 0, not 2 |
| `AtomSpan` ×2 | converge into vocab | owned, `atom_type: AtomType` (closed set at `corpus-engine-vocab/src/atoms.rs:1104`); `AtomType::from_label` must be minted |
| `Cluster` (39 defs, six families) | not a noun | `writer.rs` has zero matches; struck from owned words |
| read model (21 defs, 5 crates) | converge to 16 in 2 | one `AtlasPage<T>`; `SectionRef` + `EvidenceExcerpt` one type; `RelatedAtom`/`CrossCorpusLink` each defined twice; `AtomHead` collapses a four-producer field set; **`AtomCard` deletes** (a lossy mirror of `AtomEnvelope`); `read_atlas_ontology` is minted, not moved (its only inline site is `context_loader.rs:711`) |
| archaeology `Atom*` | split, not allow-list | `AtomProvenance` → `AnchorHistory` (27 / 4), `AtomWitness` → `WitnessTally` (13 / 1). The allow-list is the three axum binders and nothing else; I1 is 12 → 0 |
| `Source` ×4 | converge on `kernel_types::origin::Source` | none of the four is Ingest's recipe source; `KeySet` (6), `ItemSet` (12, studio), `BundleLocation` (10) rename apart |

**The correction.** `pub(crate)` on `AtomsFile.atoms` does not close the door:
the derived `Deserialize` stays public and `serde_json::from_str::<AtomsFile>`
still works outside vocab. The structural form is the `Evidence` pattern — a
private wire twin that deserialises, `AtomsFile` itself not `Deserialize`, so
`vocab::read` is the only constructor. `thiserror` would breach the leaf's
dependency budget; `Display`/`Error` are hand-written.

### 10.6 §9's open items, closed

- **`deep_research/` is an eleventh context, `research`** (25,597 lines, 17
  module rows). Its reach into the rest of `sovereign-core` is 28 `use crate::`
  lines and two touch `runtime/`; its provider boundary is already a trait
  (`ResearchPort`, `deep_research/estate.rs`); `Charter` is an exclusive word.
  Answering answers one turn from an index it is handed; research acquires
  across rounds. `dm-contexts-liftable`'s target moves 10 → 11 with the row.
- **Retrieval store is not generic.** `index/search.rs` (1,245 lines) is; the
  rest is not — the two-key `merge_shards` dedupe and its newest-mtime
  propagation (`sharding.rs:760-780`), `Evidence`'s sealed door,
  `ChunkProvenance`'s egress floor, RAPTOR's separate table, the snapshot's
  embedding-model refusal. §4's "invest less" is true of `search.rs`, not of
  the context.
- The seven `Peer*` meanings are nine plus scaffolding (§8, re-measured).
- `frontdoor.rs` is Host (§10.4).

## 11. Destinations decided — the design rungs, 2026-09-14

Design only, reviewed by the operator before any move. The arguments live in the boundary
documents; each paragraph here is what a registry row records.

### 11.1 Where the rest is argued

The host crate, the dissolution of `AppState` and the adapter rule are `quality/DAEMON_CORE.md`
§4. The caller's identity — which corrects §10.1's gift of `Principal` to Serving — is its §3.3.
Serving's corrections head `sovereign/SERVING_BOUNDARY.md`. The `[[forbid]] corpus-engine* ->
sovereign-*` row stands, and none of the clusters it appeared to block goes to `corpus-engine` or
`sovereign-enrichment-build`.

### 11.2 Compute

Leading with what the registry had wrong: Compute's `crates` list named `sovereign-compute`, and
the cluster graph chose it by name and tier. Every module of `sovereign-compute` keeps a loaded
model in a supervised child process and routes inference to it — Serving's local engine and a
single-owner resource — with no `commonwealth-work` edge and no unit, lease or donor.

| piece | what it is | home |
|---|---|---|
| `commonwealth-work` | vocabulary, fold, lease predicate, executor trait and registry, process executor, sandbox | stays — it is the context |
| `work_donor`, `ingest_executor` | the node running units off the work fold | `sovereign-daemon`'s `jobs` |
| rented pods: `worker_pod`, `worker_http`, `worker_inference_proxy`, `worker_controller`, `worker_daemon`, `worker_subprocess_runner`, `multi_pod_coordinator` (6,373 lines) | leasing a rented machine and running work on it — Compute's remote isolation | a new Compute crate, `sovereign-pods` — except `worker_pod`'s owner↔pod wire protocol, which moved to the shared `sovereign-contracts` leaf (REVIEW-build-serving-worker-port, 2026-09-15) so the serving host can name it without a third package `[[exception]]` |
| `worker_eligibility`, `pinned_pod_snapshot`, `pinned_transport` (1,863) | which RPC inference workers may hold a shard; a pinned pod presented as a venue | `sovereign-serving-host` |
| `guest_tunnel` (134) | an iroh dial to a lender, exposed as a local address | Fabric, `sovereign-mesh` — it is reach |
| `sovereign-compute` | Serving's local engine | re-tagged `serving`; rename proposed, `sovereign-slots` |

Three findings ride with the table. **Pods speak a third unit vocabulary**, beside
`commonwealth-work`'s units and `sovereign-grants`' legacy ingest lease. A rented pod is a donor
whose isolation is a VM — the `oicp_types::Isolation` variant exists — and converging pods onto
`commonwealth-work` units is a behaviour rung; after it, if `sovereign-pods` names only the
commonwealth package and leaves, it joins that package. **One config table holds two contexts**:
`ComputeSection` carries both the slot children and the donor's `work_offer`, and splits in a
config rung with a compatibility reader. **One supervisor**: `sovereign-compute`'s supervisor is
context-neutral and the only one in the tree with restart, backoff and a persisted crash log;
`worker_subprocess_runner` repeats half of it, and `commonwealth-work`'s process executor calls
itself the seventh spawn-with-timeout, kept because of `commonwealth-work -> sovereign-*`. That
forbid names the sovereign family, not a leaf, so process supervision becomes a leaf outside it
that all three reach down to.
