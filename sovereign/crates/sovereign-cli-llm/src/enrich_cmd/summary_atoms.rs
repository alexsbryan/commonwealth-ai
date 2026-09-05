// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich summary-atoms <corpus>` — project a corpus's existing RAPTOR
//! summaries into `Summary` atoms in its atlases (ei-7a).
//!
//! The sibling of [`raptor_index`](super::raptor_index) and deliberately
//! shaped like it: one positional corpus id, the same `data_dir` derivation,
//! no daemon and no inference. **It embeds nothing** — the vectors already
//! exist beside the summaries, and reusing them is the whole point (a second
//! embed would be a second decider for the seed space, ARCH §10.6).
//!
//! It is idempotent, so a re-run after adding summaries adds only the new
//! ones; and it prints its degradations rather than a bare count, because on
//! this box most RAPTOR nodes have no surviving tree and an atom count alone
//! cannot tell that from a writer that dropped them.

use sovereign_cli_shared::help;
use sovereign_tools::summary_atoms::write_summary_atoms;

pub async fn cmd_summary_atoms(args: &[String]) -> i32 {
    if help::wants_help(args) {
        print_usage();
        return 0;
    }
    let corpus_id = match args.iter().find(|a| !a.starts_with('-')) {
        Some(c) => c.clone(),
        None => {
            eprintln!("error: missing <corpus>\n");
            print_usage();
            return 2;
        }
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());
    let indexes_dir = data_dir.join("indexes");
    if !indexes_dir.join(&corpus_id).exists() {
        eprintln!(
            "error: corpus '{corpus_id}' is not installed at {}",
            indexes_dir.join(&corpus_id).display()
        );
        return 1;
    }

    println!("Projecting RAPTOR summaries into '{corpus_id}' atlases…");
    match write_summary_atoms(&indexes_dir, &corpus_id).await {
        Ok(report) => {
            println!("  {}", report.describe());
            // A run that read rows and wrote nothing is not a success to
            // report as one — either every node was already projected (said
            // so above) or nothing resolved, and the caller needs to tell
            // those apart from an exit code (ARCH §18.3).
            if report.rows_read > 0
                && report.atoms_written == 0
                && report.skipped_already_present == 0
            {
                eprintln!(
                    "error: {} summary rows read and no atom written — see the degradations above",
                    report.rows_read
                );
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage: svrn enrich summary-atoms <corpus>\n\n\
         Project the corpus's EXISTING raptor_summaries.lance rows into\n\
         `Summary` atoms in its per-article atlases, reusing the stored\n\
         embeddings as seed rows. No RAPTOR pass, no inference, no re-embed.\n\
         Idempotent: a node already projected is skipped.\n\n\
         Evidence chunks and `Composes` edges come from the `_raptor_checkpoint`\n\
         tree and are written only where that tree still has the node; how many\n\
         did NOT is printed as a degradation, never defaulted to zero."
    );
}
