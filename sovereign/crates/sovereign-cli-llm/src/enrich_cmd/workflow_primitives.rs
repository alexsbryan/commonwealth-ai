// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reusable workflow primitives for composing the LLM enrichment phases — the
//! ops that pass the reuse test (≥2 consumers), as opposed to per-phase
//! compose/parse logic (one consumer each, so not primitives).
//!
//! `exemplar_select` is the one genuinely-new retrieval primitive: the few-shot
//! exemplar selection that **both** extract (Phase 1) and name (Phase 3) use. It
//! reuses the pipeline's own `ExemplarBank::load_embedded` + `select_top_k`
//! verbatim — the refined selection, not basic RAG — so a workflow-composed phase
//! picks exactly the exemplars the bespoke runner would.

use corpus_engine::enrichment::atlas::analysis::AtlasSummary;
use corpus_engine::enrichment::pipeline::{
    assemble_phase_output,
    atlas::{SectionExtraction, SeedEntities},
    AtlasCluster, ChapterInput, ChatPrompt, Exemplar, ExemplarBank, Facet, Phase1Output,
    Phase2AtlasOutput, PhaseCache, PipelinePhase, PipelineRegistry, SketchExcerpt,
};

use super::atlas_configuration::{build_atlas_summary, finalize_configurations};
use super::atlas_phase_cmd::render_excerpts;
use corpus_engine::enrichment::atlas::analysis::configuration::Phase8ParseItem;
use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

/// Parse a facet string (the Phase-3 `name` selector) into a `Facet`.
fn parse_facet(s: Option<&str>) -> Result<Facet> {
    match s {
        Some("question") => Ok(Facet::Question),
        Some("claim") => Ok(Facet::Claim),
        Some("entity_state") => Ok(Facet::EntityState),
        Some("relation_state") => Ok(Facet::RelationState),
        Some("event") => Ok(Facet::Event),
        _ => Err(Error::Execution(
            "a `name` phase needs a `facet` (question|claim|entity_state|relation_state|event)"
                .into(),
        )),
    }
}

/// Deserialize a named sub-field of a composite `input` object into `T`
/// (defaulting a missing field to JSON null so `Option`/`Vec` fields are happy).
fn input_field<T: serde::de::DeserializeOwned>(
    input: &serde_json::Value,
    field: &str,
    what: &str,
) -> Result<T> {
    let v = input.get(field).cloned().unwrap_or(serde_json::Value::Null);
    serde_json::from_value(v)
        .map_err(|e| Error::Execution(format!("pipeline_compose: {what} `input.{field}`: {e}")))
}

use super::config::EnrichConfig;
use super::corpus_io::rebuild_corpus_state;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use sovereign_core::tool_manifest::DeclaredTool;
use std::sync::Arc;

/// `atlas_chapters` — the chapter-input prep: split the corpus's pinned source
/// into `ChapterInput`s via the configured `chapter_regex`, exactly as the
/// bespoke `enrich` path does (reusing `rebuild_corpus_state`), and emit them as
/// a collection for a `for_each` extract. Faithful: the workflow sees the same
/// chapters the bespoke pipeline does. `Read` effect.
pub(crate) struct AtlasChaptersTool;

impl AtlasChaptersTool {
    /// Bind this tool's state to its `atlas_chapters` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_chapters", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_chapters`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let cfg = EnrichConfig::require(&corpus)
            .map_err(|e| Error::Execution(format!("atlas_chapters: config: {e}")))?;
        let (chapters, _manifest) = rebuild_corpus_state(&cfg)
            .map_err(|e| Error::Execution(format!("atlas_chapters: rebuild corpus state: {e}")))?;
        let json = serde_json::to_value(&chapters)
            .map_err(|e| Error::Execution(format!("atlas_chapters: serialize: {e}")))?;
        Ok(StepOutput::Json(json))
    }
}

/// `atlas_seed` — the corpus's cached Stage-1a seed (the canonical-entity list),
/// loaded exactly as the bespoke runner does (`PhaseCache::read(SeedExtraction)`,
/// runner.rs:750). Symmetric with `atlas_chapters`: a corpus-state loader that
/// feeds the pure `pipeline_compose` adapter, so a workflow-composed Phase 1
/// threads the same seed the bespoke path does. Returns the seed object, or JSON
/// `null` when the corpus has no seed (compose then falls through to the seedless
/// prompt — identical to the runner's cache-miss path). `Read` effect.
pub(crate) struct AtlasSeedTool;

impl AtlasSeedTool {
    /// Bind this tool's state to its `atlas_seed` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_seed", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_seed`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let cache = PhaseCache::new(paths::cache_dir(&corpus));
        let seed: Option<SeedEntities> = cache
            .read(PipelinePhase::SeedExtraction)
            .map_err(|e| Error::Execution(format!("atlas_seed: read seed cache: {e}")))?;
        match seed {
            Some(s) => {
                let json = serde_json::to_value(&s)
                    .map_err(|e| Error::Execution(format!("atlas_seed: serialize: {e}")))?;
                Ok(StepOutput::Json(json))
            }
            None => Ok(StepOutput::Json(serde_json::Value::Null)),
        }
    }
}

fn str_param(params: &serde_json::Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::Execution(format!("missing required `{key}`")))
}

/// `atlas_clusters` — the corpus's cached Phase-2 atlas clusters (`AtlasCluster[]`,
/// each `{id, facet, refs}`), as a collection for a `for_each` name pass. Reads
/// the same `AtlasClusters` cache the bespoke facet-naming loop reads. Symmetric
/// with `atlas_chapters`: a corpus-state loader feeding the pure compose adapter.
/// `Read` effect.
pub(crate) struct AtlasClustersTool;

impl AtlasClustersTool {
    /// Bind this tool's state to its `atlas_clusters` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_clusters", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_clusters`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let cache = PhaseCache::new(paths::cache_dir(&corpus));
        let phase2: Option<Phase2AtlasOutput> = cache
            .read(PipelinePhase::AtlasClusters)
            .map_err(|e| Error::Execution(format!("atlas_clusters: read cluster cache: {e}")))?;
        match phase2 {
            Some(p) => {
                let json = serde_json::to_value(&p.clusters)
                    .map_err(|e| Error::Execution(format!("atlas_clusters: serialize: {e}")))?;
                Ok(StepOutput::Json(json))
            }
            None => Err(Error::Execution(format!(
                "atlas_clusters: no atlas-clusters cache for `{corpus}` — run the cluster phase first"
            ))),
        }
    }
}

/// `atlas_cluster_excerpts` — render one cluster's refs into its per-facet
/// `SketchExcerpt`s, plus the joined `query_text` used to score name exemplars.
/// Reuses the bespoke `render_excerpts` verbatim against the corpus's Phase-1
/// section map (`chapter_id → SectionExtraction`), so a workflow name pass feeds
/// the compose adapter exactly the excerpts the bespoke facet-naming loop builds.
/// `Read` effect.
pub(crate) struct AtlasClusterExcerptsTool;

impl AtlasClusterExcerptsTool {
    /// Bind this tool's state to its `atlas_cluster_excerpts` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_cluster_excerpts", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_cluster_excerpts`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let cluster: AtlasCluster = serde_json::from_value(
            params
                .get("cluster")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| {
            Error::Execution(format!(
                "atlas_cluster_excerpts: `cluster` is not an AtlasCluster: {e}"
            ))
        })?;

        let cache = PhaseCache::new(paths::cache_dir(&corpus));
        let phase1: Phase1Output = cache
            .read(PipelinePhase::Questions)
            .map_err(|e| {
                Error::Execution(format!("atlas_cluster_excerpts: read questions cache: {e}"))
            })?
            .ok_or_else(|| {
                Error::Execution(format!(
                    "atlas_cluster_excerpts: no questions cache for `{corpus}` — run extract first"
                ))
            })?;
        // The chapter_id → SectionExtraction map, exactly as the bespoke name cmd builds it.
        let sections: std::collections::HashMap<String, SectionExtraction> = phase1
            .questions_by_chapter
            .into_iter()
            .filter_map(|c| c.section_extraction.map(|se| (c.chapter_id, se)))
            .collect();

        let excerpts = render_excerpts(&cluster, &sections);
        let query_text = excerpts
            .iter()
            .map(|e| e.content.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        Ok(StepOutput::Json(serde_json::json!({
            "excerpts": excerpts,
            "query_text": query_text,
        })))
    }
}

/// `pipeline_assemble` — the general **assemble adapter**: wrap a `for_each`
/// parse step's atoms into the canonical phase-output struct via the domain's
/// `assemble_phase_output` (the single source of truth, which stamps `written_at`
/// and assigns any phase ids exactly as the runner does). ONE tool, phase-as-data
/// — the persist-side counterpart to compose/parse, so no per-phase
/// `transform:json` envelope hardcodes a domain struct shape in TOML and no domain
/// type is loosened to accept a hand-built lookalike. `Read` (pure).
pub(crate) struct PipelineAssembleTool;

impl PipelineAssembleTool {
    /// Bind this tool's state to its `pipeline_assemble` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("pipeline_assemble", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `pipeline_assemble`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let pipeline_id = str_param(params, "pipeline")?;
        let phase_str = str_param(params, "phase")?;
        let phase = match phase_str.as_str() {
            "questions" | "extract" => PipelinePhase::Questions,
            "name" | "atlas-named-clusters" => PipelinePhase::AtlasNamedClusters,
            other => {
                return Err(Error::Execution(format!(
                    "pipeline_assemble: phase `{other}` not wired (questions|name)"
                )))
            }
        };
        let atoms = params
            .get("atoms")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let out = assemble_phase_output(&pipeline_id, phase, atoms)
            .map_err(|e| Error::Execution(format!("pipeline_assemble: {e}")))?;
        Ok(StepOutput::Json(out))
    }
}

/// `atlas_summary` — load the resolved atlas + summarise it for the Phase-8
/// configure prompt (reuses the bespoke `build_atlas_summary` → the same
/// AtlasSummary the bespoke cmd builds). Feeds `pipeline_compose` (configure).
/// `Read` effect.
pub(crate) struct AtlasSummaryTool;

impl AtlasSummaryTool {
    /// Bind this tool's state to its `atlas_summary` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_summary", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_summary`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let summary = build_atlas_summary(&corpus)
            .map_err(|e| Error::Execution(format!("atlas_summary: {e}")))?;
        let json = serde_json::to_value(&summary)
            .map_err(|e| Error::Execution(format!("atlas_summary: serialize: {e}")))?;
        Ok(StepOutput::Json(json))
    }
}

/// `atlas_write_configurations` — finalize the Phase-8 parse items: validate them
/// against the atlas and merge the Configuration atoms into `configurations.json`
/// + `atoms.json` (reuses the bespoke `finalize_configurations` → `parse_configurations`
/// + `write_atlas_full` verbatim). The configure phase's write leaf — the merge is
/// domain I/O, not a clean envelope. Returns `{configurations: <count>}`. `Write`.
pub(crate) struct AtlasWriteConfigurationsTool;

impl AtlasWriteConfigurationsTool {
    /// Bind this tool's state to its `atlas_write_configurations` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atlas_write_configurations", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_write_configurations`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = str_param(params, "corpus")?;
        let items: Vec<Phase8ParseItem> = serde_json::from_value(
            params
                .get("items")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| {
            Error::Execution(format!(
                "atlas_write_configurations: `items` is not [Phase8ParseItem]: {e}"
            ))
        })?;
        let configurations = finalize_configurations(&corpus, items)
            .map_err(|e| Error::Execution(format!("atlas_write_configurations: {e}")))?;
        Ok(StepOutput::Json(
            serde_json::json!({ "configurations": configurations.len() }),
        ))
    }
}

/// `exemplar_select` — embed a query and pick the top-K few-shot exemplars from
/// the corpus's bank for a phase. Reused by extract + name. `Read` effect (needs
/// the daemon to embed the query + the bank); returns the selected exemplars as a
/// JSON array, which the phase's compose step renders into its prompt.
pub(crate) struct ExemplarSelectTool;

impl ExemplarSelectTool {
    /// Bind this tool's state to its `exemplar_select` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("exemplar_select", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `exemplar_select`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus = params
            .get("corpus")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("exemplar_select: missing required `corpus`".into()))?;
        let phase_str = params
            .get("phase")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("exemplar_select: missing required `phase`".into()))?;
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("exemplar_select: missing required `query`".into()))?;
        let k = params.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

        let phase = match phase_str {
            "questions" | "extract" => PipelinePhase::Questions,
            "atlas-named-clusters" | "name" => PipelinePhase::AtlasNamedClusters,
            other => {
                return Err(Error::Execution(format!(
                    "exemplar_select: unsupported phase `{other}` (questions|atlas-named-clusters)"
                )))
            }
        };

        let cfg = EnrichConfig::require(corpus)
            .map_err(|e| Error::Execution(format!("exemplar_select: config: {e}")))?;
        let bank_path = paths::exemplars_dir(&cfg.corpus_id).join(format!("{}.json", phase.id()));

        // The bank scores exemplars against the query by embedding — build the
        // same daemon embed closure the bespoke runner uses.
        let client = DaemonInferenceClient::from_enrich_config(&cfg)
            .map_err(|e| Error::Execution(format!("exemplar_select: daemon client: {e}")))?;
        let (embed, _chat) = client.into_closures();

        let bank = ExemplarBank::load_embedded(&bank_path, phase, &embed)
            .await
            .map_err(|e| {
                Error::Execution(format!(
                    "exemplar_select: load bank {}: {e}",
                    bank_path.display()
                ))
            })?;
        if bank.is_empty() {
            return Ok(StepOutput::Json(serde_json::Value::Array(vec![])));
        }
        let query_emb = embed(query)
            .await
            .map_err(|e| Error::Execution(format!("exemplar_select: embed query: {e}")))?;
        // The name phase filters by the cluster's facet (select_top_k_facet);
        // extract and a facet-less name both fall through to unfiltered select.
        let facet = params.get("facet").and_then(|v| v.as_str());
        let picked = bank.select_top_k_facet(&query_emb, k, facet);
        let json = serde_json::to_value(&picked)
            .map_err(|e| Error::Execution(format!("exemplar_select: serialize exemplars: {e}")))?;
        Ok(StepOutput::Json(json))
    }
}

/// `pipeline_parse` — the general **parse adapter**: resolve a pipeline by id and
/// route a model `response` to its `parse_<phase>` method (the bespoke parser +
/// post-processing, the single source of truth), returning the parsed result as
/// JSON. ONE tool reused by every phase (phase is data) — the reuse-bearing way
/// to invoke the per-phase parse from a workflow, instead of N phase tools. Pure
/// (`Read`, no corpus, no daemon), so it's hermetically testable.
pub(crate) struct PipelineParseTool;

impl PipelineParseTool {
    /// Bind this tool's state to its `pipeline_parse` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("pipeline_parse", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `pipeline_parse`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let pipeline_id = str_param(params, "pipeline")?;
        let phase = str_param(params, "phase")?;
        let response = str_param(params, "response")?;
        let pipeline = PipelineRegistry::builtin()
            .get(&pipeline_id)
            .ok_or_else(|| {
                Error::Execution(format!("pipeline_parse: unknown pipeline `{pipeline_id}`"))
            })?;

        let to_value = |v: std::result::Result<serde_json::Value, serde_json::Error>| {
            v.map_err(|e| Error::Execution(format!("pipeline_parse: serialize: {e}")))
        };
        let err = |p: &str, e: corpus_engine::error::Error| {
            Error::Execution(format!("pipeline_parse: {p}: {e}"))
        };

        let parsed = match phase.as_str() {
            "seed" => to_value(serde_json::to_value(
                pipeline
                    .parse_seed_response(&response)
                    .map_err(|e| err("seed", e))?,
            ))?,
            "questions" | "extract" => to_value(serde_json::to_value(
                pipeline
                    .parse_phase1(&response)
                    .map_err(|e| err("questions", e))?,
            ))?,
            "tensions" | "classify" => to_value(serde_json::to_value(
                pipeline
                    .parse_phase6(&response)
                    .map_err(|e| err("tensions", e))?,
            ))?,
            "configure" => to_value(serde_json::to_value(
                pipeline
                    .parse_phase8_configuration(&response)
                    .map_err(|e| err("configure", e))?,
            ))?,
            "name" | "atlas-named-clusters" => {
                let facet = parse_facet(params.get("facet").and_then(|v| v.as_str()))?;
                to_value(serde_json::to_value(
                    pipeline
                        .parse_phase3_facet(facet, &response)
                        .map_err(|e| err("name", e))?,
                ))?
            }
            other => {
                return Err(Error::Execution(format!(
                    "pipeline_parse: unsupported phase `{other}`"
                )))
            }
        };
        Ok(StepOutput::Json(parsed))
    }
}

/// `pipeline_compose` — the general **compose adapter**: resolve a pipeline by id
/// and route a phase's typed `input` (deserialized from JSON) to its
/// `compose_<phase>` method, returning the exact bespoke prompt as
/// `{system, user, schema}` for a `model:` step. ONE tool reused by every phase —
/// the reuse-bearing way to invoke per-phase prompt-building from a workflow,
/// keeping the refined render in the pipeline (source of truth, no divergence).
pub(crate) struct PipelineComposeTool;

impl PipelineComposeTool {
    /// Bind this tool's state to its `pipeline_compose` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("pipeline_compose", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `pipeline_compose`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let pipeline_id = str_param(params, "pipeline")?;
        let phase = str_param(params, "phase")?;
        let input = params
            .get("input")
            .ok_or_else(|| Error::Execution("pipeline_compose: missing required `input`".into()))?;
        let pipeline = PipelineRegistry::builtin()
            .get(&pipeline_id)
            .ok_or_else(|| {
                Error::Execution(format!(
                    "pipeline_compose: unknown pipeline `{pipeline_id}`"
                ))
            })?;

        let prompt: ChatPrompt = match phase.as_str() {
            "seed" => {
                let chapter: ChapterInput = serde_json::from_value(input.clone()).map_err(|e| {
                    Error::Execution(format!("pipeline_compose: seed `input` (a chapter): {e}"))
                })?;
                pipeline.compose_seed_prompt(&chapter).ok_or_else(|| {
                    Error::Execution("pipeline_compose: pipeline produced no seed prompt".into())
                })?
            }
            "configure" => {
                let summary: AtlasSummary = serde_json::from_value(input.clone()).map_err(|e| {
                    Error::Execution(format!(
                        "pipeline_compose: configure `input` (a summary): {e}"
                    ))
                })?;
                pipeline
                    .compose_phase8_configuration(&summary, &[])
                    .ok_or_else(|| {
                        Error::Execution(
                            "pipeline_compose: pipeline produced no configuration prompt".into(),
                        )
                    })?
            }
            // Extract: `input = { chapter, exemplars, seed? }` — the chapter, the
            // exemplar-select output, and the optional Stage-1a seed list.
            "questions" | "extract" => {
                let chapter: ChapterInput = input_field(input, "chapter", "questions")?;
                let exemplars: Vec<Exemplar> = input_field(input, "exemplars", "questions")?;
                let seed: Option<SeedEntities> = match input.get("seed") {
                    Some(s) if !s.is_null() => Some(input_field(input, "seed", "questions")?),
                    _ => None,
                };
                let refs: Vec<&Exemplar> = exemplars.iter().collect();
                pipeline.compose_phase1_with_seed(&chapter, &refs, seed.as_ref())
            }
            // Name: `input = { cluster, excerpts, exemplars }` + a `facet`.
            "name" | "atlas-named-clusters" => {
                let cluster: AtlasCluster = input_field(input, "cluster", "name")?;
                let excerpts: Vec<SketchExcerpt> = input_field(input, "excerpts", "name")?;
                let exemplars: Vec<Exemplar> = input_field(input, "exemplars", "name")?;
                let facet = parse_facet(params.get("facet").and_then(|v| v.as_str()))?;
                let refs: Vec<&Exemplar> = exemplars.iter().collect();
                pipeline
                    .compose_phase3_facet(&cluster, facet, &excerpts, &refs)
                    .ok_or_else(|| {
                        Error::Execution(
                            "pipeline_compose: pipeline produced no phase-3 naming prompt".into(),
                        )
                    })?
            }
            other => {
                return Err(Error::Execution(format!(
                    "pipeline_compose: phase `{other}` not wired (seed|questions|name|configure)"
                )))
            }
        };

        Ok(StepOutput::Json(serde_json::json!({
            "system": prompt.system,
            "user": prompt.user,
            "schema": prompt.response_schema,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::traits::Tool;

    /// `pipeline_compose` routes a typed input to the bespoke compose, no daemon:
    /// a literary_atlas seed prompt from a constructed chapter yields a non-empty
    /// system + user.
    #[tokio::test]
    async fn pipeline_compose_routes_seed() {
        let chapter = serde_json::json!({
            "chapter_id": "sec_0001",
            "title": "Chapter I",
            "text": "Mr Verloc kept a shabby shop in a Soho street. ".repeat(20),
            "metadata": {},
            "approx_tokens": 120
        });
        let out = PipelineComposeTool
            .declared().execute(
                &serde_json::json!({ "pipeline": "literary_atlas", "phase": "seed", "input": chapter.clone() }),
                &ToolContext::default(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert!(v["system"].as_str().unwrap().len() > 10, "{v}");
                assert!(v["user"].as_str().unwrap().contains("Verloc"), "{v}");
            }
            o => panic!("unexpected: {o:?}"),
        }

        // Extract: composite input { chapter, exemplars } → compose_phase1.
        let out2 = PipelineComposeTool
            .declared()
            .execute(
                &serde_json::json!({
                    "pipeline": "literary_atlas",
                    "phase": "questions",
                    "input": { "chapter": chapter, "exemplars": [] }
                }),
                &ToolContext::default(),
            )
            .await
            .unwrap();
        match out2 {
            StepOutput::Json(v) => assert!(v["user"].as_str().unwrap().contains("Verloc"), "{v}"),
            o => panic!("unexpected: {o:?}"),
        }

        // Unknown pipeline + unwired phase are loud errors.
        assert!(PipelineComposeTool
            .declared()
            .execute(
                &serde_json::json!({ "pipeline": "no-such", "phase": "seed", "input": {} }),
                &ToolContext::default()
            )
            .await
            .is_err());
    }

    /// `pipeline_parse` routes to the bespoke parser by phase, no corpus/daemon:
    /// a literary_atlas Phase-8 response parses to a JSON configurations list,
    /// and an unknown pipeline / bad phase fail loudly.
    #[tokio::test]
    async fn pipeline_parse_routes_by_phase() {
        // Phase 8 with an empty-configurations response → an empty JSON array.
        let out = PipelineParseTool
            .declared()
            .execute(
                &serde_json::json!({
                    "pipeline": "literary_atlas",
                    "phase": "configure",
                    "response": "{\"configurations\": []}"
                }),
                &ToolContext::default(),
            )
            .await
            .unwrap();
        assert!(matches!(out, StepOutput::Json(_)), "{out:?}");

        // Unknown pipeline + unsupported phase are loud errors.
        assert!(PipelineParseTool
            .declared().execute(
                &serde_json::json!({ "pipeline": "no-such", "phase": "configure", "response": "{}" }),
                &ToolContext::default()
            )
            .await
            .is_err());
        assert!(PipelineParseTool
            .declared().execute(
                &serde_json::json!({ "pipeline": "literary_atlas", "phase": "bogus", "response": "{}" }),
                &ToolContext::default()
            )
            .await
            .is_err());
    }

    /// `atlas_chapters` validates its corpus before IO (the happy path reads the
    /// corpus's pinned source — exercised by the integration run).
    #[tokio::test]
    async fn atlas_chapters_validates_corpus() {
        assert!(AtlasChaptersTool
            .declared()
            .execute(&serde_json::json!({}), &ToolContext::default())
            .await
            .is_err());
        assert!(AtlasChaptersTool
            .declared()
            .execute(
                &serde_json::json!({ "corpus": "definitely-not-real-zzz" }),
                &ToolContext::default()
            )
            .await
            .is_err());
    }

    /// `atlas_seed` is a tolerant cache read (the runner's seed-miss path): a
    /// missing `corpus` is a loud error, but a corpus with no cached seed yields
    /// JSON null (→ the seedless prompt downstream), not a failure.
    #[tokio::test]
    async fn atlas_seed_missing_corpus_errors_absent_seed_is_null() {
        assert!(AtlasSeedTool
            .declared()
            .execute(&serde_json::json!({}), &ToolContext::default())
            .await
            .is_err());
        // A corpus with no seed cache → null (the canonical "no seed" signal).
        let out = AtlasSeedTool
            .declared()
            .execute(
                &serde_json::json!({ "corpus": "definitely-not-real-zzz" }),
                &ToolContext::default(),
            )
            .await
            .unwrap();
        assert!(
            matches!(out, StepOutput::Json(serde_json::Value::Null)),
            "{out:?}"
        );
    }

    /// Validates params before any IO (the happy path needs the daemon + a bank).
    #[tokio::test]
    async fn exemplar_select_validates_params() {
        assert!(ExemplarSelectTool
            .declared()
            .execute(
                &serde_json::json!({ "phase": "questions", "query": "x" }),
                &ToolContext::default()
            )
            .await
            .is_err()); // missing corpus
        assert!(ExemplarSelectTool
            .declared()
            .execute(
                &serde_json::json!({ "corpus": "x", "phase": "bogus", "query": "y" }),
                &ToolContext::default()
            )
            .await
            .is_err()); // bad phase
        assert!(ExemplarSelectTool
            .declared().execute(
                &serde_json::json!({ "corpus": "definitely-not-real-zzz", "phase": "questions", "query": "y" }),
                &ToolContext::default()
            )
            .await
            .is_err()); // unknown corpus
    }
}
