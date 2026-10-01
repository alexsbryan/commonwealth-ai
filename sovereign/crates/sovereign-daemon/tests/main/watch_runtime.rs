// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one installer of `watched_folder_runtime` for this test binary.
//!
//! The runtime is a process-global `OnceLock`, and three files here drive
//! routes that read it (corpus_watch_http_e2e, lc_surface_e2e,
//! d9a_corpus_catalog_e2e). Under plain `cargo test` they share one process,
//! so each installing its own manager raced: the first `install` won, and a
//! test that kept its own port double read calls the installed manager never
//! made (corpus_watch_http_e2e's delete, `calls: []`, in svrn's lift). One
//! async cell builds the manager once and hands every caller the SAME port.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_index::ingest_port::double::IngestPortDouble;
use sovereign_contracts::traits::StateStore;
use sovereign_daemon::watched_folder_runtime;
use sovereign_store::memory::InMemoryStateStore;
use sovereign_tools::local_corpus::watched::registry::WatchedFolderRegistry;
use sovereign_tools::local_corpus::LocalCorpusManager;
use tokio::sync::OnceCell;

use crate::local_corpus_port_double::leaf_backed_double;

static RUNTIME: OnceCell<(PathBuf, Arc<IngestPortDouble>)> = OnceCell::const_new();

/// The installed runtime's data dir and the port its manager drives.
#[allow(clippy::unwrap_used)]
pub async fn installed() -> (PathBuf, Arc<IngestPortDouble>) {
    RUNTIME
        .get_or_init(|| async {
            let tmp = tempfile::tempdir().unwrap();
            let data_dir = tmp.path().to_path_buf();
            std::fs::create_dir_all(data_dir.join("indexes")).unwrap();
            std::fs::create_dir_all(data_dir.join("recipes")).unwrap();
            // The singleton holds paths into this dir for the process
            // lifetime; dropping the guard would pull them out from under it.
            std::mem::forget(tmp);
            let store: Arc<InMemoryStateStore> = Arc::new(InMemoryStateStore::new());
            // The leaf-backed ingest writes no source-file manifest, so no
            // corpus dir has one — the engine's answer for such a dir. What
            // the engine does with a register/remove is proven on `impl
            // LocalCorpusPort for CorpusEngine`, local_corpus_port_parity.
            let engine = Arc::new(
                leaf_backed_double(
                    data_dir.join("indexes"),
                    Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) })),
                )
                .on_source_file_progress(|_| None),
            );
            let manager = Arc::new(
                LocalCorpusManager::init(
                    engine.clone(),
                    store as Arc<dyn StateStore>,
                    None,
                    data_dir.clone(),
                    data_dir.join("vault-snapshots"),
                )
                .await
                .expect("manager init"),
            );
            watched_folder_runtime::install(manager, Arc::new(WatchedFolderRegistry::new()));
            (data_dir, engine)
        })
        .await
        .clone()
}
