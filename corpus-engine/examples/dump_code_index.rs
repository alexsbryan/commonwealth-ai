//! One-shot smoke test: open a code-indexed corpus and print the first
//! few rows, then run a `symbol_name = 'Runtime'` filter to prove the
//! typed code columns are populated and queryable.
//!
//! Usage:
//!     cargo run --example dump_code_index --features treesitter -- \
//!         /tmp/sov-code-test/sovereign
//!
//! Not part of the normal build path — intentionally stashed under
//! `examples/` so it ships as a throwaway for P1 validation.

use std::path::PathBuf;

use arrow_array::Array;
use corpus_engine::CorpusIndex;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: dump_code_index <index-path>");

    let index = CorpusIndex::open(&path).await?;
    let info = index.info().await?;
    println!("Corpus: {} ({} chunks)", info.corpus_id, info.chunk_count);
    println!("Schema version: see _corpus_meta.json");
    println!();

    // Dump the Arrow schema so we can see every column.
    let schema = index.table().schema().await?;
    println!("Columns:");
    for field in schema.fields() {
        println!("  {:<15} {:?}", field.name(), field.data_type());
    }
    println!();

    // Filter by file_path LIKE 'corpus-engine/%' to exercise metadata
    // pushdown on one of the new typed columns.
    println!("Top-5 symbols from sovereign-cli:");
    let rows: Vec<_> = index
        .table()
        .query()
        .only_if("file_path LIKE '%sovereign-cli%'")
        .limit(5)
        .execute()
        .await?
        .try_collect()
        .await?;

    for batch in &rows {
        for row in 0..batch.num_rows() {
            let symbol_name = batch
                .column_by_name("symbol_name")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::StringArray>())
                .map(|a| {
                    if a.is_null(row) {
                        "<null>".to_string()
                    } else {
                        a.value(row).to_string()
                    }
                })
                .unwrap_or_else(|| "?".into());
            let file_path = batch
                .column_by_name("file_path")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::StringArray>())
                .map(|a| a.value(row).to_string())
                .unwrap_or_else(|| "?".into());
            let symbol_kind = batch
                .column_by_name("symbol_kind")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::StringArray>())
                .map(|a| a.value(row).to_string())
                .unwrap_or_else(|| "?".into());
            let line_start = batch
                .column_by_name("line_start")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::Int32Array>())
                .map(|a| a.value(row))
                .unwrap_or(0);
            let language = batch
                .column_by_name("language")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::StringArray>())
                .map(|a| a.value(row).to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "  {symbol_name:<30} [{symbol_kind:<10}] {language:<10} {file_path}:{line_start}"
            );
        }
    }
    println!();

    // Now run a symbol_name exact-match filter — the primary P1 use case.
    println!("All Runtime symbols (symbol_name = 'Runtime'):");
    let rt_rows: Vec<_> = index
        .table()
        .query()
        .only_if("symbol_name = 'Runtime'")
        .limit(10)
        .execute()
        .await?
        .try_collect()
        .await?;
    let mut total = 0usize;
    for batch in &rt_rows {
        for row in 0..batch.num_rows() {
            total += 1;
            let file_path = batch
                .column_by_name("file_path")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::StringArray>())
                .map(|a| a.value(row).to_string())
                .unwrap_or_else(|| "?".into());
            let line_start = batch
                .column_by_name("line_start")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::Int32Array>())
                .map(|a| a.value(row))
                .unwrap_or(0);
            let symbol_kind = batch
                .column_by_name("symbol_kind")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::StringArray>())
                .map(|a| a.value(row).to_string())
                .unwrap_or_else(|| "?".into());
            println!("  [{symbol_kind}] {file_path}:{line_start}");
        }
    }
    println!("  → {total} matches");
    println!();

    // Recent changes (mtime filter) as the third Phase-1 proof.
    let since = chrono::Utc::now().timestamp() - 24 * 3600;
    let recent_rows: Vec<_> = index
        .table()
        .query()
        .only_if(format!("mtime > {since}"))
        .limit(5)
        .execute()
        .await?
        .try_collect()
        .await?;
    let recent_total: usize = recent_rows.iter().map(|b| b.num_rows()).sum();
    println!("Symbols modified in the last 24h (mtime > {since}): {recent_total} in first 5 rows");

    Ok(())
}
