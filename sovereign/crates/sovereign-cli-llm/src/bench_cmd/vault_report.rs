// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench vault-report` — raw-folder → queryable-enriched-corpus
//! build-time benchmark.
//!
//! `bench book-report` already answers "how long from attaching ONE
//! document to a Ready asset". It cannot answer the question the folder
//! product actually poses: **how long from a folder of notes to a corpus
//! you can ask an enriched question of.** That path is a different
//! pipeline (`LocalCorpusManager::ingest` → `run_folder_tiered_enrichment`)
//! and, until this harness, was almost entirely untimed — the only
//! durable evidence a vault build ever left behind was per-note RAPTOR
//! checkpoint manifests, which cover exactly one of its seven phases.
//!
//! ## What this reports
//!
//! Two headlines, in the order a user experiences them:
//!
//! - **`time_to_rag_ready_ms`** — folder walk → chunks embedded and the
//!   Lance index queryable. After this the corpus answers retrieval
//!   questions; nothing is enriched yet.
//! - **`time_to_enriched_ms`** — everything above, plus per-chunk NER,
//!   per-note RAPTOR trees, motifs, and the vault-wide theme synthesis.
//!   This is the number the enrichment initiative is actually spending
//!   against.
//!
//! Underneath: a phase span table, a per-note table (the shape
//! `svrn enrich raptor` established), and the per-phase token/call
//! ledger from [`resource_meter`](super::resource_meter).
//!
//! ## How it observes without touching the pipeline
//!
//! `ENRICH_TURBO_HANDOFF.md` §2/§6.1 is explicit: use the existing
//! measurement system, and do not scatter `eprintln`s through the
//! enrichment path. This harness adds **zero production-code changes**.
//! Every phase boundary it reports comes from a seam the pipeline
//! already exposes as an injected `Arc<dyn Trait>`:
//!
//! | Phase | Seam |
//! |---|---|
//! | walk / stage / chunk / embed / index / IVF-PQ | `LocalCorpusProgress` callback on `LocalCorpusManager::ingest` |
//! | per-chunk NER | [`MeteredEntityExtractor`] over `ChunkEntityExtractor` |
//! | per-note RAPTOR + motifs | [`MeteredTieredProvider`] over `TieredEnrichmentProvider` |
//! | vault theme synthesis | the same decorator's `finalize_corpus` |
//! | typed extension | the same decorator's `post_finalize_corpus` |
//! | tokens / LLM calls / embeds | `MeteredInference` over `InferenceProvider` |
//!
//! Decorators, not instrumentation. The pipeline does not know it is
//! being measured, which is also why the numbers are trustworthy.
//!
//! ## Cold runs are the only valid runs
//!
//! Four independent checkpoint layers make a warm re-run report near-zero
//! work: the ingest resume cursor, the GLiNER delta (which marks
//! zero-entity chunks processed), the `note_already_current` skip, and
//! the per-note RAPTOR `input_hash` checkpoint. A build-time number from
//! a warm tree is not a build-time number.
//!
//! So `--cold` is not a convenience flag — it is the documented reset,
//! executed and reported: the corpus index dir is removed, and the SQLite
//! tiered state (`conv_raptor_nodes`, `conv_motifs`, `conv_skeletons`,
//! `chunk_entities`, `chunk_entity_progress`) plus `vault_themes` are
//! cleared. `LocalCorpusManager::reset_enrichment_state` does NOT do this
//! — it is status-only, and a run after it is warm.
//!
//! The harness refuses to run without an explicit `--cold` or `--warm`,
//! and every report carries `notes_skipped_already_current`. A run that
//! claims to be cold and skipped notes is self-evidently not, from the
//! report alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use corpus_engine::enrichment::tiered::{
    run_folder_tiered_enrichment, ChunkEntityExtractor, ChunkEntityExtractorHandle, ConvBucket,
    TieredEnrichmentProvider, TieredProviderHandle,
};
use corpus_engine::{EnrichmentChunkRow, Result as EngineResult};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Speed;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::local_corpus::progress::LocalCorpusProgress;
use sovereign_tools::local_corpus::{LocalCorpusConfig, LocalCorpusManager};

use crate::bench_cmd::resource_meter::{MeteredInference, ResourceLedger, ResourceReport};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::default_globals_for_voice_eval;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench vault-report",
    summary: "Raw folder → queryable-enriched corpus, timed. Reports time_to_rag_ready and time_to_enriched with a per-phase and per-note breakdown.",
    sections: &[
        HelpSection::Usage(
            "svrn bench vault-report (--corpus-id <id> | --folder <path>) (--cold | --warm) \
             [--enrich-model <id>] [--no-gliner] [--allow-watcher] \
             [--output <path>] [--compare <timings.json>]",
        ),
        HelpSection::Flags(&[
            (
                "--corpus-id <id>",
                "Measure an already-registered folder corpus. This is the usual form — \
                 the corpus keeps its identity, so the number is comparable run to run.",
            ),
            (
                "--folder <path>",
                "Register the folder as a new document-folder corpus first, then measure it. \
                 Use for a fixture vault that is not installed yet.",
            ),
            (
                "--cold",
                "Delete this corpus's index dir AND its SQLite tiered state (raptor nodes, \
                 motifs, skeletons, chunk entities, entity progress, vault themes) before \
                 measuring. THIS IS THE ONLY MODE THAT PRODUCES A BASELINE — see the module \
                 docs for the four checkpoint layers a warm run short-circuits on.",
            ),
            (
                "--warm",
                "Measure without resetting. Legitimate for 'how long does a no-op re-sweep \
                 take', and for nothing else. The report will show most notes skipped.",
            ),
            (
                "--enrich-model <id>",
                "Serve the enrichment pipeline from a specific model instead of the daemon's \
                 primary. The report records both.",
            ),
            (
                "--no-gliner",
                "Skip the local NER pass entirely, leaving RAPTOR-only entities. The A/B \
                 partner for measuring what the NER phase actually costs.",
            ),
            (
                "--allow-watcher",
                "Proceed even though the target is a watched folder and the daemon is live. \
                 Without this the harness refuses: the daemon's sweeper and its boot-time \
                 resume_interrupted_enrichment will both write to the corpus you are timing.",
            ),
            (
                "--output <path>",
                "Extra copy of timings.json. Default: \
                 ~/.sovereign/bench-runs/vault-report/<ts>/timings.json.",
            ),
            (
                "--compare <timings.json>",
                "Print a phase-by-phase delta table against a prior run.",
            ),
        ]),
        HelpSection::Notes(
            "The harness runs the real folder pipeline in-process — LocalCorpusManager::ingest \
             for tier 1, run_folder_tiered_enrichment for tiers 2/3 — with the inference \
             provider, entity extractor and tiered provider each wrapped in a metering \
             decorator. No production code is instrumented; every number comes from a seam \
             the pipeline already exposes.",
        ),
        HelpSection::Notes(
            "Reading the report: notes_skipped_already_current is the cold-run truth-teller. \
             A --cold run should report 0. Anything else means state survived the reset and \
             the totals understate the real build.",
        ),
    ],
};

// ─────────────────────────────────────────────────────────────────────
// Report shape
// ─────────────────────────────────────────────────────────────────────

/// One phase's wall-clock span, measured first-start → last-end.
///
/// `ms` is a real elapsed duration, not a sum of concurrent work —
/// the distinction `resource_meter`'s module docs make about
/// `llm_wall_ms` applies here too. For the per-note RAPTOR phase the
/// separate `notes.sum_ms` records the summed cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSpan {
    pub phase: String,
    /// Start, ms since run start.
    pub start_ms: u64,
    /// End, ms since run start.
    pub end_ms: u64,
    /// `end_ms - start_ms`.
    pub ms: u64,
    /// Free-form per-phase detail (file counts, mention counts, …).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub detail: serde_json::Value,
}

/// One recorded ingest-side progress transition, elapsed-stamped. The
/// tier-1 analogue of `book_report`'s `StateTransition`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestTransition {
    pub ms_since_start: u64,
    pub phase: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub detail: serde_json::Value,
}

/// What happened to one note (one source document) during enrichment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteRecord {
    pub doc_id: String,
    pub chunks: usize,
    pub bucket: String,
    pub ms: u64,
    /// Start, ms since run start — recovers the concurrency picture.
    pub start_ms: u64,
    /// `built` · `skipped_already_current` · `failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Rollup over [`NoteRecord`]s. Mirrors the counters
/// `svrn enrich raptor` prints, which is the accounting shape the
/// operator already reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoteSummary {
    pub total: usize,
    pub built: usize,
    /// The cold-run truth-teller. A `--cold` run reporting non-zero here
    /// did not actually start cold.
    pub skipped_already_current: usize,
    pub failed: usize,
    /// Summed per-note cost. Exceeds the RAPTOR phase span when notes
    /// run concurrently; equals it when they do not.
    pub sum_ms: u64,
    pub median_ms: u64,
    pub mean_ms: u64,
    pub p90_ms: u64,
    pub max_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_doc_id: Option<String>,
}

/// What `--cold` actually deleted. Recorded in the report so a baseline
/// carries its own reset provenance — a number whose reset procedure
/// isn't written down is not reproducible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColdReset {
    pub index_dir: String,
    pub index_dir_existed: bool,
    pub tiered_state_cleared: bool,
    pub vault_themes_cleared: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn motif_path_built() -> String {
    "built".to_string()
}

/// The persisted run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultReportRun {
    pub schema: String,
    pub bench_id: String,
    pub started_at_unix: u64,
    pub corpus_id: String,
    pub root_path: String,
    /// `cold` or `warm`. A comparison across modes is meaningless;
    /// `--compare` refuses it.
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_reset: Option<ColdReset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrich_model: Option<String>,
    pub embed_model: String,
    /// `gliner` · `disabled` · `unavailable`. The routing truth-teller
    /// for the NER phase — trust it over the flag you passed.
    pub entity_path: String,
    /// `built` · `skipped` · `removed`. The T3 motif index's routing
    /// truth-teller, and now a historical marker: `built` = a run from
    /// before 2026-08-02, `skipped` = one of the `--no-motifs` ablation
    /// arms that settled it, `removed` = the pass no longer exists on
    /// the folder path. Defaults to `built` so runs recorded before the
    /// field existed — the 2026-08-02 cold baseline among them — still
    /// deserialize under `--compare`.
    #[serde(default = "motif_path_built")]
    pub motif_path: String,

    pub files_indexed: usize,
    pub chunks_written: u64,
    pub documents_enriched: usize,
    pub entity_mentions: usize,

    /// Folder → Lance index queryable. The retrieval-ready headline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_rag_ready_ms: Option<u64>,
    /// Folder → NER + RAPTOR + themes all persisted. The enrichment
    /// headline, and the number this initiative spends against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_enriched_ms: Option<u64>,
    pub total_ms: u64,

    pub phases: Vec<PhaseSpan>,
    pub ingest_transitions: Vec<IngestTransition>,
    pub notes: Vec<NoteRecord>,
    pub note_summary: NoteSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceReport>,
    pub terminated_at_phase: String,
}

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

    fn snapshot(&self) -> (Vec<PhaseSpan>, Vec<IngestTransition>, Vec<NoteRecord>, usize) {
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
    ) -> EngineResult<usize> {
        self.inner
            .extract_for_conversation(corpus_id, conv_uuid, chunks)
            .await
    }

    async fn extract_delta_for_corpus(
        &self,
        corpus_id: &str,
        index_path: &Path,
    ) -> EngineResult<usize> {
        let start = self.obs.now_ms();
        eprintln!("  [ner] corpus-wide entity extraction …");
        let out = self.inner.extract_delta_for_corpus(corpus_id, index_path).await;
        let end = self.obs.now_ms();
        let (mentions, detail) = match &out {
            Ok(n) => {
                self.obs.add_mentions(*n);
                (*n, serde_json::json!({ "mentions": n }))
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

    async fn reenrich_sources(&self, corpus_id: &str, source_doc_ids: &[String]) -> EngineResult<()> {
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

// ─────────────────────────────────────────────────────────────────────
// Args
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Cold,
    Warm,
}

impl Mode {
    fn label(&self) -> &'static str {
        match self {
            Mode::Cold => "cold",
            Mode::Warm => "warm",
        }
    }
}

#[derive(Debug, Default)]
struct Opts {
    corpus_id: Option<String>,
    folder: Option<PathBuf>,
    mode: Option<Mode>,
    enrich_model: Option<String>,
    no_gliner: bool,
    allow_watcher: bool,
    output: Option<PathBuf>,
    compare: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> std::result::Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut need = |label: &str| -> std::result::Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{label} needs a value"))
        };
        match a {
            "--corpus-id" => o.corpus_id = Some(need("--corpus-id")?),
            "--folder" => o.folder = Some(PathBuf::from(need("--folder")?)),
            "--cold" => o.mode = Some(Mode::Cold),
            "--warm" => o.mode = Some(Mode::Warm),
            "--enrich-model" => o.enrich_model = Some(need("--enrich-model")?),
            "--no-gliner" => o.no_gliner = true,
            "--allow-watcher" => o.allow_watcher = true,
            "--output" => o.output = Some(PathBuf::from(need("--output")?)),
            "--compare" => o.compare = Some(PathBuf::from(need("--compare")?)),
            other => return Err(format!("unknown flag '{other}' (try --help)")),
        }
        i += 1;
    }

    if o.corpus_id.is_none() && o.folder.is_none() {
        return Err("need --corpus-id <id> or --folder <path>".to_string());
    }
    if o.corpus_id.is_some() && o.folder.is_some() {
        return Err("--corpus-id and --folder are mutually exclusive".to_string());
    }
    if o.mode.is_none() {
        return Err(
            "need --cold or --warm. A build-time number is only meaningful from a cold tree \
             (four checkpoint layers short-circuit a warm one); requiring the choice keeps a \
             warm run from being baselined by accident. See --help."
                .to_string(),
        );
    }
    Ok(o)
}

// ─────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────

pub async fn cmd_vault_report(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&HELP);
        return 0;
    }
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let compare = opts.compare.clone();
    let output = opts.output.clone();
    match run(opts).await {
        Ok(report) => {
            print_summary(&report);
            if let Err(e) = persist_report(&report, output.as_deref()) {
                eprintln!("warning: persist report: {e}");
            }
            if let Some(baseline) = compare {
                if let Err(e) = print_delta(&report, &baseline) {
                    eprintln!("compare: {e}");
                    return 1;
                }
            }
            if report.note_summary.failed > 0 {
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn data_dir() -> PathBuf {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
}

async fn run(opts: Opts) -> std::result::Result<VaultReportRun, String> {
    let mode = opts.mode.clone().expect("parse_args guarantees a mode");
    let data_dir = data_dir();
    let indexes_dir = data_dir.join("indexes");
    let recipes_dir = data_dir.join("recipes");

    eprintln!("svrn bench vault-report");
    eprintln!("  data dir:  {}", data_dir.display());
    eprintln!("  mode:      {}", mode.label());

    // ── Session: inference over HTTP to the daemon, pipeline in-process ──
    eprintln!("[1/5] daemon session");
    let globals = default_globals_for_voice_eval();
    let session = build_session(&globals)
        .await
        .map_err(|e| format!("daemon bootstrap failed: {e}. Is the daemon running?"))?;

    let ledger = Arc::new(ResourceLedger::new());
    let enrich_base: Arc<dyn InferenceProvider> = match &opts.enrich_model {
        Some(model) => {
            eprintln!("      enrich model override: {model}");
            crate::bench_cmd::book_report::provider_for_model(
                &globals.daemon_base,
                model,
                &session.embed_model,
            )
            .await
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
    let inference_fn =
        sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&enrich_inference));
    let engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir.clone(), indexes_dir.clone(), embed_fn)
            .with_embedding_model(&session.embed_model)
            .with_batch_embed_fn(batch_embed_fn)
            .with_inference_fn(inference_fn),
    );

    let lc_store: Arc<dyn sovereign_core::traits::StateStore> =
        Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let manager = LocalCorpusManager::init_with_recipes_dir(
        Arc::clone(&engine),
        lc_store,
        None,
        data_dir.clone(),
        data_dir.join("vault-snapshots"),
        recipes_dir,
    )
    .await
    .map_err(|e| format!("LocalCorpusManager init: {e}"))?;

    // ── Resolve the corpus ──
    let config: LocalCorpusConfig = match (&opts.corpus_id, &opts.folder) {
        (Some(id), _) => manager
            .get(id)
            .await
            .ok_or_else(|| {
                format!(
                    "no registered corpus '{id}'. `svrn corpus status` lists them; use --folder \
                     <path> to register a new one."
                )
            })?
            .clone(),
        (None, Some(path)) => {
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
        (None, None) => unreachable!("parse_args guarantees one of the two"),
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
    if is_watched && !opts.allow_watcher {
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
    let cold_reset = if mode == Mode::Cold {
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
        build_entity_extractor(&opts, Arc::clone(&store), Arc::clone(&obs));

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
    let (mut phases, ingest_transitions, notes, entity_mentions) = obs.snapshot();
    if let Some(span) = raptor_span(&notes) {
        phases.push(span);
    }
    phases.sort_by_key(|p| p.start_ms);
    let note_summary = summarise_notes(&notes);

    Ok(VaultReportRun {
        schema: "vault-report/v1".to_string(),
        bench_id: run_id(),
        started_at_unix: unix_now(),
        corpus_id,
        root_path,
        mode: mode.label().to_string(),
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
        note_summary,
        resources: Some(ledger.snapshot()),
        terminated_at_phase: "complete".to_string(),
    })
}

/// Build the NER extractor, wrapped so the phase gets a timer.
/// Returns the routing truth-teller alongside it — what the run
/// actually used, not what was asked for.
fn build_entity_extractor(
    opts: &Opts,
    store: Arc<SqliteStateStore>,
    obs: Arc<BuildObserver>,
) -> (Option<ChunkEntityExtractorHandle>, String) {
    if opts.no_gliner {
        return (None, "disabled".to_string());
    }
    // Same selector the daemon uses (`SOVEREIGN_GLINER_MODEL_ID`), so a
    // measured run and a production run cannot disagree about which
    // backend they got — that equality is the whole point of measuring
    // through this harness rather than a bespoke probe.
    let model_id = sovereign_gliner::configured_model_id();
    if !sovereign_gliner::gliner_ner::probe_model_available(&model_id) {
        return (None, format!("unavailable ({model_id} not installed)"));
    }
    // Eager, not lazy: a lazy extractor that isn't warm yet returns
    // empty and the run would silently measure a no-op NER phase.
    match sovereign_gliner::load_labeled_extractor(&model_id, None) {
        Ok(g) => {
            // The routing string names the GENERATION, not just the id:
            // "did this run use GLiNER2?" is the question every P2.1
            // number is read against, and it must be answerable from
            // the report alone.
            let routed = format!("gliner {:?} ({model_id})", g.generation());
            let base = sovereign_gliner::GlinerChunkExtractor::new(store, g).into_handle();
            let metered: ChunkEntityExtractorHandle =
                Arc::new(MeteredEntityExtractor { inner: base, obs });
            (Some(metered), routed)
        }
        Err(e) => (None, format!("unavailable (load failed: {e})")),
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
        return Err(format!("source folder {} is not a directory", root.display()));
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
        LocalCorpusProgress::OcrPage { file_idx, file_total, .. } => (
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

/// Wall-clock span of the per-note RAPTOR phase: first note start →
/// last note end. Deliberately not the sum — notes may overlap, and a
/// sum of concurrent work is not a duration. `NoteSummary::sum_ms`
/// carries the summed cost separately.
fn raptor_span(notes: &[NoteRecord]) -> Option<PhaseSpan> {
    let built: Vec<&NoteRecord> = notes.iter().filter(|n| n.outcome != "skipped_already_current").collect();
    if built.is_empty() {
        return None;
    }
    let start = built.iter().map(|n| n.start_ms).min()?;
    let end = built.iter().map(|n| n.start_ms + n.ms).max()?;
    Some(PhaseSpan {
        phase: "raptor_per_note".to_string(),
        start_ms: start,
        end_ms: end,
        ms: end.saturating_sub(start),
        detail: serde_json::json!({ "notes": built.len() }),
    })
}

fn summarise_notes(notes: &[NoteRecord]) -> NoteSummary {
    let mut s = NoteSummary {
        total: notes.len(),
        ..Default::default()
    };
    let mut built_ms: Vec<u64> = Vec::new();
    let mut slowest: Option<(&str, u64)> = None;
    for n in notes {
        match n.outcome.as_str() {
            "built" => {
                s.built += 1;
                built_ms.push(n.ms);
                s.sum_ms += n.ms;
                if slowest.map(|(_, m)| n.ms > m).unwrap_or(true) {
                    slowest = Some((n.doc_id.as_str(), n.ms));
                }
            }
            "skipped_already_current" => s.skipped_already_current += 1,
            _ => s.failed += 1,
        }
    }
    if !built_ms.is_empty() {
        built_ms.sort_unstable();
        s.median_ms = built_ms[built_ms.len() / 2];
        s.mean_ms = s.sum_ms / built_ms.len() as u64;
        let p90_idx = ((built_ms.len() as f64 * 0.9).ceil() as usize).saturating_sub(1);
        s.p90_ms = built_ms[p90_idx.min(built_ms.len() - 1)];
        s.max_ms = *built_ms.last().unwrap_or(&0);
        s.slowest_doc_id = slowest.map(|(d, _)| d.to_string());
    }
    s
}

// ─────────────────────────────────────────────────────────────────────
// Rendering + persistence
// ─────────────────────────────────────────────────────────────────────

fn secs(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn print_summary(r: &VaultReportRun) {
    println!();
    println!("vault build report — {} ({})", r.corpus_id, r.mode);
    println!("  folder:  {}", r.root_path);
    println!(
        "  scope:   {} file(s) · {} chunk(s) · {} document(s) · {} entity mention(s)",
        r.files_indexed, r.chunks_written, r.documents_enriched, r.entity_mentions
    );
    println!("  models:  enrich={} embed={}", r.enrich_model.as_deref().unwrap_or("-"), r.embed_model);
    println!("  ner:     {}", r.entity_path);
    println!("  motifs:  {}", r.motif_path);
    println!();
    println!("  TIME TO RAG READY   {}", r.time_to_rag_ready_ms.map(secs).unwrap_or_else(|| "-".into()));
    println!("  TIME TO ENRICHED    {}", r.time_to_enriched_ms.map(secs).unwrap_or_else(|| "-".into()));
    println!();

    println!("  phase                     start      elapsed     share");
    println!("  ─────────────────────────────────────────────────────────");
    let total = r.time_to_enriched_ms.unwrap_or(r.total_ms).max(1);
    for p in &r.phases {
        println!(
            "  {:<22}  {:>8}  {:>10}  {:>6.1}%",
            p.phase,
            secs(p.start_ms),
            secs(p.ms),
            p.ms as f64 * 100.0 / total as f64
        );
    }
    println!();

    let n = &r.note_summary;
    println!("  notes: {} total · {} built · {} skipped(already current) · {} failed", n.total, n.built, n.skipped_already_current, n.failed);
    if n.built > 0 {
        println!(
            "         per-note median {} · mean {} · p90 {} · max {}{}",
            secs(n.median_ms),
            secs(n.mean_ms),
            secs(n.p90_ms),
            secs(n.max_ms),
            n.slowest_doc_id
                .as_deref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        );
        println!("         summed per-note cost {}", secs(n.sum_ms));
    }
    if r.mode == "cold" && n.skipped_already_current > 0 {
        println!();
        println!(
            "  WARNING: a --cold run skipped {} note(s) as already-current. State survived the \
             reset; these totals UNDERSTATE the real build and must not be baselined.",
            n.skipped_already_current
        );
    }
    if let Some(cr) = &r.cold_reset {
        if !cr.warnings.is_empty() {
            println!();
            println!("  WARNING: cold reset was incomplete:");
            for w in &cr.warnings {
                println!("    - {w}");
            }
        }
    }
    if let Some(res) = &r.resources {
        println!();
        println!("  resources (per-phase LLM/embed ledger)");
        for p in &res.phases {
            println!(
                "    {:<16} calls {:>4} · prompt {:>8} tok · completion {:>7} tok · embeds {:>5} ({} texts)",
                p.phase,
                p.bucket.llm_calls,
                p.bucket.prompt_tokens,
                p.bucket.completion_tokens,
                p.bucket.embed_calls,
                p.bucket.embed_texts
            );
        }
    }
    println!();
}

fn print_delta(r: &VaultReportRun, baseline: &Path) -> std::result::Result<(), String> {
    let raw = std::fs::read_to_string(baseline)
        .map_err(|e| format!("read {}: {e}", baseline.display()))?;
    let base: VaultReportRun =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", baseline.display()))?;
    if base.mode != r.mode {
        return Err(format!(
            "baseline is a '{}' run and this is a '{}' run — the comparison would be \
             meaningless (a warm run short-circuits most of the work)",
            base.mode, r.mode
        ));
    }
    println!("  delta vs {} ({})", baseline.display(), base.corpus_id);
    println!("  phase                     baseline       this        Δ");
    println!("  ─────────────────────────────────────────────────────────");
    let mut base_by_phase: BTreeMap<&str, u64> = BTreeMap::new();
    for p in &base.phases {
        *base_by_phase.entry(p.phase.as_str()).or_insert(0) += p.ms;
    }
    for p in &r.phases {
        let b = base_by_phase.get(p.phase.as_str()).copied();
        match b {
            Some(b) => println!(
                "  {:<22}  {:>10}  {:>9}  {:>+8.1}s",
                p.phase,
                secs(b),
                secs(p.ms),
                (p.ms as f64 - b as f64) / 1000.0
            ),
            None => println!("  {:<22}  {:>10}  {:>9}  {:>9}", p.phase, "-", secs(p.ms), "new"),
        }
    }
    let hb = base.time_to_enriched_ms.unwrap_or(base.total_ms);
    let hn = r.time_to_enriched_ms.unwrap_or(r.total_ms);
    println!(
        "  {:<22}  {:>10}  {:>9}  {:>+8.1}s",
        "TIME TO ENRICHED",
        secs(hb),
        secs(hn),
        (hn as f64 - hb as f64) / 1000.0
    );
    Ok(())
}

fn persist_report(
    r: &VaultReportRun,
    explicit_output: Option<&Path>,
) -> std::result::Result<(), String> {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign/bench-runs/vault-report")
        .join(&r.bench_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let json_path = dir.join("timings.json");
    let json = serde_json::to_string_pretty(r).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&json_path, &json).map_err(|e| format!("write {}: {e}", json_path.display()))?;
    eprintln!("      timings: {}", json_path.display());
    if let Some(extra) = explicit_output {
        if extra != json_path {
            std::fs::write(extra, &json)
                .map_err(|e| format!("write {}: {e}", extra.display()))?;
            eprintln!("      timings: {}", extra.display());
        }
    }
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn run_id() -> String {
    format!("{}", unix_now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(doc: &str, ms: u64, start: u64, outcome: &str) -> NoteRecord {
        NoteRecord {
            doc_id: doc.to_string(),
            chunks: 10,
            bucket: "small".to_string(),
            ms,
            start_ms: start,
            outcome: outcome.to_string(),
            error: None,
        }
    }

    #[test]
    fn mode_is_required_so_a_warm_run_is_never_baselined_by_accident() {
        let err = parse_args(&["--corpus-id".into(), "x".into()]).unwrap_err();
        assert!(err.contains("--cold or --warm"), "got: {err}");
    }

    #[test]
    fn corpus_and_folder_are_mutually_exclusive() {
        let err = parse_args(&[
            "--corpus-id".into(),
            "x".into(),
            "--folder".into(),
            "/tmp".into(),
            "--cold".into(),
        ])
        .unwrap_err();
        assert!(err.contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn summary_separates_built_from_skipped() {
        let notes = vec![
            note("a", 1000, 0, "built"),
            note("b", 3000, 10, "built"),
            note("c", 5, 20, "skipped_already_current"),
            note("d", 100, 30, "failed"),
        ];
        let s = summarise_notes(&notes);
        assert_eq!(s.total, 4);
        assert_eq!(s.built, 2);
        assert_eq!(s.skipped_already_current, 1);
        assert_eq!(s.failed, 1);
        // Skipped and failed notes must not enter the cost stats — the
        // whole point of the split is that a skip is not a build.
        assert_eq!(s.sum_ms, 4000);
        assert_eq!(s.mean_ms, 2000);
        assert_eq!(s.max_ms, 3000);
        assert_eq!(s.slowest_doc_id.as_deref(), Some("b"));
    }

    #[test]
    fn raptor_span_is_a_duration_not_a_sum() {
        // Two fully-overlapping 3s notes span 3s of wall-clock, not 6s.
        let notes = vec![note("a", 3000, 0, "built"), note("b", 3000, 0, "built")];
        let span = raptor_span(&notes).expect("built notes yield a span");
        assert_eq!(span.ms, 3000);
        assert_eq!(summarise_notes(&notes).sum_ms, 6000);
    }

    #[test]
    fn raptor_span_is_none_when_everything_was_skipped() {
        let notes = vec![note("a", 5, 0, "skipped_already_current")];
        assert!(raptor_span(&notes).is_none());
    }

    #[test]
    fn embed_phase_is_not_filed_under_index_building() {
        // The engine renders IngestProgress::Embedding as "Building the
        // index". Keying on the prose would put embedding cost — usually
        // the biggest slice of tier 1 — under a row an operator reads as
        // index construction, and the two would be indistinguishable.
        assert_eq!(stable_ingest_phase("Building the index"), "ingest:embed");
        assert_eq!(stable_ingest_phase("Writing index"), "ingest:index_write");
        assert_eq!(
            stable_ingest_phase("Optimizing search index"),
            "ingest:index_ann_build"
        );
    }

    #[test]
    fn completion_label_does_not_smuggle_its_own_duration_into_the_key() {
        // "Done in 7s" as a key means run A and run B never share a
        // phase name, which would silently break --compare.
        assert_eq!(stable_ingest_phase("Done in 0s"), "ingest:done");
        assert_eq!(stable_ingest_phase("Done in 431s"), "ingest:done");
    }

    #[test]
    fn unknown_engine_labels_are_bucketed_not_minted() {
        // The Enriching arm forwards a free-form detail string; letting
        // it through would grow an unbounded set of phase names.
        assert_eq!(
            stable_ingest_phase("clustering pass 3 of 9"),
            "ingest:engine_enrich_hook"
        );
    }

    #[test]
    fn observer_closes_a_phase_when_the_label_changes() {
        let obs = BuildObserver::new();
        obs.ingest_transition("scanning", serde_json::Value::Null);
        obs.ingest_transition("scanning", serde_json::Value::Null);
        obs.ingest_transition("staging", serde_json::Value::Null);
        obs.close_open_ingest();
        let (phases, transitions, _, _) = obs.snapshot();
        assert_eq!(transitions.len(), 3, "every transition is logged");
        let labels: Vec<&str> = phases.iter().map(|p| p.phase.as_str()).collect();
        assert_eq!(
            labels,
            vec!["scanning", "staging"],
            "repeat labels extend the open span rather than opening a new one"
        );
    }

    #[test]
    fn unobserved_time_never_inflates_the_previous_phase() {
        // The failure this guards against, measured on a real fixture
        // run: `staging` emitted its last event at 7ms, the next event
        // arrived at 8517ms, and the intervening extract+embed work was
        // reported as 8.5s of staging.
        let obs = BuildObserver::new();
        obs.ingest_transition_at(0, "staging", serde_json::Value::Null);
        obs.ingest_transition_at(7, "staging", serde_json::Value::Null);
        obs.ingest_transition_at(8517, "ingest:index_write", serde_json::Value::Null);
        let (phases, _, _, _) = obs.snapshot();
        let staging = phases.iter().find(|p| p.phase == "staging").expect("staging span");
        assert_eq!(staging.ms, 7, "staging ends at its own last event");
        let gap = phases
            .iter()
            .find(|p| p.phase == "unattributed:after:staging")
            .expect("the unobserved window is reported, not absorbed");
        assert_eq!(gap.start_ms, 7);
        assert_eq!(gap.end_ms, 8517);
    }

    #[test]
    fn preflight_refuses_an_unreadable_source_before_anything_is_deleted() {
        // A path that stats but cannot be listed is the macOS TCC shape,
        // and it is the case that would turn a cold reset into data loss.
        // A nonexistent path exercises the same refusal.
        let err = preflight_source_readable(
            Path::new("/definitely/not/a/real/vault/path"),
            &["md".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn preflight_refuses_a_readable_but_empty_source() {
        let dir = std::env::temp_dir().join("vault-report-preflight-empty");
        let _ = std::fs::create_dir_all(&dir);
        let err = preflight_source_readable(&dir, &["md".to_string()]).unwrap_err();
        assert!(err.contains("REFUSING TO RESET"), "got: {err}");
        assert!(err.contains("0 ingestible file(s)"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_refuses_a_directory_that_stats_but_cannot_be_listed() {
        // This is the TCC shape precisely: the path exists, `is_dir()`
        // is true, there ARE matching files inside — and `read_dir`
        // fails. Any guard written against `exists()`/`is_dir()` passes
        // here and proceeds to delete the corpus.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("vault-report-preflight-unreadable");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("real-note.md"), "content").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        let listable = std::fs::read_dir(&dir).is_ok();
        if listable {
            // Running as root (or a filesystem ignoring mode bits) —
            // the precondition doesn't hold, so the test proves nothing.
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        assert!(dir.exists() && dir.is_dir(), "the naive checks still pass");
        let err = preflight_source_readable(&dir, &["md".to_string()]).unwrap_err();
        assert!(err.contains("REFUSING TO RESET"), "got: {err}");
        assert!(
            err.contains("could not be read"),
            "the refusal must name the read failure, not just report an empty folder: {err}"
        );

        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_counts_matching_files_and_ignores_dot_dirs() {
        let dir = std::env::temp_dir().join("vault-report-preflight-ok");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        // .obsidian holds config, never corpus content — counting it
        // would let an all-config folder pass the guard.
        std::fs::create_dir_all(dir.join(".obsidian")).unwrap();
        std::fs::write(dir.join("a.md"), "x").unwrap();
        std::fs::write(dir.join("sub/b.md"), "x").unwrap();
        std::fs::write(dir.join("c.txt"), "x").unwrap();
        std::fs::write(dir.join(".obsidian/app.md"), "x").unwrap();
        let n = preflight_source_readable(&dir, &["md".to_string()]).unwrap();
        assert_eq!(n, 2, "nested markdown counts; dot-dirs and other extensions do not");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sub_threshold_gaps_are_not_reported_as_phases() {
        // Callback scheduling jitter between two adjacent phases is not
        // a finding; only a real observation hole is.
        let obs = BuildObserver::new();
        obs.push_gap("staging", 100, 100 + UNATTRIBUTED_GAP_MS - 1);
        let (phases, _, _, _) = obs.snapshot();
        assert!(phases.is_empty(), "got: {phases:?}");
    }
}
