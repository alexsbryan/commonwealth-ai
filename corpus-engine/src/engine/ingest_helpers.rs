//! Free-fn helpers extracted out of `engine::ingest`.
//!
//! These are the pure, no-`&self`, no-shared-state utilities that the
//! ingest pipeline calls into. Pulled out so `ingest.rs` can shrink
//! toward the §3.1 ceiling without changing call shapes.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;

use crate::progress::{SourceFileManifest, SourceFileStatus};
use crate::recipe::{ExtractorConfig, Recipe};

/// Set the `shard_indices` field on a recipe's `WikipediaJsonl` extractor
/// config. No-op for recipes with any other extractor — the caller has
/// already determined that sharding applies to this corpus (see
/// [`crate::engine::CorpusEngine::jsonl_source_shard_count`]).
///
/// Shared helper for both
/// [`crate::engine::CorpusEngine::ingest`] (solo / legacy path) and
/// [`crate::engine::CorpusEngine::ingest_with_overrides`] (peer /
/// coordinator path) so the two entry points stay in sync about how
/// partition assignments reach the extractor.
pub(crate) fn apply_jsonl_shard_override(recipe: &mut Recipe, indices: Option<Vec<usize>>) {
    if let ExtractorConfig::WikipediaJsonl {
        ref mut shard_indices,
        ..
    } = recipe.extract
    {
        *shard_indices = indices;
    }
}

/// Recursively sum file sizes under `root`. Used to populate
/// `IngestResult.index_size_bytes` after a prebuilt-snapshot restore.
///
/// Returns `None` only if the root itself can't be read; missing or
/// unreadable sub-entries are silently skipped.
pub(crate) fn dir_size_recursive(root: &Path) -> Option<u64> {
    let mut stack = vec![root.to_path_buf()];
    let mut total: u64 = 0;
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    Some(total)
}

/// A file is complete when `committed_iter_pos >= file_boundary_iter_pos`:
/// since `update_committed_iter_pos(iter_pos)` just ran, all documents up to
/// `iter_pos` are durably written.  If a file's last document was at or before
/// that position, every chunk from that file is now in the index.
pub(crate) fn mark_complete_files(
    committed_iter_pos: u64,
    file_boundary_iter_pos: &HashMap<String, u64>,
    flushed_chunks_per_file: &HashMap<String, u64>,
    manifest: Option<&mut SourceFileManifest>,
    index_path: &Path,
) {
    let Some(manifest) = manifest else { return };
    let mut changed = false;
    for (filename, &boundary) in file_boundary_iter_pos {
        if committed_iter_pos < boundary {
            continue;
        }
        if let Some(record) = manifest.files.iter_mut().find(|r| &r.filename == filename) {
            if !matches!(record.status, SourceFileStatus::Complete { .. }) {
                let chunks_indexed = *flushed_chunks_per_file.get(filename).unwrap_or(&0);
                record.status = SourceFileStatus::Complete {
                    chunks_indexed,
                    completed_at: Utc::now(),
                };
                tracing::info!(
                    filename,
                    chunks_indexed,
                    "Source file fully committed to index"
                );
                changed = true;
            }
        }
    }
    if changed {
        manifest.updated_at = Utc::now();
        if let Err(e) = manifest.save(index_path) {
            tracing::warn!("Failed to persist source manifest: {e}");
        }
    }
}

/// JSONL counterpart of `mark_complete_files`.
///
/// When the Wikipedia JSONL extractor runs in sharded mode it stamps every
/// document with `source_file = Some("shard:<n>")`. This helper parses those
/// tags out of `file_boundary_iter_pos`, and for any shard whose boundary
/// has now been durably committed to LanceDB (`committed_iter_pos >=
/// boundary`), writes the shard index into `_corpus_meta.json`'s
/// `processed_shards` array.
///
/// The coordinator reads `processed_shards` from every partition
/// subdirectory when planning the next collaborative ingest so it knows
/// which shards still need work — the sharded analogue of
/// `remaining_source_files` for HF parquet corpora.
///
/// `recorded` is the in-run memoization of which shards we've already
/// persisted, so a flush that passes the boundary of an already-recorded
/// shard doesn't rewrite the meta file.
pub(crate) fn mark_complete_shards(
    committed_iter_pos: u64,
    file_boundary_iter_pos: &HashMap<String, u64>,
    recorded: &mut std::collections::HashSet<usize>,
    index: &crate::index::CorpusIndex,
) {
    for (tag, &boundary) in file_boundary_iter_pos {
        let Some(shard_index) = crate::extractors::wikipedia_jsonl::parse_shard_source_file(tag)
        else {
            continue;
        };
        if recorded.contains(&shard_index) || committed_iter_pos < boundary {
            continue;
        }
        match index.record_processed_shard(shard_index) {
            Ok(()) => {
                tracing::info!(
                    shard_index,
                    committed_iter_pos,
                    boundary,
                    "JSONL shard fully committed to index"
                );
                recorded.insert(shard_index);
            }
            Err(e) => {
                tracing::warn!(
                    shard_index,
                    error = %e,
                    "failed to persist processed_shards entry — will retry next flush"
                );
            }
        }
    }
}
