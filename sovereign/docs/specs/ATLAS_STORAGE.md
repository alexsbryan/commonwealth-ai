# Spec: Atlas Storage — off the big-JSON, lightest lift first

> Status: **proposal** (2026-06-27). A contract per `ARCH_PRINCIPLES §1.1` once
> accepted. Owner-crates: `corpus-engine` (write path + data model,
> `src/enrichment/atlas/atoms.rs`), `sovereign-core` (read path,
> `src/atlas_context.rs` + `src/runtime/retrieval.rs`).

## Why this exists

System 2 (Atlas) stores a corpus's typed atom graph as a **single
`atlas/atoms.json`** (plus six sibling JSONs: `edges, trajectories, gaps,
tensions, cross_corpus_edges, configurations`). `AtlasGraph::load_from_disk`
(`atlas_context.rs:74`) parses the whole file into in-memory HashMaps on **first
request per corpus** (`graph()`, `atlas_context_manager.rs:910`).

That design was correct when the atlas was a write-once build artifact and
neither latency nor memory were in scope. Under query-time loading with real
users it is the wrong shape. Measured (2026-06-27 cold-window trace, wikipedia):

- **~38s synchronous parse** of a 1,671,594-atom atlas, **on the query thread**,
  blocking the user's first question (the silent `merged_pool → expansion_decision`
  gap).
- **GBs of resident RSS** for the materialised HashMaps — the reason `graph()` is
  deliberately lazy (it refuses to load all ~1,700 installed atlases at boot;
  `atlas_context.rs:922-927`).
- A background pre-warm to hide the parse was implemented, **measured to *regress*
  the racing first query** (sync `graph()` double-parses the same 1.67M-atom graph
  concurrently with the async pre-warm under CPU contention: first-query gap 139s
  vs the 38s baseline), and reverted. See the NOTE at the `init_from_cache`
  callsite in `state.rs`.

These are not three problems. They are one: **parse-everything-and-hold-everything
to serve a query that touches tens of atoms.**

The codebase has already answered this for its newer systems. System 3
(RAPTOR/tiered — `ENRICHMENT.md` calls it "the gold standard for user-facing
corpora") is SQLite. System 4 (code-intel) is SQLite behind a **lean read crate**,
`corpus-engine-scip` (rusqlite + prost, no tree-sitter, so the chat runtime gets
fast reads without the heavy build deps). **Atlas and field_model are the JSON
holdouts**, and atlas is the one that costs 38s.

## The access pattern is the decider

Every query-time consumer of the graph is **subset-shaped**, never whole-graph:

| Use | Call site | Shape |
|---|---|---|
| Atlas grounding (re-rank retrieved chunks) | `retrieval.rs` | point lookup by id / chunk |
| Atom enumeration — Claim overview ("most important thing in X") | `retrieval.rs:1245` | **type filter** (`AtomEnvelope::Claim`) |
| Atom enumeration — Entity | `retrieval.rs:803` | **type filter** (`AtomEnvelope::Entity`) |
| `atlas_navigate` traversal | `atlas_context.rs` | local traversal (`edges_by_source.get`, few hops) |
| edge count (a log metric) | `atlas_context_manager.rs:417` | aggregate |

Two consequences:

1. **Nothing needs the whole graph resident.** Point lookups, type filters, and
   local traversals all touch subsets.
2. The type filters today **iterate all 1.67M atoms in RAM** to find the Claim /
   Entity subset (`for atom in atoms_by_id.values() { let AtomEnvelope::Claim(c) =
   atom else { continue } }`). Any indexed store turns that into a subset scan.

No query-time consumer needs the whole graph in memory; the *build* computes
global structure (trajectories, tensions, cross-corpus edges) but that is
write-time and already persisted as data.

## Decision criterion (this spec's emphasis): **lightest migration lift**

Migration lift has three parts — rank options by their total:

- **Read path** — how much do `AtlasGraph`, `atlas_context_manager`, and the
  `retrieval.rs` consumers change?
- **Write path** — how much does the atlas pipeline change (it serialises a struct
  today)?
- **Existing corpora** — every shipped `atoms.json` (HF bundles) must keep working:
  re-build vs convert-on-load.

## Option A — mmap'd zero-copy archive (rkyv). **Recommended; lightest lift.**

The win we need is "stop parsing." A zero-copy archive *is* its own in-memory
layout: you `mmap` the file and access it in place — no parse, no heap
materialisation. This localises the 38s→~0 change to **one function** and largely
**preserves the consumer access shape**, which is why it is the lightest lift.

### How it works

1. **Write (build):** derive `#[derive(rkyv::Archive, Serialize)]` on `AtomEnvelope`,
   its variant structs, `Edge`, and `AtlasGraph`. The pipeline's
   `serde_json::to_writer(atoms.json, &graph)` gains a sibling
   `rkyv::to_bytes(&graph) → atlas/atoms.rkyv`. ~1 line; JSON stays as the
   canonical/human source during migration.
2. **Load (`load_from_disk`):** today = `serde_json::from_reader` → build HashMaps
   (~38s, GBs heap). After = `memmap2::Mmap::map(atoms.rkyv)` + one validation pass
   → an `&ArchivedAtlasGraph` borrowing the mmap. Open + validate is **bounded and
   far below a full parse** (target <1s on wikipedia; validation is a single
   structural walk, not per-atom deserialisation; `access_unchecked` is a pointer
   cast for trusted build artifacts).
3. **Hold:** the graphs cache becomes `HashMap<String, Arc<AtlasArchive>>` where
   `AtlasArchive { bytes: Mmap }` exposes `fn root(&self) -> &ArchivedAtlasGraph`
   (re-derived per call via `rkyv::access` over the owned bytes — cheap, and it
   sidesteps the self-referential-borrow problem cleanly: the root is derived from
   owned bytes on demand rather than stored alongside them).
4. **Read (consumers):** `archived.atoms_by_id.get(id)` and
   `edges_by_source.get(src)` work as today (rkyv ships `ArchivedHashMap` with
   `get`/`values`/`iter`). The mechanical delta: pattern matches become the
   archived variants (`ArchivedAtomEnvelope::Claim(c)`) and string fields are
   `ArchivedString` (deref to `&str`). Bounded to the ~6 sites in the table above —
   same control flow, no query language.

### Why mmap dissolves the memory problem

mmap pages are **file-backed and OS-reclaimable**, not heap-allocated structs. A
point lookup faults in a few pages; a type-filter scan faults in the scanned
range; under memory pressure the OS evicts clean pages. The "GBs of resident RSS
per warm graph" — the reason for the deliberate laziness — **stops being resident
heap**. Lazy-vs-eager loading also stops mattering much, because mapping is ~free.

### The one caveat, and its fix

A type filter still *scans* (iterate archived atoms, match the variant) — rkyv
does not give you SQLite's indexed `WHERE type = 'Claim'`. But the scan is now
over mmap'd bytes at memory-bandwidth with **zero per-atom deserialisation**
(milliseconds-to-tens-of-ms over 1.67M, vs the 38s parse). If even that is too
much for the Claim/Entity enumerations, **archive a secondary index**: precompute
`by_type: HashMap<AtomType, Vec<AtomId>>` at build time and archive it in the same
file. Then the typed enumerations become an archived-index lookup + targeted
`get`s — recovering the indexed-filter win **without** SQL. This is Phase 2, not
required for the cold-start fix.

### Why rkyv specifically

- `capnp`/`flatbuffers` need a schema IDL and a codegen step — heavier, and they
  don't map cleanly onto our existing nested Rust enums/HashMaps.
- `zerocopy`/`bytemuck` are for plain-old-data, not nested `HashMap`/`enum`/`String`.
- `rkyv` is the Rust-native zero-copy archive: derive-based, archives our exact
  types (`HashMap`, enum, `Vec`, `String`), best ergonomics for the existing model.
- New deps: `rkyv` + `memmap2`. (No zero-copy crate is in-tree today; `prost` from
  SCIP is protobuf, not zero-copy.)

### Phase 0 results (2026-06-27) — feasibility CONFIRMED

Spiked in an isolated crate (`/tmp/atlas-rkyv-spike`, rkyv 0.8.16 + memmap2) on
the **real** wikipedia `atoms.json` (758 MB, 1,671,594 atoms), release, fresh
processes for clean RSS:

| | JSON parse (baseline) | rkyv mmap (Option A) |
|---|---|---|
| Load time | 2,066 ms (release) · ~38 s (debug desktop) | **11 ms** (mmap 0 ms + access/validate 7 ms) |
| Resident RSS | **4,542 MB** | **27 MB** |
| Typed full scan (1.67M atoms) | — | **2 ms** |
| Archive size | 759 MB | 690 MB (91% of JSON) |
| Build / write | — | ~1 s one-time |

The cold-start gate (38s → <1s) is cleared by **3400×** vs the debug desktop; RSS
drops **~170×**. Crucially the typed scan is **2 ms** because it touches only the
~27 MB type-tag array — the 690 MB of payload pages in only on field access. So
the **`by_type` index (Phase 2) is likely unnecessary**; defer it until a profile
says otherwise.

**Derive-cascade finding + the design refinement it forces.** Deriving `Archive`
on the *full* `AtomEnvelope` graph (~20 types) hits two real blockers:
`Entity.attributes: serde_json::Map<String, serde_json::Value>` (atoms.rs:459 — no
`Archive` impl) and `Asset.parsed_form: Option<PathBuf>` (atoms.rs:922). The spike
**sidesteps both** and validates the lighter shape: a **flat archived projection**
that *structures only the fields consumers touch* (`id`, `atom_type`, chunk refs,
the display/embed text) and keeps each atom's variant-specific payload as an
**archived bytes blob** (the original JSON, re-parsed only on the rare deep-field
access). This is *lighter* than the original "derive on all 20 types" plan — no
custom `Value`/`PathBuf` impls — and the numbers above are measured against
exactly this shape. Revised recommendation: build the flat projection, not a 1:1
archive of the in-memory graph.

### Honest risks (Option A)

- **Schema-fragility.** An archived layout is tied to the struct shape. Mitigate
  with a `(magic, schema_version)` header; on mismatch, fall back to `atoms.json`
  and re-archive. Because `atoms.rkyv` is a derived artifact (the build or a
  convert-on-load can regenerate it from JSON), this is a re-derive, not data loss.
- **Validation cost vs safety.** `access` (checked) validates the archive — bounded
  but non-zero on 1.67M atoms. `access_unchecked` is a pointer cast. Recommend
  checked-once-at-load for downloaded corpora, measure, and consider unchecked for
  build-produced artifacts. Even checked is ≫ faster than the parse.
- **Lifetime/ownership.** The `AtlasArchive { bytes }` + `root()`-on-demand shape
  above avoids the self-referential borrow; do not store the `&Archived` next to
  its backing bytes.
- **Glassbox.** Binary, unlike `cat atoms.json`. Mitigate with a `sovereign atlas
  dump <corpus>` debug command (and JSON stays canonical during migration anyway).
- **Endianness.** rkyv archives are LE; fine for the x86/ARM (LE) targets.

## Option B — SQLite (`atoms.db`). Heavier lift; the queryable future.

Mirror RAPTOR (System 3) and SCIP (System 4): tables `atoms(id, type, chunk_id,
…)`, `edges(source, target, relation_type)`, indices on `(type)`, `(chunk_id)`,
`(source)`; a lean `corpus-engine-atlas` read crate mirroring `corpus-engine-scip`.

- **Strengths it has and Option A does not:** indexed typed filters by default;
  ad-hoc/analytics and **cross-corpus** queries (the meta-atlas, `cross_corpus_edges`);
  online updates (the delta path, `update/delta.rs`) without rewriting a whole
  archive; full house-idiom consistency (rusqlite is everywhere).
- **Why it is the heavier lift:** the consumers move from HashMap access to
  **queries** — different control flow, a read crate, query/error plumbing across
  every `retrieval.rs` site — plus the write-path rewrite and the same
  existing-corpora migration. Bigger blast radius than swapping a load function.

**Recommendation:** do Option A now (it eliminates the measured pain at the lowest
risk) and treat Option B as a *later* evolution, taken only if/when queryability
(cross-corpus analytics, online deltas at scale) becomes the binding constraint —
not to fix latency/memory, which A already fixes. The two are not exclusive: A's
archived `by_type` index is a stepping stone, and a corpus could carry both.

## Phased migration (lightest-lift-first)

**Phase 0 — Spike + validate (no production change). ✅ DONE 2026-06-27 — PASSED.**
Measured on the real 758 MB / 1.67M-atom wikipedia atlas (see "Phase 0 results"
above): load **38s → 11ms**, RSS **4.5 GB → 27 MB**, typed scan **2 ms**. Gate
(38s → <1s + large RSS drop) cleared with huge margin. Also surfaced the
`serde_json::Value`/`PathBuf` derive blockers and the **flat-projection** refinement
that sidesteps them. No kill criteria triggered.

**Phase 1 — Dual-write + prefer-archive read (the cold-start fix).** Build emits
both `atoms.json` (canonical) and `atoms.rkyv`. `load_from_disk` prefers
`atoms.rkyv`; if absent, it falls back to parsing `atoms.json` **and writes
`atoms.rkyv` beside it** (convert-on-first-load) so every existing shipped corpus
self-upgrades on first use — no re-ship required. Consumers updated to the archived
access shape. **This phase alone removes the 38s cold-start and the GBs RSS**, and
makes the reverted pre-warm permanently unnecessary.

*Execution layers (in progress, 2026-06-27):*
- **L1 — archive module (`corpus-engine/.../atlas/archive.rs`). ✅ DONE, compiles.**
  The flat-projection types (`AtomKindTag`, `ArchChunkRef`, `AtomRecord`,
  `AtlasArchiveData`), the `AtomEnvelope`→`AtomRecord` mapping (structures
  name/label/content/subtype/participants/excerpt/confidence/evidence; full atom as
  JSON payload blob), and `build_atlas_archive_bytes`. `rkyv = "0.8"` added to
  corpus-engine. Validated the data-model field access + rkyv derive + API.
- **L2 — reader (`sovereign-core/src/atlas_context.rs`). ✅ DONE.** `AtlasArchiveHolder`
  (`Backing = Mmap | Owned(AlignedVec)`, `root()` via `access_unchecked` after a
  one-time checked `access` + schema-version gate in `from_backing`); `AtlasGraph`
  holds `Arc<AtlasArchiveHolder>`; a borrowing `AtomView<'a>` (zero-copy `&str` over
  the projected fields, `atom_envelope()` parses the JSON payload on demand) +
  `EvidenceRef<'a>`; methods `atom`/`atoms`/`atoms_of_kind`/`atom_evidence`/
  `edges_from`/`edges_to`/`edge_degree`/`atom_count`/`edge_count`; `load_from_disk`
  prefers `atoms.rkyv`, else parses `atoms.json`+`edges.json` → builds → writes
  `.rkyv` (atomic tmp+rename) → Owned holder (convert-on-load); a `from_parts`
  builder (Owned backing) for the eval CLI + tests. Internal sites rewritten:
  `atom_evidence` (now returns `Vec<EvidenceRef>`), `atom_verbatim_excerpt`
  (payload-parse + `graph.atom()` proponent/attribution lookups), `atlas_navigate`
  (edges via `edges_from`/`edges_to`, evidence via `EvidenceRef`),
  `resolve_atom_id_from_entry` (Entity parse-free via projected name/aliases/
  description; Claim gated by a projected-`content` head pre-filter; the scarce
  Configuration/ArgumentReconstruction parse the payload). `rkyv = "0.8"` + `memmap2
  = "0.9"` added to sovereign-core; `AtomKindTag` re-exported from `atlas_context`.
- **L3 — consumers. ✅ DONE.** retrieval.rs Entity/Relation/Claim enumerations
  (803/909/1245) + the degree count (865) now go through `atoms_of_kind` +
  `AtomView` methods + `edge_degree`; atlas_context_manager.rs metrics (415/417/941)
  use `atom_count()`/`edge_count()`. The other listed sites (manager 778, runner 909)
  read a separately-loaded `AtomsFile` for the embedding context — out of scope, not
  the `AtlasGraph` graph — and are unchanged.
- **L4 — dual-write. ✅ DONE.** `writer::write_atlas_full` (the funnel for every
  persist path) calls `write_atlas_archive_sidecar`, a best-effort `atoms.rkyv` write
  via `build_atlas_archive_bytes` beside `atoms.json` (corpus_id/slug derived from the
  atlas dir, so no caller signature changed; failure is a `warn!`, JSON stays
  canonical).
- **L5 — tests. ✅ DONE.** `atlas_context::archive_io_tests` —
  `from_parts_projects_fields_and_edges` (projection fidelity through `AtomView`:
  name/subtype/description/salience/aliases/evidence/edges/`atom_envelope`) and
  `dual_write_then_convert_on_load_round_trips` (L4 sidecar present after
  `write_atlas`; mmap load; delete-then-reload self-upgrades). Plus
  `archive::archived_read_apis_round_trip` (corpus-engine) exercising the archived
  `by_id`/`edges_by_*`/kind/evidence/payload reads directly.
- **L6 — full suite + a live cold-window re-measure. ✅ DONE.** Full-workspace
  `sovereign-lint.sh` green; `sovereign-test.sh` **7115 pass / 0 fail**. Live
  re-measure on the real installed wikipedia atlas (758 MB / 1,671,594 atoms /
  6,817,035 edges), via the `#[ignore]`d `measure_wikipedia_*` tests in
  `atlas_context.rs` (debug build, fresh processes):

  | path | load time | RSS for the turn | notes |
  |---|---|---|---|
  | OLD `serde_json` parse (on query thread) | **~38 s** | **~4.5 GB** | the baseline this spec exists to kill |
  | NEW mmap (steady state) | **0 ms** | **Δ 1 MB** (12→13 MB) | + a full 1.67M-atom typed scan is **44 ms** |
  | one-time convert-on-load (JSON-only corpus, first load) | 110 s | 9 GB peak | builds+writes `atoms.rkyv`; off every subsequent load |

  The mmap cold load is **0 ms / +1 MB RSS** — the cold-start gate (38s→<1s, GBs→
  mmap-paged) is cleared by a wide margin, and the 44 ms typed scan confirms the
  pages fault in only on access. **Validation decision (measured, not assumed):** an
  initial *checked* `rkyv::access` at load cost **16 s** on this 1.9 GB archive and
  faulted the whole file into RSS (942 MB) — so `from_backing` uses `access_unchecked`
  + a size guard + the schema-version gate (the archive is our own atomically-written
  build artifact; a mismatch/corruption falls back to re-deriving from `atoms.json`).
  This is the "consider unchecked for build-produced artifacts" branch of the risks
  section below.

**Phase 1.5 — Compact edges + build/enrich lifecycle. ✅ DONE (2026-06-27).**
The live measure surfaced two follow-ups (neither blocks the cold-start win — the
steady-state mmap numbers are unchanged, pages fault on access):

- **Archive size.** The real wikipedia archive was **1.9 GB** — larger than the 758 MB
  `atoms.json` — because the graph archives **6.8M edges**, each stored as a JSON blob
  (~1 GB). **Fixed:** edges are now compact [`ArchEdge`] records (source/target +
  `ArchEdgeType` tag + confidence), schema bumped to **v2**. Measured on the real
  wikipedia atlas: **1880 MB → 1204 MB (−36%)**, mmap load still **0 ms / +1 MB RSS**,
  and `edges_from`/`edges_to` no longer JSON-parse per edge. The residual gap over the
  758 MB JSON is the per-atom payload blob (kept for lossless `atom_envelope()` deep
  reads) + the projected hot-path fields — both *cold* mmap pages (zero RSS cost), so
  this is disk-only; dropping the payload (precompute the verbatim/embed strings at
  build time) is a larger refactor, deferred.
- **First-load cost off the query thread.** Convert-on-load is heavy (110 s / 9 GB peak
  debug, one-time per pre-existing JSON-only corpus). **Fixed:** the archive now builds
  at lifecycle time via the SSOT `archive::build_and_write_archive` —
  (a) the build path's `write_atlas_full` sidecar (new corpora), (b) the post-install
  hook (`atlas_postinstall.rs`) for shipped/prebuilt atlases that short-circuit the
  structural-atlas build, and (c) `sovereign atlas build-archive <corpus> | --all
  [--force]` for already-installed corpora. Convert-on-load stays as the self-healing
  fallback. **Note:** the v2 schema bump invalidates any v1 `atoms.rkyv` — a running
  daemon must be rebuilt/restarted so its reader and the on-disk archive agree (a v1
  reader on a v2 file fails the version gate and re-derives v1).

**Phase 2 — Archived `by_type` index. ⚠ Likely UNNECESSARY (Phase 0 measured the
full typed scan at 2 ms).** Only add a precomputed+archived `by_type` index if a
profile on a much larger corpus shows the scan is material. Recovers the
indexed-typed-filter without SQL if ever needed.

**Phase 3 — Retire the JSON read path.** Once corpora are re-shipped (or all
self-upgraded), drop the `atoms.json` *read* fallback; keep JSON as an *export*
(glassbox/debug) if still wanted. Apply the same archive to the six sibling JSONs
as needed (start with `edges.json`, which `AtlasGraph` already folds in).

**Phase 4 (optional, deferred) — SQLite.** Only if cross-corpus queryability or
online-delta scale demands it; reuse the `corpus-engine-scip` lean-read pattern.

## Out of scope (note, don't solve here)

- **field_model (`field_skeleton.json`, System 1).** The other JSON holdout. It is
  far smaller and not implicated in the 38s; the same Phase-0/1 pattern applies if
  it ever shows up in a trace, but it is not driving this spec.
- **The atlas *embedding context*** (`AtlasEntry.embedding`, `load_one`) is loaded
  separately from the structural graph and is cache-only today; this spec targets
  the **graph** (`atoms.json` → 38s). Fold the context in later if profiling says so.

## Verification / definition of done (Phase 1)

1. Cold-window trace on wikipedia: `merged_pool → expansion_decision` gap drops
   from ~38s to <1s; no `lazy-loaded on first request` 1.67M-atom parse on the
   query thread.
2. Resident RSS for a warm wikipedia atlas drops from GBs to mmap-paged (measure
   RSS delta of a point-lookup turn).
3. No consumer regression: the chaos `atom_enum_overview` / Claim-enumeration path
   produces the same atoms (parity test: JSON-loaded graph vs archive-loaded graph
   yield identical `atoms_by_id`/typed sets on a frozen fixture).
4. Existing `atoms.json`-only corpus self-upgrades (writes `atoms.rkyv`) on first
   load and serves the turn.
5. Full workspace `scripts/sovereign-test.sh --human` green.

## Critical files

- Data model + write: `corpus-engine/src/enrichment/atlas/atoms.rs`, the atlas
  pipeline persist, `corpus-engine/src/snapshot.rs` (bundle), `recipe.rs`
  (`declared_artifact_rel_path → atlas/atoms.json`).
- Read/parse (the 38s): `sovereign-core/src/atlas_context.rs:74`
  (`AtlasGraph::load_from_disk`), `:49` (`AtlasGraph` struct), `graph()` +
  `graph_dirs` in `sovereign-tools/src/atlas_context_manager.rs:910`.
- Consumers: `sovereign-core/src/runtime/retrieval.rs:803,909,1245`.
- Existing parse cache to retire: `sovereign-core/src/runtime/evidence_loop.rs:56`.
- Pattern to copy: `corpus-engine-scip` (lean rusqlite read crate) for Option B.
