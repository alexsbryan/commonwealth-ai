// SPDX-License-Identifier: AGPL-3.0-or-later
//! Is the edges.lance BTree actually serving `only_if` equality?
//! A leaf article (~5 edges) should answer in ~ms if index-seeked and
//! ~70ms+ if the filter scans 7.3M rows.

use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::path::PathBuf;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: check_edge_index <atlas-dir>");
    let db = lancedb::connect(dir.to_str().unwrap()).execute().await?;
    let edges = db.open_table("edges").execute().await?;

    println!("indices: {:?}", edges.list_indices().await?);

    for title in ["Meitnerium", "Tenrikyo", "World War II"] {
        // warm
        let _ = edges
            .query()
            .only_if(format!("source_title = '{title}'"))
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        let t = Instant::now();
        let batches = edges
            .query()
            .only_if(format!("source_title = '{title}'"))
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        println!("{title:24} rows={rows:5}  {:?}", t.elapsed());
    }
    Ok(())
}
