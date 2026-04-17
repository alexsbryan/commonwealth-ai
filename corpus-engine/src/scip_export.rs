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
    /// `{config}` is replaced with a temp config file path (written from `config_json`).
    pub args: &'static [&'static str],
    /// File extensions this exporter covers.
    pub extensions: &'static [&'static str],
    /// Whether this exporter analyzes the whole workspace or per-file.
    pub workspace_level: bool,
    /// Install hint shown when the binary is missing.
    pub install_hint: &'static str,
    /// Optional JSON configuration written to a temp file before the export
    /// command runs. When set, `{config}` in `args` is replaced with the
    /// temp file path, which is deleted after the command completes.
    pub config_json: Option<&'static str>,
}

/// All supported SCIP exporters. Order matters: first match wins for a
/// given extension.
pub fn all_exporters() -> &'static [ScipExporterConfig] {
    &[
        ScipExporterConfig {
            language_id: "rust",
            command: "rust-analyzer",
            // `--config-path {config}` tells rust-analyzer to use all cargo features so
            // that feature-gated modules (e.g. `#[cfg(feature = "treesitter")]`) are
            // included in the SCIP output and visible to find_callers / find_callees.
            args: &["scip", ".", "--config-path", "{config}", "--output", "{output}"],
            extensions: &["rs"],
            workspace_level: true,
            install_hint: "Install via rustup: rustup component add rust-analyzer",
            config_json: Some(r#"{"cargo":{"features":"all"}}"#),
        },
        ScipExporterConfig {
            language_id: "go",
            command: "scip-go",
            args: &["--output", "{output}"],
            extensions: &["go"],
            workspace_level: true,
            install_hint: "Install with: go install github.com/sourcegraph/scip-go@latest",
            config_json: None,
        },
        ScipExporterConfig {
            language_id: "typescript",
            command: "scip-typescript",
            args: &["index", "--output", "{output}"],
            extensions: &["ts", "tsx", "js", "jsx"],
            workspace_level: true,
            install_hint: "Install with: npm install -g @sourcegraph/scip-typescript",
            config_json: None,
        },
        ScipExporterConfig {
            language_id: "python",
            command: "scip-python",
            args: &["index", ".", "--output", "{output}"],
            extensions: &["py"],
            workspace_level: true,
            install_hint: "Install with: pip install scip-python",
            config_json: None,
        },
        ScipExporterConfig {
            language_id: "java",
            command: "scip-java",
            args: &["index", "--output", "{output}"],
            extensions: &["java"],
            workspace_level: true,
            install_hint: "See https://sourcegraph.github.io/scip-java/",
            config_json: None,
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

/// Find the roots of all Cargo workspaces under `repo_root`.
///
/// A workspace root is any directory that contains a `Cargo.toml` with a
/// `[workspace]` section. We walk only one level deep to avoid false
/// positives inside `target/` or vendor directories.
///
/// **Single-repo case**: if `repo_root` itself has a `[workspace]` Cargo.toml,
/// it is returned immediately as the sole root.
///
/// **Monorepo case**: if `repo_root` contains no top-level `Cargo.toml`, the
/// function scans one level of subdirectories for workspace roots. This covers
/// repos whose workspace roots are siblings under a shared parent (e.g.
/// `corpus-engine/`, `sovereign/`, `commonwealth/` under `commonwealth-ai/`).
/// To use this path from within a single-workspace sub-repo, pass the monorepo
/// parent explicitly — see `sovereign project init --workspace-root`.
pub fn find_cargo_workspace_roots(repo_root: &Path) -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();

    // Check repo_root itself first (single-workspace or root-level workspace).
    let root_cargo = repo_root.join("Cargo.toml");
    if root_cargo.exists() {
        if let Ok(s) = std::fs::read_to_string(&root_cargo) {
            if s.contains("[workspace]") {
                roots.push(repo_root.to_path_buf());
                return roots; // single-workspace repo — done
            }
        }
    }

    // No workspace at root — scan one level of subdirectories.
    if let Ok(entries) = std::fs::read_dir(repo_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Skip common non-workspace directories.
            if matches!(name, "target" | ".git" | "node_modules" | ".sovereign") {
                continue;
            }
            let cargo_toml = path.join("Cargo.toml");
            if cargo_toml.exists() {
                if let Ok(s) = std::fs::read_to_string(&cargo_toml) {
                    if s.contains("[workspace]") {
                        roots.push(path);
                    }
                }
            }
        }
    }

    roots.sort(); // deterministic order
    roots
}

// ─── Exporter diagnostics ────────────────────────────────────

/// A language exporter that is needed for the workspace but not installed.
pub struct MissingExporter {
    pub language_id: &'static str,
    pub command: &'static str,
    pub install_hint: &'static str,
}

/// Result of checking exporter availability across a set of workspace roots.
pub struct ExporterCheck {
    /// Exporters that are available (binary found in PATH).
    pub available: Vec<&'static ScipExporterConfig>,
    /// Exporters that are needed (language files exist) but not installed.
    pub missing: Vec<MissingExporter>,
}

/// Check which SCIP exporters are available and which are missing across `roots`.
///
/// Unlike [`exporters_for_workspace`], this function surfaces *both* the
/// available exporters *and* those that are needed but absent — so callers can
/// show actionable install instructions instead of silently producing an empty
/// call graph.
pub fn check_exporters(roots: &[std::path::PathBuf]) -> ExporterCheck {
    let mut available = Vec::new();
    let mut missing = Vec::new();

    for exporter in all_exporters() {
        let has_files = roots.iter().any(|root| {
            exporter.extensions.iter().any(|ext| {
                glob::glob(&format!("{}/**/*.{}", root.display(), ext))
                    .map(|mut g| g.next().is_some())
                    .unwrap_or(false)
            })
        });
        if !has_files {
            continue;
        }
        if which::which(exporter.command).is_ok() {
            available.push(exporter);
        } else {
            missing.push(MissingExporter {
                language_id: exporter.language_id,
                command: exporter.command,
                install_hint: exporter.install_hint,
            });
        }
    }

    ExporterCheck { available, missing }
}

/// Run all applicable SCIP exporters and ingest results into the graph.
///
/// For `workspace_level` exporters (e.g. rust-analyzer) the exporter is run
/// once per Cargo workspace root. In the single-repo case this is just the
/// repo root itself. In the monorepo case, pass the sibling workspace roots
/// explicitly via `workspace_roots` (discovered with
/// [`find_cargo_workspace_roots`] at `init` time and stored in `project.json`).
///
/// When `workspace_roots` is `None` the roots are auto-detected by calling
/// [`find_cargo_workspace_roots`] on `repo_root` — appropriate for single-repo
/// projects and as a fallback.
///
/// Returns a summary of what was exported and what was skipped.
pub async fn export_all(
    repo_root: &Path,
    output_dir: &Path,
    graph: &ScipGraph,
    workspace_roots: Option<&[std::path::PathBuf]>,
    // `+ Send + Sync` makes the reference safe to hold across
    // `await` boundaries in `tokio::spawn`'d tasks. The previous
    // signature was sound for CLI use, where the task is never
    // moved across threads, but broke the daemon's Reindexer.
    progress: &(dyn Fn(ScipProgress<'_>) + Send + Sync),
) -> Result<ExportSummary> {
    // Resolve the workspace roots to run exporters in.
    let owned_auto: Vec<std::path::PathBuf>;
    let resolved_roots: &[std::path::PathBuf] = match workspace_roots {
        Some(roots) => roots,
        None => {
            owned_auto = {
                let roots = find_cargo_workspace_roots(repo_root);
                if roots.is_empty() { vec![repo_root.to_path_buf()] } else { roots }
            };
            &owned_auto
        }
    };

    // Detect available exporters across all resolved roots.
    let exporters: Vec<&'static ScipExporterConfig> = {
        let check = check_exporters(resolved_roots);
        // Log missing exporters — callers are responsible for surfacing these
        // to users; this trace is a fallback for automated/non-interactive runs.
        for m in &check.missing {
            tracing::warn!(
                language = m.language_id,
                command = m.command,
                "SCIP exporter not found in PATH — {} call graph will be empty. {}",
                m.language_id, m.install_hint,
            );
        }
        check.available
    };

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
        // workspace_level exporters run once per Cargo workspace root so that
        // each workspace's own feature set is applied correctly.
        let run_dirs: &[std::path::PathBuf] = if exporter.workspace_level {
            resolved_roots
        } else {
            std::slice::from_ref(&resolved_roots[0]) // use first root as cwd
        };

        for run_dir in run_dirs {
            // Skip this workspace if it has no files for this language.
            let has_files = exporter.extensions.iter().any(|ext| {
                glob::glob(&format!("{}/**/*.{}", run_dir.display(), ext))
                    .map(|mut g| g.next().is_some())
                    .unwrap_or(false)
            });
            if !has_files {
                continue;
            }

            let ws_label = run_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");

            progress(ScipProgress::Exporting {
                language: exporter.language_id,
            });

            // Unique output path per (language, workspace) to avoid collisions
            // when the same exporter runs in multiple workspaces.
            let scip_path = output_dir.join(format!(
                "{}-{}.scip",
                exporter.language_id, ws_label
            ));

            // Write a temp config file if the exporter needs one (e.g. rust-analyzer
            // uses a JSON config to enable all Cargo features so feature-gated modules
            // appear in the SCIP output).
            let config_file: Option<tempfile::NamedTempFile> = if let Some(json) = exporter.config_json {
                let mut f = tempfile::NamedTempFile::new()
                    .map_err(|e| Error::Io(e))?;
                std::io::Write::write_all(&mut f, json.as_bytes())
                    .map_err(|e| Error::Io(e))?;
                Some(f)
            } else {
                None
            };
            let config_path_str = config_file
                .as_ref()
                .map(|f| f.path().to_str().unwrap_or("").to_owned())
                .unwrap_or_default();

            let args: Vec<String> = exporter
                .args
                .iter()
                .map(|a| {
                    a.replace("{output}", scip_path.to_str().unwrap_or(""))
                     .replace("{config}", &config_path_str)
                })
                .collect();

            let output = tokio::process::Command::new(exporter.command)
                .args(&args)
                .current_dir(run_dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
                .map_err(|e| Error::Io(e))?;

            // Config file is deleted when `config_file` drops at end of loop iteration.
            let status = output.status;

            if !status.success() {
                let stderr_tail = String::from_utf8_lossy(&output.stderr);
                let stderr_last = stderr_tail
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::warn!(
                    language = exporter.language_id,
                    workspace = ws_label,
                    stderr = %stderr_last,
                    "SCIP export failed — {} call graph unavailable for {}",
                    exporter.language_id, ws_label,
                );
                summary.languages_skipped.push(SkippedLanguage {
                    language: format!("{} ({})", exporter.language_id, ws_label),
                    reason: format!(
                        "{} exited with status {}{}",
                        exporter.command,
                        status,
                        if stderr_last.is_empty() {
                            String::new()
                        } else {
                            format!("\n{stderr_last}")
                        }
                    ),
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

            if !summary.languages_exported.contains(&exporter.language_id.to_string()) {
                summary.languages_exported.push(exporter.language_id.to_string());
            }
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
        // Note: sym_info.symbol is Vec<u8> (not String) because
        // rust-analyzer sometimes emits non-UTF-8 SCIP symbols. We
        // convert to String with lossy replacement at comparison sites.
        for sym_info in &doc.symbols {
            if sym_info.symbol.is_empty() {
                continue;
            }
            let sym_str = scip_proto::sym_to_string(&sym_info.symbol);
            let display_name = if sym_info.display_name.is_empty() {
                scip_proto::extract_symbol_name(&sym_str)
            } else {
                sym_info.display_name.clone()
            };

            // Find the definition occurrence to get line numbers.
            let (line_start, line_end) = doc
                .occurrences
                .iter()
                .find(|occ| {
                    occ.symbol == sym_str
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
                    .find(|s| scip_proto::sym_to_string(&s.symbol) == occ.symbol)
                    .map(|s| {
                        if s.display_name.is_empty() {
                            scip_proto::extract_symbol_name(&scip_proto::sym_to_string(&s.symbol))
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
                    .find(|s| scip_proto::sym_to_string(&s.symbol) == occ.symbol)
                    .map(|s| {
                        if s.display_name.is_empty() {
                            scip_proto::extract_symbol_name(&scip_proto::sym_to_string(&s.symbol))
                        } else {
                            s.display_name.clone()
                        }
                    })
                    .or_else(|| {
                        index.external_symbols.iter().find(|s| scip_proto::sym_to_string(&s.symbol) == occ.symbol).map(|s| {
                            if s.display_name.is_empty() {
                                scip_proto::extract_symbol_name(&scip_proto::sym_to_string(&s.symbol))
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
        // config_json enables all Cargo features so treesitter-gated code appears in SCIP.
        assert!(
            rust.config_json.is_some(),
            "rust exporter must supply a config_json to enable all features"
        );
        assert!(
            rust.config_json.unwrap().contains("\"features\""),
            "rust config_json must configure cargo features"
        );
        // args must reference {config} so the temp config file path is substituted.
        assert!(
            rust.args.contains(&"{config}"),
            "rust exporter args must contain {{config}} placeholder"
        );
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
