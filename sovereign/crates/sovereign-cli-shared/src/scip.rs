// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merged SCIP graph loader for code-intelligence tools.
//!
//! Lives here so both `sovereign-cli` (whose `tools_cmd` registry
//! opens the graph on every `sovereign tools` invocation) and
//! `sovereign-cli-atos` (whose `project_cmd::cmd_serve` opens the
//! graph at daemon startup) can share one implementation.
//!
//! Pre-split, this fn lived at `project_cmd::load_merged_graph` and
//! was a flagged TODO at `tools_cmd/registry.rs:37` ("Blocked on
//! moving `load_merged_graph` to a neutral location").

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use corpus_engine_scip::ScipGraph;

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
            eprintln!("    (none — run `sovereign project init` with SCIP exporters)");
        } else {
            eprintln!(
                "    Total: {} symbols, {} edges across {} projects",
                summary.total_symbols, summary.total_refs, summary.graphs_found
            );
        }
    }

    (merged, summary)
}

/// Collect the current mtimes of every `scip_graph.db` file under
/// `data_dir`. Used by the polling reloader to detect changes.
pub fn snapshot_graph_mtimes(data_dir: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut out = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let scip_path = path.join("scip_graph.db");
            if let Ok(md) = std::fs::metadata(&scip_path) {
                if let Ok(mtime) = md.modified() {
                    out.insert(scip_path, mtime);
                }
            }
        }
    }
    out
}
