//! Incremental per-file re-index — the hot path the `CodeWatcher`
//! calls on every file save.
//!
//! Contract:
//! - File present → extract symbols, delete old chunks for that file,
//!   batch-embed the new chunks, insert.
//! - File absent (deleted) → delete chunks whose `source_doc_id` matches
//!   the relative file path, return `Deleted`.
//!
//! Batching is critical: one `embed` call per file, not one per symbol.
//! A 300-symbol file takes one round-trip to the embed model, not 300.

use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use crate::engine::CorpusEngine;
use crate::error::{Error, Result};
use crate::extractors::code::CodeExtractor;
use crate::index::{CorpusIndex, InsertChunk, InsertCodeMeta};

/// Outcome of a single `reindex_file` call. Carries enough information
/// for the watcher to log useful events without the caller having to
/// re-query the index.
#[derive(Debug, Clone)]
pub enum ReindexResult {
    /// File is absent — existing chunks for that path have been deleted.
    Deleted { chunks_removed_known: bool },
    /// File was re-indexed. `chunks_written` is 0 when the file is
    /// supported but contains no extractable symbols (e.g. empty).
    Updated {
        chunks_written: usize,
        elapsed_ms: u64,
    },
    /// File exists but its extension isn't in the language registry.
    /// No index state was touched.
    Skipped,
}

impl CorpusEngine {
    /// Incrementally re-index a single source file against an existing
    /// code corpus. Called by [`crate::update::watch::CodeWatcher`] on
    /// every debounced file event.
    ///
    /// - `corpus_id`  — the code corpus to update. Must already exist
    ///                  under `self.index_dir`.
    /// - `abs_path`   — absolute path to the file on disk.
    /// - `repo_root`  — absolute path to the corpus's source root;
    ///                  used to compute the relative path that gets
    ///                  stored as `source_doc_id` and `file_path`.
    pub async fn reindex_file(
        &self,
        corpus_id: &str,
        abs_path: &Path,
        repo_root: &Path,
    ) -> Result<ReindexResult> {
        // Compute the relative path — this is the file-level identity
        // used as `source_doc_id`, so every chunk from a file shares one
        // ID and a single `delete_chunks_by_source_doc` call clears the
        // whole file.
        let rel_path = abs_path
            .strip_prefix(repo_root)
            .unwrap_or(abs_path)
            .to_string_lossy()
            .into_owned();

        let index_path = self.index_dir().join(corpus_id);
        if !index_path.exists() {
            return Err(Error::IndexNotFound(format!(
                "No index for corpus '{corpus_id}' at {}",
                index_path.display()
            )));
        }

        let index = CorpusIndex::open(&index_path).await?;

        // ── Deleted file branch ───────────────────────────────
        // If the file is gone, nuke the chunks and bail. This is the
        // path the `notify::EventKind::Remove(_)` branch ends up in.
        if !abs_path.exists() {
            index.delete_chunks_by_source_doc(&rel_path).await?;
            return Ok(ReindexResult::Deleted {
                chunks_removed_known: true,
            });
        }

        // ── Unsupported extension branch ──────────────────────
        // The watcher filters by `is_source_file` before calling us, so
        // this is a defence-in-depth check — keeps reindex_file safe to
        // call from other callers (tests, manual reprocess) without
        // making them duplicate the language registry lookup.
        if !crate::extractors::code::is_source_file(abs_path) {
            return Ok(ReindexResult::Skipped);
        }

        let content = match tokio::fs::read_to_string(abs_path).await {
            Ok(c) => c,
            Err(e) => return Err(Error::Io(e)),
        };
        let mtime = file_mtime_secs(abs_path);

        let extractor = CodeExtractor::default();
        let chunks = extractor.extract_file(&content, &rel_path, mtime)?;

        // Delete old chunks first. This means there's a brief window
        // where a query returning zero results is possible; accepted
        // because duplicate chunks are much worse than a short gap.
        index.delete_chunks_by_source_doc(&rel_path).await?;

        if chunks.is_empty() {
            return Ok(ReindexResult::Updated {
                chunks_written: 0,
                elapsed_ms: 0,
            });
        }

        // ── Batched embed — one call per file, not per symbol ────
        let t = Instant::now();
        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let embeddings = self.batch_embed_texts(&texts).await?;

        if embeddings.len() != chunks.len() {
            return Err(Error::Embed(format!(
                "batch embed returned {} vectors for {} chunks",
                embeddings.len(),
                chunks.len()
            )));
        }

        // ── Convert extractor chunks → InsertChunks ─────────────
        let insert_pairs: Vec<(InsertChunk, Vec<f32>)> = chunks
            .into_iter()
            .zip(embeddings.into_iter())
            .map(|(chunk, emb)| {
                let metadata_json = chunk.metadata_json();
                let insert = InsertChunk {
                    content: chunk.content.clone(),
                    title: Some(chunk.symbol_name.clone()),
                    url: None,
                    metadata: Some(metadata_json.to_string()),
                    content_hash: Some(chunk.content_hash.clone()),
                    source_doc_id: Some(rel_path.clone()),
                    source_file: None,
                    code: InsertCodeMeta {
                        symbol_name: Some(chunk.symbol_name),
                        symbol_kind: Some(chunk.symbol_kind.as_str().to_string()),
                        file_path: Some(chunk.file_path),
                        line_start: Some(chunk.line_start as i32),
                        line_end: Some(chunk.line_end as i32),
                        language: Some(chunk.language.to_string()),
                        mtime: Some(chunk.mtime),
                    },
                    // Reindex path runs outside any work-queue lease — it's
                    // invoked by file-watcher deltas for code corpora, which
                    // are never partitioned across peers.
                    unit_id: None,
                };
                (insert, emb)
            })
            .collect();

        let written = insert_pairs.len();
        index.insert_batch(&insert_pairs).await?;

        Ok(ReindexResult::Updated {
            chunks_written: written,
            elapsed_ms: t.elapsed().as_millis() as u64,
        })
    }

    /// Batch-embed an owned slice of strings using the batch embed fn
    /// when available, falling back to sequential calls on the primary
    /// embed fn when not. Accesses private engine fields directly —
    /// `reindex.rs` is a child module of `engine` so this is allowed.
    async fn batch_embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if let Some(batch) = &self.batch_embed {
            let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
            (batch)(&owned).await
        } else {
            let mut out = Vec::with_capacity(texts.len());
            for text in texts {
                out.push((self.embed)(text).await?);
            }
            Ok(out)
        }
    }
}

fn file_mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
