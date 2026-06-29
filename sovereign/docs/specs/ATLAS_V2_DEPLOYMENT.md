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

**STATUS: COMPLETE (2026-06-29).** Every step below is done. The v2 store is the sole
atom backend — v1 (`atoms.rkyv` + `atoms.embeddings.bin` + `resolve_atom_id_from_entry`)
was deleted in step 4 (commit `edeca426`), every flippable corpus reads v2, and the
v2-only wikipedia + SEP snapshots are published to HF. The migration is no longer
reversible by design — that was step 4's explicit tradeoff, taken after v2 baked in use.

## Steps (each gated, reversible)

- [x] **0. Increments A–D landed + committed.** Store writer (B) wired at the 3
  lifecycle points, gated `SOVEREIGN_ATLAS_STORE_V2` (off). Eval `--atlas-backend
  lance` + `--atlas-seed ann` prove neutrality. `atlas verify-v2` audit/backfill tool.
- [x] **1. Backfill — DONE.** `atoms.lance` + `edges.csr` written for every atlas-bearing
  corpus (1808 flippable live atlases; the 3 dead atlases skipped). Reusable as one
  command, `sovereign atlas migrate-all` (below). Wikipedia took the columnar-structural
  path (step 3a), not an atom-store reconstruct.
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
- [x] **3. Flip — DONE.** All 1808 flippable live atom corpora carry `atlas/.read_v2`
  (via `atlas migrate-all`); wikipedia deliberately not flipped (it serves structural
  neighbors from the columnar graph — see 3a). The per-corpus `read_v2` marker was then
  retired entirely in step 4 — v2 is the only backend now, so there is no gate.
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
- [x] **3b. Production ANN seeding (THE biggest remaining push) — DONE + VERIFIED.**
  The ANN-over-vector seeding was proven in the *eval*; this moves it into production.
  **Design choice: a persistent SIBLING table, not a column in `atoms.lance`.** Each
  corpus gets `atlas/atoms_ann.lance` (a flat Lance `key=atom_id → embedding` vector
  table, `corpus_engine::…::ann_store::AnnSeedTable`), built ONCE at backfill by the
  resolve-join (`resolve_atom_id_from_entry` runs at build, never per query). This kills
  `resolve` on the hot path **without** an `atoms.lance` schema change or IVF-PQ — flat
  search equals exact cosine at SEP/other scale, which is what the A-gate proved.
  - **Reader/seed (`sovereign-core/atlas_context.rs`):** `AtlasGraph` carries
    `Option<Arc<AnnSeedTable>>`; new async `atlas_navigate_ann` = v1 `atlas_navigate` with a
    **per-graph adaptive** seed step: a backfilled graph seeds from its ANN table
    (`nearest_with_vectors` → atom-ids directly, re-scored with the canonical `cosine` so the
    BFS sees v1-identical weights), an un-backfilled graph seeds from exact cosine over its
    bag (exactly v1) — both feed one global top-`max_seeds` pool, so a MIXED pool never
    under-seeds. Name-match + BFS + emit are the same logic. BFS stays sync (only the ANN
    seed queries await → invariant 4 holds). All-backfilled → pure ANN (SEP-verified);
    all-cosine → equals v1.
  - **Attach (single shared path):** `open_and_attach_ann_seed_table` opens the table on
    the CALLER's long-lived runtime (daemon `AtlasContextManager` eager load + eval runner)
    — never the sync `load_from_disk` bridge, whose throwaway runtime would invalidate the
    held `lancedb::Table`. The lazy `graph()` path (atom-enumeration siblings, not the seed
    pool) intentionally skips it.
  - **Gate (`retrieval.rs apply_atlas_grounding`):** take `atlas_navigate_ann` as soon as
    ANY pool graph `has_ann_seed_table()` (the adaptive navigate handles the mix); when none
    do, plain sync `atlas_navigate` is the equivalent lighter path. **Why any, not all:** the
    daemon's pool is `provider.loaded_corpus_ids()` (every LOADED atlas, accumulating), so an
    all-or-nothing gate would be poisoned by a single un-backfilled atlas and never engage —
    the per-graph gate is what lets the rollout proceed corpus-by-corpus.
  - **Backfill (`atlas backfill-ann <corpus>…`):** `build_persistent_ann_seed_table`, a
    cheap transform of existing data (no LLM/re-embed). Default atlas filter = a no-flag
    `eval run` so the ANN table covers the cosine bag's atom universe.
  - **Verify (DONE, 2026-06-28):** backfilled 63 SEP atlases (1 — `sep-substance` —
    is empty under the default filter, so neutral in both arms), then `eval run
    --with-atlas <63> --atlas-seed cosine|ann` (+ a cosine control), all default flags.
    Result: **total source recall ann 56 == cosine 56** (control 55); per-question recall
    churn ann-vs-cosine 2/21 vs the cosine-vs-cosine **control 1/21** — and the shared
    `dialectical_putnam` diff occurs with *identical* seeding, i.e. pure `dedup_by_source`
    tie-break, not seeding (the other ann diff is ann *gaining* a source); `retrieved`
    churn 14/21 ≈ control 11/21 (the tie-break noise floor). This reproduces the A-gate
    (56/66, churn 14≈12) **through the production `atlas_navigate_ann`** — the eval drives
    the exact code the daemon runs, with 63/63 graphs seeding from their persistent ANN
    tables. Neutral → GO. After switching to the per-graph adaptive design, RE-VERIFIED:
    full re-run (63/63) recall diff 1/21 == the cosine-vs-cosine control (same `dialectical_
    putnam` tie-break question), churn 12≈11; and a MIXED pool (10 ANN tables hidden → 53
    ANN'd + 10 cosine) recall diff 1/21 == control (sources stable; churn 15 = chunk-level
    tie-break).
  - **Live pipeline deploy + verify (DONE, 2026-06-28).** Two findings the deploy surfaced:
    (1) **Atlas grounding runs in-process** in the consumer's Runtime (the desktop's
    `state.rs`, and the eval/CLI's `build_session`), NOT in `sovereign-cli-daemon` (which is
    inference-only — no atlas provider). The daemon serves synthesis; the client grounds.
    (2) **The backfill must use the PRODUCTION grounding filter.** The manager grounds with
    `AtlasContextFilter::default()` (`depth=["extracted"]`); the backfill originally used the
    eval's all-depths default, so the embed-cache key mismatched and `init_from_cache` loaded
    0 contexts (empty grounding pool). Fixed: `atlas backfill-ann` now derives its filter from
    `AtlasContextFilter::default()` (single source of truth, env-aware). After re-backfilling
    the 63 SEP atlases at the production filter, `eval run --synth --isolate` (grounding
    in-process via the new code, synthesis via the daemon) shows the **full pipeline engaging
    ANN**: `init complete contexts=63 graphs=63`, gate log `atlas-grounding: ANN seed path
    (v2) corpora=63 max_seeds=12`, `post-apply_atlas_grounding n_chunks=35 {"sep":35}`. The
    broader daemon-served chaos QA is step 3.
- [x] **4. Delete v1 — DONE (2026-06-29, commit `edeca426`).** Done as the scoped refactor
  the 2026-06-28 audit called for, not a file delete. **Phase A (rkyv storage backend):**
  `archive.rs` split into `projection.rs` (the shared `AtomRecord`/`project` types, rkyv
  derives stripped); `AtlasGraph` collapsed to a single `Arc<LancePreload>` (backend enum
  gone; `AtomView`/`EvidenceRef` are now single structs); `load_from_disk` is v2-only and
  **Err on absent** — the `read_v2` gate and the rkyv fallback are removed (no fallback by
  design); writers are fail-hard; the `rkyv` dep is dropped from both crates. **Phase B
  (seeding re-arch):** `AtlasEntry` carries `atom_id` first-class; the embedding bag is
  derived from `atoms_ann.lance` + resident atoms (no load-time re-embed); name-match reads
  resident atoms directly; `resolve_atom_id_from_entry`, the sync cosine `atlas_navigate`,
  and `atoms.embeddings.bin` are all deleted (grounding routes through `atlas_navigate_ann`).
  Wiki's typed-enumeration resolves cleanly: with rkyv gone `graph("wikipedia")` returns
  `None` and the typed-enum call sites `filter_map`/`continue` — a net fix for the ~38s
  on-query convert-on-load. **Verified:** lint green, full suite **7152/0**, SEP eval
  **60/66 (+4 vs v1** — Phase B fixed a latent resolve bug), wiki CI retrieval gate
  **0 regressed**. The 3 dead-atlas dirs remain for manual removal (out-of-repo path).
- [x] **5. HF distribution — DONE.** `sovereign corpus snapshot publish <corpus> --upload
  svrnmesh/<repo>` ships the v2 artifacts (`atoms.lance` + `edges.csr` + `atoms_ann.lance`;
  wiki `articles.lance` + `edges.lance`). SEP pushed to `svrnmesh/sep-index` (additive);
  **wikipedia pushed v2-only to `svrnmesh/wikipedia-index`** (2026-06-29, 9.43 GB, commit
  `0c3ee0f5`, sha256 `9a47a9b7…`) — the v2-only bundle was produced by moving wiki's 4 v1
  atlas files aside, building, then restoring (tar-verified no v1 leak). **Use
  `--zstd-level 3` for vector corpora** — the level-19 default is catastrophic on the
  ~incompressible embedding columns (~2 MB/min, ~100h ETA). HF_TOKEN from
  `~/.cache/huggingface/token` (private repos). Follow-up: repoint the recipes at the new
  snapshot sha256s so fresh installs pull v2-only.

## Reusable migration — any machine (the one-command port)

The whole atom + wiki port is one idempotent command, so a fresh dev machine migrates with
no hand-holding:

```
sovereign atlas migrate-all            # every atom corpus -> atoms.lance + edges.csr
                                       #   (+ atoms_ann.lance where embedded) + .read_v2 flip;
                                       #   wiki-class -> articles.lance + edges.lance (columnar)
sovereign atlas migrate-all --no-flip  # build v2 artifacts but DON'T flip (for a v1-vs-v2 bench)
sovereign atlas migrate-all <corpus>   # one corpus
```

Idempotent (skips current stores/tables), reversible (`rm <corpus>/atlas/.read_v2`), and the
ANN step uses the production grounding filter (`AtlasContextFilter::default()`) so the table
matches what the daemon seeds from. It only ANN-indexes corpora that already carry an
embeddings cache (never bulk-embeds the resident set).

**Verify the port (no-regression gate), all headless:**

```
# SEP retrieval neutrality (v2 reader+seeding == v1), markers off for a clean rkyv baseline:
sovereign atlas migrate-all --no-flip
find ~/.sovereign/indexes -maxdepth 3 -name .read_v2 -delete
sovereign eval run --bank sovereign/bench/sep/questions.toml --with-atlas <sep-list> \
  --atlas-backend rkyv  --atlas-seed cosine --atlas-depth extracted --output /tmp/v1.json
sovereign eval run --bank sovereign/bench/sep/questions.toml --with-atlas <sep-list> \
  --atlas-backend lance --atlas-seed ann    --atlas-depth extracted --output /tmp/v2.json
# expect: source recall identical (modulo the dedup_by_source tie-break), retrieved churn
#         at the cosine-vs-cosine noise floor.
sovereign atlas wikipedia neighbors wikipedia <hub-title>   # expect "PARITY: MATCH" vs SQLite
sovereign bench chaos-monkey run --bank sovereign/bench/chaos_monkey/secret_agent.toml \
  --transport direct --corpus chaos-secret-agent --out /tmp/chaos.jsonl
sovereign bench gate chaos-monkey /tmp/chaos.jsonl          # baseline-relative QA gate
# then flip:
sovereign atlas migrate-all            # (re-run; flips read_v2, skips built artifacts)
```

**Fresh machine (no prior atlases):** `sovereign corpus install <id>` restores the HF snapshot,
which already ships the v2 artifacts (no convert) — migrate-all is for in-place upgrades.

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
