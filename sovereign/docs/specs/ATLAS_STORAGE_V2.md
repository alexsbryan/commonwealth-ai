# Spec: Atlas Storage v2 — federate, don't merge

> Status: **SHIPPED (2026-06-29)** — the sole atom storage backend. Successor to
> `ATLAS_STORAGE.md` (v1, now **deleted**, commit `edeca426`). See
> `ATLAS_V2_DEPLOYMENT.md` for the migration record. Owner-crates: `corpus-engine`
> (store format + write path + meta-atlas), `sovereign-core` (read path),
> `sovereign-tools` (`atlas_context_manager`, install).

## Why this exists

v1 (`ATLAS_STORAGE.md`) replaced the 38 s / 4.5 GB `atoms.json` query-time
parse with an mmap'd rkyv archive (`atoms.rkyv`) — 0 ms load, ~1 MB RSS — and
moved the build to the install/enrich lifecycle. It was deliberately the
**lightest lift**: keep `atoms.json` canonical, convert-on-load, no re-ship.
It hit its bar and is in production.

This spec asks the *other* question: **if we designed atlas storage from
scratch — knowing exactly how atoms are generated, distributed, and queried —
what is the optimal shape?** Three realities force the answer, and they turn
out to be the same question wearing three hats:

1. **Distribution.** Corpora install from HuggingFace as a single
   `.tar.zst` snapshot (`BulkDownloader` → `SnapshotManifest` →
   `indexes/<corpus>/`). SEP ships **1,770 per-article atlases in one
   archive** via `SnapshotManifest.bundled_corpora`. Install must stay
   "download + drop"; uninstall must stay "delete a directory."
2. **The meta-atlas.** "1,563,346 canonical atoms across 15 corpora" is **not
   a merged store** — it's `meta_atlas/builder.rs` clustering Entity atoms by
   `canonical_key` into `MetaAtom`s whose `Anchor`s point *back* into each
   corpus's `atoms.json`, plus a global `bridge/` layer of cross-corpus
   `BridgeEdge`s (SEP-topic ↔ Wikipedia-topic, `Same/Broader/Narrower/
   Related`). It is already federated.
3. **Generation heterogeneity.** `structure_first` (Wikipedia) emits **1.67M
   Entity atoms + 6.8M `Involves` edges** straight from the wiki link graph
   (`EnrichmentDepth::Structural`, no LLM, includes placeholder atoms for
   off-corpus link targets). `extraction_first` (SEP, per article) emits **all
   ten atom types** via LLM extraction + `resolution.rs` entity-merge/event-
   dedupe (`EnrichmentDepth::Extracted`). Both funnel through one producer
   surface — `AtlasIngestion::ingest → AtlasData → write_atlas_full` — but the
   *shape* of what they produce could not be more different.

**The unifying thesis:** keep atoms **per-corpus, self-contained, and HF-
shippable**, and make the meta-atlas a **query-time federation** over those
stores — a thin global catalog + the existing canonical-cluster index + the
bridge — never a physical merge. This is the only shape that satisfies all
three at once, and it is the direction the code already points.

## What v1 carries as debt

v1 is correct but, freed of the compat constraint, three structural
compromises are visible — and they're exactly what blocks the three realities:

- **Three files for one thing.** A corpus's atlas is `atoms.json` (canonical),
  `atoms.rkyv` (structure, v1's contribution), **and** `atoms.embeddings.bin`
  (the pre-embedded atom vectors that seed `atlas_navigate`). The structure
  and the vectors load on *separate paths* (`AtlasGraph::load_from_disk` vs
  `AtlasContextManager`'s embedding cache). That split is the direct cause of
  the **"0 atlas contexts loaded → navigate silently skipped"** failure we hit
  in the live daemon probe, and of `resolve_atom_id_from_entry` — an
  O(seeds × N) reverse-scan that exists *only* to map a seed embedding back to
  its atom-id because the id isn't stored next to the vector.
- **Projection ⊕ payload duplication.** v1's flat record stores the hot fields
  *and* the full atom as a JSON blob (because `AtomEnvelope` won't derive
  `Archive` — the `serde_json::Value`/`PathBuf` fields). That's why the
  wikipedia archive is 1.2 GB > the 758 MB JSON even after compacting edges.
  A *row* format can't avoid this; a *columnar* one dissolves it (read the
  columns you need; one copy per field).
- **Struct-coupled format.** rkyv ties the on-disk layout to Rust structs, so a
  field change is a version bump that invalidates every archive (we lived the
  v1→v2 schema bump + clobber risk), and validation is O(whole archive) — which
  is why we had to drop to `access_unchecked`. An HF-distributed format needs a
  self-describing, forward-compatible container.

## The access pattern is still the decider

Unchanged from v1, but now with the cross-corpus and vector dimensions made
explicit. Six query-time shapes, none whole-graph:

| Use | Today | Shape |
|---|---|---|
| Point lookup by id | `atoms_by_id.get` | indexed get |
| Typed enumeration (Claim/Entity/Relation) | scan all atoms | **type filter** |
| Vector seeding (`atlas_navigate`) | cosine over `atoms.embeddings.bin` | **ANN over atom vectors** |
| Local traversal (≤2 hops, ~30 seeds) | `edges_by_*` | **CSR adjacency walk** |
| Evidence / provenance | per-variant fields | column read |
| **Cross-corpus** (meta-atlas, bridge) | walk every `atoms.json` | **federation** |

Two consequences: (a) the vector seed is a *first-class* atlas access, not a
side cache — co-locating it with the atom record kills `resolve_*` and the
context split; (b) the cross-corpus shape is a *federation*, not a row in any
single store — it wants a catalog + a bridge, not a merge.

## Phase 0 results (2026-06-27) — `atoms.lance` spike, CONFIRMED

Built `corpus-engine/examples/atoms_lance_proto.rs` and converted the **real**
wikipedia `atoms.json` (723 MB, 1,671,594 atoms) to a columnar `atoms.lance`
(representative scalar/text columns; no embedding column — wikipedia has no atom
vectors). Release, fresh reader process for clean RSS:

| | atoms.json | atoms.rkyv (v1) | **atoms.lance (v2)** |
|---|---|---|---|
| disk | 723 MB | 1204 MB | **139 MB** (5.2× / 8.7× smaller) |
| open | 38 s parse | 0 ms mmap | **2 ms** |
| reader RSS (fresh proc) | ~4.5 GB | ~1 MB | **13 MB open → 32 MB after a full scan** |
| type-filter (1.67M) | — | 44 ms | **19 ms** |
| point-lookup | — | µs | 18 ms (un-indexed; →sub-ms with a scalar index) |

**Inference co-residency (the binding concern): de-risked.** Lance is *already*
a workspace dep and runs in the daemon for `chunks.lance` (`lance = "4"`, the
full family; daemon at 29 threads with Lance live). `atoms.lance` adds **zero
new dependencies and reuses that runtime** — a fresh reader opens in 2 ms /
13 MB / +1 thread (mmap-class, not an engine warmup), faults columns on demand
(OS-reclaimable), and the query-time worker threads are the shared tokio/Lance
pool the daemon already has → **zero marginal threads** in the inference process.

**Caveats:** wikipedia's sparse all-Entity shape compresses unusually well; the
spike omits list columns (aliases/participants/full-evidence/attributes) and the
embedding column, so 139 MB is a *lower bound* on the lossless columnar size
(still far under JSON). The embedding column is a separate dominant axis
(1.67M × dims × 4 ≈ 6.8 GB raw, or IVF-PQ-compressed) relevant to *vector-bearing*
corpora (SEP), not wikipedia. The consumer rewrite (field access → columnar
reads) is unchanged — the spike proves the store, not the migration. **Verdict:
worth it on size + access; the inference risk is largely a non-issue.**

**Dense + vector follow-up (2026-06-27), CONFIRMED.** Ran the same spike on
`enron-sample-multi-wide` (real `extraction_first` atoms — the SEP shape:
6,101 Entities/Relations/Claims with populated `content`/`excerpt`) and a
synthetic-embedding scale run:

| axis | result |
|---|---|
| dense scalar columnar | 1.1 MB vs 3.50 MB JSON (**~3.2× smaller** — less than wikipedia's sparse 5.2×, still a win) |
| raw vector column (200k × 1024) | 781 MB → ~6.5 GB at 1.67M |
| **IVF-PQ index** (200k) | **16 MB → ~134 MB at 1.67M (~49× < raw)** |
| **ANN seed over the column** | **4 ms, returns atom-ids directly** — the `resolve_atom_id_from_entry` killer, proven |

Findings: (1) co-locating vectors is *free* — same raw bytes as today's
`atoms.embeddings.bin` sidecar (enron's is 6.76 MB), just unified, and the
vector-seeded corpora (SEP) have small atom counts; wikipedia has no atom
vectors and its embedding column is simply absent. (2) ANN over the co-located
column returns atom-ids directly, deleting `resolve_*` and the graph/embedding
split. (3) IVF-PQ is the compression lever (49×) but is *additive* — Lance keeps
the raw column too; realizing the 49× means dropping the raw column (a Lance
config lever). (4) **The one real inference-co-residency caveat:** IVF-PQ *build*
is CPU-heavy (k-means spiked to 49 threads / 14.6 s for 200k) — a build-time cost
that must run at install/enrich (off the query thread, as v2 already does), not
co-resident with hot inference. Query-side stays light (4 ms ANN). **Net:** worth
it; the inference risk is bounded to scheduling index builds at lifecycle time.

## The v2 shape

Three stores, each chosen for its access shape and each already an in-tree
idiom — not one hammer:

### A. Per-corpus atom store — `atoms.lance` (columnar + vectors)

Store atoms as a **Lance dataset**, exactly how chunk vectors already ship
(`chunks.lance`): one column per atom field **including the embedding**.

```
atoms.lance   columns: id(u32 interned) · str_id · kind(u8) · name · description ·
                       content · excerpt · subtype · confidence · salience ·
                       aliases(list) · participants(list<u32>) · evidence(list<struct>) ·
                       depth(u8) · is_placeholder(bool) · provenance(struct) ·
                       attributes(json str) · embedding(fixed-size-list<f32, dims>)
```

How each access maps, and what it dissolves:

- **Typed enumeration** = predicate scan of the `kind` column only (the cheap
  one) — not a 1.67M-row record scan. (v1's typed scan was 44 ms reading
  fat rows; columnar touches one narrow column.)
- **Vector seed** = ANN over the `embedding` column → returns **atom-ids
  directly**. `resolve_atom_id_from_entry` and the `AtlasContext` /
  `AtlasGraph` duality **delete entirely**. The "0 contexts" failure cannot
  occur — there is one store, and its vectors *are* a column.
- **Point lookup** = scalar index on `str_id`.
- **Deep read** = select more columns of the same row. **No payload blob, no
  duplication** — "projection vs full atom" becomes column selection. The
  `serde_json::Value` and `PathBuf` blockers become an `attributes` JSON-string
  column and a path-string column. Columnar handles the **heterogeneous depth**
  for free: placeholder entities have null `description`/`content`; claims have
  `content`, entities don't — nullable columns, no padding (v1's flat record
  paid for every field on every atom).
- **Format stability for HF** = inherited from Lance's manifest + format
  versioning, the *same* guarantee `chunks.lance` already relies on to ship.
  No hand-rolled header.

### B. Per-corpus edges — `edges.csr` (compressed sparse row)

The one bespoke piece, justified by the navigate hot path. A CSR triple over
the interned u32 id-space:

```
edges.csr   offsets: [u32; n_atoms+1]   neighbors: [u32; n_edges]   types: [u8; n_edges]   conf: [f32; n_edges]
            (+ a symmetric in-edge CSR, or a single undirected pass)
```

6.8M wikipedia edges → ~40 MB mmap'd; `edges_from(x)` is `neighbors[offsets[x]
..offsets[x+1]]` — **µs/hop**, no parse, no per-edge index probe. Interning the
ids (referenced millions of times across edges) is its own large win and is
*the* canonical compact graph layout. **Alternative:** edges as a second Lance
table with a scalar index on `src` — simpler, one fewer format, but a btree
probe + row fetch per hop (single-digit ms for a ~30-seed BFS). Recommend CSR
for the optimal; note the Lance-table fallback is probably adequate and worth
A/B-ing before committing to the bespoke file.

### C. Global federation — `meta.db` (SQLite)

The meta-atlas is relational, indexed, cross-corpus, and online-updatable —
the RAPTOR/SCIP idiom, not the vector idiom. One global SQLite DB replaces the
scattered `~/.svrnmesh/meta-atlas/*.json`:

```
catalog(corpus_id PK, embedding_model, dims, schema_version, atom_count,
        kind_histogram, stability, fingerprint, installed_at)
canonical(canonical_key, corpus_id, str_id, articulation, salience)   -- MetaAtom anchors, indexed by key
bridge(left_corpus, left_topic, right_corpus, right_topic, relation,
       confidence, signals, source)                                   -- BridgeEdge, today's bridge_edges.json
```

- **`catalog`** is the new keystone: the federation's index of *what is
  installed and in which embedding space*. It makes "1.56M atoms across 15
  corpora" a `SELECT SUM(atom_count)`, and — critically — it records each
  corpus's `(embedding_model, dims)` so the query planner knows which corpora
  are vector-comparable.
- **`canonical`** is `meta_atlas/builder.rs`'s clustering, but as indexed rows
  instead of a re-walked `canonical_atoms.json`. Anchors still point back into
  per-corpus `atoms.lance` (`corpus_id`, `str_id`). `rebuild_for_corpus` →
  `DELETE … WHERE corpus_id=? ; INSERT …` (already O(target)).
- **`bridge`** is the existing `BridgeEdge` set (the SEP↔Wikipedia
  "coalescing"), now a queryable table with indices, online-appendable from the
  `bridge_oplog`.

### Handling the two genuinely hard cross-corpus problems

**Heterogeneous embedding spaces.** 15 corpora, some 768-dim, some 1024-dim,
different models — vectors are *not* comparable across spaces (today's
"FTS-only (dim mismatch with query)"). The federation must not pretend
otherwise:

- **Within a corpus:** ANN stays in that corpus's space. The query is embedded
  **per distinct `(model, dims)` present in `catalog`** (embed once per space,
  fan out), or FTS-falls-back for a space the client can't run.
- **Across corpora:** lean on the **model-agnostic** signals — the `canonical`
  key clustering (name/alias/token, no vectors) and the offline-built `bridge`
  edges (built with ANN *within* a space + LLM adjudication at publish time, so
  the cross-space comparison is paid once, offline, not per query). This is why
  the bridge exists and why it's pre-computed: cross-space alignment is too
  expensive and too noisy to do at query time.

**Id-spaces.** Content-hash ids already partition by `corpus_id`
(`entity-<hash>` over `…|{corpus_id}`), so the global id is `corpus_id::str_id`
— globally unique by construction. Each `atoms.lance` interns its own ids to a
local u32 for CSR compactness; cross-corpus references (`canonical` anchors,
`bridge`) use the full string. The `catalog` is the registry.

## HF distribution: ship the built store

The bundle ships the **built** per-corpus stores; install is download + drop +
register, with **no convert-on-load, ever**:

```
_snapshot_manifest.json            # SnapshotManifest + store_format_version, embedding_model/dims
indexes/<corpus>/
    chunks.lance                   # unchanged
    atlas/atoms.lance              # NEW: replaces atoms.json + atoms.rkyv + atoms.embeddings.bin
    atlas/edges.csr                # NEW: replaces edges.json (read path)
    atlas/atoms.json               # EXPORT only (glassbox/debug), not read
```

- `SnapshotManifest` already carries `embedding_model` + `embedding_dimensions`
  + the `Exact / NameMismatch-probe / DimsMismatch-reject` compat gate — extend
  it with `store_format_version`. On install, the gate runs, then the corpus is
  inserted into `catalog` (its embedding space recorded) and `canonical` +
  `bridge` get `rebuild_for_corpus` — all O(this corpus), off the query thread.
- **SEP's 1,770 stay 1,770.** `bundled_corpora` ships them in one archive;
  install drops 1,770 `atoms.lance` dirs and runs 1,770 cheap
  `rebuild_for_corpus` passes (or one batch pass). No merge, no monolith.
- **Uninstall** = delete `indexes/<corpus>/` + `DELETE FROM catalog/canonical/
  bridge WHERE corpus_id=?`. The federation self-heals.

## Worked examples (the point is concreteness)

- **Install wikipedia (HF):** download `.tar.zst` → drop
  `atlas/atoms.lance` (1.67M rows, vectors included) + `atlas/edges.csr`
  (~40 MB) → compat-gate → `catalog` row + `rebuild_for_corpus`. First query:
  Lance opens lazily, columns fault on access, ANN seeds from the embedding
  column. No 38 s parse, no convert, no separate embedding cache to warm.
- **Cross-corpus query "Einstein":** `canonical.lookup("Einstein")` →
  MetaAtom with anchors in `wikipedia` and `sep-philosophy-of-physics`. Fetch
  each anchor's row from its `atoms.lance`; if the query needs ranking, embed
  the query in each anchor-corpus's space (per `catalog`) and ANN *within* each.
  Bridge edges surface "Wikipedia:Einstein `Broader` SEP:space-and-time."
  Never touched a merged store.
- **Re-enrich one SEP article:** rebuild just `sep-<slug>/atlas/atoms.lance`
  (the `structure_first` per-doc delta path already exists as
  `StructureFirstDelta`; extraction has its own) → `rebuild_for_corpus(slug)`
  → re-run bridge for the touched topics only. 1,769 untouched stores stay
  cold on disk.

## Migration path

The whole migration rests on one fact: **`AtlasGraph` is already the seam.** v1
deliberately hid the storage engine behind a method API (`atom` / `atoms` /
`atoms_of_kind` / `atom_evidence` / `edges_from` / `edges_to` / `edge_degree`);
the consumers in `retrieval.rs` + `atlas_navigate` call those methods, not the
bytes. So the backend swaps rkyv → Lance **inside `AtlasGraph`**, and the blast
radius is contained to that one type plus the handful of call sites — not a
rewrite of retrieval. `atoms.json` stays the canonical source/export throughout;
nothing about how atoms are *generated* changes, only how they're *read*. The
migration reuses the v1 lifecycle muscle: `build_and_write_archive` →
`build_and_write_store` at the same three points (build sidecar, post-install
hook, `sovereign atlas build-store` CLI).

### The one real friction: sync-zero-copy → async-owned

rkyv gives **sync, zero-copy `&'a str`** over an mmap; Lance gives **async,
owned rows** (collected `RecordBatch`es). That impedance is the only genuinely
new thing, and it splits cleanly along the access shapes we measured:

- **Edges stay sync + mmap.** `edges.csr` is a plain binary CSR (offsets /
  neighbors / types), *not* a Lance table — so the hot `atlas_navigate` BFS
  traversal keeps its sync, zero-copy, µs/hop character. No async in the inner
  loop.
- **Atom reads go async-Lance.** `atom(id)` / `atoms_of_kind(kind)` become
  `async` returning owned `AtomRow`s (bounded queries — type-filter, point
  lookup, the seed/neighborhood field reads — all measured at 2–4 ms). The
  consumers are already inside `async fn`s, so this is `.await` + `AtomView<'a>`
  → owned `AtomRow`, mechanical and parity-tested.
- **Why not preload columns into RAM for a sync API?** It would avoid the async
  ripple but resurrect RSS (loading wikipedia's 139 MB scalar columns resident
  vs the measured 13–32 MB paged). Keeping Lance's paged reads preserves the
  RSS win, which is the whole point for the inference box. **Recommend
  async-through; the cost is a bounded `.await`/owned-row ripple, scoped below.**

### Staged sequence (each stage shippable, gated, reversible)

| Stage | Change | Gate | Rollback | Verify |
|---|---|---|---|---|
| **0 — writer (dormant)** | `build_and_write_store` writes `atoms.lance` + `edges.csr` beside the rkyv at the 3 lifecycle points; reader still uses rkyv | `SOVEREIGN_ATLAS_STORE_V2` (off) | flag off = no-op | parity: lance row == rkyv atom on a frozen fixture |
| **1 — reader swap (the risky one)** | `AtlasGraph` backend becomes `Rkyv \| Lance` (CSR edges + async atom reads); `AtomView`→`AtomRow`; `atom`/`atoms_of_kind`/`atlas_navigate` async | per-corpus `read_v2` (off; flip one corpus at a time) | flag off → rkyv reader | parity test + **chaos QA suite** (atlas-grounded answers identical) + the size/RSS/latency re-measure |
| **2 — kill `resolve` + the embedding split** | ANN over the `atoms.lance` embedding column returns atom-ids directly; delete `resolve_atom_id_from_entry`, `AtlasContext`, `atoms.embeddings.bin`, `provider.get` | follows Stage 1 per vector corpus | revert the seeding fn | SEP `atlas_navigate` eval (+essay metric) + chaos |
| **3 — meta-atlas → `meta.db`** | SQLite `catalog`/`canonical`/`bridge` replaces `~/.svrnmesh/meta-atlas/*.json`; cross-corpus federates per embedding space | independent of 1–2 (reads `atoms.json` until then) | keep JSON sidecars | meta-atlas count/lookup parity; SEP↔wiki bridge resolves |
| **4 — HF + retire JSON read** | bundle ships `atoms.lance`+`edges.csr`; `SnapshotManifest.store_format_version`; install = drop+register; retire the `atoms.json` *read* fallback + rkyv | `store_format_version` floor | re-ship / re-derive from `atoms.json` (still present) | HF install of wikipedia **and** the 1,770-corpus SEP bundle, no convert |

**Mixed-mode is the core de-risk.** A corpus carries a `store_format_version`;
the reader picks Lance if present-and-flagged, else rkyv, else convert-on-load.
So corpora migrate **individually** and the system runs **mixed** — flip a tiny
corpus, watch the chaos suite, flip more, flip **wikipedia last**, roll back any
single corpus instantly. No big-bang cutover ever.

### Inference-safety invariants (the binding constraint)

The tuned llama.cpp box is the thing we protect; these are non-negotiable:

1. **Index builds never run co-resident with hot inference.** IVF-PQ k-means
   spiked to 49 threads / 14.6 s (Phase 0). All store + index builds happen at
   install/enrich/CLI time (Stage 0 writer) — exactly where the daemon already
   builds chunk/raptor indexes. The query path never builds.
2. **No new engine, no new runtime.** Lance + its tokio/arrow pool are *already*
   in the daemon (`chunks.lance`); v2 reuses them. Marginal threads ≈ 0.
3. **RSS stays paged.** async-Lance reads fault columns on demand (measured
   13–32 MB), preserving the v1 win; we do **not** preload columns resident.
4. **The hot BFS stays sync.** `edges.csr` mmap keeps `atlas_navigate`'s inner
   loop allocation-free and async-free.

### De-risk assessment (the "can we plan implementation?" gate)

**High confidence:** the seam (`AtlasGraph`), the store numbers (size/RSS/latency
all measured), the lifecycle muscle (reused from v1), mixed-mode per-corpus
rollback, and the inference-safety story (Lance already resident; builds are
lifecycle-time).

**The async/owned-row ripple — swept and enumerated (2026-06-27), CONFIRMED
small.** Filtering false positives (`atom_count`/`edge_count` are method names
shared by `ConvEntityGraph`/`TypedExtension`/`wikipedia_graph`, several already
async on their own graphs), the actual Stage-1 touch points outside
`atlas_context.rs` are:

- `runtime/retrieval.rs` — **5 sites**: `atoms_of_kind` ×3 (`:801/:910/:1247`),
  `atom(pid)` ×1 (`:923`), the `atlas_navigate(...)` call (`:1841`). _(Stale
  as a current pointer: `runtime/retrieval.rs` was later split into the
  `runtime/retrieval/` directory, `atoms_of_kind`/`atom` now live on
  `AtlasGraph` in `atlas_context.rs:211/199`, and the sync `atlas_navigate`
  was itself deleted in Phase B — replaced by `atlas_navigate_ann`
  (`atlas_context.rs:996`), called from `runtime/retrieval/atlas_grounding.rs`.
  Kept here as the historical record of the Stage-1 sweep.)_
- `sovereign-tools/atlas_context_manager.rs` — **3 metric reads** (`:417/:418/
  :945`); `atom_count`/`edge_count` can stay **sync** (cached at graph-open), so
  these need not go async at all.
- `sovereign-cli-llm/eval_cmd/runner.rs` — **1** `atlas_navigate` call (`:1276`)
  + the `pub use` (`:566`).
- Everything else (navigate / `resolve` / verbatim / evidence) is **internal to
  `atlas_context.rs`**, one file. `AtomView`/`EvidenceRef`/`EdgeView` are **never
  named outside** it — consumers receive them via iteration — so `AtomView →
  AtomRow` is contained to the enumeration loop bodies.

So the risky stage is **~9 external call sites + one file's internals**, all
mechanical, behind a parity test. **The bigger refactor is Stage 2, not Stage
1:** `AtlasContext` + `provider.get` are woven through the *loaders*
(`atlas_context_manager` ~8, eval runner ~9 — both build the embedding bag),
plus `evidence_loop.rs:910` and the `atlas_top_k` path — deleting the embedding
split touches more surface than swapping the reader. It is deletion-shaped and
gated after Stage 1, but scope it as the larger of the two.

## Honest risks / open questions

- **Consumer rewrite blast radius.** Every `retrieval.rs` + `atlas_context.rs`
  site moves from `AtomView` field access to columnar reads / Lance queries.
  This is the real cost v1 avoided. Stage it: ship `atoms.lance` behind the
  reader first (reader hides the engine), cut consumers over incrementally.
- **CSR vs Lance-table edges.** Recommend CSR for the optimal; it's the one
  bespoke format. A/B against an `edges.lance` + scalar-index before committing
  — if the navigate BFS is single-digit-ms either way, drop the bespoke file.
- **Lance format on HF.** Shipping `atoms.lance` inherits Lance's
  format-version compatibility surface across producer/consumer versions —
  acceptable because `chunks.lance` already ships this way, but it's a coupling
  to Lance's stability, not ours.
- **Cross-space ranking calibration.** Even with per-space query embedding,
  merging ANN scores *across* models needs calibration (scores aren't
  comparable). The bridge + canonical-key clustering carry most cross-corpus
  weight precisely to avoid cross-space score math; quantify how much the
  per-space ANN merge actually contributes before investing in calibration.
- **Per-corpus vs unified — resolved, but watch it.** Per-corpus + federation
  wins on HF simplicity, lifecycle isolation, and incremental rebuild. The
  pull toward a single partitioned store is cross-corpus analytics at scale; if
  meta-atlas query volume ever dominates, revisit a `catalog`-partitioned
  unified `atoms.lance` — but not before, and not at the cost of "uninstall =
  delete a directory."

## Definition of done (v2 Phase 1)

1. `atoms.lance` + `edges.csr` read path behind `AtlasGraph` (engine hidden);
   wikipedia mmap/lazy-open ≤ v1's 0 ms / ~1 MB RSS, **and on disk < the 758 MB
   `atoms.json`** (columnar + no payload duplication — the size goal v1
   couldn't reach).
2. `resolve_atom_id_from_entry` and `atoms.embeddings.bin` **deleted** (vectors
   are a column; seeding returns ids).
3. `meta.db` catalog + canonical + bridge; meta-atlas counts/lookups served
   from it; `rebuild_for_corpus` O(target).
4. Cross-corpus query embeds per `catalog` embedding space; dim-mismatch
   corpora FTS-fall-back (parity with today's behavior, made explicit).
5. HF install of wikipedia **and** the 1,770-corpus SEP bundle = download +
   drop + register, no convert-on-load; uninstall removes all federation rows.
6. Full workspace `scripts/sovereign-test.sh --human` green; parity test
   (v1-loaded graph vs v2-loaded store yield identical atoms/kinds/evidence on
   a frozen fixture).

## Critical files

- Producer surface (unchanged): `corpus-engine/.../atlas/ingestion.rs`
  (`AtlasIngestion`, `AtlasData`), `strategies/structure_first.rs`,
  `pipeline/pipelines/literary_atlas.rs` (`extraction_first`), `writer.rs`.
- v1 store to evolve: `corpus-engine/.../atlas/archive.rs`
  (`build_and_write_archive` → `build_and_write_store`),
  `sovereign-core/src/atlas_context.rs` (reader),
  `runtime/retrieval/atlas_grounding.rs` (consumers; `runtime/retrieval.rs`
  was later split into the `runtime/retrieval/` directory).
- Federation: `corpus-engine/src/meta_atlas/{builder.rs,index.rs,bridge/}`,
  `cross_corpus.rs`.
- Distribution: `corpus-engine/src/snapshot.rs` (`SnapshotManifest`,
  `bundled_corpora`), `snapshot_restore.rs` (compat gate),
  `engine/ingest_prebuilt.rs`, `acquirers/bulk_download.rs`.
- Embedding cache to retire: `atlas/atoms.embeddings.bin` +
  `atlas_context_manager.rs`'s pre-embed path.
