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
- [ ] **2. Production direct-read reader (THE hot-path stage).** Daemon's
  `AtlasGraph` reads `atoms.lance` **directly** instead of reconstruct-to-rkyv —
  backend enum `Rkyv | Lance`, `AtomView`/`EvidenceRef` become 2-variant enums
  (pub method API preserved → callers unchanged), edges via the `CsrEdges` mmap.
  **Decision: preload-sync** (open is async; query API stays sync → no `atlas_navigate`
  async ripple). Cheap for every corpus except wikipedia; wiki's payload-paging /
  async-streaming RSS variant is deferred to its flip (step 3, last). Per-corpus
  `read_v2` gate (off → rkyv). **Verify:** reader-parity test + chaos QA + SEP eval
  in the daemon path + a non-philosophy embedding spot-check (enron).
- [ ] **3. Flip.** Per-corpus `read_v2`: SEP → other sources → **wikipedia last**
  (with the preload-vs-async RSS / cold-window re-measure at the wiki flip).
- [ ] **4. Delete v1.** `resolve_atom_id_from_entry`, `AtlasContext`,
  `atoms.embeddings.bin`, the cosine-bag seeding. After all flipped + chaos green.
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
