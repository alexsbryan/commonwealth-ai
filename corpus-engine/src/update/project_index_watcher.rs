//! [`BackgroundWatcher`] that keeps the project documentation index up to date.
//!
//! Watches for changes to `*.md` files and re-indexes them via
//! [`ProjectDocsStore`]. Deleted files are removed from the index.
//!
//! The initial bulk index is done at server startup (spawned async in
//! `project_cmd.rs`); this watcher only handles incremental updates.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;

use corpus_engine_notes::project_docs::ProjectDocsStore;
use crate::update::watcher_coordinator::{BackgroundWatcher, WatcherStatus};

/// Watches `*.md` files and keeps the [`ProjectDocsStore`] in sync.
pub struct ProjectIndexWatcher {
    store: Arc<ProjectDocsStore>,
    repo_root: PathBuf,
}

impl ProjectIndexWatcher {
    pub fn new(store: Arc<ProjectDocsStore>, repo_root: PathBuf) -> Self {
        Self { store, repo_root }
    }
}

#[async_trait]
impl BackgroundWatcher for ProjectIndexWatcher {
    fn id(&self) -> &'static str {
        "project_index"
    }

    fn description(&self) -> &'static str {
        "Project documentation indexer (*.md files)"
    }

    async fn on_files_changed(&self, paths: Vec<PathBuf>) {
        for path in paths
            .iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        {
            if path.exists() {
                if let Err(e) = self.store.index_file(path, &self.repo_root).await {
                    tracing::warn!(
                        path = %path.display(),
                        "ProjectIndexWatcher: re-index failed: {e}"
                    );
                }
            } else {
                // File was deleted — remove from index.
                let rel = path
                    .strip_prefix(&self.repo_root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();
                if let Err(e) = self.store.delete_file(&rel).await {
                    tracing::warn!(
                        path = %path.display(),
                        "ProjectIndexWatcher: delete_file failed: {e}"
                    );
                }
            }
        }
    }

    async fn current_status(&self) -> WatcherStatus {
        match self.store.file_count().await {
            Ok(0) => WatcherStatus::NeverRun,
            Ok(_) => WatcherStatus::Fresh {
                pass: true,
                // Use epoch as a sentinel — we don't track individual run times.
                last_run_at: SystemTime::UNIX_EPOCH,
            },
            Err(_) => WatcherStatus::NeverRun,
        }
    }
}
