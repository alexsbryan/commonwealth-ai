// SPDX-License-Identifier: AGPL-3.0-or-later
//! Database/store construction — extracted verbatim from
//! `bootstrap_with_progress` (§3.3). Opens (or reuses) the SQLite state
//! store and stashes both the trait-object and concrete handles.
//! Narrowed to the slots it writes (not `&AppState`,
//! ARCH_PRINCIPLES §5.2) so it unit-tests via dependency injection.
//!
//! The `InsightService` built over the same connection went on
//! 2026-09-12: `state.insight_service` was written here and read by
//! nothing, the insight surface being `insight_http` on the daemon
//! (sv-surface rung 6). It also took this builder's only reason to name
//! an `InferenceProvider`.

use std::sync::Arc;

use sovereign_contracts::traits::StateStore;
use sovereign_store::sqlite::SqliteStateStore;
use tokio::sync::RwLock;

use crate::state::{BootstrapPhase, DesktopConfig};

/// Open (or reuse) the SQLite-backed `StateStore`. Returns the
/// trait-object store the command surface shares.
/// Idempotent: if `store_slot` is already populated (a Runtime rebuild),
/// the existing store is returned untouched.
pub(crate) async fn open_store(
    store_slot: &RwLock<Option<Arc<dyn StateStore>>>,
    sqlite_slot: &RwLock<Option<Arc<SqliteStateStore>>>,
    config: &DesktopConfig,
    emit: impl Fn(BootstrapPhase),
) -> Result<Arc<dyn StateStore>, String> {
    // Reuse an already-open store (Runtime rebuild). Scoped so the read
    // guard drops before the writes below.
    {
        let existing = store_slot.read().await;
        if let Some(s) = existing.as_ref() {
            return Ok(Arc::clone(s));
        }
    }

    emit(BootstrapPhase::OpeningDatabase);
    let db_path = config.data_dir.join("sovereign.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create data dir: {e}"))?;
    }
    tracing::info!("Database: {}", db_path.display());
    let sqlite_store =
        SqliteStateStore::open(&db_path).map_err(|e| format!("Failed to open database: {e}"))?;

    // Two handles for KnowledgeView wire-up: concrete Arc for
    // `set_observer`, trait-object Arc for the runtime + tools.
    let store_concrete: Arc<SqliteStateStore> = Arc::new(sqlite_store);
    let s: Arc<dyn StateStore> = store_concrete.clone();
    *store_slot.write().await = Some(Arc::clone(&s));
    *sqlite_slot.write().await = Some(store_concrete);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_store_and_populates_slots() {
        let store_slot: RwLock<Option<Arc<dyn StateStore>>> = RwLock::new(None);
        let sqlite_slot: RwLock<Option<Arc<SqliteStateStore>>> = RwLock::new(None);
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = DesktopConfig::default();
        config.data_dir = tmp.path().to_path_buf();
        let store = open_store(&store_slot, &sqlite_slot, &config, |_| {})
            .await
            .expect("open_store should succeed over a temp dir");

        assert!(store_slot.read().await.is_some(), "store slot set");
        assert!(
            sqlite_slot.read().await.is_some(),
            "concrete sqlite slot set"
        );

        // Idempotent reuse: a second call returns the already-open store.
        let _again = open_store(&store_slot, &sqlite_slot, &config, |_| {})
            .await
            .expect("reuse should succeed");
        let _ = store;
    }
}
