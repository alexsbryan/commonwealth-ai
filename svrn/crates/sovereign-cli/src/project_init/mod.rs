// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn init` / `svrn project init` — one-shot code-intelligence setup for a
//! workspace: harness auto-detection, git prompt, corpus indexing, and writing
//! the `.claude` / opencode / AGENTS.md scaffolding. The bulk is `cmd_init`;
//! file generation lives in `scaffold`, git + report rendering in `setup`.
//!
//! Moved here from the `sovereign-cli-dev` workbench (2026-08-07) so a
//! `curl | sh` install can index its own repo without a checkout. The move is
//! otherwise faithful: `cmd_init`'s body is unchanged except for the two cuts
//! named below. What was `use super::*` (a glob into `project_cmd`) is now the
//! explicit import list — every name it pulled is either shared plumbing or
//! already lived in this binary from slices 1-2.
//!
//! One thing deliberately did NOT come along: nothing. In particular
//! `project_toml` DID (via sovereign-cli-shared; since pb-code-cli-base init
//! reaches it by exec'ing the code program's `project-observe`):
//! `.sovereign/project.toml` is read by sovereign-server,
//! commonwealth-api's context injector and the desktop knowledge
//! view, so an `init` that skipped it would be broken.

mod scaffold;
mod setup;

use scaffold::*;
use setup::*;

use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use sovereign_cli_shared::dirs::default_data_dir;
use sovereign_cli_shared::mcp_client::check_mcp_server;
use sovereign_cli_shared::repo::{find_repo_root, remove_legacy_hook};

// Already in this binary from slices 1-2 — reused rather than re-ported, so
// `init` and `register` keep deriving the same corpus id and posting to the
// same daemon base (they must agree, or a repo indexes under two names).
use crate::project_registry::daemon_post;
use sovereign_cli_shared::code_index::tempfile_dir;

const HELP_INIT: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project init",
    summary: "Set up code intelligence for the workspace in the current directory.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn project init [--name <id>] [--port <port>]\n    \
             [--data-dir <dir>] [--workspace-root <path>]\n    \
             [--watcher-ignore <component>]...\n    \
             [--no-scip] [--no-hooks] [--no-claude-config]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--name <id>",          "Corpus ID (default: directory name)"),
            ("--port <port>",        "MCP server port (default: 9741)"),
            ("--data-dir <dir>",     "Index directory (default: ~/.svrnmesh/indexes)"),
            ("--workspace-root <p>", "Monorepo root; discover every Cargo/Go/etc. workspace under <p>"),
            ("--watcher-ignore <c>", "Path component the FS watcher should drop (repeatable; replaces the default of .sovereign)"),
            ("--no-scip",            "Skip SCIP call graph export"),
            ("--no-hooks",           "Skip git hook installation"),
            ("--no-claude-config",   "Skip writing .claude/settings.json (overrides harness prompt)"),
        ]),
        sovereign_cli_shared::help::HelpSection::Examples(&[
            ("svrn project init",                                   "Index the current workspace"),
            ("svrn project init --workspace-root ..",               "Index a monorepo from a sibling dir"),
            ("svrn project init --watcher-ignore .sovereign --watcher-ignore generated", "Add custom ignores at the FS watcher seam"),
            ("svrn project init --no-scip",                         "Skip call graph (no exporter installed)"),
        ]),
    ],
};

// ─── Init helpers ────────────────────────────────────────────

/// Auto-detected presence of supported AI coding assistants in the
/// user's environment. Replaces the old interactive harness prompt:
/// if a harness isn't detected, we skip it silently — no clutter.
struct HarnessDetection {
    claude_code: bool,
    opencode: bool,
}

/// Detect which coding harnesses are plausibly in use. Checks both
/// project-local dotfolders (`.claude/`, `.opencode/`), home-level
/// dotfolders (`~/.claude/`, `~/.opencode/`), and `PATH` for the
/// binary names. Any one signal flips the harness on.
#[allow(clippy::disallowed_methods)] // real $HOME: probes third-party harness dotfolders (~/.claude, ~/.opencode)
fn detect_harnesses(project_root: &Path) -> HarnessDetection {
    let home = dirs::home_dir();

    let claude_code = project_root.join(".claude").exists()
        || home.as_ref().is_some_and(|h| h.join(".claude").exists())
        || binary_on_path("claude");

    let opencode = project_root.join(".opencode").exists()
        || home.as_ref().is_some_and(|h| h.join(".opencode").exists())
        || binary_on_path("opencode");

    HarnessDetection {
        claude_code,
        opencode,
    }
}

/// Best-effort check for a binary on PATH. Uses `which(1)` on unix and
/// `where.exe` on Windows; returns false on error / not-found.
fn binary_on_path(name: &str) -> bool {
    #[cfg(unix)]
    let cmd = std::process::Command::new("which").arg(name).output();
    #[cfg(windows)]
    let cmd = std::process::Command::new("where").arg(name).output();

    cmd.map(|o| o.status.success()).unwrap_or(false)
}

/// Prompt `Detected <harness>. Write config automatically? [Y/n]` with
/// `Y` as the default. In non-TTY environments (CI, pipes) returns
/// `true` without prompting — matches the `--yes` semantics used by
/// `svrn setup`.
fn confirm_write_config(harness: &str) -> bool {
    if !io::stdin().is_terminal() {
        return true;
    }
    eprint!("  Detected {harness}. Write config automatically? [Y/n] ");
    io::stderr().flush().ok();
    let mut ans = String::new();
    io::stdin().lock().read_line(&mut ans).unwrap_or(0);
    let trimmed = ans.trim().to_lowercase();
    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
}

/// Probe the Commonwealth OICP capabilities endpoint and return available model IDs.
/// Returns an empty vec silently if Commonwealth is not reachable — this is normal
/// when the daemon isn't running at init time.
async fn fetch_commonwealth_models(commonwealth_url: &str) -> Vec<String> {
    let base = commonwealth_url
        .trim_end_matches('/')
        .replace(":9742", ":9741");
    let url = format!("{base}/oicp/v1/capabilities");

    let resp = match reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };

    resp.json::<sovereign_core::oicp::ProviderManifest>()
        .await
        .map(|m| {
            m.models
                .into_iter()
                .filter(|model| model.status.available)
                .map(|model| model.id)
                .collect()
        })
        .unwrap_or_default()
}

// ─── Init ────────────────────────────────────────────────────

pub(crate) async fn cmd_init(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_INIT);
        return 0;
    }
    let mut name: Option<String> = None;
    let mut no_scip = false;
    let mut no_hooks = false;
    let mut no_claude_config = false;
    let mut port: u16 = 9741;
    let mut data_dir: Option<PathBuf> = None;
    // Monorepo root: when set, sovereign discovers all workspace roots under
    // this path rather than treating the git root as the sole workspace.
    let mut workspace_root_arg: Option<PathBuf> = None;

    // `--yes-git` / `--no-git` let scripted callers bypass the
    // interactive git prompt. `None` means "prompt if TTY, auto-init
    // if not." `Some(true)` means "run git init without asking."
    // `Some(false)` means "do not run git init; skip the prompt."
    let mut git_override: Option<bool> = None;

    // Per-project extra ignore_paths fed into the watcher's
    // IgnoreFilter. Defaults to `.sovereign` (sovereign's project-
    // local state dir) when no `--watcher-ignore` is given; any
    // flag presence replaces the default outright so users can
    // construct a clean list.
    let mut watcher_ignore_args: Vec<String> = Vec::new();
    let mut watcher_ignore_set = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                name = args.get(i).cloned();
            }
            "--no-scip" => no_scip = true,
            "--no-hooks" => no_hooks = true,
            "--no-claude-config" => no_claude_config = true,
            "--yes-git" => git_override = Some(true),
            "--no-git" => git_override = Some(false),
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<u16>() {
                        Ok(p) => port = p,
                        Err(_) => {
                            eprintln!("error: --port must be a number");
                            return 1;
                        }
                    }
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            "--workspace-root" => {
                i += 1;
                workspace_root_arg = args.get(i).map(PathBuf::from);
            }
            "--watcher-ignore" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    watcher_ignore_args.push(v.clone());
                    watcher_ignore_set = true;
                } else {
                    eprintln!("error: --watcher-ignore requires a path component");
                    return 1;
                }
            }
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let corpus_id = name.unwrap_or_else(|| {
        repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string()
    });

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("error: cannot create data dir {}: {e}", data_dir.display());
        return 1;
    }

    let has_git = repo_root.join(".git").exists();
    // Read any prior project.toml eagerly so the git flow can honor a
    // previous `git_declined_at_init` (the user answered "no" last
    // time — we don't re-badger). This read is intentionally
    // non-fatal: a missing or unreadable project.toml means "first
    // init", which is the common case.
    // The project model is the code program's (pb-code-cli-base): its
    // `project-lifecycle` arm does that read and answers with the flags.
    let prior_git_declined: bool = match read_lifecycle(&repo_root) {
        Ok(l) => l.git_declined_at_init,
        Err(code) => return code,
    };
    let design_md_path = repo_root.join("DESIGN.md");
    let design_exists = design_md_path.exists();

    // Git auto-with-confirm. Runs BEFORE the observation report so the
    // report has an up-to-date `has_git` to render. The design-doc
    // presence is passed through because the prompt's kindness wording
    // changes based on whether the user is about to start drafting a
    // DESIGN.md (the main value prop for git) or not.
    let git_outcome = resolve_git(
        &repo_root,
        has_git,
        git_override,
        design_exists,
        prior_git_declined,
    );
    let has_git = matches!(
        git_outcome,
        GitOutcome::Present | GitOutcome::InitializedNow
    );

    // Resolve workspace roots for SCIP export and language detection.
    // Single-repo (default): just the git root.
    // Monorepo (--workspace-root): discover sibling workspaces under the given parent.
    let abs_path = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

    let scip_workspace_roots: Vec<PathBuf> = if let Some(ref wr) = workspace_root_arg {
        let canonical_wr = wr.canonicalize().unwrap_or_else(|_| wr.clone());
        let roots = corpus_engine_scip::scip_export::find_cargo_workspace_roots(&canonical_wr);
        if roots.is_empty() {
            vec![canonical_wr]
        } else {
            roots
        }
    } else {
        vec![abs_path.clone()]
    };

    // ════════════════════════════════════════════════════════════
    println!();
    println!("  Sovereign Project Intelligence");
    println!("  {}", "─".repeat(54));

    // ── Observation pass (M6.1) ─────────────────────────────────
    //
    // Before touching anything on disk, gather every fact about the
    // project we can derive without asking the user. The result
    // feeds both the human-facing report below AND
    // `.sovereign/project.toml` (written later in this function),
    // which `status` / `found` / `doctor` read from rather than
    // re-observing.
    //
    // The report is bucketed per the requirements:
    //   READY      — everything the user doesn't need to act on
    //   ACTIONABLE — install commands, copy-pasteable, unindented
    //   DEFERRED   — things we note now but address in `found`
    //
    // The code program observes, prints the report and persists
    // `.sovereign/project.toml` BEFORE any indexing/SCIP work, so the
    // durable record survives even if downstream init steps fail
    // (`sovereign-cli-dev project-observe`, pb-code-cli-base). `has_git`
    // is ours: we may have just run `git init`.
    let mut observe_args = vec![
        repo_root.display().to_string(),
        "--has-git".to_string(),
        has_git.to_string(),
    ];
    if design_exists {
        observe_args.push("--design-exists".to_string());
    }
    if matches!(git_outcome, GitOutcome::DeclinedByUser) {
        observe_args.push("--git-declined".to_string());
    }
    let observed = crate::dev_bin::run("project-observe", &observe_args);
    if observed != 0 {
        tracing::debug!(code = observed, "init: project-observe failed");
        return 1;
    }

    // Detect languages across all workspace roots for downstream
    // SCIP + indexing logic. Display already handled by the code
    // program's observation report above — no second pass of ✓-lines.
    let langs: Vec<DetectedLanguage> = {
        let mut seen = std::collections::HashSet::new();
        let mut all = Vec::new();
        for root in &scip_workspace_roots {
            for lang in detect_languages(root) {
                if seen.insert(lang.id) {
                    all.push(lang);
                }
            }
        }
        all
    };

    if langs.is_empty() {
        // Pre-code soft path (step 2b): when the user has a DESIGN.md
        // but no source yet, that's a legitimate state — they're
        // about to iterate on their design with the agent before
        // writing a single file. Don't bail; skip indexing, still
        // register with the daemon, and emit a friendly status line.
        //
        // Stateless: derived from DESIGN.md presence each run. No
        // `pre_code` lifecycle flag — see ARCH_PRINCIPLES.md §7 on
        // structural over stateful.
        if design_exists {
            println!();
            println!(
                "    \u{2026} Pre-code project — indexing deferred. Re-run `svrn project init`"
            );
            println!("      once source lands, or start with `svrn project design` now.");
        } else if !no_scip {
            // True empty directory: observation report already flagged
            // "no supported languages" as actionable. Repeat only the
            // bail path, but point at the design-first alternative.
            println!();
            println!("    Pass --no-scip to skip indexing and write agent configs anyway,");
            println!("    or run `svrn project design --import <path-to-doc>` to start");
            println!("    from an existing design document.");
            return 1;
        } else {
            println!();
            println!("    Continuing without indexing (--no-scip).");
        }
    }
    if workspace_root_arg.is_some() {
        println!();
        println!(
            "    Monorepo mode ({} workspace{})",
            scip_workspace_roots.len(),
            if scip_workspace_roots.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for root in &scip_workspace_roots {
            println!(
                "          {}",
                root.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            );
        }
    }

    // ── Step 2: Index symbols ───────────────────────────────────
    println!();
    println!("  Indexing symbols...");
    println!("    Keyword-only (FTS) index: no embedder is used, so it holds no vectors.");

    // Remove existing index so re-init is idempotent. The ingest pipeline
    // creates tables from scratch and fails with "table already exists" if
    // a previous completed index is present.
    let existing_index = data_dir.join(&corpus_id);
    if existing_index.exists() {
        // Preserve the scip_graph.db — it's expensive to rebuild and
        // the user may only want to re-index symbols, not re-export SCIP.
        let scip_backup = existing_index.join("scip_graph.db");
        let has_scip_backup = scip_backup.exists();
        let scip_tmp = data_dir.join(format!(".{corpus_id}_scip_graph.db.bak"));
        if has_scip_backup {
            let _ = std::fs::rename(&scip_backup, &scip_tmp);
        }
        if let Err(e) = std::fs::remove_dir_all(&existing_index) {
            eprintln!("    \u{26a0} Cannot remove old index: {e}");
        }
        // Restore SCIP graph after clearing.
        if has_scip_backup {
            let _ = std::fs::create_dir_all(&existing_index);
            let _ = std::fs::rename(&scip_tmp, &scip_backup);
        }
    }

    let tempdir = match tempfile_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("    \u{2717} Cannot create temp dir: {e}");
            return 1;
        }
    };

    // The index is the code program's: `sovereign-cli-dev code index`,
    // keyword-only, which execs ingest's CLI to write it (pb-code-index).
    let index_args = [
        "index".to_string(),
        abs_path.display().to_string(),
        "--corpus-id".to_string(),
        corpus_id.to_string(),
        "--data-dir".to_string(),
        data_dir.display().to_string(),
        "--full".to_string(),
        "--fts-only".to_string(),
    ];
    if crate::dev_bin::run("code", &index_args) != 0 {
        eprintln!();
        eprintln!("    \u{2717} Indexing failed (the reason is above)");
        return 1;
    }

    // ── Step 3: Build call graph ────────────────────────────────
    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");
    let scip_output_dir = tempdir.join("scip");

    if !no_scip {
        println!();
        println!("  Building call graph...");

        let exporter_check =
            corpus_engine_scip::scip_export::check_exporters(&scip_workspace_roots);

        // Surface missing exporters with actionable install instructions.
        // "could not be resolved", not "not found in PATH": resolution
        // also probes the well-known per-user toolchain dirs, so PATH
        // alone no longer describes what was searched.
        for m in &exporter_check.missing {
            println!(
                "    \u{26a0} {} exporter ({}) could not be resolved",
                m.language_id, m.command
            );
            println!("        {}", m.install_hint);
        }

        if exporter_check.available.is_empty() {
            println!("    \u{26a0} No SCIP exporters available — find_callers / find_callees will not work");
            println!("      Install the exporter(s) above then run: sovereign project refresh");
        } else {
            for exporter in &exporter_check.available {
                println!(
                    "    Using {} ({})",
                    exporter.config.command,
                    exporter.path.display()
                );
            }

            let graph = match corpus_engine_scip::ScipGraph::open(&scip_graph_path, &corpus_id) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("    \u{2717} Cannot open SCIP graph: {e}");
                    return 1;
                }
            };

            let progress_fn = |p: corpus_engine_scip::scip_export::ScipProgress<'_>| match p {
                corpus_engine_scip::scip_export::ScipProgress::Exporting { language } => {
                    eprint!("\r    Exporting {language}...      ");
                }
                corpus_engine_scip::scip_export::ScipProgress::Ingested {
                    language,
                    symbols,
                    refs,
                } => {
                    eprintln!(
                        "\r    \u{2713} {language}: {} symbols, {} references    ",
                        symbols, refs
                    );
                }
                corpus_engine_scip::scip_export::ScipProgress::Skipped { language, reason } => {
                    eprintln!("\r    \u{26a0} {language}: skipped ({reason})    ");
                }
            };

            match corpus_engine_scip::scip_export::export_all(
                &abs_path,
                &scip_output_dir,
                &graph,
                Some(&scip_workspace_roots),
                &progress_fn,
            )
            .await
            {
                Ok(summary) => {
                    println!(
                        "    \u{2713} {} symbols, {} call edges",
                        summary.total_symbols, summary.total_refs
                    );
                }
                Err(e) => {
                    eprintln!("    \u{2717} SCIP export failed: {e}");
                    eprintln!("      Call graph tools will not be available.");
                    eprintln!("      Run `svrn project refresh` after fixing the issue.");
                }
            }
        }
    }

    // ── Step 4: Write project files ─────────────────────────────
    println!();
    println!("  Writing project files...");

    // .sovereign/SOVEREIGN.md
    let sovereign_dir = repo_root.join(".sovereign");
    if let Err(e) = std::fs::create_dir_all(&sovereign_dir) {
        eprintln!("    \u{2717} Cannot create .sovereign/: {e}");
        return 1;
    }

    let lang_names: Vec<&str> = langs.iter().map(|l| l.id).collect();
    let md_content = generate_sovereign_md(&corpus_id, port, &lang_names, !no_scip);
    if let Err(e) = std::fs::write(sovereign_dir.join("SOVEREIGN.md"), &md_content) {
        eprintln!("    \u{2717} Cannot write SOVEREIGN.md: {e}");
        return 1;
    }
    println!("    \u{2713} .sovereign/SOVEREIGN.md");

    // .sovereign/project.json — stores init config for status/refresh
    // workspace_roots is only written when --workspace-root was given (monorepo
    // mode). When absent, refresh falls back to auto-detecting from the git root.
    let workspace_roots_json: serde_json::Value = if workspace_root_arg.is_some() {
        serde_json::Value::Array(
            scip_workspace_roots
                .iter()
                .map(|p| serde_json::Value::String(p.to_string_lossy().into_owned()))
                .collect(),
        )
    } else {
        serde_json::Value::Null
    };
    let mut project_config = serde_json::json!({
        "corpus_id": corpus_id,
        "port": port,
        "data_dir": data_dir.to_string_lossy(),
        "scip_enabled": !no_scip,
        "hooks_installed": !no_hooks && has_git && !no_scip,
        "claude_config_written": !no_claude_config,
    });
    if !workspace_roots_json.is_null() {
        project_config["workspace_roots"] = workspace_roots_json;
    }
    if let Err(e) = std::fs::write(
        sovereign_dir.join("project.json"),
        serde_json::to_string_pretty(&project_config).unwrap_or_default(),
    ) {
        eprintln!("    \u{2717} Cannot write project.json: {e}");
        // Non-fatal — status/refresh will just need flags.
    }

    // project.toml was written earlier in the flow (right after
    // observation) so its contents survive even if indexing fails.
    // Surface it here alongside the other config artifacts for
    // discoverability.
    println!("    \u{2713} .sovereign/project.toml");

    // .sovereign/sovereign.toml — starter config for background watchers.
    // Only written if the file doesn't already exist (preserve user edits).
    let toml_path = sovereign_dir.join("sovereign.toml");
    if !toml_path.exists() {
        let abs_root = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.clone());
        let toml_stub = format!(
            "# sovereign.toml — background watcher config for `svrn project serve`.\n\
             #\n\
             # Uncomment and fill in test_runner / lint_runner to enable the\n\
             # test_status, run_tests, lint_status MCP tools.\n\
             # Commands must emit Tier 2 JSONL on stdout (see SOVEREIGN.md).\n\
             \n\
             # [test_runner]\n\
             # command = \"cargo test 2>&1\"\n\
             # working_dir = \"{root}\"\n\
             # timeout_secs = 300\n\
             # debounce_ms = 2000\n\
             \n\
             # [lint_runner]\n\
             # command = \"cargo check --message-format json 2>&1 | sovereign-cargo-check-adapter\"\n\
             # working_dir = \"{root}\"\n\
             # timeout_secs = 120\n\
             # debounce_ms = 1000\n",
            root = abs_root.display()
        );
        if let Err(e) = std::fs::write(&toml_path, &toml_stub) {
            eprintln!("    \u{26a0} Could not write sovereign.toml: {e}");
        } else {
            println!("    \u{2713} .sovereign/sovereign.toml (starter config)");
        }
    }

    // Auto-detect which AI coding assistants are present. If none, we write
    // nothing — no clutter for non-AI-coding projects. If detected, we ask
    // once per harness whether to write its config.
    let detected = detect_harnesses(&repo_root);
    let write_claude =
        detected.claude_code && !no_claude_config && confirm_write_config("Claude Code");
    let write_opencode = detected.opencode && confirm_write_config("opencode");

    // Commonwealth URL: after `svrn setup`, the local daemon always
    // lives at http://localhost:9741 and serves both /v1 and /mcp. Users
    // who want to point at a remote Commonwealth can override via the
    // legacy `[commonwealth]` section in sovereign.toml.
    let commonwealth_url: Option<String> = {
        let toml_path = repo_root.join(".sovereign/sovereign.toml");
        let from_toml = std::fs::read_to_string(&toml_path)
            .ok()
            .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
            .and_then(|v| {
                v.get("commonwealth")
                    .and_then(|c| c.get("url"))
                    .and_then(|u| u.as_str())
                    .map(str::to_owned)
            });
        from_toml.or_else(|| write_opencode.then(|| "http://localhost:9741".to_string()))
    };

    // Probe OICP capabilities to enumerate real model IDs. Falls back to []
    // gracefully if Commonwealth is not running at init time.
    let commonwealth_models: Vec<String> = if let Some(ref url) = commonwealth_url {
        fetch_commonwealth_models(url).await
    } else {
        vec![]
    };

    // .claude/settings.json
    if write_claude {
        let claude_dir = repo_root.join(".claude");
        if let Err(e) = std::fs::create_dir_all(&claude_dir) {
            eprintln!("    \u{2717} Cannot create .claude/: {e}");
            return 1;
        }

        let settings_path = claude_dir.join("settings.json");
        let generated = generate_claude_settings(port, &corpus_id, has_git, !no_scip);

        let final_content = if settings_path.exists() {
            match std::fs::read_to_string(&settings_path) {
                Ok(existing) => merge_claude_settings(&existing, &generated),
                Err(_) => generated,
            }
        } else {
            generated
        };

        if let Err(e) = std::fs::write(&settings_path, &final_content) {
            eprintln!("    \u{2717} Cannot write settings.json: {e}");
            return 1;
        }
        println!("    \u{2713} .claude/settings.json");
    }

    // .opencode/opencode.json + AGENTS.md
    if write_opencode {
        let opencode_dir = repo_root.join(".opencode");
        if let Err(e) = std::fs::create_dir_all(&opencode_dir) {
            eprintln!("    \u{26a0} Cannot create .opencode/: {e}");
        } else {
            // Inside a project `.opencode/` dir opencode reads ONLY
            // `opencode.json` / `opencode.jsonc`. We wrote `config.json`
            // until 2026-07-28, which it silently ignores — so migrate
            // the old name rather than leaving a file that looks live.
            let config_path = opencode_dir.join("opencode.json");
            let legacy_path = opencode_dir.join("config.json");
            let generated = generate_opencode_config(
                port,
                &corpus_id,
                commonwealth_url.as_deref(),
                &commonwealth_models,
            );
            let merge_base = std::fs::read_to_string(&config_path)
                .or_else(|_| std::fs::read_to_string(&legacy_path))
                .ok();
            let final_content = match merge_base {
                Some(existing) => merge_opencode_config(&existing, &generated),
                None => generated,
            };
            match std::fs::write(&config_path, &final_content) {
                Ok(()) => {
                    println!("    \u{2713} .opencode/opencode.json");
                    if legacy_path.exists() && std::fs::remove_file(&legacy_path).is_ok() {
                        println!(
                            "    \u{2713} removed .opencode/config.json (opencode never read it)"
                        );
                    }
                }
                Err(e) => eprintln!("    \u{26a0} Cannot write .opencode/opencode.json: {e}"),
            }
        }

        // AGENTS.md — only write if absent; it's project-specific and users edit it.
        let agents_path = repo_root.join("AGENTS.md");
        if !agents_path.exists() {
            let content =
                generate_agents_md(&corpus_id, port, !no_scip, commonwealth_url.as_deref());
            match std::fs::write(&agents_path, &content) {
                Ok(()) => println!("    \u{2713} AGENTS.md"),
                Err(e) => eprintln!("    \u{26a0} Cannot write AGENTS.md: {e}"),
            }
        }
    }

    // .gitignore
    if has_git {
        if let Err(e) = update_gitignore(&repo_root) {
            eprintln!("    \u{2717} Cannot update .gitignore: {e}");
            // Non-fatal.
        } else {
            println!("    \u{2713} .gitignore updated");
        }
    }

    // ── Step 5: Legacy git-hook cleanup ─────────────────────────
    //
    // Earlier sovereign versions installed a post-commit hook that
    // shelled out to `svrn project refresh`. The daemon now
    // owns freshness (FS watcher + git HEAD poll + startup
    // catch-up), so the hook is redundant and has been a common
    // source of silent staleness when the binary path drifted.
    // Remove any legacy hook we find; never install a new one.
    if has_git {
        match remove_legacy_hook(&repo_root) {
            Ok(true) => {
                println!();
                println!("  Cleaned up legacy post-commit hook — the daemon now keeps");
                println!("  the graph fresh automatically.");
            }
            Ok(false) => { /* nothing to remove */ }
            Err(e) => {
                eprintln!("    \u{26a0} Could not clean up legacy hook: {e}");
            }
        }
        let _ = no_hooks;
    }

    // ── Step 6: Register with the running daemon ────────────────
    //
    // The daemon's Reindexer picks this up immediately — no
    // restart needed. If the daemon isn't running we fall through
    // silently; the next `svrn daemon restart` (or startup)
    // will pick up the registry entry via `Registry::load()`.
    if !no_scip {
        println!();
        println!("  Registering with daemon...");
        // Build the watcher toggle block only when the user passed
        // `--watcher-ignore` — otherwise let the daemon use the
        // serde default (which already includes `.sovereign`).
        // Built as JSON rather than a typed `corpus_engine_watchers::projects::
        // WatcherToggles`: that struct is the only thing `init` ever wanted
        // from sovereign-mesh, and sovereign-mesh links llama.cpp
        // unconditionally — a whole inference stack in an end-user binary for
        // one options bag. Every other field on the struct has a serde
        // default, so a body carrying only `ignore_paths` deserializes on the
        // daemon side exactly as `Default::default()` with that field set,
        // which is precisely what this branch used to build.
        let watchers_block = if watcher_ignore_set {
            Some(serde_json::json!({ "ignore_paths": watcher_ignore_args.clone() }))
        } else {
            None
        };
        let register_body = serde_json::json!({
            "corpus_id": corpus_id,
            "root": abs_path.display().to_string(),
            "watchers": watchers_block,
        });
        match daemon_post("/v1/projects/register", register_body).await {
            Ok(_) => {
                println!("    \u{2713} Daemon is now watching this project");
            }
            Err(e) => {
                println!("    \u{26a0} Could not reach the daemon ({e}).");
                println!("      The registry entry was still written; the daemon will");
                println!("      pick it up on next start. Try `svrn daemon status`.");
            }
        }
    }

    // ── Step 7: MCP server check ────────────────────────────────
    println!();
    println!("  MCP server...");

    let mcp_url = format!("http://localhost:{port}/mcp");
    if check_mcp_server(&mcp_url).await {
        println!("    \u{2713} {mcp_url}");
    } else {
        println!("    \u{26a0} Not running at {mcp_url}");
        println!("      Start with: sovereign daemon restart");
    }

    // ── Done ────────────────────────────────────────────────────
    println!();
    println!("  {}", "─".repeat(54));
    println!("  Ready. Open Claude Code in this directory.");
    println!();
    println!("  Quick check:");
    println!("    sovereign project status");
    println!("    sovereign project watch status");
    println!();

    // Design-first nudge (step 2c). The user just got a tool set up;
    // point them at the next natural step — DESIGN.md-driven agent
    // collaboration — without making it feel mandatory. Only surface
    // when there's genuinely no DESIGN.md yet and the project isn't
    // already founded (no point suggesting `project design` to
    // someone who's already past that stage).
    let founded = match read_lifecycle(&repo_root) {
        Ok(l) => l.founded,
        Err(code) => return code,
    };
    if !design_exists && !founded {
        println!("  Next: `svrn project design` — I'll work with the agent on your DESIGN.md.");
        println!("        Bring a path to an existing doc with `--import <path>`, or start blank.");
        println!();
    }

    0
}

/// The two `.sovereign/project.toml` lifecycle flags init reads.
struct Lifecycle {
    git_declined_at_init: bool,
    founded: bool,
}

/// Ask the code program for the lifecycle flags
/// (`sovereign-cli-dev project-lifecycle`, pb-code-cli-base). `Err` is the
/// exit code init returns; the reason is printed, never defaulted.
fn read_lifecycle(repo_root: &Path) -> Result<Lifecycle, i32> {
    let out = crate::dev_bin::output("project-lifecycle", &[repo_root.display().to_string()])?;
    let v: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| {
        eprintln!("    \u{2717} project-lifecycle answered unreadable JSON ({e}): {out}");
        1
    })?;
    let flag = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                eprintln!("    \u{2717} project-lifecycle answered without `{k}`: {out}");
                1
            })
    };
    let l = Lifecycle {
        git_declined_at_init: flag("git_declined_at_init")?,
        founded: flag("founded")?,
    };
    tracing::debug!(
        git_declined_at_init = l.git_declined_at_init,
        founded = l.founded,
        "init: lifecycle read"
    );
    Ok(l)
}

// ─── Design session ──────────────────────────────────────────

/// Local-to-`project_cmd` language detection struct. Distinct from
/// the code program's `observation::LanguageObservation` which carries the
/// human-readable `display` form its observation report prints;
/// here we only need the stable `id` to drive SCIP-tooling decisions.
struct DetectedLanguage {
    id: &'static str,
}

fn detect_languages(root: &Path) -> Vec<DetectedLanguage> {
    let mut found = Vec::new();

    // Rust: Cargo.toml
    if root.join("Cargo.toml").exists() {
        found.push(DetectedLanguage { id: "rust" });
    }

    // TypeScript/JavaScript: tsconfig.json or package.json
    if root.join("tsconfig.json").exists() || root.join("tsconfig.base.json").exists() {
        found.push(DetectedLanguage { id: "typescript" });
    } else if root.join("package.json").exists() {
        // Check if it's TS or JS.
        let is_ts =
            root.join("tsconfig.json").exists() || has_file_extension_recursive(root, "ts", 2);
        if is_ts {
            found.push(DetectedLanguage { id: "typescript" });
        } else {
            found.push(DetectedLanguage { id: "javascript" });
        }
    }

    // Go: go.mod
    if root.join("go.mod").exists() {
        found.push(DetectedLanguage { id: "go" });
    }

    // Python: pyproject.toml, setup.py, or requirements.txt
    if root.join("pyproject.toml").exists()
        || root.join("setup.py").exists()
        || root.join("requirements.txt").exists()
    {
        found.push(DetectedLanguage { id: "python" });
    }

    found
}

/// Check whether any file with the given extension exists within `max_depth`
/// directory levels under `root`. Quick heuristic — not exhaustive.
fn has_file_extension_recursive(root: &Path, ext: &str, max_depth: usize) -> bool {
    has_ext_inner(root, ext, 0, max_depth)
}

fn has_ext_inner(dir: &Path, ext: &str, depth: usize, max_depth: usize) -> bool {
    if depth > max_depth {
        return false;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == ext)
                .unwrap_or(false)
            {
                return true;
            }
        } else if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Skip hidden dirs, node_modules, target, etc.
            if !name.starts_with('.')
                && name != "node_modules"
                && name != "target"
                && has_ext_inner(&path, ext, depth + 1, max_depth)
            {
                return true;
            }
        }
    }
    false
}
