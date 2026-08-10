# Enrichment Atlas v2.3 — Plan of Record

_Living document. Last substantive update: 2026-05-27 after the
architecture-over-Enron substrate push (added `Asset` atom variant,
`EdgeType::Attaches`, `Entity.provenance` field; SCHEMA_VERSION
2.0 → 2.1 → 2.2). Since then `AtomsFile::SCHEMA_VERSION` has moved to
`2.3` (added `Entity.attributes` for the deterministic `tabular_atoms`
extractor) — see `atoms.rs:1066-1086` for the full version history._

This doc tracks the stage-by-stage rollout of the v2 enrichment pipeline.
It is authoritative for the **status of each landing** and the
**forward-looking plan**. When a landing ships, update the status
table and move its description from "Next" to "Shipped".

The live planning scratchpad lives at
`~/.claude/plans/curious-brewing-pelican.md` — content should stay in
sync with this file; this file is the durable one.

---

## Where the atlas lives in the stack

The enrichment pipeline is a v2 layer on top of `corpus-engine`'s
document store. It produces a typed-graph atlas — eleven atom types
(Entity, State, Relation, Event, Claim, Question, Configuration,
ArgumentReconstruction, Position, Opposition, **Asset**; see
`AtomEnvelope` at `atoms.rs:987`) plus fourteen edge types: seven
intra-corpus (Involves, Transition, Causes, Grounds, Tension,
Composes, Configures), three cross-corpus (Grounding, Framing,
Provenance), and four Gap-B/AD-2 typed-extension edges (EvidenceFor,
Concedes, OpposesIn, **Attaches**; see `EdgeType` at `edges.rs:45`) —
that supports queries beyond claim-and-question retrieval:
trajectories, relational dynamics, event sequences, configurational
readings, **binary-attachment traversal** (Asset atoms point at the
content-addressed store under `<corpus>/assets/`).

The Phase 4 multi-origin reconciliation primitive
(`corpus-engine/src/enrichment/reconciliation/`) operates on
`Vec<Entity>` whose atoms carry the `Provenance` AD-4 field and
writes a reversible op log to `atlas/reconciliation_oplog.jsonl`.

**Open/Closed architecture.** The atlas schema, on-disk format,
traversal engine, brief assembler, and schema-validation protocol are
stable (the closed surface). Ingestion strategies sit behind a single
`AtlasIngestion` trait (the open surface). Today one strategy is
wired end-to-end: extraction-first (LLM reads authored works). A
future structure-first strategy (Wikipedia-style deterministic
parsing) is a drop-in addition; the tag `enrichment_depth` on every
atom (`Structural`, `Extracted`, `StructuralClassified`) ensures the
brief assembler can calibrate language by provenance without
re-architecting.

**Pivot to seed-threaded map-reduce.** Early per-section extraction
fragmented entity names across chapters (`Fyodor Karamазов`, `Fyodor
Pvlvitch Karazoff`, and so on). Root cause: each chapter's Phase 1
call had no cross-chapter memory. Fix: Stage 1a extracts a canonical
entity list from the first section, caches it to `cache/seed.json`,
and threads it into every subsequent Phase 1 prompt. On *Brothers
Karamazov*, this drove `entities_introduced` mixed-script pollution
from 21 % to 0 %.

---

## Status

### ✅ Shipped

| Landing | Work | Evidence |
|---|---|---|
| Step 1 | Slim Phase 1 per-section extraction (six-facet sketches, anchor keyphrases, lenient Raw deserialisation) | `src/enrichment/pipeline/pipelines/literary_atlas.rs`, `literary_atlas_prompts/phase1_system.md` |
| Step 1 back-fill | `EnrichmentDepth` tag on `SectionExtraction`; parser pins `Extracted` | `atlas.rs:64-68,539`, `literary_atlas.rs::parse_phase1` |
| Step 1 back-fill | `AtlasIngestion` trait + `AtlasData` bundle | `atlas/ingestion.rs:49-97`, `atlas/registry.rs` |
| Step 1.5 | Terse-retry for think-truncated chapters: `PhaseFailureKind`, `compose_phase1_terse`, `--terse` CLI flag | `types.rs:336-340`, `runner.rs:395,508`, `literary_atlas.rs:158`, `sovereign-cli extract.rs:28-39,425` |
| **Landing 1** — Stage 1a seed | `SeedEntity/Entities/Origin/Strategy` types, four Pipeline trait methods, `phase_1a_extract_seed`, `cmd_seed`, `phase1a_seed_system.md` | `atlas.rs:440-476`, `trait_def.rs:76-124`, `runner.rs:297-346`, `seed_cmd.rs:53`, `enrich_cmd/mod.rs:89` |
| Phase A Step 2 | Facet-typed clustering + naming | `atlas_clustering.rs` (964 LOC), `runner.rs:841`, `atlas_phase_cmd.rs:38,113` |
| Phase A Step 3a | Entity + event atom resolution (alias merge, Levenshtein + cosine, bounded window, event dedup) | `atlas/{atoms,edges,resolution}.rs`, `atlas_resolve.rs:22,77` |
| Phase A Step 3b — deterministic variant | State/relation/claim/question resolution via `resolve_step_3b`: trajectories, Transition/Grounds/Involves edges, Rust-side participant-string → entity-atom fuzzy snap | `resolution.rs:645,1041-1077`, `atlas_resolve.rs:188` |
| **Landing 2** — Resolver hardening + parse-drift auto-recovery | (1) `fold()` now strips Latin combining diacritics via NFD + mark filter, so `Karámazov` ↔ `Karamazov` collapse in the name index. (2) `resolve_entity_id_fuzzy` gained a per-token Levenshtein vote fallback (len ≥ 5, Lev ≤ 2, single-match-wins). (3) `shared_token_overlap` now also counts Lev ≤ 1 fuzzy matches for long tokens so Phase 3a merges diacritic-drifted patronymics. (4) New `first_token_matches` guard on Rule 3 prevents siblings (Alexei vs Dmitri, shared patronymic + surname) from collapsing. (5) Seed-aware +8k output budget bump eliminates `parse_drift` on first pass. 11 new tests including `relation_key` invariant lock and explicit sibling-distinction guard. | `resolution.rs` fold/fuzzy/first_token_matches, `runner.rs:99-109,527-554`, `Cargo.toml` +`unicode-normalization`, test suite |
| **Landing 3 — Deterministic tensions + gaps** | New `atlas/analysis/` module with `tensions.rs` + `gaps.rs`. Tensions ships a candidate selector (intra-cluster + claim/claim entity-overlap + claim/state entity-overlap + dedup) producing `atlas/tension_candidates.json`. Gaps ships three deterministic detectors (transition without trigger event, ungrounded claim, open question) producing `atlas/gaps.json` with `kind`, `description`, `referenced_atoms`, `evidence`, `significance`. Two new CLI subcommands — `atlas-tensions`, `atlas-gaps`. Atlas writer extended with `write_atlas_gaps`, `write_tension_candidates`, `read_atlas_atoms`, `read_atlas_edges` helpers. 12 new analysis tests + 6 CLI parse_args tests. The LLM classifier that promotes candidates into real `Tension` edges is deferred. | `atlas/analysis/{mod,gaps,tensions}.rs`, `atlas/writer.rs`, `sovereign-cli/src/enrich_cmd/{atlas_tensions,atlas_gaps}.rs`, test suite |
| **Landing 4 — Cross-section connectivity via salience-aware resolution** | Root cause of Landing 3's `attributed_to = null` surfaced: "Fyodor" matches multiple Karamazov drift variants in the token index, so the strict fuzzy resolver correctly bails on ambiguity. Added `resolve_entity_id_with_salience` — falls back to a salience tiebreaker when strict resolution finds multiple candidates sharing the query's tokens, with a first-token-match guard so siblings don't collapse. Winner must dominate by ≥ `SALIENCE_DOMINANCE_FACTOR` (2.0×). Wired into claim attribution (`resolve_step_3b`, line 877) and relation/event participant resolution (`resolve_entity_ids`). 3 new tests. Measured impact on 5-chapter smoke: relation-participant coverage 57% → **93%** (target ≥ 90% exceeded), claim attribution 0% → **100%**, relation atoms 8 → 13, trajectories 9 → 12, edges 93 → 111. Cross-chapter relationship edges now land cleanly: Alyosha↔Zossima, Alyosha↔Fyodor, Pyotr↔Fyodor, Grigory↔Dmitri — the "relationship connections between chapters" the pivot targeted. | `resolution.rs:1131-1247` (salience function), call-site edits at 748/796/875, test suite |
| **Phase A Step 5 — Configurations (Phase 8, opt-in, LLM)** | New `atlas/analysis/configuration.rs` — `AtlasSummary` + summariser + `Phase8ParseItem` deserialiser + `parse_configurations` with atom-id normalisation (`claim-7 → claim-0007`) and prose-scan fallback so atoms referenced inline in description/interpretive_note get lifted into `constituent_atoms`. New `Pipeline` trait methods `runs_configuration_phase()` (default `false`), `compose_phase8_configuration`, `parse_phase8_configuration` — non-atlas pipelines are untouched. `LiteraryAtlasPipeline` opts in + ships a `phase8_configuration.md` prompt asset with the explicit **Ricoeur constraint** (every configuration must articulate at least one plausible alternative reading in `interpretive_note`). New CLI subcommand `sovereign enrich atlas-configuration <corpus>`. New writer `write_atlas_configurations`; atoms.json rewritten via read-modify-write to merge Configurations into the full atom set. 11 new analysis tests + 3 new CLI parse_args tests. Smoke on 5-chapter `brothers_karamazov`: 3 configurations produced with full interpretive_notes — "Three sons as spiritual archetypes / reason / passion", "Father's awakening through the novice", "The servant as moral witness". All seven spec atom types now land on disk (100 atoms across 7 types). | `atlas/analysis/configuration.rs`, `pipeline/trait_def.rs:~270-300`, `pipelines/literary_atlas.rs:541-648`, `literary_atlas_prompts/phase8_configuration.md`, `atlas/writer.rs::write_atlas_configurations`, `sovereign-cli/src/enrich_cmd/atlas_configuration.rs` |
| **Phase A Step 6 — Traversal engine + brief assembler (MVP)** | New top-level module `corpus-engine/src/atlas_traversal/` with three files: `classifier.rs` (keyword + known-entity-name pattern matcher, longest-match + word-boundary guard against `Fyodor`↔`Fyodorovich` false positives), `engine.rs` (deterministic traversal for 6 `QueryPlan` variants — EntityLookup, Trajectory, RelationLookup, TensionList, ConfigurationList, CorpusOverview), `brief.rs` (`assemble_brief` renders results as prose with `enrichment_depth`-calibrated framing — "The atlas records that…" for Extracted — and surfaces confidence hedges only below 0.7). New CLI `sovereign enrich atlas-query <corpus> "<query>" [--json]` — classifier → traversal → brief, zero LLM calls. 21 new atlas_traversal unit tests + 5 new CLI parse_args tests. Smoke on 5 representative queries against `brothers_karamazov` atlas: all five return substantive briefs — entity lookup prints Alyosha's 3 relations + 4-state trajectory; trajectory query orders states `sec_0001 → sec_0006` with transitions flagged as "no explicit trigger" (ties to the gap detector); relation query returns `relation-0008` "Devotion of a young novice"; configurations query prints all three Configuration atoms with interpretive_notes; tension query enumerates 11 open questions the corpus raises. The atlas can now *answer*, not just be built. | `atlas_traversal/{mod,classifier,engine,brief}.rs` (corpus-engine), `sovereign-cli/src/enrich_cmd/atlas_query.rs` |
| **Landing 5 — Phase 3a Rule 3.5 + classifier fold/token fallback** | The deferred drift-variant merge. New Phase 3a Rule 3.5 in `find_merge_target`: fires on `first_token_matches` + ≥ 1 shared long token (len ≥ 5, exact) with two paths — **strict** (both sides have descriptions → cosine ≥ `ENTITY_MERGE_SINGLE_TOKEN_COSINE = 0.92`) and **sparse** (exactly one side empty → merge without cosine, since a ghost reference with the same first-name + shared long token is almost always a drift). New helper `shared_long_token_count`. The `fold()` helper is now `pub` so the classifier can reuse it — the classifier now folds both query and entity names before matching, AND adds a **token-fallback pass**: when no canonical/alias fits inside the query (long names vs short query like "Who is Fyodor?"), each long query token is looked up against the entity-token inverted index; unique-owner wins, ambiguous tokens bail. 6 new resolution tests (incl. sparse-path + first-token guard + cosine-required variants) + 2 new classifier tests. Smoke on 5-chapter `brothers_karamazov`: **entity-0007 "Fyódor Kárazóv" merged into entity-0002 "Fyódor Pavlóvič Karámazòv" as an alias** — 13 → 12 entities, 13 → 11 relations (duplicate Fyodor-Adelaida relation collapsed via relation_key dedup). `sovereign enrich atlas-query bk "Who is Fyodor?"` now returns the merged entity with 6 relations, 1 attributed claim, 3-state trajectory. | `resolution.rs` (Rule 3.5 cascade + `shared_long_token_count`), `atlas/mod.rs` (re-export `fold`), `atlas_traversal/classifier.rs` (fold + token fallback) |
| **Phase C Step 7 — PhilosophyAtlasPipeline + domain generalisation** | Proof of the v2 architecture's domain-generalisation claim: the same 8-phase runner + atlas schema + traversal primitives handle a different domain with **zero Rust code branches** — all domain knowledge lives in the markdown prompt assets. New `pipelines/philosophy_atlas.rs` (`PhilosophyAtlasPipeline`) wraps `LiteraryAtlasPipeline` as `inner` and overrides only the asset-bearing methods (`phase1_system`, `compose_phase1*`, `compose_seed_prompt`, `compose_phase3_facet`, `compose_phase8_configuration`) with philosophy-tuned versions; every parser, clustering config, trait-default delegates to the literary implementation unchanged. 8 new prompt assets in `philosophy_atlas_prompts/` — argumentative-prose-tuned Phase 1 (+ terse + seed variants), five Phase 3 facet-naming prompts that replace literary vocabulary with `position / dialectical dynamic / argumentative thread / conceptual arc`, and a Ricoeur-constrained Phase 8 with philosophy-specific structural patterns (dialectical hinge, position grid, progressive refinement, negative programme). Registered as `philosophy_atlas` in `PipelineRegistry::builtin`; surfaced in `enrich init --help`. 11 new pipeline-level tests + 1 new registry test. The `sep` corpus (Stanford Encyclopedia of Philosophy, 182k chunks already indexed locally) is the intended validation testbed — an end-to-end smoke on a single article is a straightforward follow-up once source-slicing decisions are made. | `pipelines/philosophy_atlas.rs`, `pipelines/philosophy_atlas_prompts/` (8 assets), `pipeline/registry.rs`, `literary_atlas.rs` (promoted `render_phase1_user_body` + `render_generic_phase3_exemplar` to `pub(super)`), `sovereign-cli/src/enrich_cmd/init.rs` (help surface) |
| **Phase C Step 8 — Cross-corpus Grounding edges with glass-box observability** | First cross-corpus bridge ships: `Grounding` edges connect entity atoms across two resolved atlases by canonical/alias/token match (no LLM, fully deterministic). **Observability is a first-class feature, not a diagnostic afterthought.** `CrossCorpusReport` returns per-detector candidate/match/rejection counts, rejection reasons grouped by cause, and a capped sample of concrete rejected pairs with the exact folded forms that failed. Every accepted `CrossCorpusEdge` carries a `MatchTrace` recording the signal path (`canonical_exact` / `alias_exact` / `canonical_token_unique`) + confidence + alternatives-considered. New `corpus-engine/src/enrichment/atlas/cross_corpus.rs` (450 LOC) defines the types + `detect_grounding` algorithm (3-step ladder: exact → alias → long-token-unique, with ambiguity rejection). `CrossCorpusEdge::flip_for_peer` builds the mirror-view so bidirectional writes stay symmetric. New writer helpers `write_atlas_cross_corpus_edges` + `read_atlas_cross_corpus_edges`. New CLI `sovereign enrich atlas-cross-corpus <local> <peer> [--explain <edge-id>]` — summary output prints the full report, `--explain` dumps the complete decision trace for a single edge so an operator can ask "why does this bridge exist?" and see the signal path verbatim. 10 new detector tests + 6 new CLI parse_args tests. Self-match smoke (`bk × bk`) produces 12 bridged edges at confidence 1.0, 0 rejections, and `--explain cc-brothers_karamazov-0002` prints the fold → signal → peer path in full. | `atlas/cross_corpus.rs` (new), `atlas/writer.rs` (+ `write_atlas_cross_corpus_edges` / `read_atlas_cross_corpus_edges`), `atlas/mod.rs` (re-exports), `sovereign-cli/src/enrich_cmd/atlas_cross_corpus.rs` (new CLI with `--explain`) |
| **Phase C Step 9 — Schema validation + cross-corpus review** | The spec §12 audit protocol ships as a computed-on-demand observability surface. New `corpus-engine/src/enrichment/atlas/schema_validation.rs` (~900 LOC) defines `SchemaValidationReport` across 8 dimensions — extraction coverage, enrichment-depth distribution, confidence histogram, atom-type utilisation, orphan analysis, discourse-act distribution, cross-corpus connectivity, deterministic gap counts. Each dimension emits both value (numbers + histograms) and **stable gap signatures** (`coverage:zero:X`, `utilisation:under:X`, `confidence:low_fraction_over_20pct`, `orphans:fraction_over_30pct`, `discourse:dominance:X`, `cross_corpus:bridge_coverage_under_5pct`, `gaps:ungrounded_claim_over_50pct`, `gaps:transition_without_trigger_over_80pct`). `compare_across_corpora` aggregates signatures from N reports: signatures present in ≥ 2 corpora land in `convergent_gaps` as **schema-revision candidates** with targeted recommendations per signature; signatures present in exactly one corpus land in `idiosyncratic_gaps` as **prompt-tuning candidates**. New CLIs `sovereign enrich schema-report <corpus> [--json]` (single-corpus table + writes `atlas/schema_validation.json`) and `sovereign enrich schema-review <a> <b> ...` (cross-corpus convergence surface). 11 new analyzer tests + 8 new CLI parse_args tests. Smoke on `brothers_karamazov` surfaced 3 live gap signatures: `coverage:zero:Configuration` (Phase 8 not re-run after last resolve), `utilisation:under:Configuration` (same cause), and `gaps:transition_without_trigger_over_80pct` — 6/6 transitions lack trigger events, a real systematic finding that Phase 3b never links Events to Transitions and is a schema-revision candidate the moment a second corpus shows the same pattern. | `atlas/schema_validation.rs` (new), `atlas/mod.rs` (re-exports), `sovereign-cli/src/enrich_cmd/schema_review.rs` (new CLI — both schema-report + schema-review drivers) |
| **End-to-end SEP validation — Process Philosophy article** | Validated the philosophy domain path end-to-end on a real SEP article. Sliced 80 lance-indexed chunks for `plato.stanford.edu/entries/process-philosophy/` into a single source file, ran the full pipeline on a 5-section subset: init → seed (2 seed entities) → extract (5/5 first-pass, 17 questions) → cluster-atlas (4 clusters across claim/event facets) → name-atlas-clusters (LLM labels: "Process philosophy argues reality's primary units are dynamic organizations", "Traditional Western metaphysics prioritizing static substances over dynamic processes", etc) → atlas-resolve `--phase all` (12 entities, 8 events, 8 states, 5 relations, 22 claims, 14 questions, 59 edges, 6 trajectories) → atlas-tensions → atlas-gaps (16 gaps) → atlas-configuration (**3 Phase 8 configurations: "Parmenidean static bias as dialectical hinge" conf 0.85, "Exhaustive ontology grid: static vs dynamic" conf 0.78, "Methodological tool-critique trajectory" conf 0.72** — all three match the structural patterns the philosophy Phase 8 prompt explicitly names) → schema-report (flagged `confidence:low_fraction_over_20pct` + `gaps:transition_without_trigger_over_80pct`). **Cross-corpus schema-review across `brothers_karamazov` + `process_philosophy` surfaced one convergent gap:** `gaps:transition_without_trigger_over_80pct` present in both corpora → **schema revision candidate** with the recommendation "Phase 3b should treat trigger_event as required or the schema should drop the field." **`atlas-cross-corpus bk × process_philosophy`** returned 0 bridges with full glass-box rejection output — genuinely disjoint entity vocabularies, correctly detected. The entire v2 enrichment stack + the philosophy domain generalisation + the §12.5 cross-corpus convergence protocol all work end-to-end on real data. | `/tmp/sep_process_philosophy.txt` (extracted source), `~/.svrnmesh/enrichment/process_philosophy/`, `~/.svrnmesh/indexes/process_philosophy/atlas/` |

### ✅ Landing 5 residuals — drift-variant merging + LLM tension classifier shipped; sketch→atom mapping still open

Landing 4 cracked the connectivity problem for attribution and
relations, leaving three residuals; two of the three have since
shipped:

1. **Drift-variant merging in Phase 3a — shipped.** See the Landing 5
   row above (Phase 3a Rule 3.5 + classifier fold/token fallback):
   entity-0007 (Fyodor Kárazóv drift) now merges into entity-0002
   (Fyodor Pavlovich) as an alias.
2. **LLM tension classifier — shipped.** `atlas/analysis/tension_classifier.rs`
   ("Phase 6 LLM Tension classifier") consumes
   `TensionCandidate` entries from `tensions.rs` and promotes
   genuine pairs to `Edge { edge_type: Tension, provenance:
   LlmPairwise }`, driven by `sovereign-cli-llm`'s
   `enrich_cmd/atlas_tensions_classify.rs` CLI.
3. **Plumb the sketch → atom mapping through Phase 3b** so
   intra-cluster candidates (claim pairs inside a Phase 2
   cluster) join the candidate pool. No `sketch_to_atom` symbol
   found in the codebase as of this pass — still carried over,
   unverified whether superseded by other machinery.

### ✅ Phase A Step 5 — shipped with Landing 5 numbering above.

### ✅ Phase A Step 6 — shipped above (traversal engine + brief assembler MVP)

See the "Phase A Step 6" row in the Shipped table above:
`atlas_traversal/{mod,classifier,engine,brief}.rs` +
`sovereign enrich atlas-query`. The manifest pass (`atlas/manifest.json`)
and the 20-question `validate-atlas` diagnostic battery from the
original plan are not confirmed shipped in this pass — not re-verified
here.

### ❌ Deferred — Phase 3b′ LLM atom interpretation

The prompt-driven per-facet resolver from the original plan. Re-enters
when we flip `enrichment_depth = Extracted → Interpreted` for atoms
whose deterministic resolution is ambiguous. Needed for Phase 5, not
for shipping a usable atlas.

### ✅ Phase C — Generalisation — shipped above (Steps 7, 8, 9)

- **Step 7 — Philosophy domain (SEP):** shipped as `philosophy_atlas`
  (see the Shipped table row above); validated end-to-end on a real
  SEP article ("Process Philosophy"), not just "Compatibilism" as
  originally planned.
- **Step 8 — Cross-corpus edges:** shipped as `atlas/cross_corpus.rs`
  (Grounding detector only — Framing and Provenance detectors are not
  confirmed shipped in this pass). Retroactive match via
  `atlas/pending_citations.json` not re-verified here.
- **Step 9 — Schema review:** shipped as `atlas/schema_validation.rs`
  + `sovereign enrich schema-review`.

---

## Landing 2 — what shipped and why

**Problem carried from Landing 1 smoke test on 5-chapter
`brothers_karamazov` subset:**

1. Relation-participant strings still drifted 60 % mixed-script
   even with seed threading — the seed block is rendered once at
   the top of the Phase 1 user message and is not visible to the
   model during the facet-specific subprompt-like paragraphs.
2. 3/5 chapters failed `parse_drift` on first pass — the seed block
   enlarges the Phase 1 input, the reasoning trace bloats, and the
   six-facet JSON output truncates mid-relation. Recoverable via
   `--retry-failed --terse` but requires a second invocation.

**What Landing 2 changed:**

- **`resolve_entity_id_fuzzy`** (resolution.rs:1041–1127) gained a
  third fallback: per-token Levenshtein vote. After the existing
  exact-fold + long-token-inverted-index paths return None, each
  folded query token (length ≥ 5) scans the token index for matches
  within Levenshtein ≤ 2. If a query token's matches all belong to
  one entity, that entity gets a vote; a single entity with ≥ 1 vote
  wins. Policy: prefer silence to a wrong snap. `Karazoff` ↔
  `Karamazoff` (2 edits) now snaps; `Ivan` ↔ `Ilya` (3 edits) does
  not; `Marinka` (1 edit from both `Marina` and `Marika`) stays
  unresolved.
- **`PHASE1_SEED_OUTPUT_BUDGET = 24576`** (runner.rs:99–109) — when
  the seed cache is present AND the runner has a token-aware chat
  closure configured, the default Phase 1 branch routes through
  that closure with the bumped budget. Non-seed runs are
  unchanged. Addresses the parse-drift failures seen in the
  5-chapter smoke test without needing a second invocation.
- **`relation_key` invariant locked.** A new test pins the
  current policy: two relations with the same resolved participant
  set but different labels collapse to one atom (first label wins).
  When the fuzzy snap became more permissive, the risk of
  collision rose; the test freezes the contract so future changes
  must decide the policy deliberately.

**Landing 2 victory condition** (as of shipping):

- ✅ **5/5 chapters parse first-pass** on the 5-chapter
  `brothers_karamazov` smoke test. No `--retry-failed --terse`
  invocations needed. Parse-drift eliminated at current corpus
  prompt sizes.
- ✅ **Mixed-script pollution eliminated.** Relation-participant
  Latin+Cyrillic mixing dropped from 60 % → 0 % (Phase 1 output).
- ⚠️ **Resolver coverage landed incrementally but below the 90 %
  aspiration.** End-to-end `atlas-resolve --phase all` participant
  coverage on the 5-chapter subset:
    - Relation participants: 36 % → 57 % (baseline → Landing 2).
    - Event participants: 47 % → 66 %.
  The remaining gap is not fold/match logic — the residual classes are
  (a) short 4-char first names (`Ivan` alone) blocked by the fuzzy
  min-length guard, (b) characters referenced in a relation without a
  prior `entities_introduced` sketch (Mitya in sec_0004 when Dmitri's
  introduction dropped it), and (c) generic references like
  "The general's widow". Addressing these is work for a later landing
  (widen seed coverage across sections, or post-hoc entity atom
  inference from relation mentions).
- ✅ **Sibling-distinction correctness preserved.** An explicit
  `first_token_matches` guard on Phase 3a Rule 3 ensures Alexei,
  Dmitri, and Ivan Karamazov stay three atoms instead of collapsing
  into one via the shared `Fyodorovic Karamazov` tokens.
- ✅ **Entity atom count tightened meaningfully** — 25 raw Phase 1
  sketches collapse to 13 post-merge atoms; the 13 are semantically
  coherent and a query like "who is Alyosha?" would return a single
  atom with its six drift variants as aliases.

---

## Verification

Baseline at Phase C Step 9 completion:
- **corpus-engine**: 657 tests green (+99 over the 558 pre-Landing-2
  baseline; +11 over Phase C Step 8's 646 for the schema_validation
  analyzer + convergence comparator tests).
- **sovereign-cli**: 323 tests green (+8 over Phase C Step 8's 315
  for the `schema-report` + `schema-review` parse_args suites).

**Smoke runs:**

| After | Target |
|---|---|
| Landing 2 | 5/5 chapters parse first-pass; relation-participant mixed-script drift < 10 %; `atlas-resolve --phase all` on 5-chapter subset writes coherent atoms + trajectory index. |
| Phase A Step 4 | Tension edges land on at least one claim-vs-claim and one claim-vs-state pair with chapter-level evidence. |
| Phase A Step 6 | `sovereign enrich query brothers_karamazov "how does Alyosha's faith change across the novel?"` returns a trajectory brief with ≥ 3 grounded states + transition events. `validate-atlas` scores ≥ 3/5 on ≥ 15 of 20 diagnostic questions. |
| Phase C Step 7 | The same runner produces an atlas for a SEP article with no domain branches. |
| Phase C Step 8 | Cross-corpus query against two extraction-first corpora produces grounding / provenance edges. Retroactive match proved (enrich A first → enrich B → cross-corpus edges appear without re-running A). |

**Performance.** `atlas_traversal_bench.rs` on M2 Max. Targets: single-
corpus < 60 ms, cross-corpus < 120 ms exclusive of LLM inference.

---

## Schema validation (spec §12)

`atlas/schema_validation.json` is incremental and runner-owned — each
phase appends. Currently written by Phase 1 (extraction coverage +
depth tally) and Phases 3a/3b (confidence distribution + atom-type
utilisation). Future phases add orphan-passage analysis (Phase 4),
discourse-act distribution (Phase 3b′), and cross-corpus metrics
(Step 8). `sovereign enrich schema-report <corpus>` (to land with
Step 6) prints the §12.4 diagnostic table.

---

## Out of scope

- **Phase B — Structure-first Wikipedia pipeline.** Deferred. The
  `AtlasIngestion` trait + `enrichment_depth` tag + depth-aware
  brief assembler make it a drop-in addition later.
- Re-enrichment migration tooling for existing v1 corpora.
- Training a smaller model on atlas output.
- Human-readable atlas visualisation (the brief assembler targets
  the LLM's context window).
