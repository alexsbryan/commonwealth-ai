# Atlas atom-detail re-parsed the full edges.json per click (1.3GB on Wikipedia = ~1min stall); fixed with a process-global edges cache…

Atlas atom-detail re-parsed the full edges.json per click (1.3GB on Wikipedia = ~1min stall); fixed with a process-global edges cache mirroring cached_atoms

Clicking an article in a notebook's Explore tab calls
`atlas_get_atom_detail` → `FileAtlasReader::get_atom_detail` →
`build_detail` (`sovereign-tools/src/atlas_view/atom_detail.rs`), which
built the one-hop graph context by reading `edges.json` fresh from disk
every click via `read_atlas_edges` (`corpus-engine/.../atlas/writer.rs`:
`fs::read` + `serde_json::from_slice` over the whole file).

Atoms were already cached process-globally (`atom_browse::cached_atoms`,
mtime+size keyed) but edges were NOT — a deliberate original choice
("atom detail is click-driven not per-keystroke, so no edges cache").
That assumption broke on the Wikipedia atlas:
`~/.sovereign/indexes/wikipedia/atlas/` → `atoms.json` 731 MB (cached),
**`edges.json` 1.3 GB (re-parsed per click)** → ~1-minute stall on EVERY
article click. The Explore *list* (`atlas_list_atoms`) only touches atoms,
which is why the list loaded but clicks hung.

Fix (2026-07-14): added `cached_edges` + `cached_cross_corpus_edges`
in `atom_detail.rs` — process-global `OnceLock<RwLock<HashMap<PathBuf,_>>>`
keyed by mtime+size, exactly mirroring `cached_atoms`. First click on a
corpus pays the parse once (logged at INFO on target `atlas_view` with
elapsed_ms), every later click is a read-lock + Arc clone. `get_atom_detail`
now logs per-click `elapsed_ms` at debug.

UPDATE 2026-07-14 — the edges parse was NOT the dominant cost. Driving
the app headlessly (command bridge on :9745, `SOVEREIGN_COMMAND_BRIDGE=1`)
showed a cold click = 119s, warm click still ~90s. Full breakdown of
`atlas_get_atom_detail` (desktop `atlas_commands.rs`):
  1. `get_atom_detail` (reader): ~20s cold (edges parse) / ~0.5s warm.
  2. **`resolve_sections_to_chunks` (`corpus-engine/src/index/read.rs`): a
     FULL SCAN of the entire 1.9M-row / 4.4 GB Wikipedia chunks.lance, per
     click, `indices_loaded=0`** — the real ~90s stall. It maps an evidence
     row's `section_id`→`chunk_id` for the reading-surface deep-link, but
     called `all_chunks_full()` which pulls the `content` column (the 4.4 GB).
     Worse: on Wikipedia it resolves NOTHING (atom `section_id` e.g. `2019641`
     doesn't match chunk-metadata section_id) → 90s of pure waste, chunk_id=None.

Fixes applied (all verified via bridge — warm clicks 90s→~0.5s):
  - Edges cache + atom-id index (above).
  - `resolve_sections_to_chunks` projects ONLY (id, metadata), early-exits →
    4.4 GB→2.8 GB. Added `section_chunk_index()` (full-map builder) sharing a
    `scan_section_chunk_map` helper.
  - Desktop `atlas_get_atom_detail`: section resolution is now NON-BLOCKING +
    cached — a process-global per-corpus `section_id→chunk_id` map
    (`SectionMapState::{Building,Ready}` in `atlas_commands.rs`). Click resolves
    from cache when ready; else chunk_id=None immediately + ONE-TIME background
    build. Load never waits on the scan.

Residual / follow-ups (not done): (1) first click per session still pays
the ~20s cold edges parse — background-warm the edges cache on Explore open to
kill it. (2) Caches pin ~2 GB RAM (731 MB atoms + 1.3 GB edges) per explored
corpus; Phase-2 = persist edges + a section→chunk index in an indexed store so
clicks are indexed lookups, never full parses/scans. (3) Separate data bug: the background
`section_chunk_index()` build logged `sections=0` — Wikipedia `chunks.lance`
metadata carries NO `section_id` field at all, so evidence deep-links can NEVER
resolve (chunk_id always None). The old code scanned 2.8 GB per click to build
that empty map; now it's built once and the empty map is cached (instant None
thereafter). Real fix = tag chunks with section_id at Wikipedia atlas-build
time. (4) `by_id` atom index is rebuilt (1.69M entries) every click (~0.3s, but
3s+ under memory pressure) — cache it like atoms if it matters. (5) build takes
minutes not ~90s under contention/debug build. Pre-existing `wikipedia`
corpus_id-collision WARN is unrelated, see [[invariant_corpus_id_chunk_id_unique]].

---
