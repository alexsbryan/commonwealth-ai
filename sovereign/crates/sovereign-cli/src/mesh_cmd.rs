//! `sovereign mesh` and `sovereign corpus` subcommand handlers.
//!
//! These are lightweight commands that don't require loading a full model
//! or database — they manage the embedded Commonwealth daemon and corpus
//! indexes.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, ReconstructionMethod};
use sovereign_mesh::{parse_deep_link, EmbeddedDaemon};

/// Same location the desktop app uses: `<data_dir>/sovereign/`.
/// Sharing the path means a mesh created from the CLI is picked up
/// by the next desktop launch (and vice versa).
fn mesh_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
}

/// Run a mesh subcommand. Returns the exit code.
pub async fn run_mesh(args: &[String]) -> i32 {
    if args.is_empty() {
        print_mesh_usage();
        return 1;
    }

    match args[0].as_str() {
        "create" => cmd_create(&args[1..]).await,
        "join" => cmd_join(&args[1..]).await,
        "status" => cmd_status().await,
        "balance" => cmd_balance().await,
        "leave" => cmd_leave().await,
        "logs" => cmd_logs().await,
        "help" | "--help" | "-h" => {
            print_mesh_usage();
            0
        }
        other => {
            eprintln!("Unknown mesh subcommand: {other}");
            print_mesh_usage();
            1
        }
    }
}

/// Run a corpus subcommand. Returns the exit code.
pub async fn run_corpus(args: &[String]) -> i32 {
    if args.is_empty() {
        print_corpus_usage();
        return 1;
    }

    match args[0].as_str() {
        "list" => cmd_corpus_list().await,
        "install" => cmd_corpus_install(&args[1..]).await,
        "remove" => cmd_corpus_remove(&args[1..]).await,
        "status" => cmd_corpus_status().await,
        "reconstruct-manifest" => cmd_corpus_reconstruct_manifest(&args[1..]).await,
        "help" | "--help" | "-h" => {
            print_corpus_usage();
            0
        }
        other => {
            eprintln!("Unknown corpus subcommand: {other}");
            print_corpus_usage();
            1
        }
    }
}

fn print_mesh_usage() {
    eprintln!(
        "Usage: sovereign mesh <subcommand>

Manage your community mesh.

Subcommands:
  create --name <name>    Create a new mesh and print the join link
  join <link>             Join an existing mesh via deep link
  status                  Show mesh status (members, knowledge, model)
  balance                 Show your contribution to the mesh
  leave                   Leave the current mesh
  logs                    Show mesh daemon logs
"
    );
}

fn print_corpus_usage() {
    eprintln!(
        "Usage: sovereign corpus <subcommand>

Manage knowledge corpora shared across the mesh.

Subcommands:
  list                              List installed and available corpora
  install <id>                      Install a corpus (e.g., 'wikipedia')
  remove <id>                       Remove an installed corpus
  status                            Show shard status for all corpora
  reconstruct-manifest <id>         Reconstruct source-file manifest for a
                                    mid-flight index (required before
                                    collaborative ingestion)
    --source-dir <path>             Directory containing the parquet shards
                                    (defaults to ~/.sovereign/indexes/_downloads/<id>)
    --yes                           Skip confirmation prompt
"
    );
}

// ── Mesh subcommand implementations ──────────────────────

async fn cmd_create(args: &[String]) -> i32 {
    let mut name = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--name" {
            if let Some(n) = iter.next() {
                name = Some(n.clone());
            }
        }
    }

    let Some(mesh_name) = name else {
        eprintln!("Missing --name argument");
        eprintln!("Usage: sovereign mesh create --name <name>");
        return 1;
    };

    let node_name = hostname().unwrap_or_else(|| "sovereign-node".to_string());

    let daemon = EmbeddedDaemon::new(mesh_data_dir());
    match daemon.create_mesh(&mesh_name, &node_name).await {
        Ok(result) => {
            println!();
            println!("Mesh created: {}", result.mesh_name);
            println!();
            println!("Share this link with people you trust:");
            println!("  {}", result.join_link);
            println!();
            println!("Or share the join key directly:");
            println!("  {}", result.join_key);
            println!();
            0
        }
        Err(e) => {
            eprintln!("Failed to create mesh: {e}");
            1
        }
    }
}

async fn cmd_join(args: &[String]) -> i32 {
    let Some(link_str) = args.first() else {
        eprintln!("Missing join link");
        eprintln!("Usage: sovereign mesh join <sovereign://join/...>");
        return 1;
    };

    let link = match parse_deep_link(link_str) {
        Some(l) => l,
        None => {
            eprintln!("Invalid join link: {link_str}");
            return 1;
        }
    };

    let node_name = hostname().unwrap_or_else(|| "sovereign-node".to_string());
    let daemon = EmbeddedDaemon::new(mesh_data_dir());

    match daemon.join_mesh(&link, &node_name).await {
        Ok(result) => {
            println!();
            println!("Joined mesh: {}", result.mesh_name);
            println!("Your node ID: {}", result.node_id);
            println!();
            0
        }
        Err(e) => {
            eprintln!("Failed to join mesh: {e}");
            1
        }
    }
}

async fn cmd_status() -> i32 {
    println!("(mesh status requires a running daemon — this will be wired through the embedded daemon in a future update)");
    0
}

async fn cmd_balance() -> i32 {
    println!("(contribution balance requires a running daemon)");
    0
}

async fn cmd_leave() -> i32 {
    println!("(mesh leave requires a running daemon)");
    0
}

async fn cmd_logs() -> i32 {
    println!("(mesh logs are written to stderr when the daemon runs)");
    0
}

// ── Corpus subcommand implementations ────────────────────

async fn cmd_corpus_list() -> i32 {
    println!("Available built-in corpora:");
    println!();
    println!("  wikipedia       Wikipedia (6.8M articles, ~22 GB download)");
    println!("  stackexchange   Stack Exchange (12.4M answers, ~40 GB)");
    println!("  openalex        OpenAlex scholarly abstracts (~45 GB)");
    println!("  gutenberg       Project Gutenberg (~25 GB)");
    println!("  sep             Stanford Encyclopedia of Philosophy (~0.5 GB)");
    println!("  crs_reports     Congressional Research Service reports (~4 GB)");
    println!();
    println!("Install with: sovereign corpus install <id>");
    0
}

async fn cmd_corpus_install(args: &[String]) -> i32 {
    let Some(id) = args.first() else {
        eprintln!("Missing corpus ID");
        eprintln!("Usage: sovereign corpus install <id>");
        return 1;
    };
    println!("(corpus install '{id}' — requires wiring to CorpusEngine)");
    0
}

async fn cmd_corpus_remove(args: &[String]) -> i32 {
    let Some(id) = args.first() else {
        eprintln!("Missing corpus ID");
        return 1;
    };
    println!("(corpus remove '{id}' — requires wiring to CorpusEngine)");
    0
}

async fn cmd_corpus_status() -> i32 {
    println!("(corpus status requires a running daemon)");
    0
}

async fn cmd_corpus_reconstruct_manifest(args: &[String]) -> i32 {
    // Parse: <corpus_id> [--source-dir <path>] [--yes]
    let mut corpus_id: Option<String> = None;
    let mut source_dir: Option<PathBuf> = None;
    let mut yes = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--source-dir" => {
                if let Some(p) = iter.next() {
                    source_dir = Some(PathBuf::from(p));
                } else {
                    eprintln!("--source-dir requires a path argument");
                    return 1;
                }
            }
            "--yes" | "-y" => yes = true,
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                eprintln!("Usage: sovereign corpus reconstruct-manifest <corpus_id> [--source-dir <path>] [--yes]");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID");
        eprintln!("Usage: sovereign corpus reconstruct-manifest <corpus_id> [--source-dir <path>] [--yes]");
        return 1;
    };

    // Resolve the sovereign index dir: same logic as the daemon uses.
    let index_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join("indexes");

    // Build a no-op embed function — reconstruction reads metadata only.
    let noop_embed: corpus_engine::EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok(vec![0.0_f32; 0]) })
    });

    let recipes_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join("recipes");

    let engine = CorpusEngine::new(recipes_dir, index_dir, noop_embed);

    let report = match engine.reconstruct_source_manifest(&corpus_id, source_dir.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };

    // Print report.
    let method_label = match &report.method {
        ReconstructionMethod::IterPosVerification => "iter-pos verification (parquet row counts)".to_string(),
        ReconstructionMethod::ChunkCountHeuristic { median_rows_per_file } => {
            format!("chunk-count heuristic (median {median_rows_per_file} rows/file)")
        }
        ReconstructionMethod::SingleFile => "single-file source (no shard splitting)".to_string(),
    };

    let total = report.manifest.files.len();
    let complete = report.manifest.files.iter().filter(|f| {
        matches!(f.status, corpus_engine::SourceFileStatus::Complete { .. })
    }).count();
    let in_progress = report.manifest.files.iter().filter(|f| {
        matches!(f.status, corpus_engine::SourceFileStatus::InProgress { .. })
    }).count();
    let pending = report.manifest.files.iter().filter(|f| {
        matches!(f.status, corpus_engine::SourceFileStatus::Pending)
    }).count();

    println!();
    println!("Manifest reconstruction report for '{corpus_id}'");
    println!("  Method:           {method_label}");
    println!("  Files total:      {total}");
    println!("  Complete:         {complete}");
    println!("  In-progress:      {in_progress}  (reset to Pending — conservative)");
    println!("  Pending:          {pending}");
    if report.conservative_reprocessing_count > 0 {
        println!(
            "  Re-process count: {} (in-flight at crash time)",
            report.conservative_reprocessing_count
        );
    }
    if !report.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for w in &report.warnings {
            println!("  - {w}");
        }
    }
    println!();

    if !yes {
        eprint!("Write manifest to index? [y/N] ");
        // Flush stderr before reading stdin.
        use std::io::Write;
        let _ = std::io::stderr().flush();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            eprintln!("Could not read input — aborting. Use --yes to skip prompt.");
            return 1;
        }
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return 0;
        }
    }

    // The manifest has already been written by reconstruct_source_manifest().
    // Confirm path for the user.
    let index_path = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
        .join("indexes")
        .join(&corpus_id)
        .join("_source_manifest.json");
    println!("Manifest written to: {}", index_path.display());
    println!();
    println!("Next step: sovereign corpus collaborate {corpus_id}");
    println!();
    0
}

// ── Helpers ──────────────────────────────────────────────

fn hostname() -> Option<String> {
    // `HOSTNAME` / `COMPUTERNAME` env vars aren't reliably set in
    // GUI-launched or systemd-spawned child processes — notably
    // macOS doesn't export HOSTNAME to `cargo tauri dev`. The
    // `hostname` crate wraps the real `gethostname(2)` syscall and
    // returns something useful on every platform we care about.
    // Strip the `.local` Bonjour suffix so "Alexs-MBP.local" renders
    // cleanly in the mesh roster.
    ::hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|s| {
            s.strip_suffix(".local")
                .map(|t| t.to_string())
                .unwrap_or(s)
        })
}
