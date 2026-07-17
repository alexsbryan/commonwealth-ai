// SPDX-License-Identifier: AGPL-3.0-or-later
//! One-shot helper: build BTree scalar indexes on the columnar wiki
//! graph's `edges.lance` (`source_title`, `target_title`) and
//! `articles.lance` (`title`), so the `only_if` point lookups in
//! `ColumnarWikipediaGraph` seek instead of scanning 7.3M rows
//! (measured 2026-07-17: ~100-150ms per neighbors() call, 1-2.2s per
//! PPR walk — the same filtered-scan disease `build_title_btree`
//! cured on the chunks table).
//!
//! Usage:
//!     cargo run -p corpus-engine --example build_edge_btree -- \
//!         ~/.sovereign/indexes/wikipedia/atlas

use lancedb::index::scalar::BTreeIndexBuilder;
use lancedb::index::Index;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: build_edge_btree <atlas-dir>");
    let db = lancedb::connect(dir.to_str().unwrap()).execute().await?;

    let started = std::time::Instant::now();
    let edges = db.open_table("edges").execute().await?;
    for col in ["source_title", "target_title"] {
        edges
            .create_index(&[col], Index::BTree(BTreeIndexBuilder::default()))
            .execute()
            .await?;
        println!("edges.{col} BTree built ({:?})", started.elapsed());
    }
    let articles = db.open_table("articles").execute().await?;
    articles
        .create_index(&["title"], Index::BTree(BTreeIndexBuilder::default()))
        .execute()
        .await?;
    println!("articles.title BTree built ({:?})", started.elapsed());
    Ok(())
}
