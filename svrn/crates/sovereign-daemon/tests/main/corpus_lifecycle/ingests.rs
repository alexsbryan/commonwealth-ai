// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Ingests`]: the registry install `corpus_lifecycle`'s port double runs,
//! and the on-disk status those runs leave. Moved out of `corpus_lifecycle.rs`
//! whole, unchanged but for visibility.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use corpus_index::ingest_port::daemon::{
    CorpusDiskStatus, IngestResult, InstallRefusal, PreparedInstall,
};
use corpus_index::ingest_port::double::IngestPortDouble;
use sovereign_contracts::daemon_wire::IngestProgress;
use tokio::sync::Notify;

/// What the double's registry install does when the daemon runs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Run {
    /// Report progress, hold a partition in progress, and return only when
    /// the port's cancel reaches it.
    HeldUntilCancelled,
    /// Report progress and finish with a ready canonical.
    Completes,
    /// Fail the way a mid-install embed failure does.
    Fails,
}

/// The registry install the node's port double runs, and the on-disk
/// status those runs leave, shared across the fresh states a test builds
/// (the reinstall and the resume each build one, as before the split).
pub(super) struct Ingests {
    known: HashSet<String>,
    run: Mutex<Run>,
    disk: Mutex<HashMap<String, CorpusDiskStatus>>,
    held: Mutex<HashMap<String, Arc<Notify>>>,
    /// The port's `index_dir`. A completed install reads it on the spot to
    /// place a staged SEC fact store (`corpus_ingest.rs`); empty, so nothing
    /// is staged and nothing is placed.
    indexes: tempfile::TempDir,
}

pub(super) fn absent(corpus_id: &str) -> CorpusDiskStatus {
    CorpusDiskStatus {
        corpus_id: corpus_id.to_string(),
        canonical_present: false,
        partition_present: false,
        canonical_in_progress: false,
        partition_in_progress: false,
        committed_iter_pos: 0,
        shards_completed: Vec::new(),
        shards_total: 0,
    }
}

impl Ingests {
    /// Installs of `known` resolve; any other id is not in the registry.
    pub(super) fn new(known: &[&str], run: Run) -> Arc<Self> {
        Arc::new(Self {
            known: known.iter().map(|s| s.to_string()).collect(),
            run: Mutex::new(run),
            disk: Mutex::default(),
            held: Mutex::default(),
            indexes: tempfile::tempdir().expect("an index dir"),
        })
    }

    pub(super) fn set_run(&self, run: Run) {
        *self.run.lock().unwrap() = run;
    }

    fn mark(&self, corpus_id: &str, edit: impl FnOnce(&mut CorpusDiskStatus)) {
        let mut disk = self.disk.lock().unwrap();
        edit(
            disk.entry(corpus_id.to_string())
                .or_insert_with(|| absent(corpus_id)),
        );
    }

    fn prepare(self: &Arc<Self>, corpus_id: &str) -> Result<PreparedInstall, InstallRefusal> {
        if !self.known.contains(corpus_id) {
            return Err(InstallRefusal::RecipeNotFound(format!(
                "No registry entry for corpus {corpus_id}"
            )));
        }
        let (me, id, run) = (
            Arc::clone(self),
            corpus_id.to_string(),
            *self.run.lock().unwrap(),
        );
        Ok(PreparedInstall {
            // The post-install atlas pass is not these tests' subject.
            opts_out_of_auto_enrichment: true,
            run: Box::new(move |progress| {
                Box::pin(async move {
                    if run == Run::Fails {
                        return Err(corpus_index::Error::Embed(
                            "simulated mid-install embed failure".into(),
                        ));
                    }
                    me.mark(&id, |d| {
                        d.partition_present = true;
                        d.partition_in_progress = true;
                    });
                    if let Some(progress) = &progress {
                        progress(IngestProgress::Embedding {
                            chunks_embedded: 1,
                            total: 600,
                            docs_processed: 1,
                            chunks_per_sec: 1.0,
                            expected_docs: None,
                        });
                    }
                    if run == Run::HeldUntilCancelled {
                        let held = Arc::new(Notify::new());
                        me.held
                            .lock()
                            .unwrap()
                            .insert(id.clone(), Arc::clone(&held));
                        held.notified().await;
                        return Err(corpus_index::Error::Cancelled(id));
                    }
                    me.mark(&id, |d| {
                        d.partition_present = false;
                        d.partition_in_progress = false;
                        d.canonical_present = true;
                    });
                    Ok(IngestResult {
                        corpus_id: id,
                        chunks_created: 600,
                        index_size_bytes: 0,
                        duration_secs: 0,
                        docs_skipped: 0,
                    })
                })
            }),
        })
    }

    /// The port's cancel: signals a held run, and says whether there was one.
    fn cancel(&self, corpus_id: &str) -> bool {
        match self.held.lock().unwrap().remove(corpus_id) {
            Some(held) => {
                held.notify_one();
                true
            }
            None => false,
        }
    }

    pub(super) fn double(self: &Arc<Self>) -> IngestPortDouble {
        let (prepare, cancel, wipe, disk) = (
            Arc::clone(self),
            Arc::clone(self),
            Arc::clone(self),
            Arc::clone(self),
        );
        IngestPortDouble::new()
            .with_index_dir(self.indexes.path())
            .on_prepare_registry_install(move |id| prepare.prepare(id))
            .on_cancel_corpus_ingest(move |id| cancel.cancel(id))
            .on_remove_corpus_everything(move |id| {
                wipe.disk.lock().unwrap().remove(id);
                Ok(())
            })
            .on_corpus_disk_status(move |id| {
                disk.disk
                    .lock()
                    .unwrap()
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| absent(id))
            })
            .with_in_progress_ingestions(Vec::new())
            .on_cached_article_stats(|_| None)
            .on_compute_article_stats(|_| None)
    }
}
