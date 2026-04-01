use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sovereign_core::error::Result;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::DocumentChunk;

use super::chunk::chunk_text;
use super::parse::{list_parseable_files, parse_file};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Ingest results summary.
pub struct IngestResult {
    pub files_processed: usize,
    pub files_skipped: usize,
    pub chunks_created: usize,
}

/// Ingest all parseable files from a directory.
/// Pipeline: parse → chunk → (optionally embed) → store
pub async fn ingest_directory(
    dir: &Path,
    store: &dyn StateStore,
    inference: Option<&dyn InferenceProvider>,
) -> Result<IngestResult> {
    let files = list_parseable_files(dir)?;
    let mut files_processed = 0;
    let mut files_skipped = 0;
    let mut chunks_created = 0;

    for file_path in &files {
        match ingest_file(file_path, store, inference).await {
            Ok(n) => {
                files_processed += 1;
                chunks_created += n;
                eprintln!(
                    "[ingest] {} → {} chunks",
                    file_path.file_name().unwrap_or_default().to_string_lossy(),
                    n,
                );
            }
            Err(e) => {
                files_skipped += 1;
                eprintln!(
                    "[ingest] Skipped {}: {e}",
                    file_path.file_name().unwrap_or_default().to_string_lossy(),
                );
            }
        }
    }

    Ok(IngestResult {
        files_processed,
        files_skipped,
        chunks_created,
    })
}

/// Ingest a single file. Returns the number of chunks created.
pub async fn ingest_file(
    path: &Path,
    store: &dyn StateStore,
    inference: Option<&dyn InferenceProvider>,
) -> Result<usize> {
    let doc = parse_file(path)?;
    let text_chunks = chunk_text(&doc.content);

    let mut chunks = Vec::with_capacity(text_chunks.len());
    for tc in &text_chunks {
        // Try to generate embedding if inference is available.
        let embedding = if let Some(inf) = inference {
            inf.embed(&tc.content).await.ok()
        } else {
            None
        };

        chunks.push(DocumentChunk {
            id: format!("{}:{}", doc.source, tc.index),
            source: doc.source.clone(),
            content: tc.content.clone(),
            chunk_index: tc.index,
            embedding,
            created_at: now(),
        });
    }

    let count = chunks.len();
    store.store_chunks(&chunks).await?;
    Ok(count)
}
