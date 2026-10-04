// SPDX-License-Identifier: AGPL-3.0-or-later
//! The folder-vault build probe's evidence (pb-bench-dials-vault).
//!
//! svrn registers or resolves a folder corpus, optionally resets it cold,
//! runs the folder pipeline (ingest, NER, per-note RAPTOR, vault synthesis)
//! with every seam metered, and writes what it observed. bench rolls it up,
//! renders and persists it (principle 12).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::ResourceReport;

/// What the vault-build probe builds, and how. Each field is one of
/// `svrn bench vault-report`'s flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultBuildProbe {
    /// The folder corpus to build.
    pub source: VaultSource,
    /// Reset the corpus's index dir and tiered state first (`--cold`);
    /// false is `--warm`.
    pub cold: bool,
    /// Serve enrichment from this model instead of the daemon's primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrich_model: Option<String>,
    /// Skip the local NER pass.
    pub no_gliner: bool,
    /// Proceed on a watched folder while the daemon is live.
    pub allow_watcher: bool,
}

/// Which folder corpus the vault-build probe builds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VaultSource {
    /// An already-registered folder corpus (`--corpus-id`).
    Corpus {
        /// Its id.
        corpus_id: String,
    },
    /// A folder registered as a new document-folder corpus first (`--folder`).
    Folder {
        /// The folder.
        path: PathBuf,
    },
}

/// What one vault build observed: its scope, its span and note records, and
/// the ledger. bench adds the rollups (note summary, the per-note RAPTOR
/// span) and the run's identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultBuildEvidence {
    /// The corpus built.
    pub corpus_id: String,
    /// Its folder.
    pub root_path: String,
    /// What `--cold` deleted; `None` on a warm run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_reset: Option<ColdReset>,
    /// The enrichment model the build ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrich_model: Option<String>,
    /// The embedding model.
    pub embed_model: String,
    /// `gliner …` · `disabled` · `unavailable …`: the NER path actually taken.
    pub entity_path: String,
    /// The motif pass's marker (`removed`).
    pub motif_path: String,
    /// Files the ingest indexed.
    pub files_indexed: usize,
    /// Chunks the ingest wrote.
    pub chunks_written: u64,
    /// Documents the enrichment planned.
    pub documents_enriched: usize,
    /// NER mentions extracted.
    pub entity_mentions: usize,
    /// Folder → Lance index queryable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_rag_ready_ms: Option<u64>,
    /// Folder → NER + RAPTOR + themes persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_enriched_ms: Option<u64>,
    /// The whole build.
    pub total_ms: u64,
    /// Observed phase spans, in the order they closed.
    pub phases: Vec<PhaseSpan>,
    /// Every ingest progress transition.
    pub ingest_transitions: Vec<IngestTransition>,
    /// One record per note built, skipped or failed.
    pub notes: Vec<NoteRecord>,
    /// The per-phase LLM/embed ledger.
    pub resources: ResourceReport,
}
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
