// SPDX-License-Identifier: AGPL-3.0-or-later
//! One-shot helper: build the BTree scalar index on `title` for an
//! installed corpus, so `fetch_chunks_by_title`'s `only_if` predicate
//! seeks instead of scanning (~500ms → ms on wikipedia's 1.9M rows).
//!
//! Usage:
//!     cargo run -p corpus-engine --example build_title_btree -- \
//!         ~/.svrnmesh/indexes/wikipedia

use std::path::PathBuf;

use corpus_engine::CorpusIndex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: build_title_btree <index-path>");

    let started = std::time::Instant::now();
    let index = CorpusIndex::open(&path).await?;
    println!("Opened {}", path.display());
    index.build_title_scalar_index().await?;
    println!("BTree title index built in {:?}", started.elapsed());
    Ok(())
}
