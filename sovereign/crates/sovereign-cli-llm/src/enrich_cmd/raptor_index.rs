// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich raptor-index <corpus>` — (re)build the derived RAPTOR
//! summary-node ANN index (`raptor_summaries.lance`) from a corpus's
//! `conv_raptor_nodes`.
//!
//! The auto-hook at the end of an `enrich raptor` run calls the SAME builder
//! (`sovereign_tools::raptor_index::build_corpus_raptor_index`); this verb is
//! the re-runnable escape hatch — rebuild after adding nodes out-of-band, or
//! build the index for a corpus that was RAPTOR-enriched before this feature
//! shipped. It is read-only over SQLite plus a single LanceDB write (no daemon
//! or inference), so it is far lighter than `enrich raptor`.

use std::path::PathBuf;

use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::raptor_index::{build_corpus_raptor_index, RaptorIndexOutcome};

use sovereign_cli_shared::help;

pub async fn cmd_raptor_index(args: &[String]) -> i32 {
    if help::wants_help(args) {
        print_usage();
        return 0;
    }
    // One positional arg: the corpus id.
    let corpus_id = match args.iter().find(|a| !a.starts_with('-')) {
        Some(c) => c.clone(),
        None => {
            eprintln!("error: missing <corpus>\n");
            print_usage();
            return 2;
        }
    };

    // Same path derivation as `enrich raptor` (daemon-compatible): `data_dir`
    // owns both the state DB and the indexes dir.
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let indexes_dir = data_dir.join("indexes");
    let db_path = data_dir.join("sovereign.db");
    let index_path = indexes_dir.join(&corpus_id);

    if !index_path.exists() {
        eprintln!(
            "error: corpus '{corpus_id}' is not installed at {}",
            index_path.display()
        );
        return 1;
    }

    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: open state db {}: {e}", db_path.display());
            return 1;
        }
    };

    println!("Building RAPTOR summary-node ANN index for '{corpus_id}'…");
    let outcome = build_corpus_raptor_index(&store, &index_path, &corpus_id).await;
    println!("  {outcome}");

    match outcome {
        RaptorIndexOutcome::Failed { .. } => 1,
        _ => 0,
    }
}

fn print_usage() {
    eprintln!(
        "Usage: svrn enrich raptor-index <corpus>\n\n\
         (Re)build the RAPTOR summary-node ANN index (raptor_summaries.lance)\n\
         from a corpus's conv_raptor_nodes. Run `enrich raptor` first to build\n\
         the trees; this verb is the re-runnable index refresh (the `enrich\n\
         raptor` run already builds it once)."
    );
}
