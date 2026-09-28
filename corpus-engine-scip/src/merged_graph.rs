// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one merged SCIP graph loader (phase-b pb-code-freshness): every
//! `<indexes>/*/scip_graph.db` merged into one in-memory graph. It was two,
//! sovereign-cli-shared's `scip` and the daemon's `build_merged_scip_graph`;
//! both callers link this leaf, and the first still re-exports it.

use std::path::Path;

use crate::ScipGraph;

/// Summary returned by [`load_merged_graph`] — aggregated counts for
/// the startup banner and structured logging.
#[derive(Debug, Clone, Copy, Default)]
pub struct MergedGraphSummary {
    pub graphs_found: usize,
    pub total_symbols: usize,
    pub total_refs: usize,
}

/// Walk `data_dir/*/scip_graph.db` and merge each into a fresh
/// in-memory ScipGraph. If `verbose`, prints a per-graph line to
/// stderr (used for the startup banner); reloads pass `false`.
pub async fn load_merged_graph(data_dir: &Path, verbose: bool) -> (ScipGraph, MergedGraphSummary) {
    let merged = ScipGraph::open_in_memory("merged").expect("in-memory ScipGraph");

    let mut summary = MergedGraphSummary::default();

    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let scip_path = path.join("scip_graph.db");
            if !scip_path.exists() {
                continue;
            }
            let corpus_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            match merged.import_from_path(&scip_path).await {
                Ok((syms, refs)) => {
                    if syms > 0 || refs > 0 {
                        tracing::info!(
                            corpus = %corpus_name,
                            symbols = syms,
                            references = refs,
                            "merged SCIP graph from corpus"
                        );
                        if verbose {
                            eprintln!(
                                "    \u{2713} {corpus_name}: {} symbols, {} edges",
                                syms, refs
                            );
                        }
                        summary.total_symbols += syms;
                        summary.total_refs += refs;
                        summary.graphs_found += 1;
                    }
                }
                Err(e) => {
                    if verbose {
                        eprintln!("    \u{2717} {corpus_name}: {e}");
                    } else {
                        tracing::warn!(
                            corpus = %corpus_name,
                            error = %e,
                            "scip reload: import_from_path failed"
                        );
                    }
                }
            }
        }
    }

    if verbose {
        if summary.graphs_found == 0 {
            eprintln!("    (none — run `svrn project init` with SCIP exporters)");
        } else {
            eprintln!(
                "    Total: {} symbols, {} edges across {} projects",
                summary.total_symbols, summary.total_refs, summary.graphs_found
            );
        }
    }

    (merged, summary)
}
