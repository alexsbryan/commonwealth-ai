// SPDX-License-Identifier: AGPL-3.0-or-later
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
            args: &[
                "scip",
                ".",
                "--config-path",
                "{config}",
                "--output",
                "{output}",
            ],
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
            // VERIFIED 2026-08-07 by running it. Two things the obvious
            // spelling gets wrong, and both fail rather than degrade:
            // the binary is under `/cmd/`, and upstream MOVED — the
            // module at `github.com/sourcegraph/scip-go` now declares
            // its own path as `github.com/scip-code/scip-go`, so a
            // `go install` of the old path dies with "module declares
            // its path as". The `~/go/bin` reminder is not padding
            // either: `go install` writes there and it is off PATH on a
            // default macOS shell, which looks exactly like the install
            // having failed.
            install_hint:
                "Install with: go install github.com/scip-code/scip-go/cmd/scip-go@latest \
                           (then ensure ~/go/bin is on PATH)",
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
            // VERIFIED 2026-08-07: `pip install scip-python` fails with
            // "No matching distribution found" — despite the name, and
            // despite indexing Python, Sourcegraph publishes this one on
            // npm (@sourcegraph/scip-python 0.6.6), not PyPI.
            install_hint: "Install with: npm install -g @sourcegraph/scip-python",
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
    Exporting {
        language: &'a str,
    },
    Ingested {
        language: &'a str,
        symbols: usize,
        refs: usize,
    },
    Skipped {
        language: &'a str,
        reason: &'a str,
    },
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

/// An exporter that is needed AND was resolved to a concrete binary.
///
/// Carries the absolute `path` rather than just the config so the spawn
/// in [`run_exporters_collect`] executes the very binary detection
/// found. Resolving by name at check time and again by name at spawn
/// time is how a daemon ends up reporting an exporter it cannot run:
/// the two lookups happen in different processes with different PATHs.
pub struct ResolvedExporter {
    pub config: &'static ScipExporterConfig,
    /// Absolute path to the exporter binary.
    pub path: std::path::PathBuf,
    /// Which probe found it — `ProcessPath` means a service daemon with
    /// a different environment may NOT see it.
    pub via: crate::tool_path::ResolvedVia,
}

/// Result of checking exporter availability across a set of workspace roots.
pub struct ExporterCheck {
    /// Exporters that are available, each resolved to an absolute path.
    pub available: Vec<ResolvedExporter>,
    /// Exporters that are needed (language files exist) but not installed.
    pub missing: Vec<MissingExporter>,
}

/// Check which SCIP exporters are available and which are missing across `roots`.
///
/// Unlike [`exporters_for_workspace`], this function surfaces *both* the
/// available exporters *and* those that are needed but absent — so callers can
/// show actionable install instructions instead of silently producing an empty
/// call graph.
///
/// Resolution goes through [`crate::tool_path::resolve`], NOT bare
/// `which`, and that is the whole point: `which` answers for whoever is
/// asking. The daemon runs under launchd/systemd with a minimal PATH
/// while `doctor` runs in the operator's shell, so a name-only lookup
/// let doctor report an exporter as present that the daemon could not
/// execute — the instrument validating the wrong environment
/// (ARCH §18.4). One resolver, same answer for both.
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
        match crate::tool_path::resolve(exporter.command) {
            Some(found) => available.push(ResolvedExporter {
                config: exporter,
                path: found.path,
                via: found.via,
            }),
            None => missing.push(MissingExporter {
                language_id: exporter.language_id,
                command: exporter.command,
                install_hint: exporter.install_hint,
            }),
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
                if roots.is_empty() {
                    vec![repo_root.to_path_buf()]
                } else {
                    roots
                }
            };
            &owned_auto
        }
    };

    // Detect available exporters across all resolved roots.
    let exporters: Vec<ResolvedExporter> = {
        let check = check_exporters(resolved_roots);
        // Log missing exporters — callers are responsible for surfacing these
        // to users; this trace is a fallback for automated/non-interactive runs.
        for m in &check.missing {
            tracing::warn!(
                language = m.language_id,
                command = m.command,
                "SCIP exporter not found in PATH — {} call graph will be empty. {}",
                m.language_id,
                m.install_hint,
            );
        }
        check.available
    };

    let mut summary = ExportSummary::default();

    if exporters.is_empty() {
        tracing::warn!("No SCIP exporters available — call graph will be empty");
        return Ok(summary);
    }

    // Symbol count BEFORE we touch anything. The viability check at the end
    // uses it to refuse replacing a populated graph with an empty/degraded
    // export. We deliberately DO NOT clear up front any more: the old
    // `graph.clear()` here is exactly what wiped a good index when a
    // present-in-PATH-but-broken exporter (e.g. a stale rust-analyzer shim)
    // then failed at runtime, leaving the graph empty yet returning Ok.
    let prior_symbols = graph.symbol_count().await;

    // Run every available exporter and COLLECT all parsed rows (no graph
    // mutation yet) — shared with `export_changed` so the exporter plumbing
    // lives in exactly one place.
    let (all_symbols, all_refs) = run_exporters_collect(
        &exporters,
        resolved_roots,
        output_dir,
        progress,
        &mut summary,
    )
    .await?;

    // ── Viability gate — the "never wipe on failure" contract ──
    // Only now, with the full export collected, do we decide whether it is safe
    // to replace the graph. A non-viable export (0 symbols after an exporter
    // failed, a populated graph collapsing to empty, or a >50% symbol loss
    // coinciding with a failure) is REFUSED: we return `ExportAborted` and
    // leave the existing graph untouched, rather than swapping in a wipe and
    // reporting success. Callers fail closed on this — the CLI prints an error
    // and the daemon Reindexer's `?` bails before its staging→live rename.
    let had_failures = !summary.languages_skipped.is_empty();
    if let Err(reason) = export_is_viable(all_symbols.len(), had_failures, prior_symbols) {
        tracing::error!(
            collected_symbols = all_symbols.len(),
            prior_symbols,
            had_failures,
            skipped = ?summary.languages_skipped.iter().map(|s| &s.language).collect::<Vec<_>>(),
            "SCIP export refused — existing graph preserved: {reason}"
        );
        return Err(Error::ExportAborted(reason));
    }

    // Atomically swap the freshly-collected export in. The graph is never
    // observably empty mid-swap, and a failure here rolls back to the prior
    // graph (see `ScipGraph::replace_all`).
    graph.replace_all(all_symbols, all_refs).await?;

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

/// Run every available exporter across the resolved workspace roots and
/// COLLECT all parsed (symbols, refs). Per-language failures are recorded
/// into `summary.languages_skipped` and the run continues — the caller
/// decides viability and how to apply the rows. Shared by `export_all`
/// (full atomic replace) and `export_changed` (per-file merge); neither
/// touches the graph here.
async fn run_exporters_collect(
    exporters: &[ResolvedExporter],
    resolved_roots: &[std::path::PathBuf],
    output_dir: &Path,
    progress: &(dyn Fn(ScipProgress<'_>) + Send + Sync),
    summary: &mut ExportSummary,
) -> Result<(Vec<ScipSymbolRecord>, Vec<ScipRefRecord>)> {
    // Collect every language's parsed rows first, then swap them in atomically
    // via `replace_all` — never clear-then-hope.
    let mut all_symbols: Vec<ScipSymbolRecord> = Vec::new();
    let mut all_refs: Vec<ScipRefRecord> = Vec::new();

    std::fs::create_dir_all(output_dir).map_err(Error::Io)?;

    for resolved in exporters {
        let exporter = resolved.config;
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

            let ws_label = run_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");

            progress(ScipProgress::Exporting {
                language: exporter.language_id,
            });

            // Unique output path per (language, workspace) to avoid collisions
            // when the same exporter runs in multiple workspaces.
            let scip_path = output_dir.join(format!("{}-{}.scip", exporter.language_id, ws_label));

            // Write a temp config file if the exporter needs one (e.g. rust-analyzer
            // uses a JSON config to enable all Cargo features so feature-gated modules
            // appear in the SCIP output).
            let config_file: Option<tempfile::NamedTempFile> =
                if let Some(json) = exporter.config_json {
                    let mut f = tempfile::NamedTempFile::new().map_err(Error::Io)?;
                    std::io::Write::write_all(&mut f, json.as_bytes()).map_err(Error::Io)?;
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

            // Spawn the ABSOLUTE path detection resolved, and hand the
            // child an augmented PATH. Both halves are required, and the
            // second is the non-obvious one: resolving the binary alone
            // still fails under a minimal service PATH because the
            // exporters shell out to their own runtimes — rust-analyzer
            // invokes `cargo`, and scip-typescript is a
            // `#!/usr/bin/env node` script that dies without node on
            // PATH. Existing PATH entries keep priority, so an
            // operator's explicit environment always wins.
            let mut cmd = tokio::process::Command::new(&resolved.path);
            cmd.args(&args)
                .current_dir(run_dir)
                .env("PATH", crate::tool_path::augmented_path_env())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped());
            // A full export is an O(workspace) CPU burn for minutes. Run
            // it niced so it yields to whatever the operator (or the
            // daemon's inference slots) is doing mid-flow; on an idle box
            // nice+10 still gets every core.
            #[cfg(unix)]
            unsafe {
                cmd.pre_exec(|| {
                    let _ = libc::nice(10);
                    Ok(())
                });
            }
            let output = cmd.output().await.map_err(Error::Io)?;

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

            // Parse this language's SCIP file and COLLECT its rows. We do not
            // touch the graph here — the atomic replace happens once at the
            // end, only if the whole export proves viable.
            let (symbols, refs) = parse_scip_file(&scip_path, exporter.language_id)?;
            let sym_count = symbols.len();
            let ref_count = refs.len();

            all_symbols.extend(symbols);
            all_refs.extend(refs);

            if !summary
                .languages_exported
                .contains(&exporter.language_id.to_string())
            {
                summary
                    .languages_exported
                    .push(exporter.language_id.to_string());
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

    Ok((all_symbols, all_refs))
}

/// Summary of an incremental changed-files re-index.
#[derive(Debug, Default)]
pub struct ChangedExportSummary {
    /// Files whose rows were re-indexed (deleted + re-inserted).
    pub files_merged: usize,
    pub symbols_merged: usize,
    pub refs_merged: usize,
    pub languages: Vec<String>,
    pub languages_skipped: Vec<SkippedLanguage>,
}

/// Incrementally re-index ONLY `changed_files` and merge the result into the
/// graph, leaving every OTHER file's symbols/refs untouched. Runs the same
/// exporters as `export_all`, keeps just the rows whose `file_path` EXACTLY
/// matches a changed file, and applies them via `ScipGraph::replace_files`
/// (which also drops the merged files from the stale set).
///
/// Correctness rests on one invariant the whole system already relies on: the
/// stored `file_path` (`doc.relative_path`), the stale-set entries
/// (`CodeWatcher` passes the workspace-relative path), and `changed_files`
/// here are all the SAME workspace-relative form — so exact matching on the
/// delete-set and the insert rows never drifts. Callers should pass the
/// relative paths from `ScipGraph::stale_files_snapshot()` (or git).
///
/// This NEVER wipes on failure: a failed exporter simply contributes no rows,
/// and unchanged files keep their entries. NOTE: whole-workspace exporters
/// (e.g. rust-analyzer) still run over the whole workspace, so the exporter
/// cost is unchanged for Rust — the win here is a scoped, safe DB merge and
/// staleness clearing, not a cheaper exporter run. True sub-second Rust
/// freshness wants a tree-sitter overlay feeding `replace_files` directly.
pub async fn export_changed(
    repo_root: &Path,
    output_dir: &Path,
    graph: &ScipGraph,
    changed_files: &[String],
    workspace_roots: Option<&[std::path::PathBuf]>,
    progress: &(dyn Fn(ScipProgress<'_>) + Send + Sync),
) -> Result<ChangedExportSummary> {
    let mut result = ChangedExportSummary::default();
    if changed_files.is_empty() {
        return Ok(result);
    }

    // Resolve roots (same policy as export_all).
    let owned_auto: Vec<std::path::PathBuf>;
    let resolved_roots: &[std::path::PathBuf] = match workspace_roots {
        Some(roots) => roots,
        None => {
            owned_auto = {
                let roots = find_cargo_workspace_roots(repo_root);
                if roots.is_empty() {
                    vec![repo_root.to_path_buf()]
                } else {
                    roots
                }
            };
            &owned_auto
        }
    };

    let exporters = check_exporters(resolved_roots).available;
    if exporters.is_empty() {
        return Ok(result);
    }
    std::fs::create_dir_all(output_dir).map_err(Error::Io)?;

    let mut summary = ExportSummary::default();
    let (collected_syms, collected_refs) = run_exporters_collect(
        &exporters,
        resolved_roots,
        output_dir,
        progress,
        &mut summary,
    )
    .await?;

    // Keep only rows for the changed files (exact match on the shared
    // relative form — see the invariant above).
    let changed: std::collections::HashSet<&str> =
        changed_files.iter().map(|s| s.as_str()).collect();
    let syms: Vec<ScipSymbolRecord> = collected_syms
        .into_iter()
        .filter(|s| changed.contains(s.file_path.as_str()))
        .collect();
    let refs: Vec<ScipRefRecord> = collected_refs
        .into_iter()
        .filter(|r| changed.contains(r.file_path.as_str()))
        .collect();

    result.files_merged = changed_files.len();
    result.symbols_merged = syms.len();
    result.refs_merged = refs.len();
    result.languages = summary.languages_exported.clone();
    result.languages_skipped = summary.languages_skipped;

    // Merge only the changed files' rows; every other file stays put, and a
    // failed exporter cannot degrade them.
    graph.replace_files(changed_files, syms, refs).await?;

    Ok(result)
}

/// Decide whether a freshly-collected full export may replace the live graph,
/// or whether replacing it would destroy data (the P0 "refresh wiped the
/// index" class). Pure — no I/O — so the policy is unit-testable in isolation.
///
/// Returns `Err(reason)` when the caller must PRESERVE the existing graph:
///   (a) 0 symbols collected while an exporter failed — the canonical
///       broken-exporter wipe (protects the daemon's empty-staging→rename
///       path, where `prior_symbols` is 0 and only this rule fires);
///   (b) a previously-populated graph collapsing to 0 symbols, even with no
///       reported failure — zero-from-nonzero on a full export is a wipe
///       signature (protects the CLI live-graph path);
///   (c) a >50% symbol loss that coincides with an exporter failure — far more
///       likely a partial/broken export than a real mass deletion.
/// Otherwise `Ok(())` — including legitimate first builds (prior 0, no failure)
/// and normal edits.
fn export_is_viable(
    collected_symbols: usize,
    had_failures: bool,
    prior_symbols: usize,
) -> std::result::Result<(), String> {
    if collected_symbols == 0 && had_failures {
        return Err(format!(
            "export produced 0 symbols and one or more exporters failed at runtime; \
             preserving existing graph ({prior_symbols} symbols)"
        ));
    }
    if collected_symbols == 0 && prior_symbols > 0 {
        return Err(format!(
            "export produced 0 symbols but the graph currently holds {prior_symbols}; \
             a full export collapsing to empty is a wipe — preserving existing graph"
        ));
    }
    if had_failures && prior_symbols > 0 && collected_symbols.saturating_mul(2) < prior_symbols {
        return Err(format!(
            "export produced {collected_symbols} symbols, under half the existing \
             {prior_symbols}, while an exporter failed; likely a degraded export — \
             preserving existing graph"
        ));
    }
    Ok(())
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
    let data = std::fs::read(path).map_err(Error::Io)?;

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

            // Find the definition occurrence to get line numbers. The BODY span
            // comes from `enclosing_range` (the whole function / type), NOT
            // `range` (just the name identifier). A single-line `range` is
            // `[line, start_col, END_COL]` — reading `range[2]` as a line was the
            // bug that stored an end-COLUMN as `line_end` for every definition,
            // making bodies un-extractable downstream (code-intel enrichment,
            // `symbol_lookup::read_symbol_body`). `enclosing_range` is
            // `[start_line, start_col, end_line, end_col]`; `[2]` is the true end
            // line. Mirrors the caller-scope logic below. Falls back to the name
            // range only for single-line symbols with no enclosing range
            // (const / field / variable).
            let (line_start, line_end) = doc
                .occurrences
                .iter()
                .find(|occ| {
                    occ.symbol == sym_str
                        && (occ.symbol_roles & scip_proto::SymbolRole::DEFINITION) != 0
                })
                .map(|occ| {
                    let start = occ.range.first().copied().unwrap_or(0);
                    // `range_lines` owns the 3-vs-4-element discrimination —
                    // see its doc comment. `.max(start)` is the invariant, not
                    // a patch: a span is never inverted, so a consumer can
                    // slice `line_start..=line_end` without a clamp of its own.
                    let end = scip_proto::range_lines(&occ.enclosing_range)
                        .or_else(|| scip_proto::range_lines(&occ.range))
                        .map(|(_, e)| e)
                        .unwrap_or(start)
                        .max(start);
                    (start, end)
                })
                .unwrap_or((0, 0));

            symbols.push(ScipSymbolRecord {
                name: display_name,
                qualified_name: sym_str.clone(),
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
        //
        // Each scope carries BOTH the bare display name (for human-
        // readable `caller_symbol` output) and the full SCIP descriptor
        // (for unambiguous cross-crate `caller_qualified`).
        let mut def_scopes: Vec<(String, i32, i32, String)> = Vec::new(); // (qualified_caller, start, end, display_caller)
        for occ in &doc.occurrences {
            // Skip rust-analyzer `local N` symbols (local vars / block scopes). A
            // call's caller is the enclosing FUNCTION, never a local scope. Without
            // this, a short function whose body reads as a local scope makes the
            // local the innermost enclosing definition → the caller resolves to
            // `local 0`, whose package can't be parsed → every call edge drops as
            // "external" (we saw 0 capabilities on a fresh repo). Filtering locals
            // here lets the enclosing function win the scope race.
            if (occ.symbol_roles & scip_proto::SymbolRole::DEFINITION) != 0
                && !occ.symbol.starts_with("local ")
            {
                let start = occ.range.first().copied().unwrap_or(0);
                // Same decoder as the symbol span above. This one is not
                // cosmetic: the scope end decides which definition a reference
                // is attributed to, so a column read as a line silently
                // mis-assigns callers. The `start + 50` guess survives only as
                // the no-range fallback.
                let end = scip_proto::range_lines(&occ.enclosing_range)
                    .map(|(_, e)| e)
                    .unwrap_or(start + 50)
                    .max(start);
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
                def_scopes.push((occ.symbol.clone(), start, end, display));
            }
        }

        // Sort scopes by start line for binary search.
        def_scopes.sort_by_key(|(_, start, _, _)| *start);

        for occ in &doc.occurrences {
            // Skip definitions — they're not "calls".
            if (occ.symbol_roles & scip_proto::SymbolRole::DEFINITION) != 0 {
                continue;
            }
            if occ.symbol.is_empty() {
                continue;
            }

            let occ_line = occ.range.first().copied().unwrap_or(0);
            // Precise character span of the identifier token. rust-analyzer has
            // always emitted it; until 2026-08-20 the ingest decoded the line
            // and discarded the columns, which is why every reference in the
            // graph was a line pointer rather than a rewritable anchor. A range
            // we cannot decode records -1 (see `ScipRefRecord::has_span`) — a
            // defaulted 0 would point a rewriter at the head of the line.
            let (occ_start_col, occ_end_line, occ_end_col) =
                match scip_proto::range_span(&occ.range) {
                    Some((_, sc, el, ec)) => (sc, el, ec),
                    None => (-1, -1, -1),
                };

            // Find the enclosing definition scope (caller).
            let caller = def_scopes
                .iter()
                .rev()
                .find(|(_, start, end, _)| occ_line >= *start && occ_line <= *end);

            if let Some((caller_qualified, _, _, caller_name)) = caller {
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
                        index
                            .external_symbols
                            .iter()
                            .find(|s| scip_proto::sym_to_string(&s.symbol) == occ.symbol)
                            .map(|s| {
                                if s.display_name.is_empty() {
                                    scip_proto::extract_symbol_name(&scip_proto::sym_to_string(
                                        &s.symbol,
                                    ))
                                } else {
                                    s.display_name.clone()
                                }
                            })
                    })
                    .unwrap_or_else(|| scip_proto::extract_symbol_name(&occ.symbol));

                refs.push(ScipRefRecord {
                    caller_symbol: caller_name.clone(),
                    callee_symbol: callee_name,
                    caller_qualified: caller_qualified.clone(),
                    callee_qualified: occ.symbol.clone(),
                    file_path: file_path.clone(),
                    line: occ_line,
                    start_col: occ_start_col,
                    end_line: occ_end_line,
                    end_col: occ_end_col,
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

    // ── export_is_viable: the "never wipe on failure" policy ──

    #[test]
    fn viable_normal_full_export() {
        // Healthy re-export of a populated repo, no failures.
        assert!(export_is_viable(180_000, false, 179_500).is_ok());
    }

    #[test]
    fn viable_first_build_from_empty() {
        // First ever export: prior 0, no failure, real symbols → allowed.
        assert!(export_is_viable(50_000, false, 0).is_ok());
    }

    #[test]
    fn viable_legit_empty_repo() {
        // Nothing to index and nothing failed (e.g. empty staging, empty repo).
        assert!(export_is_viable(0, false, 0).is_ok());
    }

    #[test]
    fn refused_zero_symbols_with_exporter_failure() {
        // THE P0: a present-but-broken exporter fails at runtime → 0 symbols.
        // Must be refused regardless of prior count (protects the daemon's
        // empty-staging→rename path where prior is 0).
        assert!(export_is_viable(0, true, 0).is_err());
        assert!(export_is_viable(0, true, 189_000).is_err());
    }

    #[test]
    fn refused_populated_graph_collapsing_to_empty() {
        // Zero-from-nonzero on a full export is a wipe even with no reported
        // failure (protects the CLI live-graph path).
        assert!(export_is_viable(0, false, 189_000).is_err());
    }

    #[test]
    fn refused_catastrophic_drop_with_failure() {
        // >50% symbol loss coinciding with a failure → likely degraded export.
        assert!(export_is_viable(50_000, true, 189_000).is_err());
    }

    #[test]
    fn allowed_moderate_drop_without_failure() {
        // A real refactor deleting some code, no exporter failure → allowed
        // (we only treat a drop as suspicious when an exporter actually failed).
        assert!(export_is_viable(120_000, false, 189_000).is_ok());
    }

    #[test]
    fn allowed_drop_over_half_without_failure() {
        // Even a large drop is allowed when NO exporter failed — could be a
        // legitimate mass deletion; we don't second-guess a clean export.
        assert!(export_is_viable(1_000, false, 189_000).is_ok());
    }

    #[test]
    fn allowed_minor_drop_with_failure() {
        // A failure in one small language plus a modest overall dip (>half
        // retained) is normal partial coverage, not a wipe.
        assert!(export_is_viable(180_000, true, 189_000).is_ok());
    }

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
