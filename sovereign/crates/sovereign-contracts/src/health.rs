// SPDX-License-Identifier: AGPL-3.0-or-later
//! Health types, the `HealthCheckable` trait, and related utilities.
//!
//! All serialisable structs use `i64` Unix-second timestamps and `u64`
//! millisecond durations so they survive a serde round-trip without
//! pulling in `chrono` at every call site.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::Result;

// ─── Component ──────────────────────────────────────────────────────────────

/// A named subsystem that can be health-checked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Component {
    /// A single corpus index (id = corpus_id string).
    CorpusIndex(String),
    /// The enrichment pipeline for a corpus (id = corpus_id).
    Enrichment(String),
    /// The SQLite / Postgres state store.
    StateStore,
    /// The LLM router / circuit breaker.
    LlmRouter,
}

impl Component {
    /// Stable human-readable name in `kind/id` form (e.g. `corpus-index/sep`), for logs and UI listings.
    pub fn display_name(&self) -> String {
        match self {
            Self::CorpusIndex(id) => format!("corpus-index/{id}"),
            Self::Enrichment(id) => format!("enrichment/{id}"),
            Self::StateStore => "state-store".into(),
            Self::LlmRouter => "llm-router".into(),
        }
    }
}

// ─── HealthStatus ────────────────────────────────────────────────────────────

/// Rolled-up status for a component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// No issues, or advisory-only ones (`UpdateAvailable`).
    Healthy,
    /// Operational but impaired — the component still serves, repairs may be queued.
    Degraded,
    /// Hard failure; results from the component can't be trusted until repaired.
    Unhealthy,
}

/// Return the worst (highest-severity) status in `statuses`.
/// Returns `Healthy` when `statuses` is empty.
pub fn worst_status(statuses: impl IntoIterator<Item = HealthStatus>) -> HealthStatus {
    statuses.into_iter().max().unwrap_or(HealthStatus::Healthy)
}

// ─── SlotName ────────────────────────────────────────────────────────────────

/// Which inference / embed slot is referenced in an issue.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotName {
    /// The main chat/completion slot.
    Primary,
    /// The embedding slot.
    Embed,
    /// The fallback completion slot that serves while the primary circuit is open.
    Fallback,
    /// Any other slot, carrying its configured name.
    Custom(String),
}

// ─── UpdateDelta ─────────────────────────────────────────────────────────────

/// Counts of how many documents would change in an update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDelta {
    /// Documents upstream that are absent locally.
    pub new_documents: usize,
    /// Documents whose upstream content changed.
    pub updated_documents: usize,
    /// Local documents removed upstream.
    pub deleted_documents: usize,
}

// ─── HealthIssue ─────────────────────────────────────────────────────────────

/// A concrete problem found during a health check.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HealthIssue {
    // ── Corpus index issues ──────────────────────────────────────────────────
    /// Ingest was interrupted; fewer chunks were written than expected.
    PartialIngestion {
        /// Corpus whose ingest was interrupted.
        corpus_id: String,
        /// Chunks actually written before the interruption.
        chunks_indexed: u64,
        /// Chunks the ingest planned to write.
        chunks_expected: u64,
        /// Saved ingest cursor to resume at; `None` = restart from scratch.
        resume_from: Option<String>,
    },
    /// The active embed slot's model / dimensions differ from what the index
    /// was built with.
    EmbedModelMismatch {
        /// Corpus whose index carries the mismatch.
        corpus_id: String,
        /// Embed model the index was built with.
        index_model: String,
        /// Embed model currently active in the embed slot.
        active_model: String,
        /// Vector dimensions stored in the index.
        index_dims: usize,
        /// Vector dimensions the active model produces.
        active_dims: usize,
    },
    /// A sample of stored embeddings contains all-zero or non-finite vectors.
    CorruptEmbeddings {
        /// Corpus containing the bad vectors.
        corpus_id: String,
        /// Chunk ids from the sample whose vectors are all-zero or non-finite (not exhaustive).
        bad_chunk_ids: Vec<u64>,
    },
    /// The full-text search index is out of sync with the chunk store.
    FtsDesync {
        /// Corpus with the desynced FTS index.
        corpus_id: String,
        /// Rows in the FTS index.
        fts_count: u64,
        /// Rows in the chunk store (the source of truth the FTS index should mirror).
        chunk_count: u64,
    },
    /// A newer version of the corpus dataset is available upstream.
    UpdateAvailable {
        /// Corpus with an upstream update.
        corpus_id: String,
        /// Installed dataset version.
        current_version: String,
        /// Newest version available upstream.
        latest_version: String,
        /// How many documents the update would add/change/delete.
        delta: UpdateDelta,
    },

    // ── Enrichment issues ────────────────────────────────────────────────────
    /// Some claims reference chunks whose content has changed.
    StaleEnrichment {
        /// Corpus whose enrichment is stale.
        corpus_id: String,
        /// Claims whose source chunk content changed since they were extracted.
        stale_claim_count: u64,
    },
    /// Some claims reference chunk IDs that no longer exist.
    OrphanedEnrichment {
        /// Corpus with the orphaned claims.
        corpus_id: String,
        /// Claims pointing at chunk ids that no longer exist.
        orphan_claim_count: u64,
    },
    /// Fewer chunks have been enriched than the coverage threshold requires.
    LowEnrichmentCoverage {
        /// Corpus below its coverage threshold.
        corpus_id: String,
        /// Chunks the enrichment pipeline has processed.
        enriched_chunks: u64,
        /// All chunks in the corpus.
        total_chunks: u64,
        /// Measured coverage, percent.
        coverage_pct: f32,
        /// Configured minimum coverage, percent.
        threshold_pct: f32,
    },
    /// An enrichment-requesting ingest died part-way and left a working
    /// directory behind that no installed-corpus listing can see.
    ///
    /// Distinct from [`Self::PartialIngestion`], which describes a corpus
    /// that IS installed and short of its expected chunk count. This one is
    /// about a directory that is not installed at all: `installed_indexes()`
    /// skips anything still flagged `ingestion_in_progress`, so before this
    /// variant existed the failure was reported by nobody — the corpus simply
    /// appeared absent, and the operator's only trace was a WARN in the
    /// daemon log at the moment it happened.
    /// `docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §3 traces one such install
    /// end to end.
    IncompleteIngestPartition {
        /// Corpus the dead ingest was building, read from its own meta rather
        /// than parsed out of the directory name (which carries a partition
        /// suffix, e.g. `<corpus_id>-partition-<node>`).
        corpus_id: String,
        /// The directory on disk, so the operator can find or remove it.
        path: String,
        /// True when the search indexes finished before the ingest died —
        /// the fingerprint of a failure in a LATE phase, enrichment being the
        /// one that runs after `build_indexes`. `false` points at a much
        /// earlier death, mid-embed.
        indexes_built: bool,
    },

    // ── Router issues ────────────────────────────────────────────────────────
    /// The circuit breaker for the primary LLM slot is open.
    RouterCircuitOpen {
        /// Consecutive failures that tripped the breaker.
        failure_count: u32,
        /// Most recent failure message — why the circuit opened.
        last_error: String,
        /// True when a fallback slot is serving; `false` makes this issue urgent (`is_urgent`).
        fallback_active: bool,
    },

    // ── Model integrity issues ───────────────────────────────────────────────
    /// A locally-cached model file's checksum does not match the manifest.
    ModelChecksumFailure {
        /// Slot whose cached model failed verification.
        slot: SlotName,
        /// Model whose file failed the check.
        model_id: String,
        /// Checksum the manifest declares.
        expected_hash: String,
        /// Checksum computed from the file on disk.
        actual_hash: String,
    },

    // ── State-store issues ───────────────────────────────────────────────────
    /// SQLite `PRAGMA integrity_check` returned a non-`ok` result.
    StateStoreCorruption {
        /// The non-`ok` result text the integrity check returned.
        detail: String,
    },
    /// The SQLite WAL file has grown past the safe threshold.
    WalOvergrowth {
        /// Current WAL size.
        wal_size_bytes: u64,
        /// Configured safe ceiling the WAL exceeded.
        threshold_bytes: u64,
    },
}

impl HealthIssue {
    /// The severity implied by this issue type.
    pub fn implied_status(&self) -> HealthStatus {
        match self {
            // Hard failures → Unhealthy
            Self::EmbedModelMismatch { .. }
            | Self::CorruptEmbeddings { .. }
            | Self::StateStoreCorruption { .. }
            | Self::ModelChecksumFailure { .. } => HealthStatus::Unhealthy,

            // Operational but impaired → Degraded
            Self::PartialIngestion { .. }
            | Self::FtsDesync { .. }
            | Self::StaleEnrichment { .. }
            | Self::OrphanedEnrichment { .. }
            | Self::LowEnrichmentCoverage { .. }
            | Self::IncompleteIngestPartition { .. }
            | Self::RouterCircuitOpen { .. }
            | Self::WalOvergrowth { .. } => HealthStatus::Degraded,

            // Informational → Healthy (just advisory)
            Self::UpdateAvailable { .. } => HealthStatus::Healthy,
        }
    }

    /// True when the issue warrants immediate user attention or notification.
    pub fn is_urgent(&self) -> bool {
        matches!(
            self,
            Self::EmbedModelMismatch { .. }
                | Self::CorruptEmbeddings { .. }
                | Self::StateStoreCorruption { .. }
                | Self::ModelChecksumFailure { .. }
                | Self::RouterCircuitOpen {
                    fallback_active: false,
                    ..
                }
        )
    }

    /// Short machine-readable tag for this issue variant.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::PartialIngestion { .. } => "partial_ingestion",
            Self::EmbedModelMismatch { .. } => "embed_model_mismatch",
            Self::CorruptEmbeddings { .. } => "corrupt_embeddings",
            Self::FtsDesync { .. } => "fts_desync",
            Self::UpdateAvailable { .. } => "update_available",
            Self::StaleEnrichment { .. } => "stale_enrichment",
            Self::OrphanedEnrichment { .. } => "orphaned_enrichment",
            Self::LowEnrichmentCoverage { .. } => "low_enrichment_coverage",
            Self::IncompleteIngestPartition { .. } => "incomplete_ingest_partition",
            Self::RouterCircuitOpen { .. } => "router_circuit_open",
            Self::ModelChecksumFailure { .. } => "model_checksum_failure",
            Self::StateStoreCorruption { .. } => "state_store_corruption",
            Self::WalOvergrowth { .. } => "wal_overgrowth",
        }
    }
}

// ─── RecoveryAction ──────────────────────────────────────────────────────────

/// What the monitor should do to try to fix an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Re-embed all chunks in a corpus with the active model.
    ReEmbed {
        /// Corpus to re-embed.
        corpus_id: String,
    },
    /// Rebuild the FTS index for a corpus.
    RebuildFts {
        /// Corpus whose FTS index is rebuilt.
        corpus_id: String,
    },
    /// Resume an interrupted ingest from the saved cursor.
    ResumeIngestion {
        /// Corpus whose ingest is resumed.
        corpus_id: String,
        /// Saved cursor, from `HealthIssue::PartialIngestion::resume_from`.
        resume_from: String,
    },
    /// Delete stale / orphaned claim rows and re-enqueue extraction.
    RefreshEnrichment {
        /// Corpus whose claims are refreshed.
        corpus_id: String,
    },
    /// Apply a corpus dataset update (delta or full).
    ApplyCorpusUpdate {
        /// Corpus to update.
        corpus_id: String,
    },
    /// Probe the router to see if the circuit should be closed.
    ProbeRouter,
    /// Run `PRAGMA wal_checkpoint(TRUNCATE)` on the SQLite database.
    CheckpointWal,
    /// No automated repair available; requires user action.
    UserActionRequired {
        /// What the user should do, in plain instructions.
        guidance: String,
    },
}

// ─── RepairKind ──────────────────────────────────────────────────────────────

/// Canonical identifier for a repair type (used in the `pending_health_decisions` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairKind {
    /// Re-embed a corpus with the active model.
    ReEmbed,
    /// Rebuild a corpus's FTS index.
    RebuildFts,
    /// Resume an interrupted ingest from its saved cursor.
    ResumeIngestion,
    /// Drop stale/orphaned claims and re-enqueue extraction.
    RefreshEnrichment,
    /// Apply an available corpus dataset update.
    ApplyCorpusUpdate,
    /// Probe whether the router circuit can be closed.
    ProbeRouter,
    /// Checkpoint-truncate the SQLite WAL.
    CheckpointWal,
    /// No repair — the user chose to dismiss the issue.
    Dismiss,
}

// ─── UserOption ──────────────────────────────────────────────────────────────

/// A choice presented to the user when a repair requires authorisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOption {
    /// Which repair choosing this option triggers (`Dismiss` = do nothing).
    pub kind: RepairKind,
    /// Short button text.
    pub label: String,
    /// What the option will do, in a sentence the user can act on.
    pub description: String,
}

// ─── UserDecision ────────────────────────────────────────────────────────────

/// A user's response to a pending decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDecision {
    /// Row id of the `PendingDecision` being answered (its persisted `id`).
    pub decision_id: i64,
    /// The option the user picked.
    pub chosen: RepairKind,
}

// ─── ScheduledRepair ─────────────────────────────────────────────────────────

/// A repair that has been approved and queued for execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledRepair {
    /// Component the repair targets.
    pub component: Component,
    /// The issue that motivated the repair.
    pub issue: HealthIssue,
    /// The concrete recovery action to run.
    pub action: RecoveryAction,
    /// Unix timestamp (seconds) when the repair was enqueued.
    pub enqueued_at: i64,
}

// ─── RepairProgress ──────────────────────────────────────────────────────────

/// In-flight progress for a running repair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairProgress {
    /// Component being repaired.
    pub component: Component,
    /// Machine-readable tag of the running action.
    pub action_tag: String,
    /// Completion measure for progress bars. Declared for UI use — no checker emits progress yet.
    pub pct_complete: f32,
    /// Human-readable progress line.
    pub message: String,
}

// ─── RepairOutcome ───────────────────────────────────────────────────────────

/// Result returned from `HealthCheckable::repair()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    /// The issue was fully resolved.
    Resolved,
    /// Some progress was made; another check-cycle will determine if more is needed.
    PartialProgress {
        /// What was accomplished and what remains.
        detail: String,
    },
    /// The repair failed; the issue persists.
    Failed {
        /// Why the repair failed.
        reason: String,
    },
    /// This repair requires a user decision before it can proceed.
    NeedsUserDecision {
        /// The question put to the user.
        question: String,
        /// The choices offered; each maps to a `RepairKind`.
        options: Vec<UserOption>,
        /// The stakes, shown alongside the question (e.g. "Data loss is possible if the database is corrupt.").
        consequence: String,
    },
}

// ─── HealthReport ────────────────────────────────────────────────────────────

/// The full output of a single health-check run for one component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    /// Component that was checked.
    pub component: Component,
    /// Worst implied status across `issues` (see `worst_status`).
    pub status: HealthStatus,
    /// Concrete problems found; empty when healthy.
    pub issues: Vec<HealthIssue>,
    /// One-line human summary: issue count plus tags.
    pub summary: String,
    /// Unix timestamp (seconds) when the check completed.
    pub measured_at: i64,
}

impl HealthReport {
    /// All-clear report: `Healthy`, no issues, `measured_at` stamped now.
    pub fn healthy(component: Component) -> Self {
        let measured_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        Self {
            component,
            status: HealthStatus::Healthy,
            issues: vec![],
            summary: "All checks passed.".into(),
            measured_at,
        }
    }

    /// Build a report from found issues: status is the worst `implied_status`,
    /// the summary lists issue tags, `measured_at` is stamped now.
    pub fn from_issues(component: Component, issues: Vec<HealthIssue>) -> Self {
        let measured_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let status = worst_status(issues.iter().map(|i| i.implied_status()));
        let summary = if issues.is_empty() {
            "All checks passed.".into()
        } else {
            format!(
                "{} issue(s): {}",
                issues.len(),
                issues
                    .iter()
                    .map(|i| i.tag())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Self {
            component,
            status,
            issues,
            summary,
            measured_at,
        }
    }
}

// ─── PendingDecision ─────────────────────────────────────────────────────────

/// A repair decision waiting for user input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDecision {
    /// Set after the decision is persisted to the store.
    pub id: Option<i64>,
    /// Component the decision concerns.
    pub component: Component,
    /// The issue awaiting a decision.
    pub issue: HealthIssue,
    /// The question shown to the user.
    pub question: String,
    /// The offered choices.
    pub options: Vec<UserOption>,
    /// The stakes of leaving the issue unaddressed.
    pub consequence: String,
    /// Unix timestamp (seconds) when this decision was surfaced.
    pub surfaced_at_secs: i64,
    /// `SystemTime` snapshot (not serialised; use `surfaced_at_secs` for persistence).
    #[serde(skip)]
    pub surfaced_at: Option<SystemTime>,
}

impl PendingDecision {
    /// Same component and same issue *variant* (`tag()`), payload ignored —
    /// the dedupe test that stops the monitor re-surfacing a decision it
    /// already asked.
    pub fn matches(&self, component: &Component, issue: &HealthIssue) -> bool {
        &self.component == component && self.issue.tag() == issue.tag()
    }
}

// ─── HealthCheckable ─────────────────────────────────────────────────────────

/// A subsystem that can observe its own health and optionally repair itself.
///
/// Implementors live in `sovereign-tools` (`CorpusIndexChecker`, `EnrichmentChecker`),
/// `sovereign-store` (`StateStoreChecker`), and `sovereign-inference`
/// (`RouterCircuitChecker`). The trait is object-safe so `HealthMonitor` can hold
/// a `Vec<Arc<dyn HealthCheckable>>`.
pub trait HealthCheckable: Send + Sync + 'static {
    /// The component this checker is responsible for.
    fn component(&self) -> Component;

    /// Observe the component and return a report.  Must not mutate shared state.
    fn check(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HealthReport>> + Send + '_>>;

    /// Attempt to repair `issue`. Returns `RepairNotSupported` if no automated
    /// repair exists for this issue type.
    fn repair(
        &self,
        issue: &HealthIssue,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<RepairOutcome>> + Send + '_>>;

    /// Whether the monitor may call `repair()` without asking the user first.
    fn can_repair_autonomously(&self, issue: &HealthIssue) -> bool;
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_status_empty_is_healthy() {
        assert_eq!(worst_status(vec![]), HealthStatus::Healthy);
    }

    #[test]
    fn worst_status_picks_highest() {
        let statuses = vec![
            HealthStatus::Healthy,
            HealthStatus::Degraded,
            HealthStatus::Unhealthy,
        ];
        assert_eq!(worst_status(statuses), HealthStatus::Unhealthy);

        let statuses2 = vec![HealthStatus::Healthy, HealthStatus::Degraded];
        assert_eq!(worst_status(statuses2), HealthStatus::Degraded);
    }

    #[test]
    fn issue_implied_status_table() {
        assert_eq!(
            HealthIssue::CorruptEmbeddings {
                corpus_id: "x".into(),
                bad_chunk_ids: vec![]
            }
            .implied_status(),
            HealthStatus::Unhealthy
        );
        assert_eq!(
            HealthIssue::FtsDesync {
                corpus_id: "x".into(),
                fts_count: 0,
                chunk_count: 1
            }
            .implied_status(),
            HealthStatus::Degraded
        );
        assert_eq!(
            HealthIssue::UpdateAvailable {
                corpus_id: "x".into(),
                current_version: "1".into(),
                latest_version: "2".into(),
                delta: UpdateDelta {
                    new_documents: 1,
                    updated_documents: 0,
                    deleted_documents: 0
                }
            }
            .implied_status(),
            HealthStatus::Healthy
        );
    }

    #[test]
    fn health_issue_serde_round_trip() {
        let issues = vec![
            HealthIssue::PartialIngestion {
                corpus_id: "sep".into(),
                chunks_indexed: 100,
                chunks_expected: 200,
                resume_from: Some("batch-5".into()),
            },
            HealthIssue::EmbedModelMismatch {
                corpus_id: "sep".into(),
                index_model: "mxbai-embed-large-v1".into(),
                active_model: "qwen3-embedding-0.6b".into(),
                index_dims: 768,
                active_dims: 1024,
            },
            HealthIssue::RouterCircuitOpen {
                failure_count: 3,
                last_error: "timeout".into(),
                fallback_active: true,
            },
            HealthIssue::WalOvergrowth {
                wal_size_bytes: 300_000_000,
                threshold_bytes: 256_000_000,
            },
        ];

        for issue in &issues {
            let json = serde_json::to_string(issue).unwrap();
            let back: HealthIssue = serde_json::from_str(&json).unwrap();
            // Re-serialise to compare (HealthIssue doesn't implement PartialEq intentionally)
            assert_eq!(
                serde_json::to_string(&back).unwrap(),
                json,
                "round-trip failed for {:?}",
                issue.tag()
            );
        }
    }

    #[test]
    fn health_report_from_issues_summary() {
        let report = HealthReport::from_issues(
            Component::StateStore,
            vec![HealthIssue::WalOvergrowth {
                wal_size_bytes: 300_000_000,
                threshold_bytes: 256_000_000,
            }],
        );
        assert_eq!(report.status, HealthStatus::Degraded);
        assert!(report.summary.contains("wal_overgrowth"));
    }
}
