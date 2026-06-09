// SPDX-License-Identifier: AGPL-3.0-or-later
//! HealthMonitor construction — extracted verbatim from
//! `bootstrap_with_progress` (§3.3). Terminal, self-contained phase:
//! consumes already-built handles, registers checkers, and spawns the
//! monitor. Idempotent across Runtime rebuilds (the monitor survives).
//!
//! The signature takes the two `AppState` fields it actually touches
//! (the monitor slot + the shutdown token) rather than `&AppState`
//! (ISP, ARCH_PRINCIPLES §5.2) — which also lets it unit-test with the
//! project's standard mocks, no model and no Tauri `AppHandle` required.

use std::sync::Arc;

use corpus_engine::CorpusEngine;
use sovereign_core::health_monitor::{HealthMonitor, MonitorConfig};
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::index_validator::EmbedSlotConfig;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::state::DesktopConfig;

/// Build + start the desktop `HealthMonitor` and register its checkers
/// (corpus index, enrichment, state store, router circuit). No-op if a
/// monitor already exists in `health_slot` (Runtime rebuilds reuse it).
pub(crate) async fn build_health_monitor(
    health_slot: &RwLock<Option<Arc<HealthMonitor>>>,
    health_shutdown: &CancellationToken,
    config: &DesktopConfig,
    store: &Arc<dyn StateStore>,
    corpus_engine: &Arc<CorpusEngine>,
    inference: &Arc<dyn InferenceProvider>,
    embed_model_name: &str,
) {
    // Only build the monitor once (it survives Runtime rebuilds).
    if health_slot.read().await.is_none() {
        let embed_dims = config
            .embed_model_path
            .as_ref()
            .map(|_| {
                // If we successfully embedded a probe above, use that dimension.
                // Fall back to a reasonable default — the checker will detect mismatches.
                0usize
            })
            .unwrap_or(0);
        let embed_slot = Arc::new(tokio::sync::RwLock::new(EmbedSlotConfig {
            model_id: embed_model_name.to_string(),
            output_dims: embed_dims,
        }));

        let monitor = Arc::new(HealthMonitor::new(
            MonitorConfig::default(),
            Arc::clone(store),
        ));

        // Register CorpusIndexChecker.
        monitor
            .register(Arc::new(
                sovereign_tools::index_validator::CorpusIndexChecker::new(
                    Arc::clone(corpus_engine),
                    Arc::clone(&embed_slot),
                ),
            ))
            .await;

        // Register EnrichmentChecker.
        monitor
            .register(Arc::new(
                sovereign_tools::enrichment_checker::EnrichmentChecker::new(Arc::clone(
                    corpus_engine,
                )),
            ))
            .await;

        // Register StateStoreChecker (SQLite only).
        let db_path = config.data_dir.join("sovereign.db");
        if let Ok(sqlite_store) = SqliteStateStore::open(&db_path) {
            monitor
                .register(Arc::new(
                    sovereign_store::state_store_checker::StateStoreChecker::new(
                        Arc::new(sqlite_store),
                        db_path,
                    ),
                ))
                .await;
        }

        // Register RouterCircuitChecker with a standalone HealthTracker.
        // The monitor probes the inference provider on repair to test liveness.
        {
            let tracker = Arc::new(sovereign_inference::health::HealthTracker::new());
            monitor
                .register(Arc::new(
                    sovereign_inference::router_circuit::RouterCircuitChecker::new(
                        Arc::clone(&tracker),
                        Arc::clone(inference),
                    ),
                ))
                .await;
        }

        let m = Arc::clone(&monitor);
        let shutdown = health_shutdown.clone();
        tokio::spawn(async move { m.run(shutdown).await });
        *health_slot.write().await = Some(monitor);
        tracing::info!("HealthMonitor started");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::builders::test_support::{temp_corpus_engine, StubInference};
    use sovereign_store::memory::InMemoryStateStore;

    /// The bootstrap "untestability" was overstated: the builder takes the
    /// handles it needs, so it runs against `InMemoryStateStore` + a stub
    /// provider + a temp `CorpusEngine` — no model, no `AppHandle`.
    #[tokio::test]
    async fn builds_and_starts_a_health_monitor_with_mocks() {
        let slot: RwLock<Option<Arc<HealthMonitor>>> = RwLock::new(None);
        let shutdown = CancellationToken::new();

        let data_tmp = tempfile::tempdir().expect("data tempdir");
        let mut config = DesktopConfig::default();
        config.data_dir = data_tmp.path().to_path_buf();

        let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
        let (_corpus_tmp, corpus) = temp_corpus_engine();
        let inference: Arc<dyn InferenceProvider> = Arc::new(StubInference);

        build_health_monitor(
            &slot, &shutdown, &config, &store, &corpus, &inference, "test-embed",
        )
        .await;
        assert!(
            slot.read().await.is_some(),
            "HealthMonitor should be built and stored"
        );

        // Idempotent: a second call is a no-op (the monitor survives).
        build_health_monitor(
            &slot, &shutdown, &config, &store, &corpus, &inference, "test-embed",
        )
        .await;
        assert!(slot.read().await.is_some());

        shutdown.cancel(); // let the spawned monitor loop exit
    }
}
