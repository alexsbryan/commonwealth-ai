# Deterministic Authoring Harness — Spec

> Contract per `ARCH_PRINCIPLES.md §1.1`: every claim here is verifiable against the
> code on the commit it lands. This spec covers the **deterministic rung only**; the
> semantic rung (ontology quality, chunk coherence, vector relevance, answer quality)
> is a separate spec.

This is **`sovereign recipe test` grown into the deterministic rung of the authoring
harness** — not a new subsystem. It runs the real `corpus-engine` pipeline over a
*frozen sample* and emits an exact **Pass / Fail verdict per stage**, with the failing
items **shown, not summarized**. It answers exactly one question: *did the recipe do
what the recipe declares?* Whether what it declares is what the author *wanted* is
judgment, and judgment is out of scope here.

**Status:** the deterministic ladder + enrich rung are implemented (Increments 0–6).
`sovereign recipe test` runs the frozen-sample ladder **Acquire → Extract → Filter →
Chunk → Index** model-free, plus an opt-in `--enrich` rung. The mechanism lives in
`corpus-engine/src/harness/`, the verdict policy + renderer in
`sovereign-eval/src/authoring_harness/`. Remaining: the Desktop seam (Increment 7).

**Enrich-rung design note (SSOT):** the harness *verifies the atoms the real
ingest/enrichment pipeline produced* (`harness::verify_atoms_at`) rather than re-running
enrichment itself — atom production is the pipeline's single responsibility, and
re-running it inside the harness would duplicate that orchestration. `--enrich` therefore
requires the corpus to have been ingested+enriched (index + `atlas/atoms.json` co-located);
an atoms-only artifact (no index) is reported and the rung is skipped. The numeric-audit
sub-check is deferred: its SSOT (`sovereign-core::runtime::numeric_audit::uncited_numerics`)
lives in the runtime layer and targets synthesized prose, not atoms.

---

## 1. Scope

**In — exact, falsifiable verdicts:** acquire integrity, extract field-coverage (the
`SectionMissReport` generalized to all structured extractors), filter behaviour, chunk
degeneracy, index + keyword-retrieval round-trip, evidence-link integrity, numeric audit.

**Out — semantic layer, separate spec:** is the ontology the *right* ontology, are chunks
*coherent*, is *vector* retrieval *relevant*, is a synthesized answer *good*. None of these
receives a deterministic verdict. Emitting a green here when the honest answer is "I cannot
judge this deterministically" is the single failure mode this layer exists to prevent — a
subtly-wrong green is worse than no harness, because the author is trusting it precisely
because they can't read the TOML themselves.

**Explicit non-goals (do NOT build):** stratified sampling; any score, confidence, or
weighted aggregate; a parallel/mock pipeline; ML-based "did it work?" judgments; a
config-hiding GUI. The TOML stays the surface. The harness stays a thin observation layer
over the production stages.

---

## 2. Invariants (enforced by tests, not toggles)

- **I1 — Reproducible.** Same `(sample_id, recipe)` → byte-identical `HarnessRun`. A verdict
  that changes between runs is always attributable to the TOML, never to sample or timing
  variance. (`HarnessRun` carries no timestamps; evidence lists are sorted.)
- **I2 — No second pipeline.** The harness calls the same
  `Acquirer → Extractor → Filter → Chunker → Embedder → Index` *stages* a real ingest uses,
  bounded to the sample. The stages are literally shared: both `ingest`
  (`engine/ingest.rs:504,564`) and the harness runner construct them through the same
  `pub(crate)` factories `make_extractor` / `make_chunker`
  (`engine/ingest_factories.rs:131,397`), and both perform the doc→chunk transform through
  the single shared helper `engine::chunk_doc` (`engine/ingest_helpers.rs`). A focused
  parity test over a `LocalFile` fixture is the drift tripwire.
- **I3 — Acquire is frozen out of the loop.** Acquisition is the one inherently
  non-deterministic stage, so it runs **once**, at sample capture, through the real
  `acquire_source` (`engine/ingest_factories.rs:24`); the sample is content-addressed
  (sha256, `asset_store` layout) and **never re-fetched** during iteration. The network
  leaves the loop entirely.
- **I4 — Retrieval check is exact.** The deterministic round-trip rides the **Tantivy FTS
  (keyword)** path — exact token match, deterministic order
  (`index/search.rs`; FTS-only build via `build_indexes(false, true, None)`,
  `index/create.rs:715`). Approximate IVF-PQ vector relevance is not asserted here.
- **I5 — No aggregation into a number.** A `Verdict` is `Pass | Fail | Warn` plus shown
  evidence. The only roll-up is *all-pass → green / any-fail → red* (`HarnessRun::green()`);
  `Warn` never gates.
- **I6 — Cache without lying.** Each stage output is cacheable keyed on
  `(sample_id, stage_config_hash, upstream_output_hash)` — the `filter_signature` hashing
  pattern (`filters/mod.rs:201`). A cache hit is behaviour-identical to a cold run; cold ==
  warm is enforced by test. *(Deferred until the enrich rung makes it pay; the frozen sample
  already removes the network, the dominant cost.)*

---

## 3. Data model (small on purpose)

Lives in `sovereign-eval/src/authoring_harness/mod.rs` (the harness `Verdict` must NOT
collide with `mechanism_fidelity::stopping::Verdict`).

```rust
struct HarnessRun  { sample_id: ContentHash, recipe_hash: ContentHash, stages: Vec<StageResult> }
struct StageResult { stage: Stage, config_hash: ContentHash, cache_hit: bool, verdicts: Vec<Verdict> }
struct Verdict     { check: CheckId, status: Status /*Pass|Fail|Warn*/, expected: String, observed: String, evidence: Vec<EvidenceItem> }
struct EvidenceItem{ locus: Locus /*Doc|Chunk|Atom*/, excerpt: String }
```

No weight, no threshold baked into the type. A threshold (e.g. required field-coverage)
lives in a plain `Declaration` read off the recipe and is **printed in `expected`**, so the
bar is always on screen.

---

## 4. The verdict ladder

```
  capture (once, frozen)              iterate (deterministic, offline)
        │
   ┌────▼────┐  ┌─────────┐  ┌────────┐  ┌────────┐  ┌───────┐  ┌──────────┐
   │ Acquire │─▶│ Extract │─▶│ Filter │─▶│ Chunk  │─▶│ Index │─▶│ (Enrich) │
   └────┬────┘  └────┬────┘  └───┬────┘  └───┬────┘  └───┬───┘  └────┬─────┘
        │            │           │           │           │           │
   integrity    field-cov   kept/dropped  degeneracy  FTS needle  link-integrity
  (at capture) (FieldMiss)   + reasons    + sizes    + model-match + numeric audit
```

**Extract — field-coverage** *(build first; highest ROI)*. "Declared" = the `html_sections`
/ `xml_sections` rules (which stash `metadata.section_name`), the `tabular_atoms` columns
(`id_column` + `numeric_attributes` + `string_attributes`, stashed as the full typed row in
`metadata`), the `json`/`jsonl`/`csv` field set. Pass when every declared field is present
in ≥ its `min_coverage` (default: present in all sample docs). Evidence per field: `found
N/M`, the missed `doc_id`s, and the **nearby text** in each miss. Pure boolean presence
(a `metadata` lookup), **no fuzzy matching**. Generalizes today's `html_sections`
`SectionMissReport` (`extractors/html_sections.rs:308`) into `FieldMiss` across every
structured extractor. **There is no `column_aware` *extractor*** — `column_aware` is a
retrieval-time `recipe.index` field and is out of scope for extraction coverage.

**Filter — kept/dropped + reasons.** Pass when kept ≥ 1. A declared filter that drops 0 docs
is a **Warn** (shown, not a hard fail). Evidence: `kept N / dropped M`; a sample of each with
the firing filter's `DocumentFilter::description()` (`filters/mod.rs:52`); for `boilerplate`,
a before/after on one doc.

**Chunk — degeneracy + size.** Pass when count > 0; no empty chunks; sizes within the
chunker's declared `max_chars`; not collapsed to a single chunk. Evidence: size histogram;
largest and smallest chunk rendered. Degeneracy detection only — "is this chunk coherent?"
is semantic.

**Index — round-trip + model-match.** Pass when the index builds and opens; the declared
embed model matches what the index recorded; a deterministically-chosen rare token from a
known sample doc, **FTS-queried**, returns that doc. Evidence: the token, its source
`doc_id`, the hit list. Default mode is **model-free**: insert zero-vectors at a tiny fixed
dim and `build_indexes(false, true, None)` (FTS-only Tantivy). Under `--enrich` the Index
upgrades to real embeddings (enrichment needs vectors), and the model-match becomes a real
check (engine embed model vs `recipe.index.embedding_model`; cf. the warn at
`engine/mod.rs:1618` and `Error::IncompatibleEmbedding`, `error.rs:47`).

**Acquire — integrity** *(recorded at capture, then frozen — I3)*. Pass when: ≥1 doc; none
empty; for `http_api`, pagination advanced as configured; the hardcoded safety (robots.txt,
1s/domain, size-warning at 1.5× estimate; `safety.rs`) ran. Evidence: count; sample of
`{url, bytes, content-type}`; on fail the exact cause (401, 0 docs, oversize).

**Enrich — link-integrity + numeric audit** *(only when atoms exist; opt-in `--enrich`)*.
Pass when: every atom evidence id (`ChunkRef.chunk_id`, e.g. `sec_NNNNN`,
`enrichment/atlas/atoms.rs`) resolves to a real chunk via `ChapterEntry.chunk_ids`
(`enrichment/pipeline/chapter_manifest.rs:50`); every embedded quote is a **verbatim
substring** of its cited chunk (`verify_artifact_against_content`, `meshapp/src/wrapped.rs`);
every `$`/`%` figure value-matches a source-of-truth number
(`runtime::numeric_audit::uncited_numerics`, `sovereign-core/.../numeric_audit.rs:34`).
Evidence: each unresolved id; each non-substring quote, with the chunk it claims; each figure
with no provenancing source. Checks the **integrity of links and numbers**, never whether an
atom is *meaningful* — that carve is the entire boundary between this layer and the semantic
one. This is the only rung that pays for the model per doc.

---

## 5. Iteration

Edit TOML → re-run. Because acquire is frozen (I3), every iteration is offline; rungs 1–5
are model-free and run in well under a second over a 50-doc sample. A `--watch` mode re-runs
on save. A **run-to-run diff** falls out of the data model for free — a join over `Verdict`s
by `check` id — and is deferred until wanted.

---

## 6. Where it lives

- **Mechanism (facts)** — `corpus-engine/src/harness/`: the bounded staged runner + typed,
  judgment-free `StageOutput`s. Forced here because `make_extractor`/`make_chunker`/
  `acquire_source` are `pub(crate)` on `CorpusEngine`.
- **Policy + presentation** — `sovereign-eval/src/authoring_harness/`: the `Verdict` /
  `HarnessRun` types and the pure check functions `(&StageOutput, &Declaration) ->
  Vec<Verdict>`, beside `mechanism_fidelity/` and `chaos_monkey/`. (`sovereign-eval` already
  depends on `corpus-engine`.)
- **Orchestrator** — `sovereign recipe test` (`sovereign-cli-llm/src/recipe_cmd.rs`),
  **consolidated into one verb**: the first run auto-freezes the sample under
  `~/.sovereign/harness/<recipe>/` (content-addressed via `FilesystemAssetStore` +
  `capture.json` sidecar) and prints what it froze; later runs are offline. `--recapture`
  refreshes the freeze, `--enrich` adds the enrich rung, `--watch` re-runs on save, `--json`
  emits the `ResultRow` JSONL, `--pin <url|path>` force-adds a doc. There is no separate
  `recipe harness` namespace.
- **Output** — `ResultRow` JSONL (the shape the other benches emit) plus a rendered
  per-stage report. No new daemon, port, or trait.
- **Desktop** — one Tauri command (`recipe_author_run_harness`) returns the `HarnessRun`;
  the recipe-author dashboard's `last_test_status` placeholder
  (`sovereign-desktop/.../recipe_author_commands.rs:269`) is wired to real per-stage verdict
  cards.
- **CI** — these verdicts are the chaos-monkey-style **hard, baseline-diffed** lanes in
  `scripts/sovereign-ci-bench.sh`: build-breaking for first-party recipes, advisory (tracked)
  for user-authored ones.

---

## 7. Decisions ratified

1. **Sample = first N docs in acquire order, frozen.** Default N: 50 for the
   extract/filter/chunk/index rungs; a smaller N (10–20) for the enrich rung, which pays the
   model per doc. Ship a one-line **representativeness report** ("covers 4 of 7 observed
   document shapes; field `Y` has 0 instances *in the sample*") plus a manual `--pin <doc>`.
   **No stratified sampling** — transparency over cleverness.
2. **Coverage default = present-in-all**, with a per-field `min_coverage` override; the
   threshold is always printed in `expected`.
3. **Numeric/link audits run whenever atoms exist**, gated only by the author choosing
   `--enrich`. They are cheap deterministic post-passes, not part of the model cost.
4. **CLI consolidated into `recipe test`** (no `recipe harness` namespace) and **the Enrich
   rung ships in the first demo** — both per the authoring-session decisions captured in the
   plan file. The demo anchors on `sep` (all six rungs incl. enrich) with `us-code`
   (`xml_sections`) spotlighting the field-coverage rung's shown-miss evidence.
