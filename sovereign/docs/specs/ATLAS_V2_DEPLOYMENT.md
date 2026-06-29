# ATLAS_STORAGE_V2 — deployment tracker

Migration of the per-corpus atlas from the v1 rkyv archive (`atoms.rkyv` +
`atoms.embeddings.bin` + `resolve_atom_id_from_entry`) to the v2 store
(`atoms.lance` + `edges.csr` + ANN-over-vector-column). Design: `ATLAS_STORAGE_V2.md`.

**De-risk is complete.** The retrieval-neutrality of the v2 store + ANN seeding —
the load-bearing risk before touching the tuned inference daemon — is proven:

- **A** (ANN-seeding gate): `atlas_navigation` byte-identical 0/21 on SEP.
- **B** (store writer): `atoms.lance` row == rkyv atom (parity test); `edges.csr` byte-exact.
- **C+D** (store + seeding end-to-end): rkyv+cosine vs lance+ann over 57 SEP atlases →
  `atlas_navigation` 0/21, retrieved churn 11 < the v1-vs-v1 noise floor 13.
- **Migration audit** (`atlas verify-v2`): 37/40 non-sep/non-wiki "other sources"
  reconstruct losslessly across every pipeline (code self-atlas, literary,
  custom-ontology, email, doc); 3 are dead atlases (0-atom/missing); ~10 carry only
  provenance edges (atom→section/chunk) that v1 BFS can't traverse either = neutral.

What remains is **deployment** — realizing the proven-correct design in the daemon.

## Steps (each gated, reversible)

- [x] **0. Increments A–D landed + committed.** Store writer (B) wired at the 3
  lifecycle points, gated `SOVEREIGN_ATLAS_STORE_V2` (off). Eval `--atlas-backend
  lance` + `--atlas-seed ann` prove neutrality. `atlas verify-v2` audit/backfill tool.
- [ ] **1. Backfill.** `atlas verify-v2 --all --generate` writes `atoms.lance` +
  `edges.csr` for all 1,812 atlas-bearing corpora. Skip the 3 dead atlases;
  wikipedia is the one slow run (1.67M-atom reconstruct — run isolated, watch RSS).
- [x] **2. Production direct-read reader (THE hot-path stage) — DONE + verified.** Daemon's
  `AtlasGraph` reads `atoms.lance` **directly** instead of reconstruct-to-rkyv —
  backend enum `Rkyv | Lance`, `AtomView`/`EvidenceRef` become 2-variant enums
  (pub method API preserved → callers unchanged), edges via the `CsrEdges` mmap.
  **Decision: preload-sync** (open is async; query API stays sync → no `atlas_navigate`
  async ripple). This reader reads every atom resident + parses each payload once at
  open. Correct + cheap at SEP/other scale (hundreds–low-thousands of atoms);
  **wrong for wikipedia** — see step 3a. Per-corpus `read_v2` gate (off → rkyv).
  - [x] **2a** `pub AtomRow` + `atom_envelope` reader type (corpus-engine `store.rs`).
  - [x] **2b** `LancePreload` (resident atoms via canonical `project` + `edges.csr` mmap,
    sync query API, async `open` + `open_blocking` bridge) + reader-parity test —
    *green* (resident record == rkyv projection incl. aliases/evidence from payload).
  - [x] **2c** `AtlasGraph` backend enum + `AtomView`/`EvidenceRef` 2-variant enums +
    `from_lance_preload`/`load_lance_from_disk`. Cross-backend parity test proves the
    Lance backend is byte-identical to rkyv through the whole public API — *green*;
    the existing rkyv `archive_io_tests` still pass (no v1 regression).
  - [x] **2d** `load_from_disk` gate: per-corpus `atlas/.read_v2` marker (production
    flip) **or** `SOVEREIGN_ATLAS_READ_V2` allowlist/`all` (eval/staging); rkyv is the
    default **and** the fallback if the store is absent/unreadable (a flip can't strand
    an atlas). `backend_kind()` for glassbox logging + the gate test — *green*.
  - [x] **2e** `eval --atlas-backend lance` drives the **production** direct reader.
    SEP neutrality re-run (rkyv+cosine vs direct-lance+ann, 57 atlases / 21 q):
    `atlas_navigation` **byte-identical 0/21**, **0 direct-load errors** on all 57 real
    SEP atlases. The `retrieved` churn (17/21) is the pre-existing `dedup_by_source`
    tie-break — proven by the same-config **control** (rkyv+cosine vs itself, 2nd
    process) churning 13/21 with `atlas_navigation` also 0/21: the churn happens with
    *zero* changes. Reader proven retrieval-neutral (deterministically by the 2c
    byte-parity test; on real data by this eval). The daemon-served chaos QA + enron
    embedding spot-check move to step 3 (where the daemon actually uses the gate).
- [ ] **3. Flip.** Per-corpus `read_v2` marker: SEP → other **embedding-bearing**
  sources. Wikipedia is deliberately **not** flipped (see 3a). **Verify per flip
  (daemon-served, the gate is live here — distinct from 2e's in-eval reader):** chaos
  QA on the flipped corpus + a non-philosophy embedding spot-check (enron) + confirm
  `backend_kind=lance` in the daemon log. Roll back by removing the marker.
- [x] **3a. Wikipedia → columnar-structural v2 — W1–W4 DONE + verified on real wiki.**
  See `WIKIPEDIA_ATLAS_V2.md`. Wiki's *structural* enrichment (link graph, section_path,
  pov/citation, QID) baked into Lance: `articles.lance` (catalog, 3.1 MB / 51,845
  in-scope) + predicate-queryable `edges.lance` (7.85M links, BTree-indexed on
  source_title). `ColumnarWikipediaGraph` serves the `WikipediaGraph` neighbor API; the
  runtime gate (`open_wikipedia_graph`) picks columnar-if-present across all 3 loaders.
  **Real-wiki verify:** full-set neighbor parity vs SQLite (MATCH; limited-set diffs are
  equal-occurrence tie-break noise) + **~17× faster** (720 ms vs 13.5 s) + ~6× smaller
  (845 MB vs 5.25 GB). Remaining wiki tail: retire `atoms.json`/`edges.json`/`atoms.rkyv`/
  SQLite (folds into step 4) + daemon chaos QA (step 3). W5 (article embeddings → ANN)
  is a future upgrade, not required for done.
- [ ] **3b. Production ANN seeding (THE biggest remaining push).** The
  ANN-over-vector-column seeding is proven in the *eval* (`eval_cmd/atlas_ann.rs`) but
  production `atlas_navigate` still uses the v1 cosine-bag + `resolve_atom_id_from_entry`.
  Move it into production: co-locate the atom embeddings as a vector column in
  `atoms.lance` (+ IVF-PQ index), and replace the cosine-bag seed step with ANN that
  returns atom-ids directly. This is the SEP track's actual retrieval-quality + scaling
  win, and the prerequisite for deleting `resolve`. Gated per-corpus (the flipped ones).
  **Verify:** the SEP+essay eval in the daemon path (== the A/C+D harness) + chaos QA.
- [ ] **4. Delete v1** (`resolve_atom_id_from_entry`, `AtlasContext`,
  `atoms.embeddings.bin`, the cosine-bag) after 3b lands + all embedding-bearing corpora
  flipped + chaos green; and retire wiki's `atoms.json`/`edges.json`/`atoms.rkyv`/SQLite
  (3a). The rkyv `AtlasGraph` backend retires once **every** atom-corpus is on Lance —
  wiki no longer needs it (it's on `ColumnarWikipediaGraph`, a separate reader).
- [ ] **5. HF distribution.** Bundle ships `atoms.lance` + `edges.csr`;
  `SnapshotManifest.store_format_version`; install = drop + register (no convert).

## Cleanup / notes from the audit

- **Dead atlases** (delete the dirs): `arch-principles-atlas`, `system-overview-atlas`,
  `wikipedia-newsworthy` — 0-atom / missing `atoms.json`.
- **Provenance-only-edge corpora** (~10: `commonwealth-ai-system-overview`, `enron-*`,
  `conversations-personal`, …) — pre-existing; their atlas BFS is already seed-only.
  v2 neutral. Worth a separate look at why their edges target sections/chunks not atoms.
- **verify-v2 edge check** is count-level (`l_edges <= r_edges`); a stronger gate would
  assert `l_edges == rkyv both-atom-edge count`. The sample investigation + SEP 0/21
  cover correctness for now.

## Inference-safety invariants (hold across every step)

1. Index/store builds never co-resident with hot inference (lifecycle-time only).
2. No new engine — Lance + its tokio/arrow pool already run in the daemon.
3. RSS stays paged where it matters (wikipedia); preload-sync is fine at SEP/other scale.
4. Hot BFS stays sync — `edges.csr` mmap + preloaded atoms keep `atlas_navigate` sync.
