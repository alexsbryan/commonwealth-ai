// SPDX-License-Identifier: AGPL-3.0-or-later
//! `StateStoreChecker` — health-checks the SQLite state database.
//!
//! Runs `PRAGMA integrity_check` and monitors WAL file size.
//! Postgres is not applicable for either check so both return no issues.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::error::{Error, Result};
use sovereign_core::health::{
    Component, HealthCheckable, HealthIssue, HealthReport, RepairKind, RepairOutcome,
};

use crate::sqlite::SqliteStateStore;

/// 256 MiB — WAL files larger than this are flagged.
const WAL_THRESHOLD_BYTES: u64 = 256 * 1024 * 1024;

// ─── StateStoreChecker ───────────────────────────────────────────────────────

/// Checks the health of the SQLite state store.
///
/// Constructed with the database path so it can check the WAL sidecar file
/// (`{path}-wal`) without needing to open a second connection.
pub struct StateStoreChecker {
    store: Arc<SqliteStateStore>,
    /// Filesystem path to the SQLite database file.
    db_path: PathBuf,
}

impl StateStoreChecker {
    pub fn new(store: Arc<SqliteStateStore>, db_path: PathBuf) -> Self {
        Self { store, db_path }
    }
}

// ── HealthCheckable impl ──────────────────────────────────────────────────────

impl HealthCheckable for StateStoreChecker {
    fn component(&self) -> Component {
        Component::StateStore
    }

    fn check(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HealthReport>> + Send + '_>>
    {
        Box::pin(async move {
            let mut issues = Vec::new();

            // ── 1. SQLite integrity check ──────────────────────────────────
            match self.store.integrity_check().await {
                Ok(detail) if detail != "ok" => {
                    issues.push(HealthIssue::StateStoreCorruption { detail });
                }
                Err(e) => {
                    issues.push(HealthIssue::StateStoreCorruption {
                        detail: format!("integrity_check failed: {e}"),
                    });
                }
                Ok(_) => {}
            }

            // ── 2. WAL size check ──────────────────────────────────────────
            let wal_path = self.db_path.with_extension("").with_file_name({
                let name = self
                    .db_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("store.db");
                format!("{name}-wal")
            });
            if let Ok(meta) = std::fs::metadata(&wal_path) {
                let wal_size = meta.len();
                if wal_size > WAL_THRESHOLD_BYTES {
                    issues.push(HealthIssue::WalOvergrowth {
                        wal_size_bytes: wal_size,
                        threshold_bytes: WAL_THRESHOLD_BYTES,
                    });
                }
            }

            Ok(HealthReport::from_issues(Component::StateStore, issues))
        })
    }

    fn repair(
        &self,
        issue: &HealthIssue,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<RepairOutcome>> + Send + '_>>
    {
        let issue = issue.clone();
        Box::pin(async move {
            match &issue {
                HealthIssue::WalOvergrowth { .. } => {
                    self.store.wal_checkpoint().await?;
                    Ok(RepairOutcome::Resolved)
                }
                HealthIssue::StateStoreCorruption { .. } => Ok(RepairOutcome::NeedsUserDecision {
                    question:
                        "The state store has integrity errors. Manual intervention is required."
                            .into(),
                    options: vec![sovereign_core::health::UserOption {
                        kind: RepairKind::Dismiss,
                        label: "Dismiss".into(),
                        description: "Acknowledge and continue.".into(),
                    }],
                    consequence: "Data loss is possible if the database is corrupt.".into(),
                }),
                _ => Err(Error::RepairNotSupported),
            }
        })
    }

    fn can_repair_autonomously(&self, issue: &HealthIssue) -> bool {
        matches!(issue, HealthIssue::WalOvergrowth { .. })
    }
}
