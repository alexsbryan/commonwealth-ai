# corpus-engine Decomposition Plan

A forward-looking plan-of-record for splitting `corpus-engine` (currently a
~121k-LOC god-crate of 41 public modules) into a layered set of focused
crates with one-way dep arrows. Authored 2026-05-23 after the first carve-out
(`corpus-engine-scip`) shipped and demonstrated the pattern.

This document describes **intent**, not current state. For the current
crate map, see `sovereign/SYSTEM_OVERVIEW.md` §2. For the rules this plan
applies, see `sovereign/ARCH_PRINCIPLES.md` §3 (file size), §5 (interface
segregation), §8 (dependency hygiene), §10 (refactor discipline), and §14
(small focused PRs).

---

## Why decompose

`corpus-engine` today is the dep-graph bottleneck of the workspace: 17
direct consumers, 20 transitive — **67% of the 30-crate workspace** is on
the recompile path for any edit. The crate confuses three different
products:

1. The **corpus data plane** (acquire → extract → chunk → embed → index →
   enrich → search) — what "corpus engine" should be.
2. The **agent's persistent state** (notes, project_docs, ATOS features /
   plans / design-signals, lint/test results) — has nothing to do with
   corpora; lives here because that's where rusqlite was wired up.
3. The **agent's freshness machinery** (watchers, scip — now carved out —
   archaeology) — distinct lifecycle, distinct triggers, distinct
   consumers.

Today each consumer uses 1–3 of those concerns but carries the whole
weight. Editing `notes.rs` (a chat-history feature) revalidates the entire
corpus pipeline. Editing `scip_graph.rs` (a code-intel feature, pre-carve)
did the same. The goal is to **align the crate boundaries with the concern
boundaries** so each edit's blast radius is bounded by its actual reach.

---

## Target topology

Ten crates beyond `oicp-types`, organized into layers. Dep arrows go
**up** the tier numbers, never down.

```
Tier 0 — leaf types, no internal deps
  oicp-types                                    (exists — wire types)
  corpus-engine-types                           (CorpusKind, ChunkRange, ShardInfo,
                                                 IndexInfo, IndexStats)

Tier 1 — pure data transformation, no I/O
  corpus-engine-extract                         (chunkers, extractors, filters,
                                                 pii, safety — bytes → chunks)

Tier 2 — storage substrate
  corpus-engine-index                           (LanceDB binding, CorpusIndex,
                                                 search, neighbors, rerank, dedup)

Tier 3 — agent state (independent of the data plane)
  corpus-engine-notes                           (NoteStore, project_docs, notes_sync)
  corpus-engine-atos                            (FeatureStore, plan_items, design_signals)
  corpus-engine-archaeology                     (git_archaeology, archaeology_eval,
                                                 rough_edges)

Tier 4 — code intelligence
  corpus-engine-scip            [DONE 2026-05-23]   (scip_graph, scip_export)

Tier 5 — watchers (background reactive layer)
  corpus-engine-watchers                        (lint_results, test_results,
                                                 watcher_coordinator,
                                                 lint_watcher, test_watcher,
                                                 project_index_watcher)

Tier 6 — corpus data plane (what's LEFT of corpus-engine after carve-outs)
  corpus-engine                                 (engine, ingest, acquirers, recipe,
                                                 registry, sharding, snapshot,
                                                 sovereign_config, progress, yield_hook,
                                                 update/{watch, delta})

Tier 7 — Understanding, as a package (redrawn 2026-09-14: "Step 7, redrawn")
  corpus-engine-vocab → understanding-vocab     (the products and the read door)
  understanding-atlas                           (pure: resolve, classify, walk)
  understanding-host                            (stores, writers, runner — the I/O)

Tier 8 — domain-specific corpora
  corpus-engine-wikipedia                       (wikipedia_graph, canonical_sync,
                                                 extractors/wikipedia_*,
                                                 chunkers/portal_event_bullet,
                                                 update/newsworthy_*,
                                                 acquirers/wikipedia)
```

After full decomposition, `corpus-engine` becomes a coherent ~30k-LOC
ingest pipeline ("the corpus piece of `corpus engine`") rather than a
catch-all.

---

## Per-crate scope, LOC moved, consumer fan-out

| Crate                       | LOC moved (est.) | Direct consumers today | Risk    |
|-----------------------------|------------------|------------------------|---------|
| `corpus-engine-types`       | ~400             | 6+                     | Low     |
| `corpus-engine-extract`     | ~10,000          | 2 (corpus-engine + tests) | Medium  |
| `corpus-engine-index`       | ~5,000           | corpus-engine + enrich  | Medium  |
| `corpus-engine-notes`       | ~4,400           | 11                     | Low     |
| `corpus-engine-atos`        | ~2,800           | 3                      | Low     |
| `corpus-engine-archaeology` | ~2,000           | 1–2                    | Low     |
| `corpus-engine-scip`        | ~3,700 (DONE)    | 7                      | —       |
| `corpus-engine-watchers`    | ~3,300           | 5                      | Low     |
| `understanding-*` (was `corpus-engine-enrich`) | 102,227 measured 2026-09-14, not ~40,000 | 14 crates name it | **Very high** — Step 7, redrawn |
| `corpus-engine-wikipedia`   | ~5,000           | 2–3                    | Medium  |

---

## Sequencing — by blast-radius impact, not by LOC

The right order isn't "biggest first" — it's "highest-frequency edits
decoupled first," because those compound day after day.

| Step | Carve-out                  | Status | Why this order |
|------|----------------------------|--------|----------------|
| 0    | scip                       | DONE   | Cleanest seam in the workspace + active edit zone |
| 1    | **watchers**               | DONE   | Active edit zone. Asymmetric win: watcher-edit rebuild set 22→12 crates (measured; the 18→5 estimate was optimistic — sovereign-tools consumes the result stores and is itself a hub, so R3 shrinks this further) |
| 2    | **atos**                   |        | Smallest consumer set (3). Lets sovereign-atos stop depending on corpus-engine entirely |
| 3    | **notes**                  |        | 11 consumers feels broad, but the API is narrow (NoteStore + a few enum kinds). Big LOC win: notes.rs is 2781 lines, a §3.1 violation |
| 4    | **archaeology**            |        | Two consumers, small surface. Tidy-up |
| 5    | **wikipedia**              |        | First "domain-specific corpus" carve-out. Sets the pattern for future domain-specific carve-outs (SEP, etc.) |
| 6    | **types**                  |        | After the above, the remaining shared types are clearer. Moves them to a leaf crate so siblings depend on them without depending on each other |
| 7    | **enrich**                 | PARTIAL | Hardest carve-out. Save for last — after 1–6, the seam between "what corpus-engine ingests" and "what enrichment does to it" is much clearer. **2026-09-03: the vocabulary half landed as `corpus-engine-vocab`** (atoms, edges, taxonomy, canonical, stable_key — ~3.1k lines, behaviour-free, re-exported at the historical paths) after the drive was collapsed first (§10.7): `[enrichment] type` now resolves through one `EnrichmentPassRegistry`. The behaviour half (pipelines, domains, tiered) stays |
| 8    | **extract + index**        |        | Only if step 7 demands it. Bottom-of-graph cleanup |

Steps 1–4 are tractable as ~1–2-day refactor sessions. Step 5 needs a
Wikipedia-deep dive. Step 6 is bookkeeping. Step 7 is a multi-week effort.

---

## Predicted blast-radius wins

Crates rechecked per edit, with each milestone landed:

| Edit                          | Today | After scip+watchers | After 1–4 | Fully decomposed |
|-------------------------------|-------|---------------------|-----------|------------------|
| scip code                     | 20    | 7 (DONE)            | 7         | 7                |
| lint/test watcher code        | 18    | 18                  | **5**     | 5                |
| NoteStore code                | 18    | 18                  | **11**    | 11               |
| FeatureStore (ATOS)           | 18    | 18                  | **3**     | 3                |
| Wikipedia ingest              | 18    | 18                  | 18        | **~4**           |
| Enrichment pipeline           | 18    | 18                  | 18        | **~6**           |
| Core engine/ingest            | 18    | 18                  | 18        | **8**            |
| Extractor (chunker)           | 18    | 18                  | 18        | **~5**           |

After steps 1–4 (~1 week), **agent-state edits** are all narrow. After
step 7 (~3 weeks), **data-plane edits** are also narrow.

---

## Costs honestly stated

| Phase                                | Calendar    | What it buys |
|--------------------------------------|-------------|--------------|
| Step 1 (watchers)                    | 1.5–2 days  | Watcher iteration becomes fast — the current pain |
| Steps 2–4 (atos, notes, archaeology) | 3–5 days    | Agent-state edits stop touching the corpus pipeline |
| Step 5 (wikipedia)                   | 3–5 days    | Domain-specific code separated; pattern for future corpora |
| Step 6 (types)                       | 1 day       | Bookkeeping crate; no behavior change |
| Step 7 (enrich)                      | 2–3 weeks   | Hardest. Highest LOC reduction. Many internal dep arrows to reverse |
| Step 8 (extract + index)             | 1 week each | Bottom-of-graph cleanup. Only if step 7 demands it |

**Realistic total: 6–8 weeks of focused refactor work.** First 1–4
(~2 weeks) deliver 80% of the felt benefit; the rest is the long tail.

---

## Pattern conventions (from the scip carve-out)

The first carve-out established the conventions every subsequent one
should follow:

1. **New crate has a narrow local Error type** (typically `Io` + the
   crate-specific failure modes). Don't share the wide `corpus_engine::Error`
   — that's the coupling we're escaping. Add a `From<NewCrate::Error> for
   corpus_engine::Error` impl in `corpus-engine/src/error.rs` so internal
   corpus-engine users can still `?`-bubble where needed.
2. **No re-export shim left behind in corpus-engine.** ARCH §8.3 cites
   the `oicp-types` version-skew incident as evidence that lingering
   re-exports invite type-identity bugs. Migrate every consumer to the new
   path; remove the shim in the same PR sequence.
3. **`git mv` to preserve history.** Always rename, never copy + delete.
4. **Phased PRs**: new crate + shim → migrate consumers crate-by-crate →
   drop the shim → docs update. Per ARCH §14.1 each PR reviews
   independently.
5. **Verify after each PR with `SOVEREIGN_LINT_FULL=1 bash
   scripts/sovereign-lint.sh`.** Workspace-wide cargo check. Pass-1
   fail-0 is the bar before moving to the next PR.
6. **Update `SYSTEM_OVERVIEW.md` in the same PR sequence** (per ARCH §1.1).
   Add the new crate to §2; remove any files that were §3.3 outliers
   (now living in their own crate).
7. **Write a memory note** at session end so future sessions don't
   re-litigate decisions made along the way.

---

## What we explicitly will NOT do

- **Don't create a `corpus-engine-error` crate.** Per-crate narrow errors
  with `From` conversions at boundaries is the right pattern. A shared
  error crate becomes a god-trait by another name.
- **Don't generalize a `StateStore` trait across the agent-state crates.**
  ARCH §5 already gave us the StateStore pattern in `sovereign-core`. The
  carved crates expose concrete stores; consumers compose them via
  `sovereign-core`'s narrow traits.
- **Don't carve `enrich` until 1–6 are done.** Enrichment has the deepest
  internal coupling (resolution.rs is 4914 lines; literary_atlas.rs is
  3127). Without the other carve-outs settled, you can't tell where the
  enrichment ↔ ingest boundary actually lives. *Superseded 2026-09-14:* the
  `domains` campaign orders crates biggest-first, and the boundary is now
  measured rather than waited for (Step 7, redrawn).
- **Don't split the remaining `corpus-engine` back into ingest /
  engine / recipe / snapshot sub-crates.** After the decomposition, what
  remains is ~30k LOC of coherent ingest pipeline. Keep it whole unless a
  concrete pain emerges.

---

## How you'll know it worked

Three mile markers, in order:

1. **After step 1 (watchers):** editing watcher code triggers cargo to
   recheck ~5 crates instead of 18. The `sovereign daemon restart →
   check` loop drops from ~90s to ~25s.
2. **After step 4 (notes, atos, archaeology done):** `sovereign-atos`'s
   Cargo.toml no longer lists `corpus-engine`. Any edit to `recipe.rs` no
   longer recompiles `sovereign-atos`. Same shape for notes-only edits.
3. **After step 7 (enrich):** `corpus-engine/src/lib.rs` is under 100
   lines. The remaining ~30k LOC focuses entirely on the acquire →
   extract → chunk → index pipeline. ARCH §3.1 has zero outstanding
   violations in corpus-engine.

That third milestone is the real "perfect separation" — not because
nothing's ever coupled, but because the *seams match the concerns* and
the *concerns match the workflow*. Editing a watcher is a watcher
concern. Editing a note is a note concern. Editing an ingest pipeline is
an ingest concern. None of those should rebuild each other.

**A fourth marker, added 2026-08-28 — and it is the one this list was missing.**
Every marker above measures *rebuild*: blast radius, recompile scope, seconds off
a loop. None measures mass, and over the window that executed these carve-outs the
two moved in opposite directions. Blast radius improved as predicted. Meanwhile
`corpus-engine/src` went **4.3MB to 5.6MB (+30%)**, 210 to 250 files, through
seven extractions — and `notes.rs`, which left at 2,781 lines and was recorded as
cleared from the ARCH §3.1 list, is **7,794** today.

That is not an argument against carving; the blast-radius wins are real and were
delivered. It is an argument that a carve-out is **mass-neutral by construction** —
the leaf was never the tangle, and the origin crate stays the default destination
for whatever arrives next. So a carve-out owes a count that went down, the same way
a drive-collapse does (ARCH §10.7, the pattern to prefer when both are available).
A refactor whose only number is rebuild time has not been measured for the thing
that actually grows.

---

## Step 7, redrawn — Understanding leaves as a package (2026-09-14)

Design only, the `domains` campaign's rung D6, reviewed before anything moves. Leading with what
the plan above had wrong: Tier 7's `corpus-engine-enrich` was one crate of ~40,000 lines, and
what it describes measures 102,227 lines in 170 files (`quality/DOMAINS.md` §6, §8). One crate
that size re-mints the god crate this plan exists to split. The seam the plan waited for is
now measured, and it is three ports and a leaf rather than a line through `enrichment/`.

### The shape

| tier | crate | holds | may name |
|---|---|---|---|
| published language | `corpus-engine-vocab`, renamed `understanding-vocab` when the door moves into it | the products — atoms, edges, ontology, the field skeleton, articulation types, the atlas layout constants — and the read door | `kernel-types` only (its `[[package_leaf]]` row) |
| pure | `understanding-atlas` (108 files, 50,919 lines today) | the arithmetic over the language: analysis, resolution by name, classification, the ground walk, domains, ontology, reconciliation, pipeline types and schemas, meta-atlas and traversal logic | the language, `kernel-types`, and the row types the index leaf publishes; **no `async fn`** |
| host | `understanding-host` (62 files, 51,308 lines) | the capabilities with I/O: stores and the ANN, writers, the field engine, entity extraction, the phase runner | `corpus-engine`, the index leaf, the pure tier, the language |

Measured by scanning each file for the capability it reaches (index, engine, store, embedding,
completion, filesystem, clock, env), with inline tests stripped and 14 false positives adjudicated
by hand. The crate names are proposals for the operator.

**The pure tier is synchronous.** That is the structural form of "arithmetic over the language",
and it has a failing input today: the ground walk awaits an ANN query through `AtlasProvider`.
Four host files — `resolution`, `ground::select`, `atlas_clustering`, `investigation`'s root,
7,535 lines — are impure only because they call an injected embed or completion function, so
vectors or responses passed in would make them pure. Where an algorithm has to embed as it goes,
it stays in the host and says why; that count, not a line total, is what the pure tier is judged
by.

**This is the `corpus-mcp` package growing, not a second package.** `sovereign-enrichment-build`
(the build orchestrator) and `sovereign-enrichment-catalog` (the enrichment store's layout) are
already members of `[[package]] corpus-mcp`, and that package's lift — a third party querying an
index and reading what enrichment produced against a bare `llama-server` — is Understanding's
applicable-as for its query half. The carved tiers join that crate list.

### Direction: Understanding reads Ingest and Retrieval, never the reverse

- **The pass port is the engine's.** Today `engine/ingest` runs enrichment at install by resolving
  `EnrichmentPassRegistry::builtin()` inside `corpus-engine` and handing each pass an
  `EnrichmentContext` (recipe, index, embed and completion functions). The engine is the caller,
  so the `EnrichmentPass` trait and its context belong to `corpus-engine`, and the engine receives
  the registry at construction from whoever assembles it. The passes implement the port in
  `understanding-host`, which already depends on `corpus-engine`. Passes still run at install; what
  changes is who names whom.
- **A config block is owned by its reader and carried by the recipe.** The recipe blocks
  Understanding interprets (the `[enrichment]` table and the ontology and pattern declarations)
  move to the language, and Ingest's `Recipe` embeds them — `corpus-engine` already depends on
  vocab. Today 40 recipe sites in 22 Understanding files are this edge running the wrong way.
- **The same rule settles the index leaf** (below): a type the index persists is defined by the
  index, and the document that configures it embeds it.

### The ports: reuse before minting

| capability | today | decided |
|---|---|---|
| embed texts | `EmbedFn`, named in 19 Understanding files and 87 outside | keep; Retrieval's, published by the leaf |
| complete a prompt | two closure ports for one capability, `InferenceFn` and `ChatCompletionFn`, beside `InferenceProvider::complete` | converge on one closure port before anything moves (ARCH 8), declared by the pure tier |
| read chunks and embeddings | the concrete `CorpusIndex`; `EvidenceFetcher` is the one Understanding-owned read port, implemented by `sovereign-core` and `corpus-mcp` | the leaf publishes the read methods; `EvidenceFetcher` stays the query port |
| the walk's seeds | `AtlasProvider` exposes a lancedb `AnnSeedTable` | closed: seeds arrive as language values, and the ANN store is a host implementation |
| write atlas artifacts | 195 write sites in 34 files, four private atomic-write copies, no store trait | one write door in the host, the mirror of the read door. `AssetStore` was checked and is a different question: content-addressed binary assets, not per-corpus files rewritten in place |
| clock and env | read inline in 12 and 5 host files | `now` passed in; settings as constructor arguments |
| progress | `EnrichmentProgressSink` and `EnrichProgressFn`, the latter aliased again in `sovereign-tools` | reuse; the alias goes |

### The read-port leaf, measured again

It is not self-contained at 5,376 lines. The seven index files name no enrichment code, but they
reach `CorpusIndex` in `index/mod.rs`, which names the recipe's merge policy and display metadata,
the filter configuration, corpus metadata, `CorpusKind` and `StreamAxes`, and carries the field
model's JSON persistence; `index/read` names the chunker's `CommittedChunk`. With `mod.rs` the leaf
is 7,758 lines, and by the rule above the settings it persists and the `{id, content_hash}` row it
reads are defined in the leaf and embedded by the recipe.

The three items that make the retrieval-to-understanding cycle each go to the side both reach
down to. `FieldSkeleton` goes to the language, and its four JSON methods leave `CorpusIndex` as
host functions over the index directory. `stream_axes` splits: the per-corpus metadata goes to the
leaf beside `IndexMeta`, the per-atom articulation types go to the language, and stability
derivation stays with Ingest. `ATLAS_DIRNAME` goes to the language, beside the door.

For `corpus-mcp`'s exception row: the leaf retires it for `serve` once the leaf publishes opening
an index by corpus id, which today is a path join and an open with no engine involved. `ingest`
still names `corpus-engine`, because it ingests, so the row's other two burn-down steps stand.
Correcting the earlier cluster note: `corpus-mcp` writes no atlas file in production — those sites
are test-only, and its `ingest` verb hands the build to `sovereign-enrichment-build`.

### The door

DOMAINS.md §10.5 decided the sealed door: a private wire twin, and `AtomsFile` not
`Deserialize`. The inventory adds where the door is. The `read_atlas_*` functions live in the host
file `writer`, so every reader of a product depends on host code — 64 call sites in 44 files outside
`corpus-engine` — and 9 parsers outside and 3 inside bypass it. The door and the layout constants
move to the language. The incident that justifies sealing it is already pinned by a `corpus-mcp`
test: parsing `ontology.json` as bare policies succeeds with every field defaulted.
`read_atlas_ontology` exists, so nothing is minted.

### Who depends on which tier

| consumer | crates (production sites) | names |
|---|---|---|
| runs enrichment | `sovereign-cli-llm` 413, `sovereign-tools` 219, `sovereign-enrichment-build` 211, `sovereign-mesh` 44 | host |
| queries at turn time | `sovereign-core` 27, `corpus-mcp` 17 | pure tier, language, the query ports |
| reads products | `sovereign-meshapp` 20, `sovereign-api` 7, `sovereign-runtime-recipe` 6, `sovereign-cli` 2 | language |
| implements a port | `sovereign-gliner`, `sovereign-cli-daemon` | host's `ChunkEntityExtractor` |

### Not settled

Whether the algorithms that embed as they go can take vectors in without changing what they
compute; if not, they stay host and the pure tier is smaller than 50,919. `enrichment/atlas/wiki_store/`
was not opened.

---

## Pointers

- **Inspirations + concrete failure modes:** `sovereign/ARCH_PRINCIPLES.md`
  §3 (file size), §5 (interface segregation), §8 (dep hygiene), §10
  (refactor discipline), §14 (small PRs).
- **First carve-out worked example:** the scip move (2026-05-23) — see
  the git history around the move of `scip_graph.rs`,
  `scip_export.rs`, `scip_proto.rs` from `corpus-engine/src/` to
  `corpus-engine-scip/src/`, the local Error type, and the consumer
  migration PRs. Also the memory note
  `project_corpus_engine_scip_carveout.md`.
- **Current workspace state:** `sovereign/SYSTEM_OVERVIEW.md` §2.
- **Why the data tells this story:** the original audit ran via
  `cargo metadata --no-deps --format-version 1` + Python classification
  of each consumer's import sites by concern family. The script is
  preserved in the diagnosis chat thread; rerunning it after each
  carve-out shows the blast-radius reduction.

---

## Status

- [x] **Step 0**: `corpus-engine-scip` (2026-05-23)
- [x] **Step 1**: `corpus-engine-watchers` (2026-07-13) — 3,172 LOC out (6 files: lint/test result stores + watcher_coordinator + lint/test/project-index watchers). Two bonus wins: (a) the `treesitter` gate on the watcher impls was **vestigial** — none touch tree-sitter — so the new crate compiles unconditionally, which also fixed sovereign-tools' feature-unification wart; (b) `sovereign-work-atlas` dropped its **direct** `corpus-engine` dep (the watcher trait was its only direct use → now depends on the small watchers leaf; corpus-engine remains reached transitively via `sovereign-core`, so this trims declared/observed coupling, not work-atlas's rebuild set). Also extracted `corpus-engine-yield` (Tier-0 leaf, one trait) so the shared `YieldHook` has one trait identity across the data plane + watchers — see lesson #6.
- [x] **Step 2**: `corpus-engine-atos` (2026-05-23) — 2,818 LOC out; features.rs §3.1 cleared
- [x] **Step 3**: `corpus-engine-notes` (2026-05-23) — 4,379 LOC out (notes_sync stayed in corpus-engine to break the cycle on `ExtractedDoc`); notes.rs §3.1 cleared
- [x] **Step 4**: `corpus-engine-archaeology` (2026-05-23) — 2,709 LOC out, three consumers
- [ ] Step 5: `corpus-engine-wikipedia`
- [ ] Step 6: `corpus-engine-types`
- [ ] Step 7: Understanding as a package — vocabulary half landed 2026-09-03; redrawn 2026-09-14 above
- [ ] Step 8: `corpus-engine-extract` + `corpus-engine-index` (optional)

### Pattern lessons accumulating

After the four executed carve-outs (scip, atos, notes, archaeology):

1. **Multi-import braces** (`use corpus_engine::{A, B, C};`) need a Python rewriter — sed misses items inside the braces. The script that worked: parse the brace group, partition by destination crate, emit two `use` statements.
2. **Helper `ce_err`-style functions** (`fn ce_err(e: corpus_engine::Error) -> Error`) need a parallel `ce_<crate>_err` per carved-out error type. Multiple may live in the same file when the file calls into multiple carved crates (e.g., recipe_author/project.rs has ce_err, ce_atos_err, ce_notes_err).
3. **Cyclic-dep watch**: if the new crate would need to reach back into corpus-engine for a type (as notes_sync needed `ExtractedDoc`), the bridge file stays in corpus-engine, not the new crate. notes_sync is the worked example.
4. **Feature gates at module-top** (`#![cfg(feature = "stores")]` in rough_edges.rs) leak in the move — drop them as soon as the new crate compiles unconditionally.
5. **`Error::InvalidInput` is the most common missing variant** in the narrow-error pattern. The atos and notes Error enums both needed it. (The watchers carve was the exception — only `Io` is constructed there, so a single-variant enum was the honest narrow error.)
6. **A shared *trait* that both the carved crate and corpus-engine must implement/consume needs a Tier-0 leaf, not a re-export.** The watchers carve needed `YieldHook` on both sides with one trait *identity* (the daemon installs a single `Arc<dyn YieldHook>` on the CorpusEngine AND the watchers). Extract the trait to a dependency-free leaf crate (`corpus-engine-yield`) that both depend on; corpus-engine may keep a crate-root `pub use` of it (that is legitimate API surface, not a shim — it is a single-source type, so no oicp-types-style skew). This differs from the "no re-export shim" rule (#2 / ARCH §8.3), which is about *moved* types the origin no longer uses. Watch for this whenever a carved subsystem shares a callback/hook trait with the data plane.
7. **Check for vestigial feature gates before copying them.** The watcher impls carried a `treesitter` gate inherited from when they were grouped with the SCIP `CodeWatcher`, but grep proved none of them touch tree-sitter. Dropping the gate let the new crate compile unconditionally — and *fixed* a latent feature-unification wart (sovereign-tools imported a `treesitter`-gated symbol without enabling the feature, compiling only by luck of sibling unification). `grep -L 'tree_sitter\|scip\|extractors::code' <moving files>` before you replicate a gate.

Pick up by reading the next unchecked step and the pattern conventions
above, then proceed in the scip-shaped PR sequence.
