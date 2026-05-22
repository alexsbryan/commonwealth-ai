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

#[cfg(feature = "treesitter")]
use std::path::Path;
#[cfg(feature = "treesitter")]
use std::time::UNIX_EPOCH;
use std::time::Instant;

use crate::engine::CorpusEngine;
use crate::error::{Error, Result};
#[cfg(feature = "treesitter")]
use crate::extractors::code::CodeExtractor;
#[cfg(feature = "treesitter")]
use crate::index::InsertCodeMeta;
use crate::index::{code_meta_from_json, CorpusIndex, InsertChunk};
use crate::recipe::{ChunkerConfig, ExtractorConfig};

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

#[cfg(feature = "treesitter")]
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

        let t = Instant::now();

        if chunks.is_empty() {
            // File supported but empty after extraction (e.g. an
            // empty source file). Drop any prior chunks; nothing to
            // insert.
            index.delete_chunks_by_source_doc(&rel_path).await?;
            return Ok(ReindexResult::Updated {
                chunks_written: 0,
                elapsed_ms: t.elapsed().as_millis() as u64,
            });
        }

        // ── Move 6 P6: chunk-level delta ───────────────────────
        // Hash-match the freshly-extracted chunks against the
        // committed set. Skipping re-embed for unchanged chunks turns
        // a single-line edit on a 1000-line file into one embed call
        // instead of 30+.
        let committed = index.committed_chunks_for_doc(&rel_path).await?;
        let new_text_chunks: Vec<crate::chunkers::TextChunk> = chunks
            .iter()
            .enumerate()
            .map(|(idx, c)| crate::chunkers::TextChunk {
                content: c.content.clone(),
                index: idx,
            })
            .collect();
        let diff = crate::chunkers::chunk_delta(
            &committed,
            new_text_chunks,
            |s| blake3::hash(s.as_bytes()).to_hex().to_string(),
        );

        if diff.is_noop() {
            tracing::debug!(
                corpus_id = %corpus_id,
                path = %rel_path,
                committed = committed.len(),
                "reindex_file.noop — every chunk hash-matched a committed row"
            );
            return Ok(ReindexResult::Updated {
                chunks_written: 0,
                elapsed_ms: t.elapsed().as_millis() as u64,
            });
        }

        // Drop only the chunks whose content disappeared from the
        // new file. Brief query window during the embed step
        // matches the prior behaviour.
        if !diff.deleted.is_empty() {
            index.delete_chunks_by_ids(&diff.deleted).await?;
        }

        if diff.added.is_empty() {
            tracing::debug!(
                corpus_id = %corpus_id,
                path = %rel_path,
                deleted = diff.deleted.len(),
                kept = diff.kept_unchanged.len(),
                "reindex_file.delete_only — additions empty, no re-embed needed"
            );
            return Ok(ReindexResult::Updated {
                chunks_written: 0,
                elapsed_ms: t.elapsed().as_millis() as u64,
            });
        }

        // Map added TextChunk back to its source ExtractedChunk so we
        // recover the symbol metadata. Match by content hash —
        // `chunk_delta` did not return the source ExtractedChunk
        // because the primitive is chunker-shape-agnostic.
        use std::collections::HashMap;
        let by_hash: HashMap<String, &_> = chunks
            .iter()
            .map(|c| (c.content_hash.clone(), c))
            .collect();

        let added_extracted: Vec<_> = diff
            .added
            .iter()
            .filter_map(|tc| {
                let h = blake3::hash(tc.content.as_bytes()).to_hex().to_string();
                by_hash.get(&h).copied()
            })
            .collect();
        if added_extracted.len() != diff.added.len() {
            // Hash collision or extractor edge case — fall back to a
            // full re-embed for safety. Should be vanishingly rare
            // (blake3 collisions don't happen in practice).
            tracing::warn!(
                corpus_id = %corpus_id,
                path = %rel_path,
                added = diff.added.len(),
                resolved = added_extracted.len(),
                "reindex_file.fallback — could not resolve some added chunks to extractor output"
            );
            // Re-do the work the legacy path did: delete everything,
            // re-embed everything.
            index.delete_chunks_by_source_doc(&rel_path).await?;
            let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
            let embeddings = self.batch_embed_texts(&texts).await?;
            let insert_pairs = build_insert_pairs(chunks, embeddings, &rel_path)?;
            let written = insert_pairs.len();
            index.insert_batch(&insert_pairs).await?;
            return Ok(ReindexResult::Updated {
                chunks_written: written,
                elapsed_ms: t.elapsed().as_millis() as u64,
            });
        }

        // ── Batched embed — only the added chunks ──────────────
        let texts: Vec<&str> = added_extracted
            .iter()
            .map(|c| c.content.as_str())
            .collect();
        let embeddings = self.batch_embed_texts(&texts).await?;

        if embeddings.len() != added_extracted.len() {
            return Err(Error::Embed(format!(
                "batch embed returned {} vectors for {} chunks",
                embeddings.len(),
                added_extracted.len()
            )));
        }

        let owned_added: Vec<_> = added_extracted.into_iter().cloned().collect();
        let insert_pairs = build_insert_pairs(owned_added, embeddings, &rel_path)?;
        let written = insert_pairs.len();
        index.insert_batch(&insert_pairs).await?;

        tracing::debug!(
            corpus_id = %corpus_id,
            path = %rel_path,
            kept = diff.kept_unchanged.len(),
            deleted = diff.deleted.len(),
            added = written,
            "reindex_file.delta_applied"
        );

        Ok(ReindexResult::Updated {
            chunks_written: written,
            elapsed_ms: t.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(feature = "treesitter")]
fn build_insert_pairs(
    chunks: Vec<crate::extractors::code::CodeChunk>,
    embeddings: Vec<Vec<f32>>,
    rel_path: &str,
) -> Result<Vec<(InsertChunk, Vec<f32>)>> {
    if chunks.len() != embeddings.len() {
        return Err(Error::Embed(format!(
            "build_insert_pairs: {} chunks vs {} embeddings",
            chunks.len(),
            embeddings.len()
        )));
    }
    Ok(chunks
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
                source_doc_id: Some(rel_path.to_string()),
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
                unit_id: None,
            };
            (insert, emb)
        })
        .collect())
}

impl CorpusEngine {
    /// Re-index a single logical document against an existing corpus by
    /// `source_doc_id`. The freshness daemon's hot path: fetch a
    /// MediaWiki article over HTTP, hand the response body to this
    /// method, and the chunks for that article in `wikipedia` are
    /// replaced atomically.
    ///
    /// One unified function instead of two — the underlying
    /// `delete_chunks_by_source_doc` is idempotent on absent rows, so
    /// "absent → present" (initial fetch) and "present → updated"
    /// (revision refresh) collapse into the same call site. Callers
    /// that care about the transition track it externally.
    ///
    /// Contract:
    ///   - `extractor` runs against a temp file containing
    ///     `raw_content` (the existing extractor trait is
    ///     path-shaped, so we stage a temp file rather than refactor
    ///     the trait for one new caller).
    ///   - The extractor's per-doc metadata JSON is preserved on
    ///     each chunk, with one transformation: when
    ///     `chunker == PortalEventBullet`, the chunk's
    ///     `outgoing_links` are replaced by the wikilinks that
    ///     actually appear in *that bullet* — so the per-chunk
    ///     `outbound_links` field is bullet-scoped, not section-scoped.
    pub async fn reindex_by_source_doc_id(
        &self,
        corpus_id: &str,
        source_doc_id: &str,
        raw_content: &str,
        extractor_config: &ExtractorConfig,
        chunker_config: &ChunkerConfig,
    ) -> Result<ReindexResult> {
        let index_path = self.index_dir().join(corpus_id);
        if !index_path.exists() {
            return Err(Error::IndexNotFound(format!(
                "No index for corpus '{corpus_id}' at {}",
                index_path.display()
            )));
        }

        // Stage the raw content as a temp file so the existing
        // path-based Extractor trait can run unchanged. The temp file
        // is dropped after extraction completes.
        let tmp = tempfile::NamedTempFile::new().map_err(Error::Io)?;
        tokio::fs::write(tmp.path(), raw_content)
            .await
            .map_err(Error::Io)?;

        let extractor = self.make_extractor(extractor_config);
        let docs: Vec<crate::extractors::ExtractedDoc> = extractor
            .extract(tmp.path())?
            .collect::<Result<Vec<_>>>()?;

        let chunker = self.make_chunker(chunker_config);
        let is_portal_bullet = matches!(chunker_config, ChunkerConfig::PortalEventBullet { .. });

        let t = Instant::now();
        let mut chunk_records: Vec<(String, Option<serde_json::Value>, Option<String>, Option<String>)> =
            Vec::new();
        // (content, per_chunk_metadata_json, doc_title, doc_url)

        for doc in &docs {
            let pieces = chunker.chunk(&doc.content);
            for piece in pieces {
                let metadata_json = if is_portal_bullet {
                    // Replace section-scoped outgoing_links with the
                    // links that actually appear in this bullet.
                    rescope_outgoing_links_for_bullet(&doc.metadata, &piece.content)
                } else {
                    doc.metadata.clone()
                };
                chunk_records.push((
                    piece.content,
                    metadata_json,
                    doc.title.clone(),
                    doc.url.clone(),
                ));
            }
        }

        let index = CorpusIndex::open(&index_path).await?;
        // Delete first — brief query gap is acceptable; duplicate
        // chunks from a half-applied refresh are not.
        index.delete_chunks_by_source_doc(source_doc_id).await?;

        if chunk_records.is_empty() {
            return Ok(ReindexResult::Updated {
                chunks_written: 0,
                elapsed_ms: t.elapsed().as_millis() as u64,
            });
        }

        // Batched embed — one call per article.
        let texts: Vec<&str> = chunk_records.iter().map(|(c, _, _, _)| c.as_str()).collect();
        let embeddings = self.batch_embed_texts(&texts).await?;
        if embeddings.len() != chunk_records.len() {
            return Err(Error::Embed(format!(
                "batch embed returned {} vectors for {} chunks",
                embeddings.len(),
                chunk_records.len()
            )));
        }

        let insert_pairs: Vec<(InsertChunk, Vec<f32>)> = chunk_records
            .into_iter()
            .zip(embeddings)
            .map(|((content, metadata, title, url), emb)| {
                let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                let code = code_meta_from_json(metadata.as_ref());
                let insert = InsertChunk {
                    content,
                    title,
                    url,
                    metadata: metadata.as_ref().map(|m| m.to_string()),
                    content_hash: Some(content_hash),
                    source_doc_id: Some(source_doc_id.to_string()),
                    source_file: None,
                    code,
                    unit_id: None,
                };
                (insert, emb)
            })
            .collect();

        let written = insert_pairs.len();
        index.insert_batch(&insert_pairs).await?;

        // Glassbox: every reindex_by_source_doc_id call is a live
        // mutation that downstream consumers (atlas, /v1/knowledge/
        // search) read from. Emitting an info event so newsworthy
        // watcher ticks land as concrete chunk-counts in the daemon
        // log — operators can grep `reindex.committed
        // corpus_id=wikipedia source_doc_id=…` and answer "what did
        // the watcher actually write?" without attaching a Lance
        // reader.
        let elapsed_ms = t.elapsed().as_millis() as u64;
        tracing::info!(
            corpus_id,
            source_doc_id,
            chunks_written = written,
            elapsed_ms,
            "reindex.committed"
        );

        Ok(ReindexResult::Updated {
            chunks_written: written,
            elapsed_ms,
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

#[cfg(feature = "treesitter")]
fn file_mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// For a portal-event bullet chunk, narrow the `outgoing_links` array
/// in the parent doc's metadata down to those whose `target_title`
/// actually appears as a `[[…]]` wikilink inside this bullet.
///
/// The `wikipedia_api_article` extractor produces section-scoped
/// `WikipediaChunkMetadata.outgoing_links`. For per-event retrieval
/// we want bullet-scoped attribution — otherwise every bullet on the
/// page carries the union of every wikilink on the page, which
/// hollows out the freshness-daemon's per-bullet tracked-set
/// extraction.
fn rescope_outgoing_links_for_bullet(
    section_metadata: &Option<serde_json::Value>,
    bullet_text: &str,
) -> Option<serde_json::Value> {
    let mut meta = section_metadata.clone()?;
    let bullet_targets =
        crate::chunkers::portal_event_bullet::extract_bullet_links(bullet_text);

    let object = meta.as_object_mut()?;
    let bullet_target_set: std::collections::HashSet<&str> =
        bullet_targets.iter().map(String::as_str).collect();

    if let Some(serde_json::Value::Array(links)) = object.get("outgoing_links") {
        let filtered: Vec<serde_json::Value> = links
            .iter()
            .filter(|link| {
                let Some(target) = link.get("target_title").and_then(|v| v.as_str()) else {
                    return false;
                };
                let normalised = target.replace(' ', "_");
                bullet_target_set.contains(normalised.as_str())
                    || bullet_target_set.contains(target)
            })
            .cloned()
            .collect();
        object.insert("outgoing_links".to_string(), serde_json::Value::Array(filtered));
    }

    // Always emit the bullet-scoped link list as a flat string array
    // alongside, so MeshStore tracked-set extraction doesn't have to
    // walk the structured WikiLink JSON.
    object.insert(
        "outbound_links".to_string(),
        serde_json::Value::Array(
            bullet_targets
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );

    Some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::CorpusIndex;
    use crate::recipe::{ChunkerConfig, ExtractorConfig};
    use std::path::Path;
    use std::sync::Arc;

    fn mock_embed_fn() -> crate::types::EmbedFn {
        Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }))
    }

    async fn fixture_engine(
        index_dir: &Path,
    ) -> (
        CorpusEngine,
        CorpusIndex,
    ) {
        let recipes_dir = index_dir.parent().unwrap().join("recipes");
        std::fs::create_dir_all(&recipes_dir).unwrap();
        let engine = CorpusEngine::new(recipes_dir, index_dir.to_path_buf(), mock_embed_fn());

        let idx_path = index_dir.join("test-corpus");
        let index = CorpusIndex::create(
            &idx_path,
            "test-corpus",
            "Test Corpus",
            "test-model",
            4,
            false,
            "MIT",
        )
        .await
        .expect("create index");
        (engine, index)
    }

    #[tokio::test]
    async fn reindex_by_source_doc_id_inserts_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();
        let (engine, _index) = fixture_engine(&idx_dir).await;

        let result = engine
            .reindex_by_source_doc_id(
                "test-corpus",
                "Donald_Trump",
                "Body of the Donald Trump article. One paragraph.",
                &ExtractorConfig::Plaintext {
                    title_pattern: None,
                    strip_boilerplate: None,
                },
                &ChunkerConfig::Passthrough,
            )
            .await
            .expect("reindex absent doc");

        match result {
            ReindexResult::Updated {
                chunks_written,
                ..
            } => assert_eq!(chunks_written, 1, "passthrough chunker → one chunk"),
            other => panic!("expected Updated, got {other:?}"),
        }

        // Reopen the index and verify the chunk is queryable.
        let reopened = CorpusIndex::open(&idx_dir.join("test-corpus")).await.unwrap();
        assert_eq!(reopened.chunk_count().await.unwrap(), 1);
        let ids = reopened.list_indexed_source_doc_ids().await.unwrap();
        assert!(ids.contains("Donald_Trump"));
    }

    #[tokio::test]
    async fn reindex_by_source_doc_id_replaces_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();
        let (engine, _index) = fixture_engine(&idx_dir).await;

        // First call — initial insert.
        engine
            .reindex_by_source_doc_id(
                "test-corpus",
                "Joe_Biden",
                "Original revision of the Biden article.",
                &ExtractorConfig::Plaintext {
                    title_pattern: None,
                    strip_boilerplate: None,
                },
                &ChunkerConfig::Passthrough,
            )
            .await
            .expect("initial insert");

        // Second call with new content — must replace, not append.
        engine
            .reindex_by_source_doc_id(
                "test-corpus",
                "Joe_Biden",
                "Updated revision with substantially different content body.",
                &ExtractorConfig::Plaintext {
                    title_pattern: None,
                    strip_boilerplate: None,
                },
                &ChunkerConfig::Passthrough,
            )
            .await
            .expect("refresh");

        let reopened = CorpusIndex::open(&idx_dir.join("test-corpus")).await.unwrap();
        // Total chunk count is 1 — old must be replaced, not duplicated.
        // (If the delete-by-source-doc step had been skipped, count
        //  would be 2 with both revisions co-resident.)
        assert_eq!(
            reopened.chunk_count().await.unwrap(),
            1,
            "old chunk must be replaced, not duplicated",
        );
        let ids = reopened.list_indexed_source_doc_ids().await.unwrap();
        assert!(ids.contains("Joe_Biden"));
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn reindex_by_source_doc_id_returns_error_when_corpus_missing() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();
        let (engine, _index) = fixture_engine(&idx_dir).await;

        let err = engine
            .reindex_by_source_doc_id(
                "no-such-corpus",
                "Anything",
                "body",
                &ExtractorConfig::Plaintext {
                    title_pattern: None,
                    strip_boilerplate: None,
                },
                &ChunkerConfig::Passthrough,
            )
            .await;
        assert!(matches!(err, Err(Error::IndexNotFound(_))));
    }
}
