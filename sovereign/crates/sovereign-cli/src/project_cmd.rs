//! `sovereign project` subcommand — one-shot workspace setup for code intelligence.
//!
//! Run `sovereign project init` from any repo root and the entire code
//! intelligence stack is wired up: tree-sitter symbol index, SCIP call
//! graph, `.claude/settings.json`, `SOVEREIGN.md`, git hooks, and a
//! filesystem watcher. Two minutes from first run to fully working tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, IngestProgress};

// ─── Dispatch ────────────────────────────────────────────────

pub async fn run_project(args: &[String]) -> i32 {
    if args.is_empty() {
        print_usage();
        return 1;
    }

    match args[0].as_str() {
        "init" => cmd_init(&args[1..]).await,
        "status" => cmd_status(&args[1..]).await,
        "refresh" => cmd_refresh(&args[1..]).await,
        "serve" => cmd_serve(&args[1..]).await,
        "help" | "--help" | "-h" => {
            print_usage();
            0
        }
        other => {
            eprintln!("Unknown project subcommand: {other}");
            print_usage();
            1
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage: sovereign project <command>

Commands:
  init                Set up code intelligence for the current workspace
    --name <id>       Corpus ID (default: directory name)
    --no-scip         Skip SCIP call graph export
    --no-hooks        Skip git hook installation
    --no-claude-config  Skip writing .claude/settings.json
    --port <port>     MCP server port (default: 8080)
    --data-dir <dir>  Index directory (default: ~/.sovereign/indexes)

  status              Show the status of code intelligence
  refresh             Update the call graph (runs automatically on commit)
    --quiet           Suppress progress output
  serve               Start a lightweight MCP server (no model required)
    --port <port>     Listen port (default: 8080)
    --data-dir <dir>  Index directory (default: ~/.sovereign/indexes)
  help                Show this help"
    );
}

// ─── Init ────────────────────────────────────────────────────

async fn cmd_init(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut no_scip = false;
    let mut no_hooks = false;
    let mut no_claude_config = false;
    let mut port: u16 = 8080;
    let mut data_dir: Option<PathBuf> = None;

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

    // ════════════════════════════════════════════════════════════
    println!();
    println!("  Sovereign Project Intelligence");
    println!("  {}", "─".repeat(54));

    // ── Step 1: Detect workspace ────────────────────────────────
    println!();
    println!("  Detecting workspace...");

    let langs = detect_languages(&repo_root);
    if langs.is_empty() {
        eprintln!("    ! No supported languages detected");
        eprintln!("      Supported: Rust, TypeScript, JavaScript, Go, Python");
        return 1;
    }
    for lang in &langs {
        println!("    \u{2713} {}", lang.display);
    }
    if has_git {
        let commit_count = git_commit_count(&repo_root).unwrap_or(0);
        println!(
            "    \u{2713} Git repository ({} commit{})",
            commit_count,
            if commit_count == 1 { "" } else { "s" }
        );
    }

    // ── Step 2: Index symbols ───────────────────────────────────
    println!();
    println!("  Indexing symbols...");

    let abs_path = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

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

    let recipe_toml = format!(
        r#"[corpus]
id = "{corpus_id}"
name = "{corpus_id}"
description = "Code corpus generated by `sovereign project init`"
license = "private"
mesh_sharing = false
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "code"
context_lines = 3
max_lines_per_chunk = 150

[chunk]
type = "passthrough"

[index]
fts = true
vector = false
"#,
        corpus_id = corpus_id,
        path = abs_path.display(),
    );

    let tempdir = match tempfile_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("    \u{2717} Cannot create temp dir: {e}");
            return 1;
        }
    };
    let recipe_path = tempdir.join(format!("{corpus_id}.toml"));
    if let Err(e) = std::fs::write(&recipe_path, &recipe_toml) {
        eprintln!("    \u{2717} Cannot write recipe: {e}");
        return 1;
    }

    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
    let recipes_dir = tempdir.clone();
    let engine = CorpusEngine::new(recipes_dir, data_dir.clone(), embed);

    // Progress callback — inline progress bar.
    let progress: corpus_engine::ProgressCallback = Box::new(|p| match p {
        IngestProgress::Extracting { documents_processed } => {
            eprint!(
                "\r    Extracting... {documents_processed} files    "
            );
        }
        IngestProgress::Embedding {
            chunks_embedded,
            total,
            ..
        } => {
            if total > 0 {
                let pct = (chunks_embedded as f32 / total as f32 * 100.0).min(100.0);
                let filled = (pct / 5.0) as usize;
                let empty = 20usize.saturating_sub(filled);
                eprint!(
                    "\r    {}{} {:3.0}%  {} symbols embedded   ",
                    "\u{2588}".repeat(filled),
                    "\u{2591}".repeat(empty),
                    pct,
                    chunks_embedded,
                );
            } else {
                eprint!("\r    Embedding... {chunks_embedded} symbols   ");
            }
        }
        IngestProgress::Indexing {
            chunks_indexed,
            total,
        } => {
            if total > 0 {
                let pct = (chunks_indexed as f32 / total as f32 * 100.0).min(100.0);
                let filled = (pct / 5.0) as usize;
                let empty = 20usize.saturating_sub(filled);
                eprint!(
                    "\r    {}{} {:3.0}%  {} symbols indexed    ",
                    "\u{2588}".repeat(filled),
                    "\u{2591}".repeat(empty),
                    pct,
                    chunks_indexed,
                );
            }
        }
        IngestProgress::Complete {
            total_chunks,
            duration_secs,
        } => {
            eprintln!(
                "\r    \u{2713} {} symbols indexed in {}s                ",
                total_chunks, duration_secs
            );
        }
        _ => {}
    });

    let spec = CorpusSpec::RecipePath(recipe_path);
    match engine.ingest(&spec, Some(progress)).await {
        Ok(result) => {
            // Complete variant already printed by the callback, but
            // if it wasn't triggered, print the summary now.
            if result.chunks_created > 0 {
                eprintln!(
                    "\r    \u{2713} {} symbols indexed                           ",
                    result.chunks_created
                );
            }
        }
        Err(e) => {
            eprintln!();
            eprintln!("    \u{2717} Indexing failed: {e}");
            return 1;
        }
    }

    // ── Step 3: Build call graph ────────────────────────────────
    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");
    let scip_output_dir = tempdir.join("scip");

    if !no_scip {
        println!();
        println!("  Building call graph...");

        let exporters = corpus_engine::scip_export::exporters_for_workspace(&abs_path);

        if exporters.is_empty() {
            println!("    \u{26a0} No SCIP exporters found in PATH");
            println!("      find_callers and find_callees will not be available");
            let hints: Vec<&str> = corpus_engine::scip_export::all_exporters()
                .iter()
                .filter(|e| {
                    langs.iter().any(|l| l.id == e.language_id)
                })
                .map(|e| e.install_hint)
                .collect();
            for hint in hints {
                println!("      {hint}");
            }
        } else {
            for exporter in &exporters {
                println!("    Using {}", exporter.command);
            }

            let graph = match corpus_engine::ScipGraph::open(&scip_graph_path, &corpus_id) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("    \u{2717} Cannot open SCIP graph: {e}");
                    return 1;
                }
            };

            let progress_fn = |p: corpus_engine::scip_export::ScipProgress<'_>| match p {
                corpus_engine::scip_export::ScipProgress::Exporting { language } => {
                    eprint!("\r    Exporting {language}...      ");
                }
                corpus_engine::scip_export::ScipProgress::Ingested {
                    language,
                    symbols,
                    refs,
                } => {
                    eprintln!(
                        "\r    \u{2713} {language}: {} symbols, {} references    ",
                        symbols, refs
                    );
                }
                corpus_engine::scip_export::ScipProgress::Skipped { language, reason } => {
                    eprintln!(
                        "\r    \u{26a0} {language}: skipped ({reason})    "
                    );
                }
            };

            match corpus_engine::scip_export::export_all(
                &abs_path,
                &scip_output_dir,
                &graph,
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
                    eprintln!("      Run `sovereign project refresh` after fixing the issue.");
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
    let project_config = serde_json::json!({
        "corpus_id": corpus_id,
        "port": port,
        "data_dir": data_dir.to_string_lossy(),
        "scip_enabled": !no_scip,
        "hooks_installed": !no_hooks && has_git && !no_scip,
        "claude_config_written": !no_claude_config,
    });
    if let Err(e) = std::fs::write(
        sovereign_dir.join("project.json"),
        serde_json::to_string_pretty(&project_config).unwrap_or_default(),
    ) {
        eprintln!("    \u{2717} Cannot write project.json: {e}");
        // Non-fatal — status/refresh will just need flags.
    }

    // .claude/settings.json
    if !no_claude_config {
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

    // .gitignore
    if has_git {
        if let Err(e) = update_gitignore(&repo_root) {
            eprintln!("    \u{2717} Cannot update .gitignore: {e}");
            // Non-fatal.
        } else {
            println!("    \u{2713} .gitignore updated");
        }
    }

    // ── Step 5: Git hooks ───────────────────────────────────────
    if !no_hooks && has_git && !no_scip {
        println!();
        println!("  Installing git hooks...");
        match install_post_commit_hook(&repo_root, &corpus_id) {
            Ok(()) => println!("    \u{2713} .git/hooks/post-commit"),
            Err(e) => {
                eprintln!("    \u{2717} Cannot install hook: {e}");
                eprintln!("      Run `sovereign project refresh` manually after commits.");
            }
        }
    }

    // ── Step 6: MCP server check ────────────────────────────────
    println!();
    println!("  MCP server...");

    let mcp_url = format!("http://localhost:{port}/mcp");
    if check_mcp_server(&mcp_url).await {
        println!("    \u{2713} {mcp_url}");
    } else {
        println!("    \u{26a0} Not running at {mcp_url}");
        println!("      Start with: sovereign-server --config <config.toml>");
        println!("      Or configure Claude Code to use stdio transport.");
    }

    // ── Done ────────────────────────────────────────────────────
    println!();
    println!("  {}", "─".repeat(54));
    println!("  Ready. Open Claude Code in this directory.");
    println!();
    println!("  Quick check:");
    println!("    sovereign project status");
    println!();

    0
}

// ─── Status ──────────────────────────────────────────────────

async fn cmd_status(args: &[String]) -> i32 {
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    // Load project config if available.
    let config = load_project_config(&repo_root);
    let corpus_id = config
        .as_ref()
        .and_then(|c| c["corpus_id"].as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });
    let port = config
        .as_ref()
        .and_then(|c| c["port"].as_u64())
        .unwrap_or(8080) as u16;

    println!();
    println!("  {corpus_id}");
    println!("  {}", "─".repeat(50));
    println!();

    // Index
    let index_path = data_dir.join(&corpus_id);
    if index_path.exists() {
        match corpus_engine::CorpusIndex::open(&index_path).await {
            Ok(idx) => match idx.info().await {
                Ok(info) => {
                    let age = format_age(info.last_updated);
                    println!(
                        "  Index         \u{2713} {} symbols  last updated {age}",
                        info.chunk_count
                    );
                }
                Err(_) => {
                    println!("  Index         \u{2713} present (cannot read stats)");
                }
            },
            Err(_) => {
                println!("  Index         \u{2717} corrupt or unreadable");
            }
        }
    } else {
        println!("  Index         \u{2717} not found");
        println!("                  Run: sovereign project init");
    }

    // Call graph
    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");
    if scip_graph_path.exists() {
        match corpus_engine::ScipGraph::open(&scip_graph_path, &corpus_id) {
            Ok(graph) => {
                let sym_count = graph.symbol_count().await;
                let ref_count = graph.ref_count().await;
                let stale_count = graph.stale_file_count().await;
                if stale_count > 0 {
                    println!(
                        "  Call graph    \u{26a0} {} symbols, {} edges  ({stale_count} files modified since last export)",
                        sym_count, ref_count
                    );
                    println!("                  Run: sovereign project refresh");
                } else {
                    println!(
                        "  Call graph    \u{2713} {} symbols, {} edges",
                        sym_count, ref_count
                    );
                }
            }
            Err(_) => {
                println!("  Call graph    \u{2717} corrupt or unreadable");
            }
        }
    } else {
        let scip_enabled = config
            .as_ref()
            .and_then(|c| c["scip_enabled"].as_bool())
            .unwrap_or(true);
        if scip_enabled {
            println!("  Call graph    \u{2717} not exported");
            println!("                  Run: sovereign project refresh");
        } else {
            println!("  Call graph    \u{2500} disabled (--no-scip)");
        }
    }

    // MCP server
    let mcp_url = format!("http://localhost:{port}/mcp");
    if check_mcp_server(&mcp_url).await {
        println!("  MCP server    \u{2713} {mcp_url}");
    } else {
        println!("  MCP server    \u{2717} not running");
        println!("                  Run: sovereign-server --config <config.toml>");
    }

    // SOVEREIGN.md
    let sovereign_md = repo_root.join(".sovereign").join("SOVEREIGN.md");
    if sovereign_md.exists() {
        println!("  SOVEREIGN.md  \u{2713} .sovereign/SOVEREIGN.md");
    } else {
        println!("  SOVEREIGN.md  \u{2717} not found");
    }

    // Claude config
    let claude_settings = repo_root.join(".claude").join("settings.json");
    if claude_settings.exists() {
        // Check if it has sovereign MCP config.
        let has_sovereign = std::fs::read_to_string(&claude_settings)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["mcpServers"]["sovereign"].as_object().cloned())
            .is_some();
        if has_sovereign {
            println!("  Claude config \u{2713} .claude/settings.json");
        } else {
            println!("  Claude config \u{26a0} .claude/settings.json (no sovereign MCP entry)");
        }
    } else {
        println!("  Claude config \u{2717} not found");
    }

    // Git hook
    let hook_path = repo_root.join(".git").join("hooks").join("post-commit");
    if hook_path.exists() {
        let has_sovereign = std::fs::read_to_string(&hook_path)
            .ok()
            .map(|s| s.contains("sovereign project refresh"))
            .unwrap_or(false);
        if has_sovereign {
            println!("  Git hook      \u{2713} installed");
        } else {
            println!("  Git hook      \u{2717} exists but missing sovereign refresh");
        }
    } else if repo_root.join(".git").exists() {
        println!("  Git hook      \u{2717} not installed");
    }

    // Tools available
    println!();
    println!("  Tools available:");
    print!("    symbol_lookup    recent_changes    code_search");
    let scip_available = scip_graph_path.exists();
    if scip_available {
        println!();
        println!("    find_callers     find_callees");
    } else {
        println!();
    }

    println!();
    println!("  Run `sovereign project refresh` to update the call graph.");
    println!();

    0
}

// ─── Refresh ─────────────────────────────────────────────────

async fn cmd_refresh(args: &[String]) -> i32 {
    let mut quiet = false;
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--quiet" | "-q" => quiet = true,
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let abs_path = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    let config = load_project_config(&repo_root);
    let corpus_id = config
        .as_ref()
        .and_then(|c| c["corpus_id"].as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });

    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");

    if !quiet {
        eprintln!("  Refreshing call graph...");
    }

    let graph = match corpus_engine::ScipGraph::open(&scip_graph_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: cannot open SCIP graph: {e}");
            eprintln!("Run `sovereign project init` first.");
            return 1;
        }
    };

    // Get pre-refresh counts for delta display.
    let prev_symbols = graph.symbol_count().await;
    let prev_refs = graph.ref_count().await;

    let tempdir = match tempfile_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot create temp dir: {e}");
            return 1;
        }
    };
    let scip_output_dir = tempdir.join("scip");

    let progress_fn = |p: corpus_engine::scip_export::ScipProgress<'_>| {
        if quiet {
            return;
        }
        match p {
            corpus_engine::scip_export::ScipProgress::Exporting { language } => {
                eprint!("\r    Exporting {language}...      ");
            }
            corpus_engine::scip_export::ScipProgress::Ingested {
                language,
                symbols,
                refs,
            } => {
                eprintln!(
                    "\r    \u{2713} {language}: {} symbols, {} references    ",
                    symbols, refs
                );
            }
            corpus_engine::scip_export::ScipProgress::Skipped { language, reason } => {
                eprintln!("\r    \u{26a0} {language}: skipped ({reason})    ");
            }
        }
    };

    let start = std::time::Instant::now();
    match corpus_engine::scip_export::export_all(&abs_path, &scip_output_dir, &graph, &progress_fn)
        .await
    {
        Ok(summary) => {
            let elapsed = start.elapsed().as_secs();
            let sym_delta = summary.total_symbols as i64 - prev_symbols as i64;
            let ref_delta = summary.total_refs as i64 - prev_refs as i64;

            if !quiet {
                eprintln!(
                    "    \u{2713} {} symbols ({}{})",
                    summary.total_symbols,
                    if sym_delta >= 0 { "+" } else { "" },
                    sym_delta
                );
                eprintln!(
                    "    \u{2713} {} edges ({}{})",
                    summary.total_refs,
                    if ref_delta >= 0 { "+" } else { "" },
                    ref_delta
                );
                eprintln!("    \u{2713} Done in {elapsed} seconds");
            }
            0
        }
        Err(e) => {
            eprintln!("error: SCIP export failed: {e}");
            1
        }
    }
}

// ─── Serve ───────────────────────────────────────────────────

async fn cmd_serve(args: &[String]) -> i32 {
    let mut port: u16 = 8080;
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
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
            _ => {}
        }
        i += 1;
    }

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    if !data_dir.exists() {
        eprintln!("error: index directory does not exist: {}", data_dir.display());
        eprintln!("Run `sovereign project init` in at least one project first.");
        return 1;
    }

    eprintln!("  Sovereign Code Intelligence MCP Server");
    eprintln!("  {}", "─".repeat(54));

    // ── Build CorpusEngine (zero-vector, no model) ──────────────

    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
    let recipes_dir = data_dir.clone();
    let engine = Arc::new(CorpusEngine::new(recipes_dir, data_dir.clone(), embed));

    // List discovered indexes.
    match engine.installed_indexes().await {
        Ok(indexes) => {
            let code_indexes: Vec<_> = indexes
                .iter()
                .filter(|i| i.embedding_model == "nomic-embed-text-v2" || i.chunk_count > 0)
                .collect();
            if code_indexes.is_empty() {
                eprintln!("  warning: no indexes found in {}", data_dir.display());
            } else {
                eprintln!("  Corpora:");
                for info in &code_indexes {
                    eprintln!("    \u{2713} {} ({} symbols)", info.corpus_id, info.chunk_count);
                }
            }
        }
        Err(e) => {
            eprintln!("  warning: could not list indexes: {e}");
        }
    }

    // ── Discover and merge SCIP graphs ──────────────────────────

    eprintln!();
    eprintln!("  Call graph:");

    let merged_graph = Arc::new(
        corpus_engine::ScipGraph::open_in_memory("merged")
            .expect("in-memory ScipGraph"),
    );

    let mut total_symbols = 0usize;
    let mut total_refs = 0usize;
    let mut graphs_found = 0;

    if let Ok(entries) = std::fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let scip_path = path.join("scip_graph.db");
            if scip_path.exists() {
                let corpus_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?");
                match merged_graph.import_from_path(&scip_path).await {
                    Ok((syms, refs)) => {
                        if syms > 0 || refs > 0 {
                            eprintln!(
                                "    \u{2713} {corpus_name}: {} symbols, {} edges",
                                syms, refs
                            );
                            total_symbols += syms;
                            total_refs += refs;
                            graphs_found += 1;
                        }
                    }
                    Err(e) => {
                        eprintln!("    \u{2717} {corpus_name}: {e}");
                    }
                }
            }
        }
    }

    if graphs_found == 0 {
        eprintln!("    (none — run `sovereign project init` with SCIP exporters)");
    } else {
        eprintln!(
            "    Total: {} symbols, {} edges across {} projects",
            total_symbols, total_refs, graphs_found
        );
    }

    // ── Register tools ──────────────────────────────────────────

    let mut tools = sovereign_core::ToolRegistry::new();
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&engine),
    )));
    tools.register(Box::new(
        sovereign_tools::CodeSearchTool::new(Arc::clone(&engine)),
    ));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&engine),
    )));
    tools.register(Box::new(sovereign_tools::FindCalleesTool::new(
        Arc::clone(&engine),
        Arc::clone(&merged_graph),
    )));
    tools.register(Box::new(sovereign_tools::FindCallersTool::new(
        Arc::clone(&engine),
        Arc::clone(&merged_graph),
    )));

    let tools = Arc::new(tools);
    eprintln!();
    eprintln!("  Tools: {} registered", tools.count());

    // ── Start MCP HTTP server ───────────────────────────────────

    let bind_addr = format!("127.0.0.1:{port}");
    eprintln!("  Listening on http://{bind_addr}/mcp");
    eprintln!();
    eprintln!("  {}", "─".repeat(54));
    eprintln!("  Ready. Configure Claude Code with:");
    eprintln!();
    eprintln!("    {{");
    eprintln!("      \"mcpServers\": {{");
    eprintln!("        \"sovereign\": {{");
    eprintln!("          \"type\": \"http\",");
    eprintln!("          \"url\": \"http://localhost:{port}/mcp\"");
    eprintln!("        }}");
    eprintln!("      }}");
    eprintln!("    }}");
    eprintln!();

    let app = mcp_server::router(tools);

    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind to {bind_addr}: {e}");
            return 1;
        }
    };

    let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    if let Err(e) = axum::serve(listener, service).await {
        eprintln!("error: server failed: {e}");
        return 1;
    }

    0
}

// ─── Lightweight MCP server ──────────────────────────────────
//
// Minimal JSON-RPC 2.0 / MCP implementation that serves just the
// five code intelligence tools. No model, no auth, localhost only.

mod mcp_server {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::extract::{ConnectInfo, Extension};
    use axum::http::StatusCode;
    use axum::response::sse::{Event, KeepAlive, Sse};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use futures::stream::{self, Stream};
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use tower_http::cors::CorsLayer;

    use sovereign_core::registry::ToolRegistry;
    use sovereign_core::types::{StepOutput, ToolContext};

    #[derive(Deserialize)]
    pub struct JsonRpcRequest {
        #[allow(dead_code)]
        #[serde(default)]
        pub jsonrpc: String,
        pub id: Value,
        pub method: String,
        #[serde(default)]
        pub params: Option<Value>,
    }

    #[derive(Serialize)]
    pub struct JsonRpcResponse {
        pub jsonrpc: &'static str,
        pub id: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error: Option<JsonRpcError>,
    }

    #[derive(Serialize)]
    pub struct JsonRpcError {
        pub code: i32,
        pub message: String,
    }

    impl JsonRpcResponse {
        fn ok(id: Value, value: Value) -> Self {
            Self { jsonrpc: "2.0", id, result: Some(value), error: None }
        }
        fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
            Self {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(JsonRpcError { code, message: message.into() }),
            }
        }
    }

    fn call_tool_text(text: impl Into<String>, is_error: bool) -> Value {
        serde_json::json!({
            "content": [{ "type": "text", "text": text.into() }],
            "isError": is_error,
        })
    }

    const MCP_TOOLS: &[&str] = &[
        "symbol_lookup", "code_search", "recent_changes",
        "find_callees", "find_callers",
    ];

    pub fn router(tools: Arc<ToolRegistry>) -> Router {
        Router::new()
            .route("/mcp", post(mcp_init).get(mcp_sse))
            .route("/mcp/message", post(mcp_message))
            .layer(Extension(tools))
            .layer(CorsLayer::permissive())
    }

    fn is_localhost(addr: &SocketAddr) -> bool {
        addr.ip().is_loopback()
    }

    async fn mcp_init(
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
        Json(req): Json<JsonRpcRequest>,
    ) -> impl IntoResponse {
        if !is_localhost(&peer) {
            return (
                StatusCode::FORBIDDEN,
                Json(JsonRpcResponse::err(req.id, -32001, "MCP is local-only")),
            ).into_response();
        }
        let result = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "sovereign-code",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        (StatusCode::OK, Json(JsonRpcResponse::ok(req.id, result))).into_response()
    }

    async fn mcp_sse(
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, StatusCode> {
        if !is_localhost(&peer) {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(Sse::new(stream::pending()).keep_alive(KeepAlive::default()))
    }

    async fn mcp_message(
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
        Extension(tools): Extension<Arc<ToolRegistry>>,
        Json(req): Json<JsonRpcRequest>,
    ) -> impl IntoResponse {
        if !is_localhost(&peer) {
            return (
                StatusCode::FORBIDDEN,
                Json(JsonRpcResponse::err(req.id, -32001, "MCP is local-only")),
            ).into_response();
        }

        let id = req.id.clone();
        let response = match req.method.as_str() {
            "tools/list" => {
                let mut tool_list = Vec::new();
                for desc in tools.descriptors() {
                    if MCP_TOOLS.contains(&desc.id.as_str()) {
                        tool_list.push(serde_json::json!({
                            "name": desc.id,
                            "description": desc.description,
                            "inputSchema": desc.parameters,
                        }));
                    }
                }
                JsonRpcResponse::ok(id, serde_json::json!({ "tools": tool_list }))
            }
            "tools/call" => {
                let Some(params) = req.params else {
                    return (StatusCode::OK, Json(JsonRpcResponse::err(id, -32602, "missing params"))).into_response();
                };
                let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
                    return (StatusCode::OK, Json(JsonRpcResponse::err(id, -32602, "missing 'name'"))).into_response();
                };
                let name = name.to_string();
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));

                if !MCP_TOOLS.contains(&name.as_str()) {
                    JsonRpcResponse::err(id, -32601, format!("tool not found: {name}"))
                } else {
                    match tools.get(&name) {
                        Ok(tool) => {
                            if let Err(e) = tool.validate(&arguments) {
                                JsonRpcResponse::ok(id, call_tool_text(e.to_string(), true))
                            } else {
                                let ctx = ToolContext {
                                    conversation_id: "mcp".to_string(),
                                    task_id: None,
                                    working_directory: None,
                                    in_reasoning_loop: false,
                                };
                                match tool.execute(&arguments, &ctx).await {
                                    Ok(StepOutput::Text(text)) => {
                                        JsonRpcResponse::ok(id, call_tool_text(text, false))
                                    }
                                    Ok(StepOutput::Json(value)) => {
                                        let text = serde_json::to_string_pretty(&value)
                                            .unwrap_or_else(|_| value.to_string());
                                        JsonRpcResponse::ok(id, call_tool_text(text, false))
                                    }
                                    Ok(other) => {
                                        JsonRpcResponse::ok(id, call_tool_text(format!("{other:?}"), false))
                                    }
                                    Err(e) => {
                                        JsonRpcResponse::ok(id, call_tool_text(format!("Tool `{name}` failed: {e}"), true))
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            JsonRpcResponse::ok(
                                id,
                                call_tool_text(
                                    format!("`{name}` not registered. Run `sovereign project init` first."),
                                    false,
                                ),
                            )
                        }
                    }
                }
            }
            "initialize" => {
                let result = serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "sovereign-code",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                });
                JsonRpcResponse::ok(id, result)
            }
            other => JsonRpcResponse::err(id, -32601, format!("method not found: {other}")),
        };

        (StatusCode::OK, Json(response)).into_response()
    }
}

// ─── Language detection ──────────────────────────────────────

struct DetectedLanguage {
    id: &'static str,
    display: String,
}

fn detect_languages(root: &Path) -> Vec<DetectedLanguage> {
    let mut found = Vec::new();

    // Rust: Cargo.toml
    if root.join("Cargo.toml").exists() {
        let detail = detect_rust_detail(root);
        found.push(DetectedLanguage {
            id: "rust",
            display: detail,
        });
    }

    // TypeScript/JavaScript: tsconfig.json or package.json
    if root.join("tsconfig.json").exists() || root.join("tsconfig.base.json").exists() {
        found.push(DetectedLanguage {
            id: "typescript",
            display: "TypeScript".to_string(),
        });
    } else if root.join("package.json").exists() {
        // Check if it's TS or JS.
        let is_ts = root.join("tsconfig.json").exists()
            || has_file_extension_recursive(root, "ts", 2);
        if is_ts {
            found.push(DetectedLanguage {
                id: "typescript",
                display: "TypeScript".to_string(),
            });
        } else {
            found.push(DetectedLanguage {
                id: "javascript",
                display: "JavaScript".to_string(),
            });
        }
    }

    // Go: go.mod
    if root.join("go.mod").exists() {
        found.push(DetectedLanguage {
            id: "go",
            display: "Go".to_string(),
        });
    }

    // Python: pyproject.toml, setup.py, or requirements.txt
    if root.join("pyproject.toml").exists()
        || root.join("setup.py").exists()
        || root.join("requirements.txt").exists()
    {
        found.push(DetectedLanguage {
            id: "python",
            display: "Python".to_string(),
        });
    }

    found
}

/// Detect Rust workspace details from Cargo.toml.
fn detect_rust_detail(root: &Path) -> String {
    let cargo_toml = root.join("Cargo.toml");
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
        if let Ok(parsed) = content.parse::<toml::Value>() {
            if let Some(members) = parsed
                .get("workspace")
                .and_then(|w| w.get("members"))
                .and_then(|m| m.as_array())
            {
                return format!("Rust workspace ({} crates)", members.len());
            }
        }
    }
    "Rust".to_string()
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
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                if has_ext_inner(&path, ext, depth + 1, max_depth) {
                    return true;
                }
            }
        }
    }
    false
}

// ─── Git helpers ─────────────────────────────────────────────

fn find_repo_root() -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout);
        Some(PathBuf::from(s.trim()))
    } else {
        None
    }
}

fn git_commit_count(root: &Path) -> Option<usize> {
    let output = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if output.status.success() {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()
    } else {
        None
    }
}

// ─── File generation ─────────────────────────────────────────

fn generate_sovereign_md(
    corpus_id: &str,
    port: u16,
    langs: &[&str],
    has_scip: bool,
) -> String {
    let call_graph_section = if has_scip {
        "## Call graph

`find_callers` and `find_callees` use the SCIP graph.
New symbols have no graph entries until the next git commit
(the post-commit hook keeps this current automatically).
To refresh manually: `sovereign project refresh`"
    } else {
        "## Call graph

Call graph tools are not available (no SCIP exporter found).
Install a SCIP exporter and run `sovereign project refresh` to enable."
    };

    format!(
        r#"<!-- Generated by sovereign project init. Safe to commit. -->
# Sovereign Code Intelligence

MCP server: http://localhost:{port}/mcp
Corpus: {corpus_id}
Languages: {langs}

## Tools

| Tool | When to use | Correctness |
|---|---|---|
| `symbol_lookup` | You know the exact name | Always correct |
| `code_search` | You know the concept, not the name | Approximate |
| `recent_changes` | Session start — see active subsystems | Always correct |
| `find_callers` | What calls this function? | SCIP-based |
| `find_callees` | What does this function call? | SCIP-based |

## Hybrid strategy — when to use which tool

Use both the MCP code intelligence tools and built-in file tools
(Grep, Glob, Read). Each is better at different things:

**Discovery** — "What's in this module? What files exist?"
Use Glob to find files, Grep to search for patterns, Read to scan
a file. MCP tools need a name to look up; file tools find names.

**Precision** — "Show me CorpusEngine" / "What are its fields?"
Call `symbol_lookup("CorpusEngine")`. Returns the exact definition
with file path and line numbers. Cheaper and faster than reading
the whole file.

**Impact** — "What depends on this function?"
Call `find_callers("reindex_file")` before modifying it. This is
compiler-resolved (SCIP) — catches trait dispatch and method calls
that grep misses.

**Patterns** — "How do similar functions work?"
Call `find_callees("ingest")` to see what an existing function calls,
then follow the same pattern when implementing a new one.

**Explore then refine** — "How does checkpoint resume work?"
Start with `code_search("checkpoint resume")` to find relevant
symbols, then call `symbol_lookup` on each result to get the
precise definitions.

**Orientation** — "What changed recently?"
Call `recent_changes(hours: 24)` at session start to see which
subsystems are active before diving into code.

## Session start

1. Call `recent_changes(hours: 24)` to see what's active
2. Call `symbol_lookup` for any type before assuming its shape
3. Call `find_callers` on a trait before modifying any implementation

{call_graph_section}

## Project-specific invariants

Add invariants below this line. These are read by the coding agent
at session start and treated as authoritative guidance from the
architect.

---
"#,
        langs = langs.join(", "),
    )
}

fn generate_claude_settings(
    port: u16,
    corpus_id: &str,
    has_git: bool,
    has_scip: bool,
) -> String {
    let scip_instruction = if has_scip {
        "Before making significant changes to a function, call \
         find_callers to understand its impact radius. \
         Before implementing a function, call find_callees on \
         similar functions to understand established patterns."
    } else {
        "Call graph tools are not available. \
         Use code_search to find usage patterns."
    };

    let git_instruction = if has_git {
        " This is a git repository. recent_changes reflects \
         filesystem modification time, not git history."
    } else {
        ""
    };

    let system_prompt = format!(
        "You have access to Sovereign code intelligence for \
         the {corpus_id} codebase via MCP.\n\n\
         Read .sovereign/SOVEREIGN.md for the hybrid strategy, \
         tool reference, and project-specific invariants.\n\n\
         Use a hybrid approach: Grep/Glob/Read for discovery \
         (finding files, scanning modules), MCP tools for \
         precision (exact definitions, call graphs, recent \
         changes). Start broad, then refine with symbol_lookup.\n\n\
         AT SESSION START: call recent_changes(hours: 24) \
         to orient yourself.\n\n\
         BEFORE ASSUMING A TYPE'S SHAPE: call symbol_lookup. \
         Do not guess at field names or method signatures.\n\n\
         {scip_instruction}\n\n\
         {git_instruction}"
    );

    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "sovereign": {
                "type": "http",
                "url": format!("http://localhost:{port}/mcp")
            }
        },
        "systemPrompt": system_prompt,
    }))
    .unwrap_or_default()
}

/// Merge generated settings into existing .claude/settings.json without
/// clobbering user settings. Adds the sovereign MCP entry and prepends
/// the system prompt.
fn merge_claude_settings(existing: &str, generated: &str) -> String {
    let mut base: serde_json::Value =
        serde_json::from_str(existing).unwrap_or(serde_json::json!({}));
    let new: serde_json::Value = match serde_json::from_str(generated) {
        Ok(v) => v,
        Err(_) => return generated.to_string(),
    };

    // Merge mcpServers — add sovereign without removing others.
    if let Some(new_servers) = new.get("mcpServers").and_then(|v| v.as_object()) {
        let base_servers = base
            .as_object_mut()
            .unwrap()
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(base_map) = base_servers.as_object_mut() {
            for (k, v) in new_servers {
                base_map.insert(k.clone(), v.clone());
            }
        }
    }

    // Update the systemPrompt. If the existing prompt contains
    // non-Sovereign content, preserve it after the Sovereign block.
    if let Some(new_prompt) = new.get("systemPrompt").and_then(|v| v.as_str()) {
        if let Some(existing_prompt) = base.get("systemPrompt").and_then(|v| v.as_str()) {
            if existing_prompt.contains("Sovereign code intelligence") {
                // Replace the existing Sovereign block with the latest version.
                // If there's user content after the "---" separator, keep it.
                let user_section = existing_prompt
                    .split("\n\n---\n\n")
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                if user_section.is_empty() {
                    base["systemPrompt"] = serde_json::json!(new_prompt);
                } else {
                    base["systemPrompt"] =
                        serde_json::json!(format!("{new_prompt}\n\n---\n\n{user_section}"));
                }
            } else {
                // No Sovereign content yet — prepend.
                base["systemPrompt"] =
                    serde_json::json!(format!("{new_prompt}\n\n---\n\n{existing_prompt}"));
            }
        } else {
            base["systemPrompt"] = serde_json::json!(new_prompt);
        }
    }

    serde_json::to_string_pretty(&base).unwrap_or_else(|_| generated.to_string())
}

// ─── .gitignore ──────────────────────────────────────────────

fn update_gitignore(root: &Path) -> std::io::Result<()> {
    let gitignore_path = root.join(".gitignore");
    let entries_to_add = [".sovereign/project.json", ".sovereign/scip/"];

    let existing = if gitignore_path.exists() {
        std::fs::read_to_string(&gitignore_path)?
    } else {
        String::new()
    };

    let mut additions = Vec::new();
    for entry in entries_to_add {
        if !existing.lines().any(|line| line.trim() == entry) {
            additions.push(entry);
        }
    }

    if additions.is_empty() {
        return Ok(());
    }

    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }

    if !content.is_empty() {
        content.push_str("\n# Sovereign code intelligence\n");
    } else {
        content.push_str("# Sovereign code intelligence\n");
    }
    for entry in additions {
        content.push_str(entry);
        content.push('\n');
    }

    std::fs::write(&gitignore_path, content)
}

// ─── Git hooks ───────────────────────────────────────────────

fn install_post_commit_hook(root: &Path, corpus_id: &str) -> std::io::Result<()> {
    let hook_path = root.join(".git/hooks/post-commit");
    let _ = corpus_id; // corpus_id resolved from project.json by refresh

    // Resolve the binary path: use the current executable if it exists,
    // otherwise fall back to "sovereign" on PATH. This way the hook
    // works both for developers running from a local build and for
    // global installs.
    let current_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let hook_block = if let Some(ref exe) = current_exe {
        format!(
            r#"# Sovereign: refresh call graph after commit
if [ -x "{exe}" ]; then
  "{exe}" project refresh --quiet &
elif command -v sovereign >/dev/null 2>&1; then
  sovereign project refresh --quiet &
fi
"#,
            exe = exe.display()
        )
    } else {
        "# Sovereign: refresh call graph after commit\nsovereign project refresh --quiet &\n"
            .to_string()
    };

    if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)?;
        if existing.contains("sovereign") && existing.contains("project refresh") {
            // Already installed — don't duplicate.
            return Ok(());
        }
        // Append to existing hook.
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        content.push_str(&hook_block);
        std::fs::write(&hook_path, content)?;
    } else {
        let content = format!("#!/bin/sh\n{hook_block}");
        std::fs::write(&hook_path, content)?;
    }

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    Ok(())
}

// ─── MCP check ───────────────────────────────────────────────

async fn check_mcp_server(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });

    client
        .post(url)
        .json(&init_body)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ─── Helpers ─────────────────────────────────────────────────

fn default_data_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".sovereign").join("indexes"))
}

fn tempfile_dir() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    let suffix = format!("sovereign-project-{}", std::process::id());
    let path = base.join(suffix);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn load_project_config(root: &Path) -> Option<serde_json::Value> {
    let path = root.join(".sovereign").join("project.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Format a Unix timestamp as a human-readable relative time.
fn format_age(unix_ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if unix_ts == 0 {
        return "unknown".to_string();
    }

    let diff = now.saturating_sub(unix_ts);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{} min ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hours ago", diff / 3600)
    } else {
        format!("{} days ago", diff / 86400)
    }
}
