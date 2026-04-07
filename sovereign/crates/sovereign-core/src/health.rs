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
    Healthy,
    Degraded,
    Unhealthy,
}

/// Return the worst (highest-severity) status in `statuses`.
/// Returns `Healthy` when `statuses` is empty.
pub fn worst_status(statuses: impl IntoIterator<Item = HealthStatus>) -> HealthStatus {
    statuses
        .into_iter()
        .max()
        .unwrap_or(HealthStatus::Healthy)
}

// ─── SlotName ────────────────────────────────────────────────────────────────

/// Which inference / embed slot is referenced in an issue.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotName {
    Primary,
    Embed,
    Fallback,
    Custom(String),
}

// ─── UpdateDelta ─────────────────────────────────────────────────────────────

/// Counts of how many documents would change in an update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDelta {
    pub new_documents: usize,
    pub updated_documents: usize,
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
        corpus_id: String,
        chunks_indexed: u64,
        chunks_expected: u64,
        resume_from: Option<String>,
    },
    /// The active embed slot's model / dimensions differ from what the index
    /// was built with.
    EmbedModelMismatch {
        corpus_id: String,
        index_model: String,
        active_model: String,
        index_dims: usize,
        active_dims: usize,
    },
    /// A sample of stored embeddings contains all-zero or non-finite vectors.
    CorruptEmbeddings {
        corpus_id: String,
        bad_chunk_ids: Vec<u64>,
    },
    /// The full-text search index is out of sync with the chunk store.
    FtsDesync {
        corpus_id: String,
        fts_count: u64,
        chunk_count: u64,
    },
    /// A newer version of the corpus dataset is available upstream.
    UpdateAvailable {
        corpus_id: String,
        current_version: String,
        latest_version: String,
        delta: UpdateDelta,
    },

    // ── Enrichment issues ────────────────────────────────────────────────────
    /// Some claims reference chunks whose content has changed.
    StaleEnrichment {
        corpus_id: String,
        stale_claim_count: u64,
    },
    /// Some claims reference chunk IDs that no longer exist.
    OrphanedEnrichment {
        corpus_id: String,
        orphan_claim_count: u64,
    },
    /// Fewer chunks have been enriched than the coverage threshold requires.
    LowEnrichmentCoverage {
        corpus_id: String,
        enriched_chunks: u64,
        total_chunks: u64,
        coverage_pct: f32,
        threshold_pct: f32,
    },

    // ── Router issues ────────────────────────────────────────────────────────
    /// The circuit breaker for the primary LLM slot is open.
    RouterCircuitOpen {
        failure_count: u32,
        last_error: String,
        fallback_active: bool,
    },

    // ── Model integrity issues ───────────────────────────────────────────────
    /// A locally-cached model file's checksum does not match the manifest.
    ModelChecksumFailure {
        slot: SlotName,
        model_id: String,
        expected_hash: String,
        actual_hash: String,
    },

    // ── State-store issues ───────────────────────────────────────────────────
    /// SQLite `PRAGMA integrity_check` returned a non-`ok` result.
    StateStoreCorruption { detail: String },
    /// The SQLite WAL file has grown past the safe threshold.
    WalOvergrowth { wal_size_bytes: u64, threshold_bytes: u64 },
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
                | Self::RouterCircuitOpen { fallback_active: false, .. }
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
    ReEmbed { corpus_id: String },
    /// Rebuild the FTS index for a corpus.
    RebuildFts { corpus_id: String },
    /// Resume an interrupted ingest from the saved cursor.
    ResumeIngestion { corpus_id: String, resume_from: String },
    /// Delete stale / orphaned claim rows and re-enqueue extraction.
    RefreshEnrichment { corpus_id: String },
    /// Apply a corpus dataset update (delta or full).
    ApplyCorpusUpdate { corpus_id: String },
    /// Probe the router to see if the circuit should be closed.
    ProbeRouter,
    /// Run `PRAGMA wal_checkpoint(TRUNCATE)` on the SQLite database.
    CheckpointWal,
    /// No automated repair available; requires user action.
    UserActionRequired { guidance: String },
}

// ─── RepairKind ──────────────────────────────────────────────────────────────

/// Canonical identifier for a repair type (used in the `pending_health_decisions` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairKind {
    ReEmbed,
    RebuildFts,
    ResumeIngestion,
    RefreshEnrichment,
    ApplyCorpusUpdate,
    ProbeRouter,
    CheckpointWal,
    Dismiss,
}

// ─── UserOption ──────────────────────────────────────────────────────────────

/// A choice presented to the user when a repair requires authorisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOption {
    pub kind: RepairKind,
    pub label: String,
    pub description: String,
}

// ─── UserDecision ────────────────────────────────────────────────────────────

/// A user's response to a pending decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDecision {
    pub decision_id: i64,
    pub chosen: RepairKind,
}

// ─── ScheduledRepair ─────────────────────────────────────────────────────────

/// A repair that has been approved and queued for execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledRepair {
    pub component: Component,
    pub issue: HealthIssue,
    pub action: RecoveryAction,
    /// Unix timestamp (seconds) when the repair was enqueued.
    pub enqueued_at: i64,
}

// ─── RepairProgress ──────────────────────────────────────────────────────────

/// In-flight progress for a running repair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairProgress {
    pub component: Component,
    pub action_tag: String,
    pub pct_complete: f32,
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
    PartialProgress { detail: String },
    /// The repair failed; the issue persists.
    Failed { reason: String },
    /// This repair requires a user decision before it can proceed.
    NeedsUserDecision {
        question: String,
        options: Vec<UserOption>,
        consequence: String,
    },
}

// ─── HealthReport ────────────────────────────────────────────────────────────

/// The full output of a single health-check run for one component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub component: Component,
    pub status: HealthStatus,
    pub issues: Vec<HealthIssue>,
    pub summary: String,
    /// Unix timestamp (seconds) when the check completed.
    pub measured_at: i64,
}

impl HealthReport {
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
                issues.iter().map(|i| i.tag()).collect::<Vec<_>>().join(", ")
            )
        };
        Self { component, status, issues, summary, measured_at }
    }
}

// ─── PendingDecision ─────────────────────────────────────────────────────────

/// A repair decision waiting for user input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDecision {
    /// Set after the decision is persisted to the store.
    pub id: Option<i64>,
    pub component: Component,
    pub issue: HealthIssue,
    pub question: String,
    pub options: Vec<UserOption>,
    pub consequence: String,
    /// Unix timestamp (seconds) when this decision was surfaced.
    pub surfaced_at_secs: i64,
    /// `SystemTime` snapshot (not serialised; use `surfaced_at_secs` for persistence).
    #[serde(skip)]
    pub surfaced_at: Option<SystemTime>,
}

impl PendingDecision {
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
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RepairOutcome>> + Send + '_>,
    >;

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
                index_model: "nomic-v1".into(),
                active_model: "qwen3".into(),
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
