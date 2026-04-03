use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::CorpusState;

use super::registry::CorpusRegistry;

// ─── Progress Reporting ──────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CorpusInstallPhase {
    Downloading,
    Parsing,
    Complete,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct CorpusProgress {
    pub phase: CorpusInstallPhase,
    pub percent: f32,
    pub chunks_processed: usize,
    pub chunks_total: Option<usize>,
}

/// Thread-safe progress callback.
pub type ProgressCallback = Arc<dyn Fn(CorpusProgress) + Send + Sync>;

fn noop_progress() -> ProgressCallback {
    Arc::new(|_| {})
}

// ─── Corpus Manager ──────────────────────────────────────────

const BATCH_SIZE: usize = 100;

pub struct CorpusManager {
    registry: CorpusRegistry,
    store: Arc<dyn StateStore>,
    #[allow(dead_code)]
    inference: Option<Arc<dyn InferenceProvider>>,
    data_dir: PathBuf,
}

impl CorpusManager {
    pub fn new(
        registry: CorpusRegistry,
        store: Arc<dyn StateStore>,
        inference: Option<Arc<dyn InferenceProvider>>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            registry,
            store,
            inference,
            data_dir,
        }
    }

    fn downloads_dir(&self) -> PathBuf {
        self.data_dir.join("downloads")
    }

    /// Install a corpus: download the source, parse it into chunks, and store them.
    pub async fn install_corpus(
        &self,
        id: &str,
        progress: Option<ProgressCallback>,
    ) -> Result<CorpusState> {
        let progress = progress.unwrap_or_else(noop_progress);

        let definition = self
            .registry
            .get_corpus(id)
            .ok_or_else(|| Error::NotFound(format!("Corpus '{id}' not in registry")))?
            .clone();
        let parser = self
            .registry
            .parser_for_corpus(id)
            .ok_or_else(|| Error::NotFound(format!("No parser for corpus '{id}'")))?;

        // Download phase.
        progress(CorpusProgress {
            phase: CorpusInstallPhase::Downloading,
            percent: 0.0,
            chunks_processed: 0,
            chunks_total: None,
        });

        let source_path = self
            .download_source(id, &definition.source_url, &progress)
            .await?;

        // Parse phase (sync parser runs on blocking thread).
        progress(CorpusProgress {
            phase: CorpusInstallPhase::Parsing,
            percent: 0.0,
            chunks_processed: 0,
            chunks_total: None,
        });

        let store = self.store.clone();
        let progress_clone = progress.clone();

        let chunks_count = tokio::task::spawn_blocking(move || -> Result<usize> {
            let iter = parser.parse(&source_path)?;
            let mut batch = Vec::with_capacity(BATCH_SIZE);
            let mut total = 0usize;

            for result in iter {
                let chunk = match result {
                    Ok(c) => c,
                    Err(e) => {
                        // Log and skip bad records.
                        eprintln!("Skipping chunk: {e}");
                        continue;
                    }
                };
                batch.push(chunk);

                if batch.len() >= BATCH_SIZE {
                    total += batch.len();
                    let owned_batch = std::mem::take(&mut batch);
                    let handle = tokio::runtime::Handle::current();
                    handle.block_on(store.store_chunks(&owned_batch))?;

                    progress_clone(CorpusProgress {
                        phase: CorpusInstallPhase::Parsing,
                        percent: 0.0,
                        chunks_processed: total,
                        chunks_total: None,
                    });
                }
            }

            // Flush remainder.
            if !batch.is_empty() {
                total += batch.len();
                let handle = tokio::runtime::Handle::current();
                handle.block_on(store.store_chunks(&batch))?;
            }

            Ok(total)
        })
        .await
        .map_err(|e| Error::Execution(format!("Parse task panicked: {e}")))??;

        // Record corpus state.
        let now = now();
        let state = CorpusState {
            corpus_id: id.to_string(),
            installed_at: now,
            source_date: chrono_today(),
            chunks_count: chunks_count as i64,
            index_size_mb: 0,
            last_updated: now,
        };
        self.store.save_corpus_state(&state).await?;

        progress(CorpusProgress {
            phase: CorpusInstallPhase::Complete,
            percent: 100.0,
            chunks_processed: chunks_count,
            chunks_total: Some(chunks_count),
        });

        Ok(state)
    }

    /// Install a corpus from a local file (no download).
    pub async fn install_from_path(
        &self,
        id: &str,
        source_path: &Path,
        progress: Option<ProgressCallback>,
    ) -> Result<CorpusState> {
        let progress = progress.unwrap_or_else(noop_progress);

        let parser = self
            .registry
            .parser_for_corpus(id)
            .ok_or_else(|| Error::NotFound(format!("No parser for corpus '{id}'")))?;

        progress(CorpusProgress {
            phase: CorpusInstallPhase::Parsing,
            percent: 0.0,
            chunks_processed: 0,
            chunks_total: None,
        });

        let store = self.store.clone();
        let progress_clone = progress.clone();
        let path = source_path.to_path_buf();

        let chunks_count = tokio::task::spawn_blocking(move || -> Result<usize> {
            let iter = parser.parse(&path)?;
            let mut batch = Vec::with_capacity(BATCH_SIZE);
            let mut total = 0usize;

            for result in iter {
                let chunk = match result {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Skipping chunk: {e}");
                        continue;
                    }
                };
                batch.push(chunk);

                if batch.len() >= BATCH_SIZE {
                    total += batch.len();
                    let owned_batch = std::mem::take(&mut batch);
                    let handle = tokio::runtime::Handle::current();
                    handle.block_on(store.store_chunks(&owned_batch))?;

                    progress_clone(CorpusProgress {
                        phase: CorpusInstallPhase::Parsing,
                        percent: 0.0,
                        chunks_processed: total,
                        chunks_total: None,
                    });
                }
            }

            if !batch.is_empty() {
                total += batch.len();
                let handle = tokio::runtime::Handle::current();
                handle.block_on(store.store_chunks(&batch))?;
            }

            Ok(total)
        })
        .await
        .map_err(|e| Error::Execution(format!("Parse task panicked: {e}")))??;

        let now = now();
        let state = CorpusState {
            corpus_id: id.to_string(),
            installed_at: now,
            source_date: chrono_today(),
            chunks_count: chunks_count as i64,
            index_size_mb: 0,
            last_updated: now,
        };
        self.store.save_corpus_state(&state).await?;

        progress(CorpusProgress {
            phase: CorpusInstallPhase::Complete,
            percent: 100.0,
            chunks_processed: chunks_count,
            chunks_total: Some(chunks_count),
        });

        Ok(state)
    }

    /// Remove a corpus: delete all its chunks and state.
    pub async fn remove_corpus(&self, id: &str) -> Result<u64> {
        let deleted = self.store.delete_chunks_by_corpus(id).await?;
        self.store.delete_corpus_state(id).await?;

        // Clean up downloaded source files.
        let downloads = self.downloads_dir();
        if let Ok(entries) = std::fs::read_dir(&downloads) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(id) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        Ok(deleted)
    }

    /// Update a corpus by removing old data and re-installing.
    pub async fn update_corpus(
        &self,
        id: &str,
        progress: Option<ProgressCallback>,
    ) -> Result<CorpusState> {
        self.remove_corpus(id).await?;
        self.install_corpus(id, progress).await
    }

    /// List all installed corpora.
    pub async fn installed(&self) -> Result<Vec<CorpusState>> {
        self.store.list_corpus_states().await
    }

    /// Check which installed corpora might have updates available.
    pub async fn check_for_updates(&self) -> Result<Vec<CorpusState>> {
        let states = self.store.list_corpus_states().await?;
        let mut outdated = Vec::new();
        for state in states {
            if self.registry.get_corpus(&state.corpus_id).is_some() {
                // Flag as outdated if installed more than 90 days ago.
                let age_days = (now() - state.last_updated) / 86400;
                if age_days > 90 {
                    outdated.push(state);
                }
            }
        }
        Ok(outdated)
    }

    // ─── Download ────────────────────────────────────────────

    async fn download_source(
        &self,
        corpus_id: &str,
        url: &str,
        progress: &ProgressCallback,
    ) -> Result<PathBuf> {
        let downloads = self.downloads_dir();
        std::fs::create_dir_all(&downloads)
            .map_err(|e| Error::Storage(format!("Cannot create downloads dir: {e}")))?;

        let ext = extract_extension(url);
        let part_path = downloads.join(format!("{corpus_id}.part"));
        let final_path = downloads.join(format!("{corpus_id}.{ext}"));

        // If already downloaded, skip.
        if final_path.exists() {
            return Ok(final_path);
        }

        // Check for partial download (resume support).
        let existing_len = if part_path.exists() {
            std::fs::metadata(&part_path)
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        let client = reqwest::Client::new();
        let mut request = client.get(url);
        if existing_len > 0 {
            request = request.header("Range", format!("bytes={existing_len}-"));
        }

        let response = request
            .send()
            .await
            .map_err(|e| Error::Storage(format!("Download failed: {e}")))?;

        if !response.status().is_success() && response.status().as_u16() != 206 {
            return Err(Error::Storage(format!(
                "Download failed with status {}",
                response.status()
            )));
        }

        // If server returned 200 (not 206) when we requested a range,
        // the server doesn't support resume — start fresh.
        let should_append = response.status().as_u16() == 206;
        let total_size = response.content_length().map(|cl| {
            if should_append {
                cl + existing_len
            } else {
                cl
            }
        });

        use std::io::Write;
        let mut file = if should_append {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)
                .map_err(|e| Error::Storage(format!("Cannot open part file: {e}")))?
        } else {
            std::fs::File::create(&part_path)
                .map_err(|e| Error::Storage(format!("Cannot create part file: {e}")))?
        };

        let mut downloaded = if should_append { existing_len } else { 0 };
        let mut last_report = downloaded;

        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| Error::Storage(format!("Download stream error: {e}")))?
        {
            file.write_all(&chunk)
                .map_err(|e| Error::Storage(format!("Write error: {e}")))?;
            downloaded += chunk.len() as u64;

            if downloaded - last_report >= 1_048_576 {
                last_report = downloaded;
                let pct = total_size
                    .map(|t| (downloaded as f32 / t as f32) * 100.0)
                    .unwrap_or(0.0);
                progress(CorpusProgress {
                    phase: CorpusInstallPhase::Downloading,
                    percent: pct,
                    chunks_processed: 0,
                    chunks_total: None,
                });
            }
        }

        // Rename .part -> final.
        std::fs::rename(&part_path, &final_path)
            .map_err(|e| Error::Storage(format!("Rename failed: {e}")))?;

        Ok(final_path)
    }
}

// ─── Helpers ─────────────────────────────────────────────────

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn chrono_today() -> String {
    let secs = now();
    let days = secs / 86400;
    // Simple date calculation (good enough for YYYY-MM-DD).
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    // Civil calendar algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days_since_epoch + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

fn extract_extension(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    // Handle double extensions like .xml.bz2
    let filename = path.rsplit('/').next().unwrap_or("file");
    if filename.ends_with(".xml.bz2") {
        "xml.bz2".to_string()
    } else if filename.ends_with(".tar.gz") {
        "tar.gz".to_string()
    } else {
        filename
            .rsplit('.')
            .next()
            .unwrap_or("dat")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::CorpusParser;
    use sovereign_core::types::{DocumentChunk, SourceType};
    use sovereign_store::memory::InMemoryStateStore;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_registry() -> CorpusRegistry {
        CorpusRegistry::from_toml(
            r#"
[[corpus]]
id = "gutenberg"
name = "Gutenberg"
description = "Test"
source_url = "https://example.com/gutenberg.tar.gz"
format = "text-dir"
size_compressed_gb = 0.01
size_indexed_gb = 0.01
update_frequency = "static"
license = "Public Domain"
tiers = ["full"]

[[tier]]
id = "full"
name = "Full"
description = "All"
corpora = ["gutenberg"]
"#,
        )
        .unwrap()
    }

    fn make_test_corpus(dir: &Path) {
        let file = dir.join("book.txt");
        let mut f = std::fs::File::create(file).unwrap();
        write!(
            f,
            "Title: A Test Book\r\n\
             Author: Test\r\n\
             \r\n\
             *** START OF THE PROJECT GUTENBERG EBOOK A TEST BOOK ***\r\n\
             \r\n\
             Chapter 1\r\n\
             \r\n\
             This is the content of the first chapter of the test book.\r\n\
             \r\n\
             Chapter 2\r\n\
             \r\n\
             This is the content of the second chapter.\r\n\
             \r\n\
             *** END OF THE PROJECT GUTENBERG EBOOK A TEST BOOK ***\r\n"
        )
        .unwrap();
    }

    #[tokio::test]
    async fn install_and_remove_corpus() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();
        make_test_corpus(&source_dir);

        let store = Arc::new(InMemoryStateStore::new());
        let registry = test_registry();
        let manager = CorpusManager::new(
            registry,
            store.clone(),
            None,
            dir.path().to_path_buf(),
        );

        // Install from local path.
        let state = manager
            .install_from_path("gutenberg", &source_dir, None)
            .await
            .unwrap();

        assert_eq!(state.corpus_id, "gutenberg");
        assert!(state.chunks_count > 0);

        // Verify installed.
        let installed = manager.installed().await.unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].corpus_id, "gutenberg");

        // Remove.
        let deleted = manager.remove_corpus("gutenberg").await.unwrap();
        assert!(deleted > 0);

        // Verify removed.
        let installed = manager.installed().await.unwrap();
        assert!(installed.is_empty());
    }

    #[tokio::test]
    async fn install_fires_progress() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();
        make_test_corpus(&source_dir);

        let store = Arc::new(InMemoryStateStore::new());
        let registry = test_registry();
        let manager = CorpusManager::new(
            registry,
            store,
            None,
            dir.path().to_path_buf(),
        );

        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();
        let progress: ProgressCallback = Arc::new(move |_p| {
            cc.fetch_add(1, Ordering::Relaxed);
        });

        manager
            .install_from_path("gutenberg", &source_dir, Some(progress))
            .await
            .unwrap();

        // Should have at least 2 callbacks (parsing start + complete).
        assert!(call_count.load(Ordering::Relaxed) >= 2);
    }

    #[tokio::test]
    async fn update_replaces_corpus() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();
        make_test_corpus(&source_dir);

        let store = Arc::new(InMemoryStateStore::new());
        let registry = test_registry();
        let manager = CorpusManager::new(
            registry,
            store.clone(),
            None,
            dir.path().to_path_buf(),
        );

        // Install first.
        let state1 = manager
            .install_from_path("gutenberg", &source_dir, None)
            .await
            .unwrap();

        // Add another book to the source.
        let file = source_dir.join("book2.txt");
        let mut f = std::fs::File::create(file).unwrap();
        write!(
            f,
            "Title: Another Book\r\n\
             *** START OF THE PROJECT GUTENBERG EBOOK ANOTHER BOOK ***\r\n\
             More content here.\r\n\
             *** END OF THE PROJECT GUTENBERG EBOOK ANOTHER BOOK ***\r\n"
        )
        .unwrap();

        // Reinstall from path (simulates update).
        manager.remove_corpus("gutenberg").await.unwrap();
        let state2 = manager
            .install_from_path("gutenberg", &source_dir, None)
            .await
            .unwrap();

        // Should have more chunks now.
        assert!(state2.chunks_count > state1.chunks_count);
    }

    #[test]
    fn extract_extension_works() {
        assert_eq!(
            extract_extension("https://example.com/dump.xml.bz2"),
            "xml.bz2"
        );
        assert_eq!(
            extract_extension("https://example.com/data.tar.gz"),
            "tar.gz"
        );
        assert_eq!(
            extract_extension("https://example.com/file.jsonl"),
            "jsonl"
        );
        assert_eq!(
            extract_extension("https://example.com/file.jsonl?v=2"),
            "jsonl"
        );
    }
}
