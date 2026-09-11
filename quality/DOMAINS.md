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

No phase here moves a line until the one before it has named something, because
the reason this repository grew three misnamed crates is that code with no home
goes wherever it links.

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
| crates whose name describes < half their contents | 3 (`sovereign-mesh`, `commonwealth-api`, `corpus-engine`) → 0 | any survives Phase D |

**Kill bar.** If a context cannot name the standalone thing someone would use
it for, it is not a context and this file is wrong about it — merge it into its
neighbour and say which. Build feedback is the first candidate and is written
down as such rather than defended.

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
