// SPDX-License-Identifier: AGPL-3.0-or-later
//! One-shot helper: open an installed corpus and (re)build its FTS
//! indexes. Useful when an old corpus has `content_fts_built = false`
//! (e.g. the `wikipedia` Vital-L5 install pre-dated the FTS phase
//! being part of the standard ingest pipeline).
//!
//! Usage:
//!     cargo run --release -p corpus-engine --example build_fts -- \
//!         ~/.sovereign/indexes/wikipedia
//!
//! Skips the vector-index rebuild — that path is expensive and the
//! existing IVF-PQ index is fine. We just need title + content FTS.

use std::path::PathBuf;

use corpus_engine::CorpusIndex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: build_fts <index-path>");

    let started = std::time::Instant::now();
    let index = CorpusIndex::open(&path).await?;
    let info = index.info().await?;
    println!(
        "Opened {} ({} chunks, dim={})",
        info.corpus_id, info.chunk_count, info.embedding_dimensions
    );

    println!("Building FTS indexes (vector skipped)...");
    index
        .build_indexes(
            /* build_vector */ false, /* build_fts */ true, None,
        )
        .await?;
    index.mark_ingestion_complete()?;

    let elapsed = started.elapsed();
    println!("FTS build complete in {:.1}s", elapsed.as_secs_f64());
    Ok(())
}
