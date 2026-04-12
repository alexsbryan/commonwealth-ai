//! Language-agnostic SCIP exporter dispatch.
//!
//! Detects which languages are present in a workspace, checks which SCIP
//! exporter binaries are available on PATH, runs them, and ingests the
//! results into a [`ScipGraph`](crate::scip_graph::ScipGraph).
//!
//! ## Adding a new language
//!
//! 1. Add an entry to [`all_exporters`].
//! 2. Ensure the SCIP exporter binary exists (e.g. `scip-go`).
//! 3. No other files change — the dispatch, parsing, and ingestion are
//!    all language-agnostic.

use std::path::Path;

use prost::Message;

use crate::error::{Error, Result};
use crate::scip_graph::{ScipGraph, ScipRefRecord, ScipSymbolRecord};
use crate::scip_proto;

// ─── Exporter configuration ─────────────────────────────────

/// A language server that can export SCIP.
#[derive(Debug, Clone)]
pub struct ScipExporterConfig {
    pub language_id: &'static str,
    /// The command to invoke for SCIP export.
    pub command: &'static str,
    /// Arguments. `{output}` is replaced with the output path.
    pub args: &'static [&'static str],
    /// File extensions this exporter covers.
    pub extensions: &'static [&'static str],
    /// Whether this exporter analyzes the whole workspace or per-file.
    pub workspace_level: bool,
    /// Install hint shown when the binary is missing.
    pub install_hint: &'static str,
}

/// All supported SCIP exporters. Order matters: first match wins for a
/// given extension.
pub fn all_exporters() -> &'static [ScipExporterConfig] {
    &[
        ScipExporterConfig {
            language_id: "rust",
            command: "rust-analyzer",
            args: &["scip", "--output", "{output}"],
            extensions: &["rs"],
            workspace_level: true,
            install_hint: "Install via rustup: rustup component add rust-analyzer",
        },
        ScipExporterConfig {
            language_id: "go",
            command: "scip-go",
            args: &["--output", "{output}"],
            extensions: &["go"],
            workspace_level: true,
            install_hint: "Install with: go install github.com/sourcegraph/scip-go@latest",
        },
        ScipExporterConfig {
            language_id: "typescript",
            command: "scip-typescript",
            args: &["index", "--output", "{output}"],
            extensions: &["ts", "tsx", "js", "jsx"],
            workspace_level: true,
            install_hint: "Install with: npm install -g @sourcegraph/scip-typescript",
        },
        ScipExporterConfig {
            language_id: "python",
            command: "scip-python",
            args: &["index", ".", "--output", "{output}"],
            extensions: &["py"],
            workspace_level: true,
            install_hint: "Install with: pip install scip-python",
        },
        ScipExporterConfig {
            language_id: "java",
            command: "scip-java",
            args: &["index", "--output", "{output}"],
            extensions: &["java"],
            workspace_level: true,
            install_hint: "See https://sourcegraph.github.io/scip-java/",
        },
    ]
}

/// Detect which exporters apply to a workspace and are available.
pub fn exporters_for_workspace(repo_root: &Path) -> Vec<&'static ScipExporterConfig> {
    let mut found = Vec::new();

    for exporter in all_exporters() {
        let has_files = exporter.extensions.iter().any(|ext| {
            glob::glob(&format!("{}/**/*.{}", repo_root.display(), ext))
                .map(|mut g| g.next().is_some())
                .unwrap_or(false)
        });

        if has_files {
            if which::which(exporter.command).is_ok() {
                found.push(exporter);
            } else {
                tracing::warn!(
                    command = exporter.command,
                    language = exporter.language_id,
                    "SCIP exporter not found in PATH — skipping {} call graph. {}",
                    exporter.language_id,
                    exporter.install_hint,
                );
            }
        }
    }

    found
}

// ─── Export summary ──────────────────────────────────────────

/// Result of running all applicable SCIP exporters.
#[derive(Debug, Default)]
pub struct ExportSummary {
    pub languages_exported: Vec<String>,
    pub languages_skipped: Vec<SkippedLanguage>,
    pub total_symbols: usize,
    pub total_refs: usize,
}

#[derive(Debug)]
pub struct SkippedLanguage {
    pub language: String,
    pub reason: String,
    pub install_hint: String,
}

/// Progress callback for the export process.
pub enum ScipProgress<'a> {
    Exporting { language: &'a str },
    Ingested { language: &'a str, symbols: usize, refs: usize },
    Skipped { language: &'a str, reason: &'a str },
}

// ─── Export runner ───────────────────────────────────────────

/// Run all applicable SCIP exporters and ingest results into the graph.
///
/// Returns a summary of what was exported and what was skipped.
pub async fn export_all(
    repo_root: &Path,
    output_dir: &Path,
    graph: &ScipGraph,
    progress: &dyn Fn(ScipProgress<'_>),
) -> Result<ExportSummary> {
    let exporters = exporters_for_workspace(repo_root);
    let mut summary = ExportSummary::default();

    if exporters.is_empty() {
        tracing::warn!("No SCIP exporters available — call graph will be empty");
        return Ok(summary);
    }

    // Clear existing data before re-importing.
    graph.clear().await?;

    std::fs::create_dir_all(output_dir)
        .map_err(|e| Error::Io(e))?;

    for exporter in exporters {
        progress(ScipProgress::Exporting {
            language: exporter.language_id,
        });

        let scip_path = output_dir.join(format!("{}.scip", exporter.language_id));

        let args: Vec<String> = exporter
            .args
            .iter()
            .map(|a| a.replace("{output}", scip_path.to_str().unwrap_or("")))
            .collect();

        let status = tokio::process::Command::new(exporter.command)
            .args(&args)
            .current_dir(repo_root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .await
            .map_err(|e| Error::Io(e))?;

        if !status.success() {
            tracing::warn!(
                language = exporter.language_id,
                "SCIP export failed — {} call graph unavailable",
                exporter.language_id,
            );
            summary.languages_skipped.push(SkippedLanguage {
                language: exporter.language_id.to_string(),
                reason: format!("{} exited with status {}", exporter.command, status),
                install_hint: exporter.install_hint.to_string(),
            });
            progress(ScipProgress::Skipped {
                language: exporter.language_id,
                reason: "export failed",
            });
            continue;
        }

        // Parse and ingest this language's SCIP file.
        let (symbols, refs) = parse_scip_file(&scip_path, exporter.language_id)?;
        let sym_count = symbols.len();
        let ref_count = refs.len();

        graph.ingest_symbols_and_refs(symbols, refs).await?;

        summary.languages_exported.push(exporter.language_id.to_string());
        summary.total_symbols += sym_count;
        summary.total_refs += ref_count;

        progress(ScipProgress::Ingested {
            language: exporter.language_id,
            symbols: sym_count,
            refs: ref_count,
        });

        // Clean up the SCIP file.
        let _ = std::fs::remove_file(&scip_path);
    }

    // Record the export in the graph.
    graph.record_export().await;
    graph
        .record_languages(
            &summary
                .languages_exported
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
        )
        .await;

    Ok(summary)
}

// ─── SCIP file parsing ──────────────────────────────────────

/// Parse a SCIP protobuf file and extract symbols + references.
///
/// The SCIP format encodes an `Index` containing `Document`s, each with
/// `Occurrence`s and `SymbolInformation`. We build caller/callee
/// relationships by tracking which function scope each occurrence falls
/// within.
pub fn parse_scip_file(
    path: &Path,
    language_id: &str,
) -> Result<(Vec<ScipSymbolRecord>, Vec<ScipRefRecord>)> {
    let data = std::fs::read(path)
        .map_err(|e| Error::Io(e))?;

    let index = scip_proto::Index::decode(&*data)
        .map_err(|e| Error::Database(format!("SCIP decode: {e}")))?;

    let mut symbols = Vec::new();
    let mut refs = Vec::new();

    for doc in &index.documents {
        let file_path = &doc.relative_path;
        let language = if doc.language.is_empty() {
            language_id.to_string()
        } else {
            doc.language.clone()
        };

        // Collect symbol definitions from SymbolInformation entries.
        for sym_info in &doc.symbols {
            if sym_info.symbol.is_empty() {
                continue;
            }
            let display_name = if sym_info.display_name.is_empty() {
                scip_proto::extract_symbol_name(&sym_info.symbol)
            } else {
                sym_info.display_name.clone()
            };

            // Find the definition occurrence to get line numbers.
            let (line_start, line_end) = doc
                .occurrences
                .iter()
                .find(|occ| {
                    occ.symbol == sym_info.symbol
                        && (occ.symbol_roles & scip_proto::SymbolRole::DEFINITION) != 0
                })
                .map(|occ| {
                    let start = occ.range.first().copied().unwrap_or(0);
                    let end = if occ.range.len() >= 3 {
                        occ.range[2]
                    } else {
                        start
                    };
                    (start, end)
                })
                .unwrap_or((0, 0));

            symbols.push(ScipSymbolRecord {
                name: display_name,
                kind: scip_proto::kind_to_str(sym_info.kind).to_string(),
                file_path: file_path.clone(),
                line_start,
                line_end,
                language: language.clone(),
            });
        }

        // Build caller/callee references.
        // Strategy: For each non-definition occurrence, determine which
        // enclosing definition scope it falls within. That scope is the
        // "caller", and the occurrence's symbol is the "callee".
        let mut def_scopes: Vec<(&str, i32, i32, String)> = Vec::new(); // (scip_symbol, start_line, end_line, display_name)
        for occ in &doc.occurrences {
            if (occ.symbol_roles & scip_proto::SymbolRole::DEFINITION) != 0 {
                let start = occ.range.first().copied().unwrap_or(0);
                let end = if occ.enclosing_range.len() >= 3 {
                    occ.enclosing_range[2]
                } else if occ.range.len() >= 3 {
                    occ.range[2]
                } else {
                    start + 50 // reasonable default scope
                };
                let display = doc
                    .symbols
                    .iter()
                    .find(|s| s.symbol == occ.symbol)
                    .map(|s| {
                        if s.display_name.is_empty() {
                            scip_proto::extract_symbol_name(&s.symbol)
                        } else {
                            s.display_name.clone()
                        }
                    })
                    .unwrap_or_else(|| scip_proto::extract_symbol_name(&occ.symbol));
                def_scopes.push((&occ.symbol, start, end, display));
            }
        }

        // Sort scopes by start line for binary search.
        def_scopes.sort_by_key(|&(_, start, _, _)| start);

        for occ in &doc.occurrences {
            // Skip definitions — they're not "calls".
            if (occ.symbol_roles & scip_proto::SymbolRole::DEFINITION) != 0 {
                continue;
            }
            if occ.symbol.is_empty() {
                continue;
            }

            let occ_line = occ.range.first().copied().unwrap_or(0);

            // Find the enclosing definition scope (caller).
            let caller = def_scopes
                .iter()
                .rev()
                .find(|&&(_, start, end, _)| occ_line >= start && occ_line <= end);

            if let Some(&(_, _, _, ref caller_name)) = caller {
                let callee_name = doc
                    .symbols
                    .iter()
                    .find(|s| s.symbol == occ.symbol)
                    .map(|s| {
                        if s.display_name.is_empty() {
                            scip_proto::extract_symbol_name(&s.symbol)
                        } else {
                            s.display_name.clone()
                        }
                    })
                    .or_else(|| {
                        index.external_symbols.iter().find(|s| s.symbol == occ.symbol).map(|s| {
                            if s.display_name.is_empty() {
                                scip_proto::extract_symbol_name(&s.symbol)
                            } else {
                                s.display_name.clone()
                            }
                        })
                    })
                    .unwrap_or_else(|| scip_proto::extract_symbol_name(&occ.symbol));

                refs.push(ScipRefRecord {
                    caller_symbol: caller_name.clone(),
                    callee_symbol: callee_name,
                    file_path: file_path.clone(),
                    line: occ_line,
                    ref_kind: "direct".to_string(),
                });
            }
        }
    }

    Ok((symbols, refs))
}

/// Format a human-readable export report.
pub fn format_export_report(summary: &ExportSummary) -> String {
    let mut out = String::new();

    if !summary.languages_exported.is_empty() {
        out.push_str(&format!(
            "Call graph available for: {}\n",
            summary.languages_exported.join(", ")
        ));
        out.push_str(&format!(
            "  {} symbols, {} references\n",
            summary.total_symbols, summary.total_refs
        ));
    }

    for skipped in &summary.languages_skipped {
        out.push_str(&format!(
            "Skipping {} call graph — {}\n  {}\n",
            skipped.language, skipped.reason, skipped.install_hint
        ));
    }

    if summary.languages_exported.is_empty() && summary.languages_skipped.is_empty() {
        out.push_str("No languages detected — call graph is empty.\n");
    }

    out
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_exporters_not_empty() {
        assert!(!all_exporters().is_empty());
    }

    #[test]
    fn all_exporters_have_install_hints() {
        for e in all_exporters() {
            assert!(
                !e.install_hint.is_empty(),
                "Exporter {} missing install hint",
                e.language_id
            );
        }
    }

    #[test]
    fn rust_exporter_covers_rs_files() {
        let rust = all_exporters()
            .iter()
            .find(|e| e.language_id == "rust")
            .expect("rust exporter not found");
        assert!(rust.extensions.contains(&"rs"));
        assert_eq!(rust.command, "rust-analyzer");
    }

    #[test]
    fn typescript_exporter_covers_ts_tsx() {
        let ts = all_exporters()
            .iter()
            .find(|e| e.language_id == "typescript")
            .expect("typescript exporter not found");
        assert!(ts.extensions.contains(&"ts"));
        assert!(ts.extensions.contains(&"tsx"));
        assert!(ts.extensions.contains(&"js"));
        assert!(ts.extensions.contains(&"jsx"));
    }

    #[test]
    fn format_export_report_empty() {
        let summary = ExportSummary::default();
        let report = format_export_report(&summary);
        assert!(report.contains("No languages detected"));
    }

    #[test]
    fn format_export_report_with_data() {
        let summary = ExportSummary {
            languages_exported: vec!["rust".into()],
            languages_skipped: vec![SkippedLanguage {
                language: "TypeScript".into(),
                reason: "scip-typescript not found in PATH".into(),
                install_hint: "npm install -g @sourcegraph/scip-typescript".into(),
            }],
            total_symbols: 847,
            total_refs: 1200,
        };
        let report = format_export_report(&summary);
        assert!(report.contains("rust"));
        assert!(report.contains("847 symbols"));
        assert!(report.contains("TypeScript"));
        assert!(report.contains("npm install"));
    }
}
