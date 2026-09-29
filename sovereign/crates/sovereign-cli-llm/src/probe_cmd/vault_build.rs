// SPDX-License-Identifier: AGPL-3.0-or-later
//! The probe's `vault-build` mode (pb-bench-dials-vault): svrn builds a
//! folder corpus with every seam metered and writes what it observed, for
//! `svrn bench vault-report` to roll up and render. The observation strategy
//! (decorators over the pipeline's injected seams, never instrumentation) and
//! the cold-reset rules are that verb's module docs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use corpus_engine::enrichment::tiered::{
    run_folder_tiered_enrichment, ChunkEntityExtractor, ChunkEntityExtractorHandle,
    ChunkNerOutcome, ConvBucket, TieredEnrichmentProvider, TieredProviderHandle,
};
use corpus_index::index::EnrichmentChunkRow;
use corpus_index::Result as EngineResult;
use sovereign_contracts::probe::{
    ColdReset, IngestTransition, NoteRecord, PhaseSpan, VaultBuildEvidence, VaultBuildProbe,
    VaultSource,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Speed;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::local_corpus::progress::LocalCorpusProgress;
use sovereign_tools::local_corpus::{LocalCorpusConfig, LocalCorpusManager};

use super::resource_meter::{MeteredInference, ResourceLedger};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::ChatGlobals;

#[cfg(test)]
#[path = "vault_build_tests.rs"]
mod tests;

// ─────────────────────────────────────────────────────────────────────
// Observer — the shared ledger the decorators write into
// ─────────────────────────────────────────────────────────────────────

/// Shared, thread-safe build ledger. Every decorator holds an `Arc` of
/// this and stamps spans/records against a single run-start `Instant`,
/// so all timestamps in the report share one origin.
struct BuildObserver {
    start: Instant,
    inner: Mutex<ObserverState>,
}

#[derive(Default)]
struct ObserverState {
    phases: Vec<PhaseSpan>,
    ingest_transitions: Vec<IngestTransition>,
    notes: Vec<NoteRecord>,
    /// Ingest phase currently open: `(label, first_seen_ms, last_seen_ms)`.
    /// `last_seen` is tracked separately from the next phase's start
    /// because the difference between them is unobserved time that must
    /// not be silently attributed — see [`BuildObserver::ingest_transition`].
    open_ingest: Option<(String, u64, u64)>,
    entity_mentions: usize,
}

/// Gap between a phase's last event and the next phase's first event
/// that is large enough to report as unattributed rather than absorb.
/// Below this, the gap is scheduling noise between two progress
/// callbacks and attributing it either way is harmless.
const UNATTRIBUTED_GAP_MS: u64 = 200;

impl BuildObserver {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            inner: Mutex::new(ObserverState::default()),
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn push_phase(&self, phase: &str, start_ms: u64, end_ms: u64, detail: serde_json::Value) {
        if let Ok(mut g) = self.inner.lock() {
            g.phases.push(PhaseSpan {
                phase: phase.to_string(),
                start_ms,
                end_ms,
                ms: end_ms.saturating_sub(start_ms),
                detail,
            });
        }
    }

    /// Record an ingest-side transition, closing the open phase span
    /// when the label changes.
    ///
    /// A phase ends at **its own last event**, not at the next phase's
    /// first event. The difference matters: the engine only emits embed
    /// progress once per full 256-chunk batch
    /// (`corpus-engine/src/engine/ingest.rs:1461`), so a corpus smaller
    /// than one batch — and the tail of any corpus — embeds with no
    /// progress at all. Ending the previous span at the next label would
    /// file every one of those seconds under whatever phase happened to
    /// be open last, which on a first fixture run reported 8.5s of
    /// extract-and-embed as `staging` (staging really took 7ms).
    ///
    /// So the gap is reported under its own name instead. Unobserved
    /// time stays visible as unobserved; it never inflates a neighbour.
    fn ingest_transition(&self, label: &str, detail: serde_json::Value) {
        self.ingest_transition_at(self.now_ms(), label, detail);
    }

    /// [`Self::ingest_transition`] with the clock injected, so the
    /// span/gap arithmetic is testable without sleeping.
    fn ingest_transition_at(&self, t: u64, label: &str, detail: serde_json::Value) {
        let mut closed: Option<(String, u64, u64)> = None;
        if let Ok(mut g) = self.inner.lock() {
            g.ingest_transitions.push(IngestTransition {
                ms_since_start: t,
                phase: label.to_string(),
                detail,
            });
            match &mut g.open_ingest {
                Some((open_label, _, last_seen)) if open_label == label => *last_seen = t,
                Some((open_label, open_start, last_seen)) => {
                    closed = Some((open_label.clone(), *open_start, *last_seen));
                    g.open_ingest = Some((label.to_string(), t, t));
                }
                None => g.open_ingest = Some((label.to_string(), t, t)),
            }
        }
        if let Some((label, start, last_seen)) = closed {
            self.push_phase(&label, start, last_seen, serde_json::Value::Null);
            self.push_gap(&label, last_seen, t);
        }
    }

    /// Close whatever ingest phase is still open, and account for any
    /// unobserved tail between its last event and `now`.
    fn close_open_ingest(&self) {
        let closed = self
            .inner
            .lock()
            .ok()
            .and_then(|mut g| g.open_ingest.take());
        if let Some((label, start, last_seen)) = closed {
            self.push_phase(&label, start, last_seen, serde_json::Value::Null);
            self.push_gap(&label, last_seen, self.now_ms());
        }
    }

    /// Record unobserved time as its own span. Named by the phase it
    /// follows so it stays stable across runs (`--compare` can line two
    /// runs up) while still saying where in the pipeline it sits.
    fn push_gap(&self, after: &str, start_ms: u64, end_ms: u64) {
        if end_ms.saturating_sub(start_ms) < UNATTRIBUTED_GAP_MS {
            return;
        }
        self.push_phase(
            &format!("unattributed:after:{after}"),
            start_ms,
            end_ms,
            serde_json::json!({
                "note": "no progress events in this window; the pipeline emits none here",
            }),
        );
    }

    fn push_note(&self, rec: NoteRecord) {
        if let Ok(mut g) = self.inner.lock() {
            g.notes.push(rec);
        }
    }

    fn add_mentions(&self, n: usize) {
        if let Ok(mut g) = self.inner.lock() {
            g.entity_mentions += n;
        }
    }

    fn snapshot(
        &self,
    ) -> (
        Vec<PhaseSpan>,
        Vec<IngestTransition>,
        Vec<NoteRecord>,
        usize,
    ) {
        match self.inner.lock() {
            Ok(g) => (
                g.phases.clone(),
                g.ingest_transitions.clone(),
                g.notes.clone(),
                g.entity_mentions,
            ),
            Err(_) => (Vec::new(), Vec::new(), Vec::new(), 0),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Decorators — the whole observation strategy
// ─────────────────────────────────────────────────────────────────────

/// Times the corpus-wide NER pass. This phase had no timer of any kind
/// before this harness, and it is exactly the phase P2.1 (GLiNER2)
/// proposes to make 2.8× faster — so its cost had to become visible
/// before that work could carry a predicted delta.
struct MeteredEntityExtractor {
    inner: ChunkEntityExtractorHandle,
    obs: Arc<BuildObserver>,
}

#[async_trait]
impl ChunkEntityExtractor for MeteredEntityExtractor {
    async fn extract_for_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
    ) -> EngineResult<ChunkNerOutcome> {
        self.inner
            .extract_for_conversation(corpus_id, conv_uuid, chunks)
            .await
    }

    async fn extract_delta_for_corpus(
        &self,
        corpus_id: &str,
        index_path: &Path,
    ) -> EngineResult<ChunkNerOutcome> {
        let start = self.obs.now_ms();
        eprintln!("  [ner] corpus-wide entity extraction …");
        let out = self
            .inner
            .extract_delta_for_corpus(corpus_id, index_path)
            .await;
        let end = self.obs.now_ms();
        let (mentions, detail) = match &out {
            Ok(o) => {
                self.obs.add_mentions(o.mentions);
                (
                    o.mentions,
                    // The refusal count rides the phase record: a lane
                    // whose mention total dropped because the input bound
                    // refused chunks reads as a regression otherwise.
                    serde_json::json!({
                        "mentions": o.mentions,
                        "refused_over_cap": o.refused_over_cap,
                    }),
                )
            }
            Err(e) => (
                0,
                serde_json::json!({ "error": e.to_string(), "mentions": 0 }),
            ),
        };
        self.obs.push_phase("ner", start, end, detail);
        eprintln!(
            "  [ner] {mentions} mention(s) · {:.1}s",
            (end - start) as f64 / 1000.0
        );
        out
    }
}

/// Times every unit of enrichment work: one span per note, plus the
/// corpus-wide finalize passes.
///
/// `note_already_current` is decorated too, and that is the point — a
/// skip is recorded as a `NoteRecord` with outcome
/// `skipped_already_current`. That single counter is what makes a
/// falsely-cold run visible in its own report instead of silently
/// reporting a suspiciously fast build.
struct MeteredTieredProvider {
    inner: TieredProviderHandle,
    obs: Arc<BuildObserver>,
    total_docs: Mutex<usize>,
}

#[async_trait]
impl TieredEnrichmentProvider for MeteredTieredProvider {
    async fn enrich_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
        embeddings: Vec<Vec<f32>>,
        bucket: ConvBucket,
    ) -> EngineResult<()> {
        let chunk_count = chunks.len();
        let start = self.obs.now_ms();
        let out = self
            .inner
            .enrich_conversation(corpus_id, conv_uuid, chunks, embeddings, bucket)
            .await;
        let end = self.obs.now_ms();
        let ms = end.saturating_sub(start);
        let (outcome, error) = match &out {
            Ok(()) => ("built", None),
            Err(e) => ("failed", Some(e.to_string())),
        };
        let idx = {
            let mut g = self.total_docs.lock().unwrap_or_else(|p| p.into_inner());
            *g += 1;
            *g
        };
        eprintln!(
            "  [{idx}] {conv_uuid}  {chunk_count} chunks · {} · {outcome} · {:.1}s",
            bucket.label(),
            ms as f64 / 1000.0
        );
        self.obs.push_note(NoteRecord {
            doc_id: conv_uuid.to_string(),
            chunks: chunk_count,
            bucket: bucket.label().to_string(),
            ms,
            start_ms: start,
            outcome: outcome.to_string(),
            error,
        });
        out
    }

    async fn finalize_corpus(&self, corpus_id: &str) -> EngineResult<()> {
        let start = self.obs.now_ms();
        eprintln!("  [synthesis] vault-wide theme synthesis …");
        let out = self.inner.finalize_corpus(corpus_id).await;
        let end = self.obs.now_ms();
        let detail = match &out {
            Ok(()) => serde_json::Value::Null,
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        self.obs.push_phase("vault_synthesis", start, end, detail);
        eprintln!("  [synthesis] {:.1}s", (end - start) as f64 / 1000.0);
        out
    }

    async fn post_finalize_corpus(&self, corpus_id: &str) {
        let start = self.obs.now_ms();
        self.inner.post_finalize_corpus(corpus_id).await;
        let end = self.obs.now_ms();
        self.obs
            .push_phase("typed_extension", start, end, serde_json::Value::Null);
    }

    async fn reenrich_sources(
        &self,
        corpus_id: &str,
        source_doc_ids: &[String],
    ) -> EngineResult<()> {
        self.inner.reenrich_sources(corpus_id, source_doc_ids).await
    }

    async fn note_already_current(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunk_count: usize,
    ) -> bool {
        let start = self.obs.now_ms();
        let skip = self
            .inner
            .note_already_current(corpus_id, conv_uuid, chunk_count)
            .await;
        if skip {
            let end = self.obs.now_ms();
            self.obs.push_note(NoteRecord {
                doc_id: conv_uuid.to_string(),
                chunks: chunk_count,
                bucket: ConvBucket::classify_note(chunk_count).label().to_string(),
                ms: end.saturating_sub(start),
                start_ms: start,
                outcome: "skipped_already_current".to_string(),
                error: None,
            });
        }
        skip
    }
}

fn data_dir() -> PathBuf {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
}

/// Build the folder corpus `spec` names, metered, and report what was observed.
pub(crate) async fn build(
    globals: &ChatGlobals,
    spec: &VaultBuildProbe,
) -> std::result::Result<VaultBuildEvidence, String> {
    let mode = if spec.cold { "cold" } else { "warm" };
    let data_dir = data_dir();
    let indexes_dir = data_dir.join("indexes");
    let recipes_dir = data_dir.join("recipes");

    eprintln!("svrn bench vault-report");
    eprintln!("  data dir:  {}", data_dir.display());
    eprintln!("  mode:      {mode}");

    // ── Session: inference over HTTP to the daemon, pipeline in-process ──
    eprintln!("[1/5] daemon session");
    let session = build_session(globals)
        .await
        .map_err(|e| format!("daemon bootstrap failed: {e}. Is the daemon running?"))?;

    let ledger = Arc::new(ResourceLedger::new());
    let enrich_base: Arc<dyn InferenceProvider> = match &spec.enrich_model {
        Some(model) => {
            eprintln!("      enrich model override: {model}");
            super::provider_for_model(&globals.daemon_base, model, &session.embed_model).await
        }
        None => Arc::clone(&session.inference),
    };
    let enrich_inference: Arc<dyn InferenceProvider> =
        Arc::new(MeteredInference::new(enrich_base, Arc::clone(&ledger)));
    let enrich_model = Some(enrich_inference.model_id_for(Speed::Slow));

    // ── Engine: the production shape, batch-embed included ──
    // `with_batch_embed_fn` is not optional for a build-time number.
    // Without it the engine falls back to one HTTP round-trip per text
    // (the pre-2026-07-24 path), which would make every measurement a
    // measurement of the wrong pipeline.
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&enrich_inference));
    let batch_embed_fn =
        sovereign_tools::corpus::inference_to_batch_embed_fn(Arc::clone(&enrich_inference));
    let inference_fn = corpus_engine::enrichment::provider_inference::inference_to_inference_fn(
        Arc::clone(&enrich_inference),
    );
    let engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir.clone(), indexes_dir.clone(), embed_fn)
            .with_embedding_model(&session.embed_model)
            .with_batch_embed_fn(batch_embed_fn)
            .with_inference_fn(inference_fn),
    );

    let lc_store: Arc<dyn sovereign_core::traits::StateStore> =
        Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let manager = LocalCorpusManager::init_with_recipes_dir(
        engine.clone(),
        lc_store,
        None,
        data_dir.clone(),
        data_dir.join("vault-snapshots"),
        recipes_dir,
    )
    .await
    .map_err(|e| format!("LocalCorpusManager init: {e}"))?;

    // ── Resolve the corpus ──
    let config: LocalCorpusConfig = match &spec.source {
        VaultSource::Corpus { corpus_id: id } => manager
            .get(id)
            .await
            .ok_or_else(|| {
                format!(
                    "no registered corpus '{id}'. `svrn corpus status` lists them; use --folder \
                     <path> to register a new one."
                )
            })?
            .clone(),
        VaultSource::Folder { path } => {
            if !path.is_dir() {
                return Err(format!("--folder {} is not a directory", path.display()));
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("vault")
                .to_string();
            let cfg = LocalCorpusConfig::document_folder(path.clone(), name);
            eprintln!("      registering folder corpus '{}'", cfg.id);
            manager
                .register(cfg.clone())
                .await
                .map_err(|e| format!("register {}: {e}", path.display()))?;
            manager.get(&cfg.id).await.unwrap_or(cfg)
        }
    };
    let corpus_id = config.id.clone();
    let root_path = config.root_path.display().to_string();
    eprintln!("  corpus:    {corpus_id}");
    eprintln!("  folder:    {root_path}");

    // ── Watcher guard ──
    // The daemon's sweeper writes to the same corpus, and its boot-time
    // `resume_interrupted_enrichment` starts a SECOND build on top of
    // this one. Neither is detectable from inside the measurement, so
    // refuse rather than silently report a contaminated number.
    let is_watched = matches!(
        config.source_type,
        sovereign_tools::local_corpus::LocalCorpusSourceType::WatchedFolder(_)
    );
    if is_watched && !spec.allow_watcher {
        return Err(format!(
            "'{corpus_id}' is a WATCHED folder and the daemon is live. The sweeper — and \
             resume_interrupted_enrichment on any daemon restart — will write to the corpus \
             while it is being timed, so the result would not be a measurement of this run.\n\
             \n  Pause it first:  svrn corpus watch pause {corpus_id}\n  \
             Then re-run, and resume with: svrn corpus watch resume {corpus_id}\n\
             \n  Override with --allow-watcher if you have already stopped it another way."
        ));
    }

    // ── Cold reset ──
    let index_path = indexes_dir.join(&corpus_id);
    let db_path = data_dir.join("sovereign.db");
    let cold_reset = if spec.cold {
        eprintln!("[2/5] cold reset");
        // Prove the rebuild is possible BEFORE destroying what it would
        // rebuild. See `preflight_source_readable`.
        let ingestible = preflight_source_readable(&config.root_path, &config.extensions)?;
        eprintln!("      pre-flight: {ingestible} ingestible file(s) readable — reset is safe");
        Some(perform_cold_reset(&index_path, &db_path, &corpus_id).await?)
    } else {
        eprintln!("[2/5] cold reset — SKIPPED (--warm)");
        eprintln!("      expect most notes to report skipped_already_current");
        None
    };

    let obs = Arc::new(BuildObserver::new());
    let run_start = Instant::now();

    // ── Tier 1: folder → queryable Lance index ──
    eprintln!("[3/5] ingest — walk · stage · chunk · embed · index");
    ledger.set_phase("ingest");
    let cb_obs = Arc::clone(&obs);
    let progress: sovereign_tools::local_corpus::manager::ProgressCallback =
        Arc::new(move |p: LocalCorpusProgress| {
            let (label, detail) = render_local_progress(&p);
            cb_obs.ingest_transition(&label, detail);
        });

    let stats = manager
        .ingest(&corpus_id, None, Some(progress))
        .await
        .map_err(|e| format!("ingest '{corpus_id}': {e}"))?;
    obs.close_open_ingest();
    let time_to_rag_ready_ms = Some(obs.now_ms());
    eprintln!(
        "      rag_ready at t+{}ms — {} file(s), {} chunk(s)",
        time_to_rag_ready_ms.unwrap_or(0),
        stats.files_indexed,
        stats.chunks_written
    );

    // ── Tier 2/3: NER, per-note RAPTOR, vault synthesis ──
    eprintln!("[4/5] enrichment — ner · per-note raptor · vault synthesis");
    // Opening the state store and loading the NER model eagerly is real
    // cost a user pays on the way to an enriched corpus, so it gets its
    // own span rather than sitting in the gap between two phases. The
    // GLiNER load dominates it, which is worth seeing: it is a fixed
    // per-build overhead that does not shrink with vault size.
    let setup_start = obs.now_ms();
    let store = Arc::new(
        SqliteStateStore::open(&db_path).map_err(|e| format!("open {}: {e}", db_path.display()))?,
    );

    let (entity_handle, entity_path) =
        build_entity_extractor(spec.no_gliner, Arc::clone(&store), Arc::clone(&obs)).await;

    // `removed`, not a measurement. The ablation this field used to
    // report ran on 2026-08-02 and settled: the folder path's motif
    // pass cost 42.8% of a cold build, `conv_motifs` had no reader, and
    // dropping it was 1.76x faster at per-question-identical scores. So
    // the pass was deleted rather than flagged — `build_folder_artifacts`
    // now calls a builder with no motif concept in its return type, and
    // there is no longer a code path this run could take that would
    // build one. The field stays because run records from before the
    // deletion say `built` and the ablation arms say `skipped`; a
    // `--compare` across that boundary must not silently read alike.
    let motif_path = "removed".to_string();

    obs.push_phase(
        "enrichment_setup",
        setup_start,
        obs.now_ms(),
        serde_json::json!({
            "entity_path": entity_path.clone(),
            "motif_path": motif_path.clone(),
        }),
    );
    eprintln!("      entity path: {entity_path}");
    eprintln!("      motif path:  {motif_path}");

    let resolver: Arc<dyn sovereign_tools::conv_tiered_provider::IndexDirResolver> = Arc::new(
        sovereign_tools::conv_tiered_provider::StaticIndexDirResolver {
            indexes_root: indexes_dir.clone(),
        },
    );
    // Mirrors `enrichment_bootstrap::build_folder_tiered_provider` — the
    // memory tier's extractive default (T1 P1.1) is production behaviour,
    // so a build-time baseline has to carry it.
    let base_provider: TieredProviderHandle = Arc::new(
        sovereign_tools::conv_tiered_provider::FolderTieredProvider::new(
            Arc::clone(&store),
            Arc::clone(&enrich_inference),
            Arc::new(corpus_engine::IngestAtlas),
        )
        .with_index_dir_resolver(resolver)
        .with_summary_mode(sovereign_tools::raptor_atlas::SummaryMode::Extractive),
    );
    let metered_provider: TieredProviderHandle = Arc::new(MeteredTieredProvider {
        inner: base_provider,
        obs: Arc::clone(&obs),
        total_docs: Mutex::new(0),
    });

    ledger.set_phase("enrichment");
    let plan = run_folder_tiered_enrichment(
        &corpus_id,
        &index_path,
        Some(&metered_provider),
        entity_handle.as_ref(),
    )
    .await
    .map_err(|e| format!("run_folder_tiered_enrichment '{corpus_id}': {e}"))?;
    let time_to_enriched_ms = Some(obs.now_ms());
    ledger.set_phase("post_build");

    // ── Assemble ──
    eprintln!("[5/5] report");
    let (phases, ingest_transitions, notes, entity_mentions) = obs.snapshot();

    Ok(VaultBuildEvidence {
        corpus_id,
        root_path,
        cold_reset,
        enrich_model,
        embed_model: session.embed_model.clone(),
        entity_path,
        motif_path,
        files_indexed: stats.files_indexed,
        chunks_written: stats.chunks_written,
        documents_enriched: plan.total_conversations,
        entity_mentions,
        time_to_rag_ready_ms,
        time_to_enriched_ms,
        total_ms: run_start.elapsed().as_millis() as u64,
        phases,
        ingest_transitions,
        notes,
        resources: ledger.snapshot(),
    })
}

/// Build the NER extractor, wrapped so the phase gets a timer.
/// Returns the routing truth-teller alongside it — what the run
/// actually used, not what was asked for.
async fn build_entity_extractor(
    no_gliner: bool,
    store: Arc<SqliteStateStore>,
    obs: Arc<BuildObserver>,
) -> (Option<ChunkEntityExtractorHandle>, String) {
    if no_gliner {
        return (None, "disabled".to_string());
    }
    // serve's NER model, the one the daemon's readers dial too, so a
    // measured run and a production run cannot disagree about which
    // backend they got — that equality is the whole point of measuring
    // through this harness rather than a bespoke probe. Asked up front:
    // an extractor that isn't there yet would measure a no-op NER phase.
    match crate::serve_dial::serve_ner("bench vault-report").await {
        Ok(None) => (None, "unavailable (serve has no NER model)".to_string()),
        Err(e) => (None, format!("unavailable ({e})")),
        Ok(Some(g)) => {
            let model_id = g.model_id().to_string();
            // The routing string names the GENERATION, not just the id:
            // "did this run use GLiNER2?" is the question every P2.1
            // number is read against, and it must be answerable from
            // the report alone.
            let routed = format!("gliner {:?} ({model_id})", g.generation());
            let base = corpus_engine::enrichment::chunk_ner::GlinerChunkExtractor::new(store, g)
                .into_handle();
            let metered: ChunkEntityExtractorHandle =
                Arc::new(MeteredEntityExtractor { inner: base, obs });
            (Some(metered), routed)
        }
    }
}

/// Answer, before anything is deleted: can this process actually read
/// the source folder, and is there anything in it to ingest?
///
/// This guard exists because `--cold` is destructive and its safety
/// rests entirely on the re-ingest that follows it. Delete the index,
/// then read zero files, and the corpus is gone — not degraded,
/// **gone** — with no way for this process to rebuild it.
///
/// That is not hypothetical. On macOS, `~/Documents` is protected by
/// TCC: the directory stats fine and `is_dir()` returns true, but
/// `read_dir` fails with `Operation not permitted` unless the calling
/// binary has been granted Full Disk Access. A daemon launched from a
/// granted context can read the vault while a CLI run from a terminal
/// cannot — so "the daemon ingests this corpus fine" is no evidence at
/// all that this process can. The obsidian vault on this box is exactly
/// that shape (measured 2026-08-02).
///
/// `is_dir()` is therefore not the check. Listing the directory is.
fn preflight_source_readable(
    root: &Path,
    extensions: &[String],
) -> std::result::Result<usize, String> {
    if !root.exists() {
        return Err(format!("source folder {} does not exist", root.display()));
    }
    if !root.is_dir() {
        return Err(format!(
            "source folder {} is not a directory",
            root.display()
        ));
    }
    // Bounded walk — we need "is there anything ingestible", not a
    // precise census, and a vault can be large.
    const MAX_ENTRIES: usize = 100_000;
    let mut matched = 0usize;
    let mut visited = 0usize;
    let mut stack = vec![root.to_path_buf()];
    let mut first_error: Option<String> = None;
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(format!("{}: {e}", dir.display()));
                }
                continue;
            }
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                break;
            }
            let path = entry.path();
            if path.is_dir() {
                // Skip dot-dirs (.obsidian, .git) — never ingested.
                let hidden = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.starts_with('.'))
                    .unwrap_or(false);
                if !hidden {
                    stack.push(path);
                }
            } else if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let ext = ext.to_ascii_lowercase();
                if extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
                    matched += 1;
                }
            }
        }
    }

    if matched == 0 {
        let mut msg = format!(
            "REFUSING TO RESET: found 0 ingestible file(s) under {}.\n\
             A cold reset deletes this corpus's index and enrichment state, and is only \
             safe because the re-ingest rebuilds them. Reading zero files here would \
             destroy the corpus instead of rebuilding it.",
            root.display()
        );
        if let Some(e) = first_error {
            msg.push_str(&format!("\n\n  The folder could not be read: {e}"));
            if cfg!(target_os = "macos") {
                msg.push_str(
                    "\n\n  On macOS this is almost always TCC. Folders under ~/Documents, \
                     ~/Desktop and ~/Downloads are readable only by binaries granted Full \
                     Disk Access — the directory stats fine and still refuses to list. The \
                     daemon may hold that grant while this CLI does not, so the corpus \
                     ingesting normally in the app proves nothing about this process.\n  \
                     Fix: System Settings → Privacy & Security → Full Disk Access, add your \
                     terminal, restart it. Then re-run.",
                );
            }
        } else {
            msg.push_str(&format!(
                "\n\n  The folder was readable but held no files matching the corpus's \
                 extensions ({}).",
                extensions.join(", ")
            ));
        }
        return Err(msg);
    }
    Ok(matched)
}

/// The documented cold reset. `reset_enrichment_state` is deliberately
/// NOT used here — it is status-only and leaves every checkpoint layer
/// intact, so a run after it is warm while looking cold.
async fn perform_cold_reset(
    index_path: &Path,
    db_path: &Path,
    corpus_id: &str,
) -> std::result::Result<ColdReset, String> {
    let mut warnings = Vec::new();
    let existed = index_path.exists();
    if existed {
        std::fs::remove_dir_all(index_path)
            .map_err(|e| format!("remove index dir {}: {e}", index_path.display()))?;
        eprintln!("      removed {}", index_path.display());
    } else {
        eprintln!("      {} did not exist", index_path.display());
    }

    let (tiered_cleared, themes) = match SqliteStateStore::open(db_path) {
        Ok(store) => {
            let tiered = match store.delete_tiered_for_corpus(corpus_id).await {
                Ok(()) => {
                    eprintln!(
                        "      cleared raptor nodes · motifs · skeletons · chunk entities · entity progress"
                    );
                    true
                }
                Err(e) => {
                    warnings.push(format!("delete_tiered_for_corpus: {e}"));
                    false
                }
            };
            let themes = match store.delete_vault_themes_for_corpus(corpus_id).await {
                Ok(n) => {
                    eprintln!("      cleared {n} vault theme row(s)");
                    n
                }
                Err(e) => {
                    warnings.push(format!("delete_vault_themes_for_corpus: {e}"));
                    0
                }
            };
            (tiered, themes)
        }
        Err(e) => {
            warnings.push(format!("open {}: {e}", db_path.display()));
            (false, 0)
        }
    };
    for w in &warnings {
        eprintln!("      WARNING: {w} — this run is not fully cold");
    }
    Ok(ColdReset {
        index_dir: index_path.display().to_string(),
        index_dir_existed: existed,
        tiered_state_cleared: tiered_cleared,
        vault_themes_cleared: themes,
        warnings,
    })
}

/// Map a `LocalCorpusProgress` to a phase label + detail. The
/// `Ingesting` variant carries the engine's own phase label
/// (chunking / embedding / indexing / optimizing_index), which is what
/// separates embedding cost from IVF-PQ build cost without any new
/// instrumentation.
fn render_local_progress(p: &LocalCorpusProgress) -> (String, serde_json::Value) {
    match p {
        LocalCorpusProgress::Scanning { done, total } => (
            "scanning".to_string(),
            serde_json::json!({ "done": done, "total": total }),
        ),
        LocalCorpusProgress::Staging { done, total, .. } => (
            "staging".to_string(),
            serde_json::json!({ "done": done, "total": total }),
        ),
        LocalCorpusProgress::OcrPage {
            file_idx,
            file_total,
            ..
        } => (
            "ocr".to_string(),
            serde_json::json!({ "file_idx": file_idx, "file_total": file_total }),
        ),
        LocalCorpusProgress::Ingesting {
            done,
            total,
            phase_label,
            ..
        } => (
            stable_ingest_phase(phase_label),
            serde_json::json!({ "done": done, "total": total, "ui_label": phase_label }),
        ),
        LocalCorpusProgress::Clustering { stage } => (
            "clustering".to_string(),
            serde_json::json!({ "stage": format!("{stage:?}") }),
        ),
        LocalCorpusProgress::Snapshotting { done, total } => (
            "snapshotting".to_string(),
            serde_json::json!({ "done": done, "total": total }),
        ),
        LocalCorpusProgress::Writing { done, total } => (
            "writing".to_string(),
            serde_json::json!({ "done": done, "total": total }),
        ),
        LocalCorpusProgress::RollingBack { done, total } => (
            "rolling_back".to_string(),
            serde_json::json!({ "done": done, "total": total }),
        ),
        LocalCorpusProgress::Complete { .. } => {
            ("ingest_complete".to_string(), serde_json::Value::Null)
        }
        LocalCorpusProgress::Error { message, .. } => (
            "ingest_error".to_string(),
            serde_json::json!({ "message": message }),
        ),
    }
}

/// Map the ingest pipeline's *UI* phase label onto a stable phase key.
///
/// Two reasons this translation is not cosmetic.
///
/// One: the labels are user-facing prose generated per run —
/// `Done in 7s` embeds the duration itself, so using it as a key gives
/// every run a differently-named phase and `--compare` can never line
/// two runs up.
///
/// Two, and worse: the prose does not say what the phase is.
/// `ingest_progress_to_local` (manager.rs:2184) renders
/// `IngestProgress::Embedding` as **"Building the index"**, while the
/// actual index write renders as "Writing index" and the IVF-PQ build
/// as "Optimizing search index". A phase table keyed on the prose would
/// attribute embedding cost — normally the largest slice of tier 1 — to
/// a row an operator reads as index construction. These keys name the
/// engine variant behind the label, not the label.
fn stable_ingest_phase(ui_label: &str) -> String {
    let key = match ui_label {
        "Downloading" => "download",
        "Reading your documents" => "extract",
        "Chunking" => "chunk",
        // NOT the index build — see the doc comment.
        "Building the index" => "embed",
        "Writing index" => "index_write",
        "Optimizing search index" => "index_ann_build",
        // `Complete` renders as "Done in <n>s"; the duration is already
        // in `end_ms`, so the key must not carry it.
        l if l.starts_with("Done in ") => "done",
        // The `Enriching` arm passes a free-form `detail` string
        // through. Bucket it rather than minting an unbounded set of
        // phase names, and keep the raw label in `detail`.
        _ => "engine_enrich_hook",
    };
    format!("ingest:{key}")
}
