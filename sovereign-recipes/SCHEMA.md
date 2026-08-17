# Recipe schema reference

> **Generated** from `corpus-engine/src/recipe.rs` (+ the filter config types) by
> the `recipe_schema` test. Do not edit by hand — regenerate with
> `UPDATE_RECIPE_SCHEMA=1 cargo test -p corpus-engine --test recipe_schema`.
>
> This is the authoritative field list. The strings in the **TOML key** columns
> are exactly what a recipe author writes. See `GETTING_STARTED.md` for a
> walkthrough and `_templates/` for a copy-paste starting point.

A recipe is a TOML file with these top-level sections, threaded through the
acquire → extract → filter → chunk → embed → index pipeline:

- `[corpus]` — identity + catalog metadata (`CorpusMeta`)
- `[acquire]` — where the raw bytes come from (`AcquirerConfig`, tagged by `type`)
- `[extract]` — raw bytes → documents (`ExtractorConfig`, tagged by `type`)
- `[[filter]]` — optional document filters (`FilterConfig`, tagged by `type`)
- `[chunk]` — documents → chunks (`ChunkerConfig`, tagged by `type`)
- `[index]` — FTS + vector index settings (`IndexConfig`)
- `[enrichment]` — optional atlas/field-model enrichment (`EnrichmentConfig`)
- `[prebuilt]`, `[update]`, `[catalog]`, `[parameters]` — optional advanced blocks

---
## `PrebuiltConfig`

Optional pre-built index block. When present, the engine can download a pre-built LanceDB archive from HuggingFace instead of running a full ingest.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `hf_repo` | `String` | **yes** | — | HuggingFace repo in `org/name` format, e.g. `"sovereign-foundation/wikipedia-index"`. |
| `hf_filename` | `String` | **yes** | — | Filename within the HF repo, e.g. `"wikipedia-qwen3-embedding-0.6b.tar.zst"`. |
| `sha256` | `String` | **yes** | — | Hex-encoded SHA-256 of the archive. Empty string skips verification. |
| `compatible_embedding_model` | `String` | **yes** | — | Embedding model name the pre-built index was built with. Used to verify compatibility with the currently loaded model before downloading. |

## `AuthorityConfig`

`[authority]` block — see [`Recipe::authority`].

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `tool` | `String` | **yes** | — | Registered tool id (e.g. `sec_facts`) declared authoritative for this corpus's typed assertions. |

## `Recipe`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `corpus` | `CorpusMeta` | **yes** | — |  |
| `acquire` | `AcquirerConfig` | **yes** | — |  |
| `extract` | `ExtractorConfig` | **yes** | — |  |
| `chunk` | `ChunkerConfig` | **yes** | — |  |
| `index` | `IndexConfig` | no | type default |  |
| `enrichment` | `Option<EnrichmentConfig>` | no | type default | Optional epistemic enrichment configuration. When present and `enabled = true`, an enrichment phase runs after standard ingestion. Requires the engine to have been given an `InferenceFn`. |
| `authority` | `Option<AuthorityConfig>` | no | type default | Optional authority declaration (FINANCIAL_CORPORA.md §7.3): names the registered tool that is AUTHORITATIVE for a class of assertions this corpus carries in a typed store, where the same corpus's prose contains lookalike values that are NOT authoritative (comparatives, roundings, guidance) and confusing the two causes material harm. Registry data shipped by the recipe author — deliberately NOT a user setting: a "use deterministic figures" toggle would make honesty optional (§7.4). The named tool's `claims()` consults this binding. |
| `update` | `Option<UpdateConfig>` | no | type default | Optional corpus update configuration. When present, the health monitor can check for new versions and apply delta updates. |
| `prebuilt` | `Option<PrebuiltConfig>` | no | type default | Optional pre-built index. When present, users can skip full ingest by downloading a pre-built LanceDB archive from HuggingFace. |
| `catalog` | `Option<CatalogConfig>` | no | type default | Optional catalog-corpus configuration. When present, this recipe is a *catalog* of works and pairs with a templated content recipe (referenced by `content_recipe`) used for on-demand single-work ingest. See [`CatalogConfig`] and `Recipe.corpus.kind = Catalog`. |
| `filter` | `Vec<FilterConfig>` | no | type default | Document-level filters that scope the corpus by accepting or rejecting individual `ExtractedDoc`s before chunking. The canonical use case is Wikipedia "Core" — top-N by pageview rank ∪ Vital Articles list — but the mechanism works for any extractor (e.g. StackExchange `min_score`, OpenAlex `accepted_languages`). Empty / absent means the pipeline runs unfiltered. |
| `filter_mode` | `FilterModeConfig` | no | type default | How filters in `filters` combine. Defaults to [`ComposeMode::Any`] — a document is accepted if any filter accepts. Set `mode = "all"` to require every filter to accept. Lives in its own `[filter_mode]` table because TOML does not allow scalars next to an array of tables. |
| `parameters` | `BTreeMap<String, ParameterSpec>` | no | type default | Install-time parameters declared by the recipe. Concrete values are supplied by the user at `corpus install` time and interpolate into the `[acquire]` block via `{name}` placeholders. Lets a financial journalist (for example) ship one `sec-filings` recipe and let downstream users plug in their own entity list / form types / date range. See [`ParameterSpec`] and [`Recipe::resolve_parameters`]. |
| `display` | `Option<DisplayMeta>` | no | type default | Presentation hints for UI surfaces (Atlas View rail grouping, Settings → Knowledge tile icons, etc.). Pure UI metadata — retrieval and ingest ignore this block. Drives the "Conversations" group in the Atlas View when corpora declare `category = "conversation"`. `#[serde(default)]` so recipes pre-dating this block still parse — see the back-compat policy at the top of this module. |
| `retrieval` | `RetrievalConfig` | no | type default | Retrieval-time behaviour hints (see [`RetrievalConfig`]). Unlike `[display]`, the runtime *reads* this when retrieving from the corpus. `#[serde(default)]` so recipes pre-dating the block parse. |

## `DisplayMeta`

Presentation hints for a recipe. See [`Recipe::display`]. Pure UI metadata: the retrieval layer reads `category` to decide whether to render a chunk under "From your conversations" rather than the corpus_id slug (see `format_scored_chunks_with_kinds`), and the Atlas View rail groups corpora that share a category under one header. No semantic meaning is attached to category strings — add new ones as needed.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `category` | `Option<String>` | no | — | Logical group this corpus belongs to. Example values: `"conversation"`, `"reference"`, `"argument"`, `"personal"`. `None` means "ungrouped" — UI buckets these as "Other". |
| `icon` | `Option<String>` | no | — | Optional icon hint for desktop tiles. Free-form string; the frontend maps known values (`"chat-bubble"`, `"book"`, …) onto its icon set and falls back to a generic glyph for unknown values. |

## `RetrievalConfig`

Retrieval-time behaviour hints for a corpus. Unlike [`DisplayMeta`] (pure UI), these change how the runtime *retrieves* from this corpus. `#[serde(default)]` on the struct + each field so a recipe omitting the `[retrieval]` table parses with baseline behaviour.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `dedup_by_source` | `bool` | no | type default | When true, apply per-article source dedup to this corpus's retrieval: after fusion, keep each source article's single best chunk, then return the top-K *distinct* articles. Captures the canonical-source lift for corpora with narrow authoritative sources (SEP: +6 sources, 76%→85% on the eval bank, validated 2026-06-04) without the operator-only `SOVEREIGN_RERANK_DEDUP_ONLY` env var. Leave false for topical corpora (e.g. Wikipedia), where strict one-chunk-per-article truncation *regresses* recall — there the per-article tiebreak needs a cross-encoder, not blind dedup. |
| `personal_scope` | `bool` | no | type default | When true, this corpus counts as user-owned *personal* content (conversations, journals, watched folders / Obsidian vaults). Personal-scope turns restrict retrieval to personal corpora; before this flag the runtime used a hardcoded corpus-id prefix list, which silently excluded watched-folder corpora (ids are `watched-<hash>`). Reference corpora (Wikipedia, SEP, …) leave this false. |

## `FilterModeConfig`

Sidecar TOML table for [`Recipe::filter_mode`]. Splitting this from the `[[filter]]` array keeps the recipe TOML grammatically valid: the `[[filter]]` form is an array-of-tables and cannot host a scalar `mode = "any"` field directly.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `mode` | `ComposeMode` | no | type default |  |

## `ParameterSpec`

Install-time parameter declared by a recipe. Lets the recipe author defer concrete values (entity lists, date ranges, form types) until the user runs `sovereign corpus install`. The CLI prompts for each declared parameter (or accepts `--params key=value` non-interactively); the desktop renders a form. Resolved values interpolate into the `[acquire]` block via `{name}` placeholders.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `type` | `ParameterKind` | **yes** | — | Type of value expected. Drives prompting (text input, date picker, comma-separated list) and validation. |
| `description` | `String` | no | type default | Human-readable description shown in prompts and the desktop form. |
| `required` | `bool` | no | `default_true()` | Whether the user must provide a value. `true` by default — require explicit opt-out so a missing required value can't silently install an empty corpus. |
| `default` | `Option<toml::Value>` | no | type default | Default value if the user does not provide one. Type must match `kind`. Stored as `toml::Value` so the recipe can declare lists / integers / strings / dates uniformly. |

## `ParameterKind`

Type tag for [`ParameterSpec::kind`]. Drives both validation of supplied values and the UI affordance shown to the user.

Allowed values:

- `string` — Free-form string (a CIK, a search query, a tag).
- `int` — 64-bit signed integer.
- `date` — ISO-8601 calendar date (`YYYY-MM-DD`). Validated lexically; not parsed into a chrono value here so the recipe schema doesn't grow a date-library dependency.
- `list` — Comma-separated list of strings. The CLI accepts either a repeated flag or a single comma-separated value; the desktop renders a multi-tag input.

## `UpdateConfig`

Configures automatic corpus updates.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `manifest_url` | `String` | **yes** | — | URL that returns a version manifest JSON for this corpus. |
| `auto_update` | `bool` | no | type default | If true the health monitor applies updates autonomously during the maintenance window. If false, a pending decision is surfaced to the user instead. |
| `ingest_driver` | `Option<String>` | no | type default | Names the subsystem that owns ingest + ongoing updates for this corpus. When set, [`crate::engine::CorpusEngine::ingest`] short-circuits to "create an empty index, write `_corpus_meta.json`, return" instead of running the recipe's `[acquire]` pipeline — the named driver is then responsible for populating chunks on its own schedule. Current values: - `"watcher"` — daemon-side watcher (e.g. `corpus_engine::update::newsworthy_watcher::WikipediaNewsworthyWatcher`) handles fetches + reindexes via `reindex_by_source_doc_id`. The recipe's `[acquire]` block is informational shape only (the watcher reads the URL template + chunker config from it) and is not invoked by `ingest`. `None` (the default) preserves the historical contract: ingest runs the full acquire/extract/chunk/index pipeline. |

## `EnrichmentConfig`

Configures the optional enrichment pipeline. The new field model enrichment uses domain-specific prompts and HDBSCAN clustering. Set `type = "field_model"` and `domain = "philosophy"` (or another domain) to use the new pipeline. For typed-relationship investigations (e.g. SEC filings → who invests in whom while also being a customer), set `type = "investigation"` and declare your `[[enrichment.entity_types]]`, `[[enrichment.relationship_types]]`, and `[[enrichment.patterns]]` blocks. The investigation pipeline generates LLM prompts directly from the schema, so a domain expert authors the extraction shape in TOML without touching Rust. See [`EntityTypeDecl`], [`RelationshipTypeDecl`], and [`PatternDecl`].

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `enabled` | `bool` | no | type default |  |
| `type` | `String` | no | `default_enrichment_type()` | Enrichment type: "field_model" (default), "atlas", "investigation". |
| `domain` | `Option<String>` | no | type default | Domain identifier — **its meaning, and the registry it is checked against, depend on `type`.** With `type = "field_model"` the only valid values are the registered field-model domains: `philosophy`, `personal`, `conversational`, `business_email`, `institutional` (omit for `philosophy`); anything else is refused at load. With `type = "atlas"` it selects an atlas pipeline instead (`literary`, `philosophy`, `referential`), and `pipeline` overrides it. Sharing a key across two registries is what stranded two ingests on 2026-08-07 with `Unknown enrichment domain: literary`. |
| `pipeline` | `Option<String>` | no | type default | Explicit atlas pipeline id (e.g. `"literary_atlas"`, `"philosophy_atlas"`) for `type = "atlas"` recipes. Optional override: when set, the desktop "Build & enrich" bridge (`recipe_enrich_init_from_corpus`) uses it directly instead of inferring the pipeline from `domain`. Previously this key was accepted and silently dropped (decorative); making it a real field means a recipe that pins a pipeline gets the pipeline it asked for. `None` → infer from `domain`. |
| `ontology` | `Option<OntologyConfig>` | no | type default | Custom atlas ONTOLOGY for `type = "atlas"` recipes. This is the headline "build the ontology for your specific domain" path: instead of picking a prebuilt genre pipeline (`literary_atlas`/`philosophy_atlas`), the recipe author (with the agent) describes — in the domain's own language — what entities / relations / claims / events matter. A generic `ConfigurableAtlasPipeline` runs the universal 7-phase atlas machinery with this guidance and writes the same `atoms.json` that feeds chat. When present (with non-empty `guidance`), it takes precedence over `pipeline` and `domain`. `None` → fall back to a prebuilt atlas pipeline. |
| `prompt_version` | `Option<String>` | no | type default | Prompt version tag. Recorded in `_corpus_meta.json` so the health checker can detect stale enrichment when prompts change. |
| `clustering` | `Option<ClusteringToml>` | no | type default | HDBSCAN clustering parameters. |
| `alignment` | `Option<AlignmentToml>` | no | type default | Alignment parameters. |
| `fault_lines` | `Option<FaultLinesToml>` | no | type default | Fault line detection parameters. |
| `entity_types` | `Vec<EntityTypeDecl>` | no | type default | Entity types the investigation pipeline should extract from each chunk. Listed in the LLM extraction prompt so the model canonicalizes mentions to one of these typed shapes (e.g. `company`, `fund`, `person`). Empty when `enrichment_type != "investigation"`. |
| `relationship_types` | `Vec<RelationshipTypeDecl>` | no | type default | Relationship types the investigation pipeline should extract (e.g. `revenue`, `investment`, `cloud_commitment`, `board_seat`). Each relationship has typed attributes the LLM is asked to populate (`amount_usd`, `date`, etc.). |
| `patterns` | `Vec<PatternDecl>` | no | type default | Graph-level patterns to detect once the relationship graph is built. Built-in detectors cover cycle / role-overlap / threshold patterns; the recipe author chooses which to run. |
| `reconciliation` | `Option<ReconciliationToml>` | no | type default | Architecture-over-Enron Phase 4: multi-origin reconciliation policy. `None` (the default) skips reconciliation entirely; pipelines that don't carry [`crate::enrichment::atlas::atoms::Provenance`] on their entity atoms produce nothing to reconcile across anyway. Recipes that enable described-asset + email extractors set this block to tune the merger. |
| `normalization` | `Option<NormalizationConfig>` | no | type default | Corpus-specific entity-name coalescing rules for the investigation pipeline. The engine supplies the *mechanism* (alias map, prefix / suffix / qualifier stripping, identity-by-attribute); this block supplies the *vocabulary*, so domain knowledge (US states, Air Force base aliases, disposition categories) lives in the recipe as data rather than hardcoded in the abstraction layer. `None` → names fold by case/punctuation only (the engine default). Consumed by [`crate::enrichment::investigation::normalize::Normalizer`]. |

## `OntologyConfig`

Custom atlas ontology declared in `[enrichment.ontology]`. The headline "build the ontology for your domain" surface: `guidance` is domain-language instructions for what to extract (entities, relations, events, claims), injected into a NEUTRAL atlas Phase-1 prompt by [`crate::enrichment::pipeline::pipelines::configurable_atlas::ConfigurableAtlasPipeline`]. The universal atom schema + open `EntityType::Other(..)` labels let a domain expert author the extraction shape in TOML without touching Rust, and the result feeds chat via the same `atoms.json` the prebuilt genre pipelines produce. Precedence: a non-empty `guidance` here beats `pipeline`/`domain`.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `guidance` | `String` | no | type default | Domain-language extraction guidance — what entities, relations, events, and claims matter in THIS corpus's domain, in the domain's own words. Appended under a "Domain focus" heading to the neutral atlas Phase-1 system prompt. The load-bearing field; an empty `guidance` disables the custom path (falls back to a prebuilt atlas pipeline). |
| `vocabulary` | `Option<OntologyVocabulary>` | no | type default | Optional CLI/label vocabulary overrides (what a "concern", "position", "tension", "absence", and unit of "evidence" are called for this domain). Omitted fields fall back to generic defaults in the pipeline. |

## `OntologyVocabulary`

Per-domain term overrides for the configurable atlas pipeline's vocabulary. Maps onto the engine's `Vocabulary`; any omitted term uses a generic default.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `concern_term` | `Option<String>` | no | type default |  |
| `position_term` | `Option<String>` | no | type default |  |
| `tension_term` | `Option<String>` | no | type default |  |
| `absence_term` | `Option<String>` | no | type default |  |
| `evidence_term` | `Option<String>` | no | type default |  |

## `NormalizationConfig`

Data-driven entity-name normalization for the investigation pipeline. Every field is optional; an empty config folds names by case/punctuation only. See [`crate::enrichment::investigation::normalize::Normalizer`] for the mechanism that applies it.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `identity_attribute` | `std::collections::BTreeMap<String, String>` | no | type default | `entity_type → attribute`: entities of this type take their identity from the named attribute's value, not their (often noisy) name — e.g. `adjudication = "category"` collapses date-/synthetic-id-named nodes that share a disposition. Applied during the offline re-fold (`recoalesce`), which remaps relationship endpoints so it can't strand an edge; build-time coalescing stays name-based (endpoint-safe). |
| `fold` | `Vec<FoldRule>` | no | type default | Name-fold rules, each scoped to the entity types it lists. |

## `FoldRule`

One scoped name-fold rule. Applied (in order) to the entity types in `types`: alias map on the full folded form, then drop a leading qualifier, then a trailing qualifier run, then the trailing-suffix run (OCR-tolerant), then re-check the alias map on the reduced base. Identity-grade — only qualifier/suffix regions are touched, base tokens are never fuzzy-matched, so two distinct bases never merge.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `types` | `Vec<String>` | **yes** | — | Entity types this rule applies to (e.g. `["installation"]`). |
| `aliases` | `Vec<(String, String)>` | no | type default | `(folded-variant, canonical)` acronym/alias pairs, exact-matched on the folded surface form (e.g. `["wpafb", "wright patterson"]`). |
| `leading_prefixes` | `Vec<String>` | no | type default | Leading qualifier phrases dropped when followed by a base (e.g. `"air material command"`, `"atic"` → the org sat AT the base). |
| `trailing_qualifiers` | `Vec<String>` | no | type default | Trailing qualifier tokens/phrases dropped before the suffix run (e.g. US state names: `"ohio"`, `"new mexico"`). Multi-word entries match a trailing token-pair. |
| `trailing_suffixes` | `Vec<String>` | no | type default | Single-token trailing suffix vocabulary, OCR-tolerant (edit-distance 1) — `"air"`, `"force"`, `"base"`, `"afb"`, `"field"`, … A trailing run of these (plus ≤2-char OCR fragments) is stripped to reach the base. |

## `ReconciliationToml`

TOML mirror of [`crate::enrichment::reconciliation::ReconciliationPolicy`]. Kept as a separate struct so the recipe schema stays string-named (the policy struct uses Rust-native field names; the TOML can rename in a future revision without touching the runner).

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name_similarity_threshold` | `f32` | no | `default_name_similarity_threshold()` | Minimum fold-overlap similarity for a name match to count. See [`crate::enrichment::reconciliation::ReconciliationPolicy`]. |
| `cross_origin_required_signals` | `u8` | no | `default_cross_origin_required_signals()` | Minimum *distinct* signals required for a cross-origin merge. |
| `judge_when_uncertain` | `bool` | no | `default_true()` | Escalate uncertain candidates to the calibrated judge. |
| `judge_trials` | `u8` | no | `default_judge_trials()` | Judge trial count when escalation fires. |
| `column_aware` | `Option<crate::extractors::column_aware::ColumnAwareConfig>` | no | type default | Column-aware extractor configuration. `None` to skip the column-aware pass entirely (the multi-origin merger still runs on whatever other signals the corpus produces). |

## `EntityTypeDecl`

One typed entity an investigation extracts. The recipe author declares the *shape* — name, description, expected attribute keys — and the investigation pipeline generates the LLM extraction prompt directly from this schema. No Rust required. Example: ```toml [[enrichment.entity_types]] name = "company" description = "A corporation or legal entity" attributes = ["name", "ticker", "cik", "role"] ```

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `String` | **yes** | — |  |
| `description` | `String` | no | type default |  |
| `attributes` | `Vec<String>` | no | type default | Attribute keys the LLM should try to populate on each extracted instance. Free-form — the LLM extracts whatever keys it can locate in the chunk; missing keys land as null. |

## `RelationshipTypeDecl`

One typed relationship the investigation extracts (e.g. `revenue`, `investment`, `cloud_commitment`, `board_seat`). Combined with [`EntityTypeDecl`], the schema fully drives the LLM extraction prompt.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `String` | **yes** | — |  |
| `description` | `String` | no | type default |  |
| `attributes` | `Vec<String>` | no | type default | Attribute keys for the relationship instance — typically numeric (`amount_usd`, `percentage_of_total`) or temporal (`date`, `period`, `duration_years`). |
| `directional` | `bool` | no | `default_true()` | `true` for asymmetric relationships (A → B is different from B → A: e.g. `revenue` and `investment`). `false` for symmetric ones (e.g. `co_membership`). |

## `PatternDecl` (select with `type = "…"`)

A graph-level pattern to detect once the relationship graph is built. The investigation pipeline runs every declared [`PatternDecl`] after the graph is populated; matches land in `pattern_findings.json` for the audit step.

### `type = "circular_flow"`

Money / influence flows in a cycle: A→B→C→A. Powered by petgraph's Tarjan SCC; filters cycles with `len >= min_entities` whose edges all match `edge_types`.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `String` | **yes** | — |  |
| `description` | `String` | no | type default |  |
| `min_entities` | `u32` | no | `default_circular_flow_min_entities()` |  |
| `edge_types` | `Vec<String>` | **yes** | — |  |

### `type = "role_overlap"`

Same pair of entities connected by two edge types that represent distinct roles. Canonical example: `(investor, customer)` — A invests in B AND A is a major customer of B's product. `entity_roles` maps a free-form role name (used in narration) to a typed-edge specifier `"<edge_type>.<from|to>"` describing which side of the edge the entity sits on.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `String` | **yes** | — |  |
| `description` | `String` | no | type default |  |
| `entity_roles` | `BTreeMap<String, String>` | **yes** | — |  |

### `type = "threshold"`

Numeric-attribute threshold over edges of a single type. E.g. "revenue concentration > 10%": find revenue edges whose `percentage_of_total` attribute exceeds 0.10.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `String` | **yes** | — |  |
| `description` | `String` | no | type default |  |
| `edge_type` | `String` | **yes** | — |  |
| `attribute` | `String` | **yes** | — |  |
| `threshold` | `f64` | **yes** | — |  |
| `comparison` | `Comparison` | no | `default_comparison()` |  |

### `type = "custom_sql"`

**Reserved — not yet implemented.** Recipe authors can declare `type = "custom_sql"` today; the runtime parses it cleanly and the validator surfaces a warning so the author knows it won't run yet. The future implementation will execute `query` on a read-only SQLite connection materialised from the relationship graph, with `set_authorizer` rejecting `ATTACH` / `PRAGMA` / `load_extension`, a 5-second statement timeout, and single-statement enforcement. See SYSTEM_OVERVIEW.md §3.10 for the back-compat rationale: reserving the shape now lets us land the SQL escape hatch later without forcing a schema migration on recipes already in the wild.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `String` | **yes** | — |  |
| `description` | `String` | no | type default |  |
| `query` | `String` | **yes** | — | SQL query against `entities` / `relationships` / `pattern_findings` tables. Validation is parse-only today; execution arrives in a follow-up PR. |

## `Comparison`

Comparison operator for [`PatternDecl::Threshold`]. Strict (`gt`/`lt`) by default — boundary-equal cases are rare in the investigation domain and the recipe author can opt into inclusive comparisons explicitly.

Allowed values:

- `greater_than`
- `greater_or_equal`
- `less_than`
- `less_or_equal`
- `equal`

## `ClusteringToml`

HDBSCAN clustering parameters (TOML representation).

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `min_cluster_size` | `Option<usize>` | no | type default |  |
| `epsilon` | `Option<f32>` | no | type default |  |
| `label_sample_size` | `Option<usize>` | no | type default |  |

## `AlignmentToml`

Alignment parameters (TOML representation).

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `alignment_threshold` | `Option<f32>` | no | type default |  |
| `min_chunks_for_discovery` | `Option<usize>` | no | type default |  |

## `FaultLinesToml`

Fault line detection parameters (TOML representation).

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `proximity_threshold` | `Option<f32>` | no | type default |  |
| `min_confidence` | `Option<f32>` | no | type default |  |

## `CorpusMeta`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `id` | `String` | **yes** | — |  |
| `name` | `String` | **yes** | — |  |
| `description` | `String` | no | type default |  |
| `license` | `String` | no | type default |  |
| `mesh_sharing` | `bool` | no | `default_true()` |  |
| `scope` | `Option<String>` | no | type default | Distribution scope. `Some("local")` pins a corpus to the host machine: it may never be shared via the mesh regardless of `mesh_sharing`. Used by `KnowledgeView` corpora sourced from private state (e.g. `personal-knowledge`, `conversation-history`) so the privacy guarantee is structural, not policy-layer. `None` = default behaviour governed by `mesh_sharing`. |
| `query_sharing` | `Option<bool>` | no | type default | Whether peers may run federated knowledge-search queries against a node that hosts this corpus. Distinct from `mesh_sharing`, which governs byte-level redistribution (shipping the index to another node for replication). Example: Stanford Encyclopedia of Philosophy has `mesh_sharing = false` because the license prohibits redistribution of the text, but `query_sharing = true` because returning cited snippets in response to queries is fair use (what Google does). Back-compat default: `None` means "fall back to `mesh_sharing`" — preserves the pre-split behavior for any recipe or stored index that hasn't been updated. Set explicitly to override. |
| `grantable` | `bool` | no | type default | Whether this corpus MAY be temporarily lent to a user-selected set of mesh peers for a one-off compute assist (embed + enrich) under an ephemeral, revocable grant — WITHOUT ever changing its standing `mesh_sharing`/`scope`. Set `true` only by user-owned file corpora (Obsidian vault / document folder / watched folder). Structural `KnowledgeView` corpora (`personal-knowledge`, `conversation-history`, …) leave it `false` so they can never be grant-shared, even transiently. Default `false`: a corpus is not grantable unless it explicitly opts in. See the ephemeral ingest-grant store in `commonwealth-knowledge`. |
| `size_compressed_gb` | `f64` | no | type default |  |
| `size_indexed_gb` | `f64` | no | type default |  |
| `schema_version` | `u32` | no | `default_schema_version()` | Schema version for this recipe format. Defaults to 1. Increment when making breaking changes to the TOML schema. |
| `kind` | `CorpusKind` | no | type default | What kind of content this corpus holds. Defaults to `Knowledge`. Catalog corpora hold one chunk per work (metadata only) and pair with a `[catalog]` block at the recipe top level. Code corpora are produced by `sovereign code index`. See [`crate::types::CorpusKind`]. |
| `on_demand` | `bool` | no | type default | Marks a recipe as "templated, never directly ingested." On-demand recipes (e.g. `gutenberg-work`) are stamped from a catalog entry at runtime via [`crate::types::CorpusSpec::Inline`]. The plain [`crate::engine::CorpusEngine::ingest`] path refuses to run an `on_demand = true` recipe whose `[corpus] id` has not been overridden, so a misclick can't blast 70K Gutenberg books into the corpus dir. |
| `parent_corpus_id` | `Option<String>` | no | type default | Parent corpus this recipe is grouped under. Two use cases share the field: 1. **Dynamic per-work catalog children.** Set at runtime by an on-demand catalog ingest (e.g. `gutenberg-2701` carries `parent_corpus_id = "gutenberg"`) via [`crate::types::CorpusSpec::Inline`]. Search consumers group per-work corpora under their catalog and suppress repeated ingest offers for works already read. 2. **Static layer/satellite relationships declared in TOML.** `wikipedia-simple` and `wikipedia-newsworthy` declare `parent_corpus_id = "wikipedia"` to mark themselves as layers of the Core Wikipedia corpus. UI surfaces (e.g. the desktop picker) hide layered children from the top-level list and render them as toggles under the parent's row. The data layer is unaffected — each child still has its own `id`, index dir, mesh-sharing rules, and watcher (if any). Stamped onto the on-disk `IndexMeta` in both cases, so `installed_indexes()` and downstream UI can group consistently. Pointing at an id that doesn't exist is not a parse error — the desktop falls back to top-level rendering for orphans. |
| `mutable_merge` | `Option<MutableMergePolicy>` | no | type default | How `merge_shards` should reconcile rows that share a logical key across two shards. `None` (the default) keeps the content-hash-based dedupe used by every classic corpus — divergent edits of the same source document survive as two rows with different `content_hash`. The `alignment` corpus opts into [`MutableMergePolicy::SourceDocIdNewestMtime`] so that two daemons editing the same memory or plan file converge on the newer copy after a mesh merge. |

## `MutableMergePolicy`

Reconciliation policy invoked by [`crate::sharding::merge_shards`] when the merged target's `_corpus_meta.json` carries a `mutable_merge` value. Default (`None`) preserves classic content-hash dedupe.

Allowed values:

- `source_doc_id_newest_mtime` — Group rows by `source_doc_id`. When a logical key collides, keep the row with the highest `mtime`. Rows whose `source_doc_id` is null fall back to content-hash dedupe.

## `CatalogConfig`

Pairs with `CorpusMeta::kind = Catalog`. Tells the on-demand ingest service how to take a catalog entry and produce a fully ingested per-work corpus from it. See `gutenberg/recipe.toml`.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `id_field` | `String` | **yes** | — | Field name on the catalog `ExtractedDoc` (or its metadata blob) that uniquely identifies a work. Used by the on-demand flow to substitute into `download_url_template` and to derive the per-work corpus id (`<catalog_id>-<work_id>`). |
| `download_url_template` | `String` | **yes** | — | URL template with a `{id}` placeholder, e.g. `"https://www.gutenberg.org/cache/epub/{id}/pg{id}.txt"`. Resolved at on-demand ingest time and injected as the sole `[acquire] url` of the content recipe. |
| `content_recipe` | `String` | **yes** | — | Recipe id of the content recipe used to perform the per-work ingest, e.g. `"gutenberg-work"`. Must be `on_demand = true` and live in the registry. |
| `estimated_words_field` | `Option<String>` | no | type default | Optional name of a metadata column carrying an estimated word count (used to compute an ingest-time estimate the UI can show). |
| `ingest_estimate_wpm` | `Option<u32>` | no | type default | Throughput estimate for the ingest stage, in words per minute. Combined with `estimated_words` to produce the "this will take ~N minutes" surface. Default 8000 wpm (conservative for an M-class machine on the embed slot). |
| `enrich_estimate_wpm` | `Option<u32>` | no | type default | Throughput estimate for the enrichment stage, in words per minute. Default 500 wpm. |
| `target_corpus_id` | `Option<String>` | no | type default | Optional shared corpus id that catalog-driven ingests append into. When set, every successful work-ingest writes its chunks into a single growing corpus (e.g. `"wikipedia-fetched"`) instead of creating one corpus per work. Atlas, mesh-share, and retrieval all happen against the single shared corpus — a much better fit for catalogs whose long-tail can be thousands of articles. When unset (default), the legacy per-work pattern (`<catalog_id>-<work_id>`) is used. |
| `expansion_enabled` | `bool` | no | type default | Enable one-hop "minesweeper" link-expansion after fetching an article. When true, the just-ingested article's outgoing links are queued for follow-up fetch into the same `target_corpus_id`. Only meaningful when `target_corpus_id` is set — without a shared target each expansion would spawn yet another per-work corpus. |
| `expansion_link_cap` | `u32` | no | `default_expansion_link_cap()` | Maximum number of linked articles to fetch in expansion. Ranking is significance-first (lead-section links beat body-section links, then document order). Default 20 keeps the per-fetch cost bounded; raise for deeper neighbourhood pre-loading, lower for fastest-only-the-asked behaviour. |

## `RequestTemplate`

One HTTP request template. Combined with `[recipe.parameters]` values via `{name}` interpolation. `for_each` declares which parameters cross-product the template — e.g. one paginated request sequence per (entity, form_type) pair when ingesting SEC filings.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `url` | `String` | **yes** | — | URL with `{name}` placeholders for parameters and `{base_url}` for the acquirer's `base_url`. |
| `method` | `HttpMethod` | no | type default | HTTP method. Defaults to `GET`. |
| `body` | `Option<String>` | no | type default | Optional request body, with `{name}` interpolation. Used by JSON-RPC-shaped APIs that take queries via POST. |
| `for_each` | `Vec<String>` | no | type default | Cross-product the request over these parameter names. Each referenced parameter must be a `List` (or implicitly promoted scalar). The acquirer issues one full paginated sequence per cartesian-product binding. Empty = a single request with all `{name}` placeholders resolved element-wise from their declared values. |

## `HttpMethod`

HTTP method for a [`RequestTemplate`]. Kept narrow on purpose — REST acquisition rarely needs PATCH/DELETE/PUT.

Allowed values:

- `GET`
- `POST`

## `PaginationStrategy` (select with `type = "…"`)

Pagination strategy for [`AcquirerConfig::HttpApi`]. The acquirer drives the loop; the strategy translates per-page response state into the next request. None of the strategies make assumptions the recipe author can't articulate from the API's docs.

### `type = "offset"`

Offset-based: increment `param` by `page_size` each page; stop when the page returns fewer than `page_size` items found at `items_path` (a JSONPath expression on the response body).

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `param` | `String` | no | `default_offset_param()` |  |
| `page_size` | `usize` | **yes** | — |  |
| `items_path` | `String` | no | `default_items_path()` |  |

### `type = "cursor"`

Cursor-based: read the next cursor from `response_path` (JSONPath); pass it as the next request's `param`. Stops when the cursor field is null/missing.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `param` | `String` | **yes** | — |  |
| `response_path` | `String` | **yes** | — |  |

### `type = "next_url"`

Whole-URL next pointer: read a complete URL out of `response_path` and follow it as-is. Common for RFC 5988 Link-style APIs and GitHub.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `response_path` | `String` | **yes** | — |  |

### `type = "page_number"`

Page-number sequence: increment `param` from `start` to `end` (inclusive). Use when the page count is known upfront. `end` may reference a recipe parameter via `{name}` to let the user bound the run length.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `param` | `String` | no | `default_page_number_param()` |  |
| `start` | `usize` | no | `default_page_number_start()` |  |
| `end` | `usize` | **yes** | — |  |

## `FollowConfig`

Tells the acquirer how to take an API response and turn it into a list of documents to fetch and persist. Without this block, the page responses themselves are written to disk.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `document_url_path` | `String` | **yes** | — | JSONPath expression selecting an array of URL strings from the response body, e.g. `"$.hits.hits[*]._source.file_url"` for an EDGAR full-text search response. |
| `document_format` | `DocFormat` | no | type default | Format hint that drives the on-disk extension under `<acquired-dir>/docs/<sha>.<ext>`. The extractor walks the directory regardless of which format flag the acquirer set. |
| `max_concurrency` | `usize` | no | `default_follow_concurrency()` | Maximum concurrent in-flight document downloads. Default 4 — keep modest for public APIs to avoid 429s. The acquirer's `rate_limit_per_second` (if any) caps the aggregate request rate orthogonally. |

## `DocFormat`

On-disk document format hint for [`FollowConfig::document_format`].

Allowed values:

- `html`
- `json`
- `xml`
- `plaintext`

## `SectionRule`

One section to extract from each HTML file. The recipe author declares anchor regexes (`start_pattern` / `end_pattern`); the extractor strips tags first, then runs the regexes against the resulting plain text. The matched span between start and end becomes one `ExtractedDoc` per file.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `String` | **yes** | — | Stable name for this section, e.g. `"md_and_a"`. Used as part of the emitted document's title and stamped in `metadata.section_name`. |
| `description` | `String` | no | type default | Human-readable description shown in `recipe test` output and used as a hint when a miss occurs (the test harness searches nearby text for keywords from this description). |
| `start_pattern` | `String` | **yes** | — | Regex pattern matching the start of the section. Compiled at extractor construction; bad regexes fail loudly with the section name in the error. |
| `end_pattern` | `String` | **yes** | — | Regex pattern matching the end of the section. Typically a "next item heading" anchor, e.g. `(?i)item\\s+[0-9]` for SEC filings. |
| `repeating` | `bool` | no | type default | When true, emit one document per `start_pattern` match in the file instead of only the first. Each emitted section runs from its start match to the *next* start match, bounded earlier by `end_pattern` if it matches within that window (so the final repetition can terminate on a trailing anchor like `ADDITIONAL INFORMATION`). Use for documents that repeat a section an unbounded number of times — e.g. the numbered proposals in an SEC proxy statement (DEF 14A) or dated articles in a governance charter. Default `false` preserves the first-match-only behaviour relied on by single-section recipes. |

## `FallbackRule` (select with `type = "…"`)

Fallback for files where no section pattern matched. Without a fallback, files with no matching section are silently dropped.

### `type = "full_document"`

Emit the entire stripped text as a single document. Useful when "we'd rather have something than nothing" — the extractor still records the miss in `_section_misses.json` so the recipe author can iterate on the regex.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_chars` | `Option<usize>` | no | type default | Cap the output at this character count. None = no cap. |

### `type = "first_n_chars"`

Emit the first N characters of the stripped text. Cheap approximation of "the document's intro" for content-heavy pages without clear section structure.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `n` | `usize` | **yes** | — |  |

## `AcquirerConfig` (select with `type = "…"`)

### `type = "bulk_download"`

Bulk-download one or more archives over HTTP with resume. Single-source recipes use `url = "..."`. Multi-source recipes (e.g. the Stack Exchange knowledge layer pulling from several per-site .7z archives) use `urls = ["...", "..."]`. The downloader writes each archive under a per-corpus directory, so the extractor receives a directory of archives rather than a single file in the multi-source case. Exactly one of `url` / `urls` must be set; recipes that set both fail to build.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `url` | `Option<String>` | no | type default |  |
| `urls` | `Option<Vec<String>>` | no | type default |  |
| `resume` | `bool` | no | `default_true()` |  |

### `type = "web_crawl"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `seed_urls` | `Vec<String>` | **yes** | — |  |
| `link_pattern` | `String` | **yes** | — |  |
| `max_pages` | `usize` | no | `default_max_pages()` |  |

### `type = "http_api"`

Generic REST API acquirer. Replaces the never-implemented `api_paginated` stub with a real, recipe-author-friendly surface: parameterised URL templates, pagination strategies (offset / cursor / next-URL / page-number), JSONPath document-URL follow, rate limiting, custom headers / User-Agent. Combined with `[recipe.parameters]`, a domain expert can author a working recipe for SEC EDGAR / CourtListener / OpenAlex / PubMed / etc. without touching Rust. See [`crate::acquirers::http_api`].

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `base_url` | `String` | no | type default | Base URL — referenced via `{base_url}` in `requests[].url`, optional otherwise. Exists primarily so the recipe author doesn't repeat the same prefix in every template. |
| `requests` | `Vec<RequestTemplate>` | **yes** | — | One or more request templates. Each template may declare `for_each` to cross-product over named parameters declared in `[recipe.parameters]`. The acquirer issues one paginated request sequence per template × resolved `for_each` binding. |
| `pagination` | `Option<PaginationStrategy>` | no | type default | Pagination strategy. Absent = single-page request. |
| `follow` | `Option<FollowConfig>` | no | type default | Document-follow config. When present, the acquirer treats each page response as an *index* (a list of document URLs) and fetches the documents in parallel, writing them under `<acquired-dir>/docs/<sha>.<ext>` for the extractor. When absent, the page responses themselves are persisted. |
| `rate_limit_per_second` | `Option<f32>` | no | type default | Token-bucket rate limit, requests per second across all in-flight requests for this acquirer instance. None = no throttling. SEC requires ≤ 10 req/sec; OpenAlex recommends ≤ 10 req/sec with an email tag. |
| `user_agent` | `Option<String>` | no | type default | Override the default `CorpusEngine/0.1` User-Agent. Some APIs (SEC, GitHub) reject requests without a contact-bearing UA. |
| `headers` | `Option<BTreeMap<String, String>>` | no | type default | Extra HTTP headers (Authorization, Accept, etc.). Templated values may use `{name}` placeholders to reference recipe parameters (e.g. an API token). |

### `type = "local_file"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | `String` | **yes** | — |  |

### `type = "huggingface_dataset"`

Download all parquet shards for a public HuggingFace dataset. Uses the HF dataset API to enumerate shards, then downloads each with resume support, returning a directory of parquet files.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `repo` | `String` | **yes** | — | Dataset repo in `org/name` format, e.g. `"manu/project_gutenberg"`. |
| `subset` | `Option<String>` | no | type default | Optional subset prefix to filter shards, e.g. `"en"` matches filenames starting with `data/en-`. If absent, all parquet shards are downloaded. |
| `file_indices` | `Option<Vec<usize>>` | no | type default | Restrict ingestion to a specific subset of shard indices. Indices refer to position in the **sorted** manifest (ascending by filename). Both the coordinator and the peer must sort the same full manifest before slicing, so they agree on which file each index refers to. `None` = download all files (default; preserves existing behaviour). |

### `type = "custom"`

Runtime-registered acquirer. `kind` selects an implementation previously registered via [`CorpusEngine::register_acquirer`]; `params` is passed through unchanged so the implementation can deserialize its own config. Used by `KnowledgeView` so that DB-reading acquirers (SQLite, Postgres) can live outside the `corpus-engine` crate, which stays free of database dependencies.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `kind` | `String` | **yes** | — |  |
| `params` | `serde_json::Value` | no | type default |  |

## `SeMode`

Extraction shape for the Stack Exchange XML extractor. See the `StackExchangeXml` variant of [`ExtractorConfig`] for the contract.

Allowed values:

- `answer_only` — One `ExtractedDoc` per high-score answer with the question inlined. The reference shape — pair with the `breadth` recipe.
- `question_with_answers` — One `ExtractedDoc` per question, grouping up to `max_answers_per_question` top-scoring answers under a structured "Approach 1 / Approach 2" body. The knowledge shape — pair with the `passthrough` chunker and the `KnowledgeDensity` filter.

## `ExtractorConfig` (select with `type = "…"`)

### `type = "mediawiki_xml"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `namespace_filter` | `Vec<u32>` | no | `default_namespace_filter()` |  |
| `skip_redirects` | `bool` | no | `default_true()` |  |
| `decompress` | `Option<String>` | no | type default |  |

### `type = "stackexchange_xml"`

StackExchange XML data dump extractor. Supports two extraction shapes (`mode`): - [`SeMode::AnswerOnly`] (default — preserves the legacy placeholder behaviour): emit one `ExtractedDoc` per high-score answer with the question body inlined as `Q: … A (score N): …`. The single-answer reference shape — pair with the `breadth` recipe. - [`SeMode::QuestionWithAnswers`]: group up to `max_answers_per_question` top-scoring answers under each question and emit one `ExtractedDoc` per question. The full thread becomes the FTS-indexed `content`; a synthesized breadth summary (question title + first sentence of each answer) is placed in `embed_text` so the vector embedding captures the trade-off space without overflowing the embed model's context window. Pair with the `passthrough` chunker. Knowledge-density signals (answer count, score, length, closed status, tag list) are written to each grouped doc's `metadata` so the [`KnowledgeDensity`](crate::filters::FilterConfig) document filter can reject single-answer reference posts. Set `apply_to` on the filter to scope the cut to specific communities (e.g. `"stackoverflow.com"`) while letting smaller, already knowledge-dense sites pass through.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `min_score` | `i32` | no | `default_min_score()` | Minimum answer score to include (applies in both modes). Default 3 — community-validated answers, with one-line "just google it" noise excluded. |
| `mode` | `SeMode` | no | type default | Extraction mode. See `SeMode` for shape semantics. |
| `max_answers_per_question` | `usize` | no | `default_max_answers_per_question()` | In `QuestionWithAnswers` mode, cap answers grouped under each question (sorted by score, ties broken by post id). Past 5 answers, marginal trade-off coverage drops sharply while the document grows past the embed context window. |
| `min_answer_length` | `usize` | no | type default | Reject answers shorter than this many characters. Filters out one-line code snippets and "+1 to the above" noise that inflate scores without adding retrievable knowledge. Default 0 (no length floor). |
| `exclude_closed` | `bool` | no | `default_true()` | Skip questions whose `ClosedDate` attribute is non-empty (Stack Overflow marks duplicates / off-topic / opinion-based questions this way). Default true — closed posts are systematically less knowledge-dense. |
| `tag_filter` | `Option<Vec<String>>` | no | type default | Restrict to questions tagged with at least one of these tags. `None` (default) means no tag filter. Tags are matched case-insensitively. |

### `type = "jsonl"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `content_field` | `Option<String>` | no | type default |  |
| `title_field` | `Option<String>` | no | type default |  |
| `filter` | `Option<String>` | no | type default |  |
| `decompress` | `Option<String>` | no | type default |  |

### `type = "json"`

JSON-API extractor. Reads a single JSON file (typically the per-page response persisted by the `http_api` acquirer when `[acquire.follow]` is absent), runs `document_path` over it as JSONPath, and emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc) per matching object using `content_field` for the body text. See [`crate::extractors::json_api::JsonApiExtractor`].

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `document_path` | `String` | **yes** | — | JSONPath expression selecting the documents array. Common shapes: `$.results[*]`, `$.data.items[*]`, `$.hits.hits[*]._source`. |
| `content_field` | `String` | **yes** | — | Required: name of the field on each matched object that holds the document's full text. |
| `title_field` | `Option<String>` | no | type default |  |
| `url_field` | `Option<String>` | no | type default |  |
| `id_field` | `Option<String>` | no | type default |  |

### `type = "tabular_atoms"`

Deterministic tabular → typed-atom extractor for structured public datasets (e.g. the SF assessor parcel roll from DataSF's Socrata API). Reads the bare-array JSON the `http_api` acquirer persists and emits, per row: one chunk (a rendered, FTS-indexable line) AND — via the ingest flow — one atlas `Entity` atom whose declared numeric/string columns are recorded in `Entity::attributes`, the deterministic, cited substrate the LVT analytics sum over. No inference. Pair with `chunker = "passthrough"`. See [`crate::extractors::tabular_atoms`].

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `document_path` | `Option<String>` | no | type default | JSONPath selecting the row array. Defaults to `$[*]` (a bare top-level array, as Socrata returns); use `$.results[*]` for an enveloped response. |
| `id_column` | `String` | **yes** | — | Column whose value is each row's stable identity (e.g. `parcel_number`). Drives the atom id + canonical name and the chunk's `source_doc_id`. |
| `entity_type` | `Option<String>` | no | type default | Atom entity-type label (free-form; becomes `EntityType::Other(..)`). Defaults to `"row"`. |
| `numeric_attributes` | `Vec<String>` | no | type default | Columns parsed as numbers (string cells like `"172620.0"` are parsed) and stored as JSON numbers in `attributes`. |
| `string_attributes` | `Vec<String>` | no | type default | Columns kept verbatim as strings in `attributes`. |

### `type = "html"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `content_selector` | `Option<String>` | no | type default |  |
| `title_selector` | `Option<String>` | no | type default |  |

### `type = "html_sections"`

Section-aware HTML extractor: emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc) per regex-matched section per file. Use this when a domain expert (e.g. a financial journalist working with SEC filings) knows that the *interesting* text lives between specific headings — MD&A, related-party transactions, revenue disaggregation — and wants to ingest only those sections. When *no* section matches a file, the optional `fallback` block decides what to ingest (full document or first N characters). Without a fallback, the file is skipped. Misses are recorded in a sidecar `_section_misses.json` under the source directory so `sovereign recipe test` can surface "section X missed; nearby text: …; suggestion: …" for the recipe author. See [`SectionRule`] and [`FallbackRule`].

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `sections` | `Vec<SectionRule>` | **yes** | — |  |
| `fallback` | `Option<FallbackRule>` | no | type default |  |
| `title_selector` | `Option<String>` | no | type default |  |

### `type = "csv"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `content_column` | `String` | **yes** | — |  |
| `title_column` | `Option<String>` | no | type default |  |
| `delimiter` | `Option<char>` | no | type default |  |

### `type = "gutenberg_catalog"`

Project Gutenberg catalog CSV (`pg_catalog.csv`). Emits one `ExtractedDoc` per `Text` work, with content = catalog metadata block and `embed_text` = a vector-friendly summary. Pair with `chunker = "passthrough"` and a `[catalog]` block. See [`crate::extractors::gutenberg_catalog`].

_No fields._

### `type = "wikipedia_catalog"`

Wikipedia catalog — one chunk per article carrying title + abstract + section anchors. Pair with `chunker = "passthrough"`, `[corpus] kind = "catalog"`, and a `[catalog]` block whose `content_recipe` points at `wikipedia-article` for the per- article on-demand fetch. Source JSONL is produced offline by `sovereign-recipes/wikipedia-catalog/scripts/build_catalog.py` from the Wikimedia abstract dump.

_No fields._

### `type = "wikipedia_api_article"`

Per-article on-demand extractor for Wikipedia. Consumes the MediaWiki Action API JSON (`action=parse&prop=wikitext|sections| links|properties`) and emits one `ExtractedDoc` per article section with full `WikipediaChunkMetadata` — same shape as the bulk JSONL extractor produces, so fetched articles are indistinguishable from dump-extracted ones downstream (atlas link graph, section-typed retrieval, contested-marker classification all work identically).

_No fields._

### `type = "parquet"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `content_column` | `String` | **yes** | — |  |
| `label_column` | `Option<String>` | no | type default |  |
| `url_column` | `Option<String>` | no | type default | Optional column to use as the document URL (e.g. `"url"` in `wikimedia/wikipedia`). Populates search result source links. |
| `content_transform` | `Option<String>` | no | type default | Optional transform applied to the content column before chunking. `"openalex_inverted_index"` reconstructs text from OpenAlex's inverted-index JSON format (`{ "word": [pos1, pos2], ... }`). |

### `type = "plaintext"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `title_pattern` | `Option<String>` | no | type default |  |
| `strip_boilerplate` | `Option<String>` | no | type default |  |

### `type = "wikipedia_structured"`

Extractor for the `wikimedia/structured-wikipedia` HuggingFace dataset in its parquet form. For the ZIP+JSONL form (the default distribution), use `WikipediaJsonl` instead.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `title_column` | `String` | no | `default_title_column()` |  |
| `url_column` | `String` | no | `default_url_column()` |  |
| `controversy_patterns` | `Vec<String>` | no | `default_controversy_patterns()` |  |
| `factual_patterns` | `Vec<String>` | no | `default_factual_patterns()` |  |
| `structural_signals` | `bool` | no | `default_true()` |  |

### `type = "wikipedia_jsonl"`

Extractor for the `wikimedia/structured-wikipedia` dataset in its actual distribution format: a ZIP archive containing a JSONL file. Produces one `ExtractedDoc` per section with full `WikipediaChunkMetadata` (section type, revision ID, Wikidata QID, page ID, outgoing links).

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `controversy_patterns` | `Vec<String>` | no | `default_controversy_patterns()` |  |
| `factual_patterns` | `Vec<String>` | no | `default_factual_patterns()` |  |
| `article_range` | `Option<(u64, u64)>` | no | type default | Restrict processing to articles `[start, end)` in the JSONL. Set by the collaborative ingestion planner to partition the single-file Wikipedia JSONL across mesh nodes. `None` = all. |
| `shard_indices` | `Option<Vec<usize>>` | no | type default | Restrict processing to a specific set of **logical** shard indices over the ZIP's canonical JSONL entries (as produced by [`crate::engine::canonical_jsonl_shard_entries`], which filters out `__MACOSX/` and `._*` resource-fork junk). Set by the collaborative-ingestion planner for multi-shard JSONL corpora such as Wikipedia (76 shards). Mutually exclusive with `article_range` — the sharded path streams directly from the ZIP and skips the merged-JSONL cache. |

### `type = "code"`

Tree-sitter code extractor. Walks the source directory, parses each supported file with its grammar, and yields one `ExtractedDoc` per symbol (function, class, struct, etc.). Requires the `treesitter` Cargo feature on `corpus-engine`.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `context_lines` | `usize` | no | `default_code_context_lines()` |  |
| `max_lines_per_chunk` | `usize` | no | `default_code_max_lines()` |  |

### `type = "markdown"`

Section-aware markdown extractor. Walks a single `.md` file (or a directory of them) and yields one `ExtractedDoc` per heading-bounded section. Each chunk carries [`crate::extractors::markdown_types::MarkdownChunkMetadata`] (section_path, section_depth, heading_anchor, outgoing_links, inline_code_spans). Used by the narrative-stream branch of the two-stream atlas pipeline (CHARTER, ARCH_PRINCIPLES, ADRs, accepted spec.md files). Requires the `markdown` Cargo feature.

_No fields._

### `type = "custom"`

Runtime-registered per-file extractor. The engine walks `source_path` collecting files with `extension`, then calls a closure registered via [`CorpusEngine::register_extractor`](crate::engine::CorpusEngine::register_extractor) on each. Used by recipes whose source format requires a heavy dep (pdf-extract, lopdf, …) that corpus-engine declines to bundle. `sovereign-tools` registers `"pdf"` at daemon startup. Ingest fails loudly if no extractor is registered for `kind` — the operator gets a clear "register before install" message rather than a silent empty corpus.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `kind` | `String` | **yes** | — | Key the engine looks up in its custom-extractor map. |
| `extension` | `String` | **yes** | — | File extension to walk (case-insensitive, no leading dot: `"pdf"`, `"epub"`, …). |
| `params` | `serde_json::Value` | no | type default | Unstructured params forwarded to the closure's bookkeeping layer if needed (currently unused — reserved for per-recipe PDF settings like `ocr_fallback: true`). |

### `type = "email"`

Architecture-over-Enron Phase 2: RFC-5322 / MIME email extractor. Walks `source_path` recursively (maildir layout, raw `.eml` files), parses each through `mailparse`, and emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc) per message. Metadata carries the parsed headers + a `thread_id` derived from In-Reply-To / References. When the engine has an [`crate::asset_store::AssetStore`] + an [`crate::extractors::described_asset::AssetSubExtractorRegistry`] installed (the default after Phase 1), attachments dispatch through the described-asset substrate — raw bytes + parsed caches + Asset atom + Attaches edge land per attachment.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_body_bytes` | `usize` | no | `default_email_max_body_bytes()` | Cap on per-message body bytes after MIME decoding. Long- tail bodies (200MB HTML newsletters) get truncated; the extractor sets a `body_was_truncated` flag in metadata. |
| `max_attachment_bytes` | `u64` | no | type default | Per-attachment byte cap fed into the described-asset dispatcher. `0` = use the dispatcher's default. |

### `type = "described_asset"`

Architecture-over-Enron AD-3: the described-asset dispatcher. Walks `source_path` (one mixed-binary folder), hashes each file, picks a sub-extractor from the engine's [`AssetSubExtractorRegistry`](crate::extractors::described_asset::AssetSubExtractorRegistry) by magic-bytes / extension, and emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc) per asset whose `content` is the description prose (always present — opaque-fallback at worst). The dispatcher writes raw bytes + optional typed parsed form to the engine's [`AssetStore`](crate::asset_store::AssetStore) and pre-forms the `Asset` atom + `Attaches` edge into the atlas sidecar so the next atlas write picks them up. Defaults: `xlsx` + `docx` + `plaintext` + `opaque` sub- extractors registered in-tree. `sovereign-tools` registers `pdf` at daemon startup the same way it does for the `Custom` PDF extractor today.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_bytes_per_asset` | `u64` | no | `default_described_asset_max_bytes()` | Maximum bytes the dispatcher will load into RAM per asset. Larger files fall through to the opaque fallback (no double-counting of GiB-scale videos). Defaults to 64 MiB. |

### `type = "xml_sections"`

Section-aware XML extractor. Walks a directory of `.xml` files and emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc) per element whose **local-name** matches `element`. Namespace-agnostic on purpose so USLM 1.x and USLM 2.0 (different namespace URLs, same `<section>` semantics) both round-trip through the same recipe. `title_attr` reads a title off the matched element (e.g. `identifier` on USLM sections yields titles like `/us/usc/t15/s1`). See [`crate::extractors::xml_sections::XmlSectionsExtractor`].

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `element` | `String` | **yes** | — | Local-name of the element whose body becomes one `ExtractedDoc`. |
| `title_attr` | `Option<String>` | no | type default | Optional attribute (local-name) on the matched element used as the document title. |

### `type = "alignment_workspace"`

Walks the user's `~/.claude/plans/` and `~/.claude/projects/-Users-*/memory/` trees plus `~/.claude/plans/_TEMPLATE.md`, yielding one `ExtractedDoc` per `.md` file with `source_id` set to the path relative to `~/.claude/`. Pairs with `mutable_merge = "source_doc_id_newest_mtime"` so two daemons editing the same memory or plan file converge on the newer copy after a mesh merge. The acquirer points at `~/.claude` (resolved by the `local_file` path-shape); the extractor handles its own directory walk for the canonical subset.

_No fields._

### `type = "anthropic_export"`

Anthropic claude.ai chat-export extractor. Parses the `conversations.json` file produced by claude.ai's "Export data" download and emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc) per conversation (`source_id = conv_uuid`) with content rendered as a sequence of `### [YYYY-MM-DD HH:MM] {user|assistant}` turn blocks. Empty conversations and non-text content blocks are dropped; messages flatten by `created_at` (branch handling via `parent_message_uuid` is a v2 concern). Pair with [`ChunkerConfig::ThreadedTurns`] so each retrieval unit is a user-question + assistant-reply pair. See [`crate::extractors::anthropic_export::AnthropicExportExtractor`].

_No fields._

### `type = "chatgpt_export"`

OpenAI ChatGPT chat-export extractor. Parses the `conversations.json` file produced by ChatGPT's "Export data" download and emits one [`ExtractedDoc`](crate::extractors::ExtractedDoc) per conversation (`source_id = conversation_id`) with content rendered as the *same* `### [YYYY-MM-DD HH:MM] {user|assistant}` turn blocks as [`ExtractorConfig::AnthropicExport`]. Unlike the Anthropic flat list, ChatGPT stores messages as a `mapping` tree; the extractor reconstructs the current thread by walking `parent` pointers up from `current_node`. Private-Use-Area inline markers (entity/url annotations) are cleaned to readable text. Pair with [`ChunkerConfig::ThreadedTurns`]. See [`crate::extractors::chatgpt_export::ChatgptExportExtractor`].

_No fields._

## `ChunkerConfig` (select with `type = "…"`)

### `type = "paragraph"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_chars` | `usize` | no | `default_max_chunk_chars()` |  |
| `overlap_chars` | `usize` | no | `default_overlap_chars()` |  |

### `type = "sentence"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_chars` | `usize` | no | `default_max_chunk_chars()` |  |

### `type = "fixed"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_chars` | `usize` | no | `default_max_chunk_chars()` |  |
| `overlap_chars` | `usize` | no | `default_overlap_chars()` |  |

### `type = "semantic"`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_chars` | `usize` | no | `default_max_chunk_chars()` |  |

### `type = "passthrough"`

Emits the input text as a single chunk. Use when the extractor already produces chunk-sized output (e.g. the `code` extractor).

_No fields._

### `type = "portal_event_bullet"`

One chunk per `*`-prefixed bullet on a `Portal:Current_events` page. Sub-bullets fold under their parent. Used by the `wikipedia-newsworthy` recipe so each event is its own retrieval unit.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_chars` | `usize` | no | `default_portal_bullet_max_chars()` |  |

### `type = "threaded_turns"`

Chunker for chat-transcript content rendered by the `anthropic_export` extractor (or any future extractor that emits `### [YYYY-MM-DD HH:MM] {user|assistant}` turn blocks). Groups each user turn with the immediately-following assistant reply into one chunk; dangling user turns and leading assistant turns become standalone chunks. Preserves turn headers in chunk content so downstream phases can read timestamps + first-person signals (meta-atlas trace axis) and so the plain-text chunk reads naturally in retrieval surfaces. Per-span authorship is surfaced through [`crate::chunkers::threaded_turns::AttributedChunk`] for code paths that consume attribution (atlas extraction, attribution-filtered retrieval, bench scoring).

_No fields._

## `IndexConfig`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `fts` | `bool` | no | `default_true()` |  |
| `vector` | `bool` | no | `default_true()` |  |
| `embedding_model` | `String` | no | `default_embedding_model()` |  |
| `embedding_dimensions` | `usize` | no | `default_embedding_dimensions()` |  |

## `ComposeMode`

Allowed values:

- `any` — Accept if any child filter accepts. Default — matches the "Wikipedia Core = top-ranked OR vital" semantics.
- `all` — Accept only when every child filter accepts.

## `FilterConfig` (select with `type = "…"`)

One entry from a recipe's `[[filter]]` array.

### `type = "pageview_rank"`

Accept articles whose normalized title appears in a pageview-rank CSV with rank ≤ `max_rank`. The CSV is a two-column `title,rank` table.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `rank_file` | `String` | **yes** | — | Either a bundled-asset key (`@bundled:pageview_ranks_202311`) or a path relative to the recipe override directory. |
| `max_rank` | `u32` | **yes** | — |  |

### `type = "title_list"`

Accept articles whose normalized title appears in a newline-delimited title list. Useful for curated sets like Wikipedia Vital Articles.

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `list_file` | `String` | **yes** | — | Either a bundled-asset key (`@bundled:vital_articles_l5`) or a path relative to the recipe override directory. |

### `type = "knowledge_density"`

Accept Stack Exchange grouped Q&A docs (one doc per question) only when their answer set carries enough density to count as a trade-off thread rather than a single-answer reference post. See [`crate::filters::KnowledgeDensityConfig`] for fields.

_No fields._

### `type = "boilerplate"`

Reject email-shaped docs that are reduced to nothing after boilerplate (signatures, quoted-reply, corporate disclaimers) is stripped. See [`crate::filters::boilerplate::BoilerplateConfig`]. Per-recipe configurable so corpora with code-in-mail or non-Outlook clients can tune their strip behaviour.

_No fields._

## `BoilerplateConfig`

Per-recipe configuration for the boilerplate filter. Each detection axis can be disabled independently — useful for corpora where the "reply quote" lines aren't quoted prefixes (Outlook's "On Date X wrote:" pattern), or where signature-block heuristics produce false positives (e.g. code in monospace mail).

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `strip_signatures` | `bool` | no | `default_true_bool()` | Strip `-- ` -prefixed signature blocks (RFC 3676 §4.3.2) and strong heuristic siblings ("Sent from my iPhone", "Best regards,\n<name>"). |
| `strip_quoted_replies` | `bool` | no | `default_true_bool()` | Strip RFC 3676 §4.5 quoted-reply blocks — lines starting with `>` (one or more). |
| `strip_disclaimers` | `bool` | no | `default_true_bool()` | Strip common corporate-disclaimer trailers ("This email and any files transmitted with it…"). |
| `min_body_chars_after_strip` | `usize` | no | `default_min_body_chars_after_strip()` | Reject docs whose body becomes shorter than this many chars after stripping. Default 20 — anything shorter is empty for retrieval purposes. |

## `StripReport`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `quoted_reply_lines_removed` | `usize` | **yes** | — |  |
| `signature_lines_removed` | `usize` | **yes** | — |  |
| `disclaimer_lines_removed` | `usize` | **yes** | — |  |

## `KnowledgeDensityConfig`

| TOML key | Type | Required | Default | Description |
|---|---|---|---|---|
| `min_substantive_answers` | `u32` | no | `default_min_substantive_answers()` | Minimum number of answers (after the score/length floors) that must survive on the question for it to be accepted. The whole point of this filter — single-answer threads are the reference shape, three+ answer threads are the trade-off shape. |
| `answer_score_threshold` | `i32` | no | `default_answer_score_threshold()` | Score floor for an answer to count toward `min_substantive_answers`. Mirrors the extractor's `min_score`; restated here so a recipe can ratchet the density check tighter than the extraction cut (e.g. extract at score ≥ 3 but require density at score ≥ 5). |
| `min_answer_length` | `u64` | no | `default_min_answer_length()` | Length floor for an answer to count. Eliminates one-line "+1 to the above" / "use sorted()" snippets that inflate answer count without adding retrievable knowledge. |
| `exclude_closed` | `bool` | no | `default_true()` | Reject questions whose `closed` metadata flag is true. Stack Overflow's closed-question moderation flag is a high-precision signal that the community judged the thread off-topic / duplicate / opinion-based — even if it has multiple answers, the answer set tends not to be a coherent trade-off space. |
| `tag_filter` | `Option<Vec<String>>` | no | type default | Optional tag whitelist — accept only questions tagged with at least one listed tag. Use to scope the cut to architecture / design discussions on Stack Overflow while letting smaller already-knowledge-dense sites pass everything. |
| `apply_to` | `Option<Vec<String>>` | no | type default | Optional community whitelist — apply the density check only on these communities. Documents from communities not listed are accepted regardless. This is the recipe-level escape hatch that lets a single recipe combine breadth-pass sources with density-cut sources. `None` (default) applies to every community. |

