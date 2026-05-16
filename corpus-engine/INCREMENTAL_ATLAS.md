# Incremental atlas updates (Move 6)

## What this is

Atlas state evolves under per-document deltas instead of full corpus
rebuilds. Per the perf-inventory report, the dominant cost on the
recurring ingest path was full atlas rebuild on every incremental
change:

- Wikipedia newsworthy daily refresh triggered a full structural
  rebuild over 1.6M wiki atoms for a ~100-chunk per-article delta —
  ~16,000× overhead.
- Watched folders re-indexed the whole file on a one-line edit.
- Wikipedia delta-ingest didn't update the atlas at all — state
  lagged the index until a manual rebuild.

Move 6 lands the substrate (content-hash atom IDs + doc index +
atoms-delta primitive + per-pipeline `extract_for_docs` factor)
plus the source-side wiring scaffold. The retrieval surface stays
identical; only the ingest cost shape changes.

## The substrate

### Content-hash atom IDs (P0)

Atom id = `<type>-<16 hex chars of blake3>` derived from the atom's
identifying fields + the owning corpus_id. Re-extracting the same
canonical entity produces the same id every time. Cross-corpus
edges + meta-atlas anchors survive deltas.

- `AtomId::entity_content_hash(canonical_name, entity_type, corpus_id)`
- `AtomId::event_content_hash(description, event_type, first_section_id, corpus_id)`
- … (per variant; see `enrichment/atlas/atoms.rs`)
- `AtomId::is_content_hash()` predicate for migration idempotency.

**Migration**: `sovereign atlas migrate-ids [--corpus|--all] [--dry-run]`
rewrites atoms.json + edges.json + cross_corpus_edges.json from
sequential `entity-NNNN` to content-hash. Idempotent. Handles intra-
atom references (`Event.participants`, `State.entity_id`, etc.) +
edges (source/target). Cross-corpus edges' `peer.atom_id` is
intentionally left for post-migration `detect_grounding` refresh on
the peer side.

### Doc → atoms sidecar (P1)

`atlas/doc_to_atoms.json` records which atoms each source document
produced. Read by the atoms-delta primitive to find "every atom
owned by article X" in O(1).

```jsonc
{
  "schema_version": "1.0",
  "by_doc": {
    "Albert_Einstein": ["entity-ab12cd34ef567890", "event-..."],
    "Isaac_Newton": ["entity-cd56ef78901234ab", ...]
  }
}
```

Backfill CLI: `sovereign atlas build-doc-index [--corpus|--all]`.
Derives the sidecar from atoms.json by walking each atom's primary
anchor (`first_appearance.chunk_id` for Entity/Position/Opposition;
`section_position` for Event/ArgumentReconstruction; etc.).

### Atoms-delta primitive (P2)

```rust
pub struct AtomsDelta {
    pub added: Vec<AtomEnvelope>,
    pub removed_doc_ids: Vec<String>,
    pub upserted_docs: Vec<(String, Vec<AtomEnvelope>)>,
    pub added_edges: Vec<Edge>,
}

pub fn apply_atom_delta(atlas_dir, delta) -> Result<DeltaSummary>;
```

Atomic per-file rewrite of atoms.json + doc_to_atoms.json +
edges.json + cross_corpus_edges.json. Drops atoms in
`removed_doc_ids`/`upserted_docs`'s doc set; inserts atoms from
`added`/`upserted_docs`; drops edges with dropped endpoints;
merges new edges (dedup-by-id, in-place replace on collision).

Atomicity contract: per-file `<file>.tmp` + rename, sequential.
Multi-file fault model: if process killed mid-renames, atoms.json
is canonical and sidecars can be recovered via `build-doc-index`.

### StructureFirst per-doc extraction (P3)

`extract_atoms_for_articles(articles, corpus_id, cfg) -> StructureFirstDelta`
takes a set of `AggregatedArticle` records (one per source doc) and
emits a `StructureFirstDelta { atoms_delta, edges }`. Each Entity
atom uses content-hash IDs. Outgoing wikilink targets get placeholder
atoms grouped under synthetic doc_id `_placeholders`.

### Meta-atlas partial rebuild (P7)

`rebuild_for_corpus(indexes_dir, target_corpus_id, meta_atlas_path)`
drops anchors belonging to one corpus and re-walks just that corpus's
atlas. Cost: O(target_atoms) vs O(total_atoms) for full
`build_meta_atlas`. Newsworthy refresh meta-atlas update goes from
seconds to milliseconds.

### Delta-aware chunking primitive (P6)

`chunk_delta(old_chunks, new_chunks, hash_fn) -> ChunkDiff` matches
new chunks against old by content_hash. Returns
`{deleted: Vec<id>, added: Vec<TextChunk>, kept_unchanged: Vec<id>}`.
Pairs with `reindex_file` so a one-line edit on a 1000-line file
re-embeds ~1 chunk instead of 30. Integration with `reindex_file` is
the next PR.

## Source-side wiring scaffold

### Newsworthy hook (P5.a wiring)

`NewsworthyHost::on_chunks_committed_with_docs(committed)` —
extends the existing per-tick atlas-rebuild dispatch with per-doc
delta records `CommittedDocs { corpus_id, role, doc_ids }`. Default
impl strips doc_ids + delegates to legacy `on_chunks_committed`.

Watcher accumulates refreshed/fetched article titles + portal date
strings in `TickReport`, passes them via the new hook.

`MeshNewsworthyHost` implementation logs the per-doc delta + the
`SOVEREIGN_ATLAS_INCREMENTAL` flag state, then falls through to the
existing `rebuild_structural_atlas` full-rebuild path. The
incremental computation (P5.a.1) is staged in a follow-up because
it needs the LanceDB `chunks_by_source_doc_id` query +
`extract_atoms_for_articles` + `apply_atom_delta` glue.

### Watched-folder + delta-ingest hooks (P5.b/c — pending)

Hooks for `update/watch.rs::flush_ready` and `update/delta.rs::apply_update`
follow the same shape: collect doc_ids touched in the batch, call
`apply_atom_delta` per corpus. Land after content-hash migration
completes on the live install.

## Observability

- `sovereign atlas migrate-ids` — one-shot migration to content-hash.
- `sovereign atlas build-doc-index` — backfill doc_to_atoms sidecar.
- `sovereign atlas stats` — per-corpus atom count, doc count,
  stability, articulation histogram.
- `tracing::info!(corpus, role, doc_count, "newsworthy.atlas_delta_received")` —
  per-tick log from the watcher → host hook.

## Deployment recipe

The substrate code is in tree but the new id-generation paths are
NOT activated by the running daemon. Switching is a coordinated
flip:

```bash
# On each machine, independently. Must run after ALL pipelines
# (e.g. in-flight SEP ingest) have finished writing atoms.json
# with sequential ids.
sovereign daemon stop
sovereign atlas migrate-ids --all --dry-run    # preview + collision scan
sovereign atlas migrate-ids --all              # actual migration
sovereign atlas build-doc-index --all          # populate sidecar
sovereign meta-atlas build                     # refresh anchor.atom_id refs
sovereign daemon start
```

Set `SOVEREIGN_ATLAS_INCREMENTAL=1` in the daemon environment to
activate the per-doc incremental atlas path in `MeshNewsworthyHost`
once P5.a.1 lands. Until then the flag is read but unused (host
logs it + falls through to full rebuild).

## What this defers (later Moves)

- **P5.a.1** — newsworthy incremental computation. Wires
  LanceDB chunks-by-doc query → `extract_atoms_for_articles` →
  `apply_atom_delta` inside `MeshNewsworthyHost`.
- **P5.b** — watched-folder hook. Per-file edit triggers atlas
  delta (gated by `_corpus_meta.json::atlas.incremental_enabled`).
- **P5.c** — delta-ingest hook. Post-phase incremental update for
  monthly wiki manifest delta.
- **P4** — per-section LLM cache for extracted atlases
  (literary/philosophy). Cache key = sha256(text + prompt_version +
  model_id). Skips re-extracting unchanged sections.

## Files

- `corpus-engine/src/enrichment/atlas/atoms.rs` — content-hash AtomId constructors
- `corpus-engine/src/enrichment/atlas/doc_to_atoms.rs` — sidecar
- `corpus-engine/src/enrichment/atlas/atoms_delta.rs` — primitive
- `corpus-engine/src/enrichment/atlas/migrate_ids.rs` — migration logic
- `corpus-engine/src/enrichment/atlas/strategies/structure_first.rs` — per-doc factor
- `corpus-engine/src/meta_atlas/builder.rs::rebuild_for_corpus` — partial rebuild
- `corpus-engine/src/chunkers/mod.rs::chunk_delta` — delta-aware chunking primitive
- `corpus-engine/src/update/newsworthy_watcher.rs` — P5.a wiring + CommittedDocs
- `sovereign/crates/sovereign-mesh/src/newsworthy_host.rs` — host hook stub
- `sovereign/crates/sovereign-cli/src/atlas_cmd/{migrate_ids,build_doc_index,stats}.rs` — CLI surface
