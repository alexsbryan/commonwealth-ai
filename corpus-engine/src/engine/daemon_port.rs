// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's side of svrn's daemon port: the bodies that project or write the
//! engine's own types, moved here from the daemon so the daemon reaches them
//! through the port instead of naming the engine (pb-ingest-dial-daemon-ports,
//! phase-b-43).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine_yield::{ForegroundSignal, YieldHook};
use corpus_index::index::CorpusIndex;
use corpus_index::ingest_port::cancel::CancellationRegistry;
use corpus_index::ingest_port::daemon::{
    ArticleStats, CorpusDiskStatus, IngestPort, IngestResult, InstallRefusal,
    NewsworthyHostFactory, PreparedInstall, RecipeSharing, RecipeVocabulary, RegistryListing,
    SourceFileRecord,
};
use corpus_index::ingest_port::ProgressCallback;
use oicp_types::{RecipeStageReport, RecipeTestReport};
use sovereign_contracts::daemon_wire::{
    RecipeDryRunReport, RecipeParameter, RecipeParameterSchema,
};

use crate::engine::CorpusEngine;
use crate::recipe::ParameterKind;
use crate::testing::{TestOptions, TestReport};
use crate::types::CorpusSpec;
use crate::Recipe;

/// THE projection from the engine's `TestReport` onto the wire. Both arms
/// of the route answer through this one function, so a sampled run and a
/// validation-only run cannot disagree about what a field means
/// (ARCH principle 8).
pub fn dry_run_report(report: &TestReport) -> RecipeDryRunReport {
    // `extraction`/`chunking` are `None` when the stage did not run — the
    // validation-only case. They collapse to zero here because that is the
    // wire contract the panel already reads (`RecipeTestResult` in types.ts
    // types both as plain numbers), and the field that says WHICH case it is
    // is `records_attempted == 0`, documented on the DTO. Widening these to
    // `Option` is a frontend change, not this commit's.
    let (records_attempted, records_succeeded, extraction_rate) = report
        .extraction
        .as_ref()
        .map(|e| (e.records_attempted, e.records_succeeded, e.extraction_rate))
        .unwrap_or((0, 0, 0.0));
    let (total_chunks, avg_chars) = report
        .chunking
        .as_ref()
        .map(|c| (c.total_chunks, c.avg_chars))
        .unwrap_or((0, 0.0));
    RecipeDryRunReport {
        passed: report.passed(),
        errors: report.validation.errors.clone(),
        warnings: report.warnings(),
        recipe_id: report.recipe_id.clone(),
        recipe_name: report.recipe_name.clone(),
        source_reachable: report.validation.source_reachable,
        records_attempted,
        records_succeeded,
        extraction_rate,
        total_chunks,
        avg_chars,
        report_markdown: report.to_markdown(),
    }
}

/// Project the engine's rich `TestReport` onto the protocol per-stage
/// report. A stage appears only if it ran (acquisition / extraction /
/// chunking are each `Option`), mirroring the state the engine reached.
pub fn map_test_report(r: &TestReport) -> RecipeTestReport {
    let mut stages = vec![RecipeStageReport {
        name: "validate".into(),
        docs_in: 0,
        docs_out: 0,
        misses: r.validation.errors.clone(),
        // Advisory warnings aren't "misses"; surface them where the author
        // will still see them rather than dropping them on the wire.
        sample: r.validation.warnings.clone(),
    }];

    if let Some(acq) = &r.acquisition {
        stages.push(RecipeStageReport {
            name: "acquire".into(),
            docs_in: 0,
            docs_out: acq.records_fetched as u32,
            misses: Vec::new(),
            sample: vec![format!(
                "{} records, {} bytes from {}",
                acq.records_fetched, acq.bytes_downloaded, acq.source_url
            )],
        });
    }

    if let Some(ext) = &r.extraction {
        stages.push(RecipeStageReport {
            name: "extract".into(),
            docs_in: ext.records_attempted as u32,
            docs_out: ext.records_succeeded as u32,
            misses: ext
                .failed_examples
                .iter()
                .map(|f| format!("record {}: {}", f.index, f.reason))
                .collect(),
            sample: Vec::new(),
        });
    }

    if let Some(ch) = &r.chunking {
        let mut misses: Vec<String> = r
            .section_misses
            .iter()
            .map(|m| format!("{} / {}: {}", m.file, m.section, m.description))
            .collect();
        // Chunks over the recipe's configured `max_chars` are a soft miss
        // the author will want to tune the chunker for.
        if ch.chunks_over_limit > 0 {
            misses.push(format!(
                "{} chunk(s) exceed max_chars={}",
                ch.chunks_over_limit, ch.recipe_max_chars
            ));
        }
        stages.push(RecipeStageReport {
            name: "chunk".into(),
            docs_in: r
                .extraction
                .as_ref()
                .map(|e| e.records_succeeded as u32)
                .unwrap_or(0),
            docs_out: ch.total_chunks as u32,
            misses,
            sample: r.sample_chunks.iter().map(|s| s.preview.clone()).collect(),
        });
    }

    // A recipe is "ok" iff it validated clean and produced chunks — the
    // end-to-end signal an author cares about.
    let ok =
        r.validation.errors.is_empty() && r.chunking.as_ref().is_some_and(|c| c.total_chunks > 0);

    RecipeTestReport { stages, ok }
}

/// Move 6 P5.a.1 incremental computation. Returns `Err(reason)` if
/// the caller should fall back to a full rebuild; `Ok(())` on
/// success (or on no-op when the delta carried no doc_ids).
pub async fn apply_incremental(
    engine: std::sync::Arc<CorpusEngine>,
    indexes_dir: std::path::PathBuf,
    corpus_id: String,
    role: &'static str,
    doc_ids: Vec<String>,
) -> Result<(), String> {
    use crate::enrichment::atlas::atoms_delta::apply_atom_delta;
    use crate::enrichment::atlas::strategies::newsworthy_events::extract_atoms_for_portal_chunks;
    use crate::enrichment::atlas::strategies::structure_first::{
        aggregate_articles_from_chunks, extract_atoms_for_articles, StructureFirstConfig,
    };
    use crate::meta_atlas::rebuild_for_corpus;
    use understanding_vocab::read::{read_atlas_atoms, ATLAS_DIRNAME};

    if doc_ids.is_empty() {
        return Ok(());
    }

    let started = std::time::Instant::now();
    let atlas_dir = indexes_dir.join(&corpus_id).join(ATLAS_DIRNAME);

    // Pre-flight: only run the incremental path against an atlas
    // that's already migrated to content-hash ids. Sequential-id
    // atlases mix with content-hash atoms badly (apply_atom_delta
    // would leave the legacy atoms orphaned).
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => return Err(format!("read atoms.json at {}: {e}", atlas_dir.display())),
    };
    if !atoms_file.atoms().is_empty()
        && !atoms_file
            .atoms()
            .iter()
            .all(|env| env.id().is_content_hash())
    {
        return Err(
            "atoms.json contains sequential-id atoms; run `sovereign atlas migrate-ids` first"
                .to_string(),
        );
    }
    let atoms_before = atoms_file.atoms().len();
    drop(atoms_file);

    // Query LanceDB for the tick's chunks.
    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open_index_for_corpus({corpus_id}): {e}"))?;
    let chunks = index
        .chunks_by_source_doc_ids(&doc_ids)
        .await
        .map_err(|e| format!("chunks_by_source_doc_ids({} ids): {e}", doc_ids.len()))?;
    let chunk_count = chunks.len();

    // Strategy dispatch keyed off the watcher-supplied role.
    //
    // `portal` → wikipedia-newsworthy daily Portal:Current_events pages.
    //   Each chunk IS a single event bullet — extract per-bullet Event
    //   atoms + wikilink Entity placeholders via `newsworthy_events`.
    //
    // `refresh` → the parent `wikipedia` corpus's tracked-window
    //   articles. Each chunk is a section of a real article — keep the
    //   structure_first one-Entity-per-article shape.
    //
    // Any future role falls back to structure_first; new roles should
    // add their dispatch branch here together with the extractor that
    // matches the corpus's chunk shape.
    let (delta_atoms, delta_edges, articles_count) = if role == "portal" {
        let delta = extract_atoms_for_portal_chunks(&chunks, &corpus_id);
        let event_count = delta
            .atoms_delta
            .upserted_docs
            .iter()
            .filter(|(d, _)| d != "_placeholders")
            .map(|(_, atoms)| atoms.len())
            .sum::<usize>();
        (delta.atoms_delta, delta.edges, event_count)
    } else {
        let agg = aggregate_articles_from_chunks(&chunks);
        let cfg = StructureFirstConfig {
            source_corpus_id: corpus_id.clone(),
            ..Default::default()
        };
        let delta = extract_atoms_for_articles(&agg.articles, &corpus_id, &cfg);
        (delta.atoms_delta, delta.edges, agg.articles.len())
    };
    // edges already live inside delta_atoms.added_edges; drop the
    // separate handle to silence dead-code warnings on the `portal`
    // branch where we don't apply edges twice.
    let _ = delta_edges;

    // Apply.
    let summary = apply_atom_delta(&atlas_dir, delta_atoms)
        .map_err(|e| format!("apply_atom_delta({}): {e}", atlas_dir.display()))?;

    // Meta-atlas: refresh anchors for this corpus only.
    let meta_outcome = match rebuild_for_corpus(&indexes_dir, &corpus_id, None) {
        Ok(_) => "ok",
        Err(e) => {
            tracing::warn!(
                corpus_id = %corpus_id,
                role = %role,
                error = %e,
                "newsworthy.atlas_meta_partial_rebuild_failed — meta-atlas anchors may lag until next full build"
            );
            "failed"
        }
    };

    tracing::info!(
        corpus_id = %corpus_id,
        role = %role,
        doc_count = doc_ids.len(),
        chunk_count,
        articles_aggregated = articles_count,
        atoms_before = summary.atoms_before,
        atoms_after = summary.atoms_after,
        atoms_added = summary.atoms_added,
        atoms_removed = summary.atoms_removed,
        docs_upserted = summary.docs_upserted,
        meta_atlas = meta_outcome,
        wall_ms = started.elapsed().as_millis() as u64,
        atoms_before_query = atoms_before,
        "newsworthy.atlas_incremental_complete"
    );
    Ok(())
}
/// Convert a JSON parameter map (the API's wire format) into a TOML
/// parameter map, which is what
/// [`Recipe::resolve_parameters`](crate::Recipe::resolve_parameters)
/// expects. JSON strings → TOML strings, JSON integers → TOML ints,
/// JSON arrays of strings → TOML arrays. Anything else fails with a
/// helpful error.
pub fn json_params_to_toml(
    params: &std::collections::BTreeMap<String, serde_json::Value>,
) -> std::result::Result<std::collections::BTreeMap<String, toml::Value>, String> {
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in params {
        let toml_value = match v {
            serde_json::Value::String(s) => toml::Value::String(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    toml::Value::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    toml::Value::Float(f)
                } else {
                    return Err(format!("parameter `{k}` is a non-finite number"));
                }
            }
            serde_json::Value::Bool(b) => toml::Value::Boolean(*b),
            serde_json::Value::Array(arr) => {
                let mut items = Vec::with_capacity(arr.len());
                for item in arr {
                    match item {
                        serde_json::Value::String(s) => items.push(toml::Value::String(s.clone())),
                        other => {
                            return Err(format!(
                                "parameter `{k}` array entries must be strings, \
                                 got: {other:?}"
                            ))
                        }
                    }
                }
                toml::Value::Array(items)
            }
            serde_json::Value::Null => continue,
            serde_json::Value::Object(_) => {
                return Err(format!(
                    "parameter `{k}` is a JSON object — only string, int, \
                     bool, and string array values are supported"
                ));
            }
        };
        out.insert(k.clone(), toml_value);
    }
    Ok(out)
}

/// The recipe's `type` label for a parameter, as the form keys on it.
pub fn parameter_kind_label(k: &ParameterKind) -> &'static str {
    match k {
        ParameterKind::String => "string",
        ParameterKind::Int => "int",
        ParameterKind::Date => "date",
        ParameterKind::List => "list",
    }
}

/// A TOML default rendered as JSON for the form.
pub fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (k, vv) in table {
                map.insert(k.clone(), toml_to_json(vv));
            }
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
    }
}

/// The engine is svrn's daemon port: each method is the engine call the
/// daemon made directly before pb-ingest-dial-daemon-ports.
#[async_trait]
impl IngestPort for CorpusEngine {
    fn recipes_dir(&self) -> &Path {
        CorpusEngine::recipes_dir(self)
    }

    fn partition_path(&self, corpus_id: &str) -> PathBuf {
        CorpusEngine::partition_path(self, corpus_id)
    }

    fn canonical_path(&self, corpus_id: &str) -> PathBuf {
        CorpusEngine::canonical_path(self, corpus_id)
    }

    fn corpus_is_installed(&self, corpus_id: &str) -> bool {
        self.corpus(corpus_id).is_installed()
    }

    fn in_progress_ingestions(&self) -> Vec<String> {
        CorpusEngine::in_progress_ingestions(self)
    }

    fn corpora_with_stranded_partitions(&self) -> Vec<String> {
        CorpusEngine::corpora_with_stranded_partitions(self)
    }

    fn cancel_registry(&self) -> CancellationRegistry {
        CorpusEngine::cancel_registry(self)
    }

    fn has_source_manifest(&self, corpus_id: &str) -> bool {
        self.source_manifest(corpus_id).ok().flatten().is_some()
    }

    fn remaining_source_files(&self, corpus_id: &str) -> crate::error::Result<Vec<SourceFileRecord>> {
        CorpusEngine::remaining_source_files(self, corpus_id)
    }

    fn count_jsonl_articles(&self, corpus_id: &str) -> crate::error::Result<u64> {
        CorpusEngine::count_jsonl_articles(self, corpus_id)
    }

    fn jsonl_source_shard_count(&self, corpus_id: &str) -> crate::error::Result<usize> {
        CorpusEngine::jsonl_source_shard_count(self, corpus_id)
    }

    fn estimate_article_pos(
        &self,
        corpus_id: &str,
        committed_iter_pos: u64,
        sample_size: usize,
    ) -> crate::error::Result<Option<u64>> {
        CorpusEngine::estimate_article_pos(self, corpus_id, committed_iter_pos, sample_size)
    }

    fn corpus_processed_shards(&self, corpus_id: &str) -> Vec<usize> {
        CorpusEngine::corpus_processed_shards(self, corpus_id)
    }

    fn corpus_committed_iter_pos(&self, corpus_id: &str) -> u64 {
        CorpusEngine::corpus_committed_iter_pos(self, corpus_id)
    }

    fn corpus_disk_status(&self, corpus_id: &str) -> CorpusDiskStatus {
        CorpusEngine::corpus_disk_status(self, corpus_id)
    }

    fn cached_article_stats(&self, corpus_id: &str) -> Option<ArticleStats> {
        CorpusEngine::cached_article_stats(self, corpus_id)
    }

    fn compute_article_stats(&self, corpus_id: &str) -> Option<ArticleStats> {
        CorpusEngine::compute_article_stats(self, corpus_id)
    }

    fn corpus_status_rows(&self) -> std::io::Result<serde_json::Value> {
        let rows = crate::engine::status::scan_corpus_rows(self.index_dir())?;
        serde_json::to_value(rows).map_err(std::io::Error::other)
    }

    async fn open_index_transient(&self, path: &Path) -> crate::error::Result<CorpusIndex> {
        CorpusEngine::open_index_transient(self, path).await
    }

    async fn diagnose_indexes(&self) -> String {
        CorpusEngine::diagnose_indexes(self).await
    }

    fn reprocess_skeleton_failures(&self, index: &CorpusIndex) -> crate::error::Result<(usize, usize)> {
        crate::reprocess_skeleton_failures(index)
    }

    fn pack_canonical(
        &self,
        canonical_path: &Path,
        writer: Box<dyn std::io::Write + Send>,
        compression_level: i32,
    ) -> crate::error::Result<u64> {
        crate::canonical_sync::pack_canonical(canonical_path, writer, compression_level)
    }

    fn set_yield_hook(&self, hook: Arc<dyn YieldHook>) {
        CorpusEngine::set_yield_hook(self, hook)
    }

    fn set_foreground_signal(&self, signal: Arc<dyn ForegroundSignal>) {
        CorpusEngine::set_foreground_signal(self, signal)
    }

    async fn ingest_with_overrides(
        &self,
        recipe_id: &str,
        file_indices: Option<Vec<usize>>,
        article_range: Option<(u64, u64)>,
        output_path: &Path,
        progress: Option<ProgressCallback>,
        unit_id: Option<u32>,
    ) -> crate::error::Result<IngestResult> {
        CorpusEngine::ingest_with_overrides(
            self,
            recipe_id,
            file_indices,
            article_range,
            output_path,
            progress,
            unit_id,
        )
        .await
    }

    async fn expand_corpus_to_full(
        &self,
        corpus_id: &str,
        progress: Option<ProgressCallback>,
    ) -> crate::error::Result<IngestResult> {
        CorpusEngine::expand_corpus_to_full(self, corpus_id, progress).await
    }

    async fn resume_interrupted_conversation_enrichment(&self) -> usize {
        CorpusEngine::resume_interrupted_conversation_enrichment(self).await
    }

    async fn prepare_registry_install(
        self: Arc<Self>,
        corpus_id: &str,
        parameters: &BTreeMap<String, serde_json::Value>,
    ) -> std::result::Result<PreparedInstall, InstallRefusal> {
        // Fetch, coerce, resolve: the order the install route always refused
        // in, so a missing recipe is named before a malformed parameter.
        let recipe = match self.registry().fetch_recipe(corpus_id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "spawn_corpus_install: recipe fetch failed"
                );
                return Err(InstallRefusal::RecipeNotFound(e.to_string()));
            }
        };
        let toml_params = match json_params_to_toml(parameters) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "spawn_corpus_install: parameter coercion failed"
                );
                return Err(InstallRefusal::InvalidParameters(e));
            }
        };
        let resolved = match recipe.resolve_parameters(&toml_params) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "spawn_corpus_install: parameter validation failed"
                );
                return Err(InstallRefusal::InvalidParameters(e.to_string()));
            }
        };
        let recipe = recipe.with_resolved_parameters(resolved);
        let opts_out_of_auto_enrichment = recipe.opts_out_of_auto_enrichment();
        let engine = self;
        Ok(PreparedInstall {
            opts_out_of_auto_enrichment,
            run: Box::new(move |progress| {
                Box::pin(async move {
                    let spec = CorpusSpec::Inline(Box::new(recipe));
                    engine.ingest(&spec, progress).await
                })
            }),
        })
    }

    async fn recipe_sharing(&self, corpus_id: &str) -> crate::error::Result<RecipeSharing> {
        let recipe = self.load_recipe(corpus_id).await?;
        Ok(RecipeSharing {
            mesh_sharing: recipe.corpus.mesh_sharing,
            grantable: recipe.corpus.grantable,
        })
    }

    fn registry_listing(&self, id: &str) -> Option<RegistryListing> {
        self.registry().find_entry(id).map(|e| RegistryListing {
            enrichment_enabled: e.enrichment_enabled,
            toml_url: e.toml_url.clone(),
        })
    }

    fn recipe_corpus_id(&self, toml_text: &str) -> crate::error::Result<String> {
        Recipe::from_toml(toml_text).map(|r| r.corpus.id)
    }

    fn install_local_recipe(&self, toml_text: &str) -> crate::error::Result<PathBuf> {
        let recipe = Recipe::from_toml(toml_text)?;
        self.registry().install_local_recipe(&recipe, toml_text)
    }

    async fn recipe_parameter_schema(&self, corpus_id: &str) -> crate::error::Result<RecipeParameterSchema> {
        let recipe = self.registry().fetch_recipe(corpus_id).await?;
        let parameters: Vec<RecipeParameter> = recipe
            .parameters
            .iter()
            .map(|(name, spec)| RecipeParameter {
                name: name.clone(),
                kind: parameter_kind_label(&spec.kind).to_string(),
                description: spec.description.clone(),
                required: spec.required,
                default: spec.default.as_ref().map(toml_to_json),
            })
            .collect();
        Ok(RecipeParameterSchema {
            corpus_id: recipe.corpus.id,
            parameters,
        })
    }

    async fn dry_run_recipe(
        &self,
        recipe_path: &Path,
        sample_size: usize,
        offline: bool,
    ) -> crate::error::Result<RecipeDryRunReport> {
        let options = TestOptions {
            sample_size,
            embed: false,
            offline,
            ..Default::default()
        };
        self.test_recipe(recipe_path, &options)
            .await
            .map(|r| dry_run_report(&r))
    }

    async fn test_recipe_report(
        &self,
        recipe_path: &Path,
        sample_size: usize,
        offline: bool,
    ) -> crate::error::Result<serde_json::Value> {
        let options = TestOptions {
            sample_size,
            embed: false,
            offline,
            ..Default::default()
        };
        let report = self.test_recipe(recipe_path, &options).await?;
        serde_json::to_value(map_test_report(&report))
            .map_err(|e| crate::error::Error::Serialization(e.to_string()))
    }

    fn recipe_vocabulary(&self, recipe_path: &Path) -> crate::error::Result<Option<RecipeVocabulary>> {
        let recipe = Recipe::from_file(recipe_path)?;
        Ok(recipe.custom_ontology().map(|policies| {
            let terms = policies.prose.terms;
            RecipeVocabulary {
                position_term: terms.position_term,
                tension_term: terms.tension_term,
                concern_term: terms.concern_term,
                evidence_term: terms.evidence_term,
            }
        }))
    }

    fn recipe_enrichment_domain(&self, recipe_path: &Path) -> Option<String> {
        Recipe::from_file(recipe_path)
            .ok()
            .and_then(|r| r.enrichment)
            .and_then(|e| e.domain)
    }

    fn spawn_newsworthy_watcher(
        self: Arc<Self>,
        host: NewsworthyHostFactory,
        shutdown: tokio::sync::watch::Receiver<bool>,
        force_tick: tokio::sync::mpsc::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        use crate::update::newsworthy_watcher::{
            HttpMediaWikiClient, MediaWikiClient, NewsworthyConfig, WikipediaNewsworthyWatcher,
        };
        let config = NewsworthyConfig::default();
        let host = host(config.corpus_id.clone());
        let media_wiki: Arc<dyn MediaWikiClient> = Arc::new(HttpMediaWikiClient {
            base_url: "https://en.wikipedia.org/w/api.php".to_string(),
            user_agent: "commonwealth-ai/0.1 (newsworthy)".to_string(),
            http: reqwest::Client::new(),
        });
        Arc::new(WikipediaNewsworthyWatcher::new(
            host, self, media_wiki, config,
        ))
        .spawn(shutdown, force_tick)
    }

    async fn apply_newsworthy_incremental(
        self: Arc<Self>,
        indexes_dir: PathBuf,
        corpus_id: String,
        role: &'static str,
        doc_ids: Vec<String>,
    ) -> std::result::Result<(), String> {
        apply_incremental(self, indexes_dir, corpus_id, role, doc_ids).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_kind_labels_round_trip() {
        assert_eq!(parameter_kind_label(&ParameterKind::String), "string");
        assert_eq!(parameter_kind_label(&ParameterKind::Int), "int");
        assert_eq!(parameter_kind_label(&ParameterKind::Date), "date");
        assert_eq!(parameter_kind_label(&ParameterKind::List), "list");
    }

    #[test]
    fn toml_to_json_handles_arrays_and_strings() {
        let v = toml::Value::Array(vec![
            toml::Value::String("NVDA".into()),
            toml::Value::String("MSFT".into()),
        ]);
        assert_eq!(toml_to_json(&v), serde_json::json!(["NVDA", "MSFT"]));
    }
}
