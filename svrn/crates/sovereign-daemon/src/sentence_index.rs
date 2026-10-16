// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sentence index behind `knowledge:sentences` (OICP v0.6 §2.11): every
//! stored text of a corpus split by `sentence_ranges`, each sentence embedded
//! once in the background on the embed slot, the vectors kept beside the
//! corpus's index with each sentence's address, and a sentence query scanned
//! against all of them (`routes_knowledge_sentences`).
//!
//! A corpus is indexed when an install of it completes, when a sentence
//! search first names it, and at the daemon's start for every corpus that
//! already holds a sidecar. One worker embeds, one corpus at a time; a
//! search on a corpus still embedding is answered by the passages route.
//!
//! Events: target `sovereign_daemon::sentence_index` (prepared, loaded,
//! ready with `elapsed_ms` and `resident_bytes`; saves at debug).

pub mod config;
pub mod queue;
pub mod scan;
pub mod store;
mod worker;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use corpus_index::corpus::Corpus;
use corpus_index::index::CorpusIndex;
use kernel_types::Sha256Hash;

pub use config::SentenceSearchConfig;
use store::{split, CorpusSentences, SentenceRow};

use crate::state::AppState;

/// The tracing target of every event here: this module's path, so a daemon
/// logging `sovereign_daemon=info` shows them with no directive of their own.
pub const TARGET: &str = "sovereign_daemon::sentence_index";

/// One corpus's index and the lock its preparation holds.
#[derive(Default)]
pub struct Entry {
    preparing: tokio::sync::Mutex<bool>,
    pub data: RwLock<CorpusSentences>,
}

struct Shared {
    config: RwLock<Result<SentenceSearchConfig, String>>,
    corpora: Mutex<HashMap<String, Arc<Entry>>>,
    queue: Mutex<queue::Fifo>,
    wake: tokio::sync::Notify,
    worker: OnceLock<()>,
}

/// The node's sentence index: every corpus's, and the one worker filling them.
#[derive(Clone)]
pub struct SentenceIndex {
    shared: Arc<Shared>,
}

impl Default for SentenceIndex {
    fn default() -> Self {
        let config = SentenceSearchConfig::from_data_file();
        if let Err(e) = &config {
            tracing::error!(target: TARGET, error = %e, "sentence search: the data file does not read; knowledge:sentences is not offered");
        }
        Self {
            shared: Arc::new(Shared {
                config: RwLock::new(config),
                corpora: Mutex::default(),
                queue: Mutex::default(),
                wake: tokio::sync::Notify::new(),
                worker: OnceLock::new(),
            }),
        }
    }
}

impl SentenceIndex {
    /// The data file's choices, or why there are none.
    pub fn config(&self) -> Result<SentenceSearchConfig, String> {
        self.shared
            .config
            .read()
            .map_or_else(|_| Err("config lock poisoned".into()), |c| c.clone())
    }

    /// Replace the data file's choices: a test's host configured otherwise.
    #[cfg(any(test, feature = "test-doubles"))]
    pub fn set_config(&self, config: SentenceSearchConfig) {
        if let Ok(mut c) = self.shared.config.write() {
            *c = Ok(config);
        }
    }

    /// Whether `corpus_id` is sentence-indexed here: prepared in this
    /// process, or holding a sidecar from before it.
    pub fn is_indexed(&self, index_dir: &Path, corpus_id: &str) -> bool {
        let prepared = self
            .shared
            .corpora
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(corpus_id)
            .is_some_and(|e| !read(&e.data).texts.is_empty());
        prepared
            || Corpus::named(index_dir, corpus_id).is_some_and(|c| c.sentences_path().is_file())
    }

    pub(crate) fn entry(&self, corpus_id: &str) -> Arc<Entry> {
        let mut map = self
            .shared
            .corpora
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        Arc::clone(map.entry(corpus_id.to_string()).or_default())
    }

    /// Queue `corpus_id` for the worker, starting it on first use.
    pub fn enqueue(&self, state: &AppState, corpus_id: &str) {
        let fresh = {
            let mut q = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
            q.push(corpus_id)
        };
        tracing::debug!(target: TARGET, corpus = corpus_id, fresh, "sentence index: queued");
        self.shared.worker.get_or_init(|| {
            tracing::info!(target: TARGET, "sentence index: worker started");
            tokio::spawn(worker::run(state.clone(), self.clone()));
        });
        self.shared.wake.notify_one();
    }

    /// Queue `corpus_id` once the install just spawned for it has returned:
    /// polled from the active-install set every two seconds, which is where
    /// the install's own task removes it.
    pub fn after_install(&self, state: &AppState, corpus_id: String) {
        let (index, state) = (self.clone(), state.clone());
        tokio::spawn(async move {
            let at = Instant::now();
            while state
                .inner
                .ingest
                .active_ingests
                .read()
                .await
                .contains(&corpus_id)
            {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            tracing::info!(target: TARGET, corpus = %corpus_id, install_ms = at.elapsed().as_millis() as u64, "sentence index: the install returned; indexing its sentences");
            index.enqueue(&state, &corpus_id);
        });
    }

    /// Queue every corpus that already holds a sidecar, at the daemon's start,
    /// so one indexed before a restart is loaded and finished rather than
    /// embedded again.
    pub fn resume_stored(&self, state: &AppState) {
        let Some(engine) = state.inner.node.corpus_engine.as_ref() else {
            tracing::debug!(target: TARGET, "sentence index: no corpus engine; nothing to resume");
            return;
        };
        let dir = engine.index_dir();
        let held: Vec<String> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|id| Corpus::named(dir, id).is_some_and(|c| c.sentences_path().is_file()))
                .collect(),
            Err(e) => {
                tracing::warn!(target: TARGET, dir = %dir.display(), error = %e, "sentence index: the index directory does not list; nothing resumed");
                return;
            }
        };
        tracing::info!(target: TARGET, corpora = held.len(), "sentence index: resuming stored indexes");
        for id in held {
            self.enqueue(state, &id);
        }
    }

    pub(crate) fn next_queued(&self) -> Option<String> {
        let mut q = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
        let next = q.take()?;
        tracing::debug!(target: TARGET, corpus = %next, waiting = q.snapshot().waiting.len(), "sentence index: embedding now");
        Some(next)
    }

    /// The worker is done with the corpus it took.
    pub(crate) fn finished(&self) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .finish();
    }

    /// The embedding queue as it stands: the one corpus embedding now and
    /// the corpora behind it, in the worker's order. The one reader of that
    /// order a search uses (`queue::places`).
    pub fn queue(&self) -> queue::QueueSnapshot {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot()
    }

    /// `corpus_id`'s sentences not yet embedded under the embedder `probe`
    /// identifies, `None` before the corpus has been opened and counted.
    pub fn pending_of(&self, corpus_id: &str, probe: &[f32]) -> Option<usize> {
        let held = self.entry(corpus_id);
        let d = read(&held.data);
        (!d.texts.is_empty()).then(|| d.pending_under(probe))
    }

    pub(crate) async fn woken(&self) {
        self.shared.wake.notified().await
    }

    /// `corpus_id`'s index, current with its texts: loaded from its sidecar
    /// the first time, then cut again for any text added or removed since
    /// (by the record table's version), keeping every vector whose sentence
    /// is still there. `Err` names why the corpus cannot be sentence-indexed.
    pub async fn prepare(
        &self,
        index_dir: &Path,
        corpus_id: &str,
        ix: &CorpusIndex,
    ) -> Result<Arc<Entry>, String> {
        let entry = self.entry(corpus_id);
        let mut loaded = entry.preparing.lock().await;
        let sidecar = Corpus::named(index_dir, corpus_id)
            .ok_or_else(|| format!("`{corpus_id}` is not a corpus id"))?
            .sentences_path();
        if !*loaded {
            *loaded = true;
            if sidecar.is_file() {
                let at = Instant::now();
                let path = sidecar.clone();
                match tokio::task::spawn_blocking(move || CorpusSentences::load(&path)).await {
                    Ok(Ok(s)) => {
                        tracing::info!(target: TARGET, corpus = corpus_id, sentences = s.rows.len(), embedded = s.done, resident_bytes = s.resident_bytes(), elapsed_ms = at.elapsed().as_millis() as u64, "sentence index: loaded from its sidecar");
                        *write(&entry.data) = s;
                    }
                    Ok(Err(why)) => {
                        tracing::warn!(target: TARGET, corpus = corpus_id, %why, "sentence index: the sidecar does not read; its texts are cut again")
                    }
                    Err(e) => {
                        tracing::warn!(target: TARGET, corpus = corpus_id, error = %e, "sentence index: the sidecar read panicked; its texts are cut again")
                    }
                }
            }
        }
        let (version, records) = match ix.documents().await {
            Ok(Ok(v)) => v,
            Ok(Err(absence)) => return Err(format!("its texts are not stored ({absence:?})")),
            Err(e) => return Err(format!("its records do not read: {e}")),
        };
        let held = read(&entry.data).version;
        if held == version && !read(&entry.data).texts.is_empty() {
            tracing::debug!(target: TARGET, corpus = corpus_id, ?version, "sentence index: current");
            return Ok(Arc::clone(&entry));
        }
        let at = Instant::now();
        let names: Vec<Sha256Hash> = records
            .iter()
            .filter(|r| r.text_stored)
            .map(|r| r.text_sha256)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut kept: Vec<(Sha256Hash, Vec<SentenceRow>)> = Vec::new();
        let mut fresh: Vec<(Sha256Hash, String)> = Vec::new();
        for name in names {
            if let Some(rows) = read(&entry.data).rows_of(&name) {
                kept.push((name, rows));
                continue;
            }
            match ix.text(&name).await {
                Ok(Ok(stored)) => fresh.push((name, stored.text)),
                Ok(Err(absence)) => {
                    tracing::warn!(target: TARGET, corpus = corpus_id, %name, ?absence, "sentence index: a recorded text is not held; its sentences are not indexed")
                }
                Err(e) => return Err(format!("text {name} does not read: {e}")),
            }
        }
        let cut = fresh.len();
        let split_texts = tokio::task::spawn_blocking(move || {
            fresh
                .into_iter()
                .map(|(n, t)| (n, split(&t)))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| format!("splitting panicked: {e}"))?;
        kept.extend(split_texts);
        let rebuilt = read(&entry.data).rebuilt(version, kept);
        tracing::info!(target: TARGET, corpus = corpus_id, ?version, texts = rebuilt.texts.len(), texts_cut = cut, sentences = rebuilt.rows.len(), pending = rebuilt.pending(), elapsed_ms = at.elapsed().as_millis() as u64, "sentence index: prepared");
        *write(&entry.data) = rebuilt;
        Ok(Arc::clone(&entry))
    }
}

/// The read guard, through a poisoned lock: a panic while writing leaves
/// rows pending, never a vector served under the wrong address.
pub(crate) fn read(l: &RwLock<CorpusSentences>) -> std::sync::RwLockReadGuard<'_, CorpusSentences> {
    l.read().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn write(
    l: &RwLock<CorpusSentences>,
) -> std::sync::RwLockWriteGuard<'_, CorpusSentences> {
    l.write().unwrap_or_else(|p| p.into_inner())
}
