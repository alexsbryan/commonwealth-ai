//! `sovereign mesh` and `sovereign corpus` subcommand handlers.
//!
//! These are lightweight commands that don't require loading a full model
//! or database — they manage the embedded Commonwealth daemon and corpus
//! indexes.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, ReconstructionMethod};
use sovereign_mesh::deep_link::{build_https_join_link, parse_join_argument};
use sovereign_mesh::EmbeddedDaemon;

/// Same location the desktop app uses: `<platform-data-dir>/sovereign/`.
/// Sharing the path means a mesh created from the CLI is picked up by the
/// next desktop launch (and vice versa). Thin wrapper around the shared
/// `util::dirs::mesh_data_dir()` so this file still reads naturally.
fn mesh_data_dir() -> PathBuf {
    crate::util::dirs::mesh_data_dir()
}

/// Run a mesh subcommand. Returns the exit code.
pub async fn run_mesh(args: &[String]) -> i32 {
    if args.is_empty() {
        crate::util::help::print(&HELP_MESH);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        crate::util::help::print(&HELP_MESH);
        return 0;
    }

    match args[0].as_str() {
        "create" => cmd_create(&args[1..]).await,
        "join" => cmd_join(&args[1..]).await,
        "rotate" => cmd_rotate(&args[1..]).await,
        "status" => cmd_status().await,
        "balance" => cmd_balance().await,
        "leave" => cmd_leave().await,
        "logs" => cmd_logs().await,
        other => {
            eprintln!("Unknown mesh subcommand: {other}");
            crate::util::help::print(&HELP_MESH);
            1
        }
    }
}

/// Run a corpus subcommand. Returns the exit code.
pub async fn run_corpus(args: &[String]) -> i32 {
    if args.is_empty() {
        crate::util::help::print(&HELP_CORPUS);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        crate::util::help::print(&HELP_CORPUS);
        return 0;
    }

    match args[0].as_str() {
        "list" => cmd_corpus_list().await,
        "install" => cmd_corpus_install(&args[1..]).await,
        "remove" => cmd_corpus_remove(&args[1..]).await,
        "status" => cmd_corpus_status().await,
        "diag" => cmd_corpus_diag(&args[1..]).await,
        "dedupe" => cmd_corpus_dedupe(&args[1..]).await,
        "reconstruct-manifest" => cmd_corpus_reconstruct_manifest(&args[1..]).await,
        "migrate-to-partition" => cmd_corpus_migrate_to_partition(&args[1..]).await,
        "catalog" => crate::corpus_catalog_cmd::run_catalog(&args[1..]).await,
        other => {
            eprintln!("Unknown corpus subcommand: {other}");
            crate::util::help::print(&HELP_CORPUS);
            1
        }
    }
}

const HELP_MESH: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign mesh",
    summary: "Manage the local Commonwealth mesh (create / join / rotate / status).",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign mesh <subcommand> [args]"),
        crate::util::help::HelpSection::Subcommands(&[
            ("create",    "Promote the solo mesh to a joinable mesh; print invite"),
            ("join <arg>","Join an existing mesh (bare key, https url, or sovereign://)"),
            ("rotate",    "Generate a new shareable join key (invalidates the previous)"),
            ("status",    "Show mesh members, hosted knowledge, loaded models"),
            ("balance",   "Show your contribution to the mesh"),
            ("leave",     "Leave the current mesh"),
            ("logs",      "Show mesh daemon logs"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Run `sovereign mesh <subcommand> --help` for subcommand-specific flags.",
        ),
    ],
};

const HELP_MESH_CREATE: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign mesh create",
    summary: "Promote the solo mesh to a joinable mesh and print the shareable invite.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign mesh create [--name <name>]"),
        crate::util::help::HelpSection::Flags(&[
            ("--name <name>", "Human-readable mesh name (default: \"<host>'s Mesh\")"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Errors if a mesh already exists (e.g. from `sovereign setup`'s silent solo mesh).\n\
             In that case, run `sovereign mesh rotate` to generate a new shareable key instead.",
        ),
    ],
};

const HELP_MESH_JOIN: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign mesh join",
    summary: "Join an existing mesh using any of the three invite forms.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign mesh join <arg>"),
        crate::util::help::HelpSection::Examples(&[
            ("sovereign mesh join cwth-a1b2-c3d4-e5f6",
             "Bare key typed from another user's terminal"),
            ("sovereign mesh join https://sovereign.dev/join/cwth-a1b2-c3d4-e5f6",
             "Clickable https link from an email"),
            ("sovereign mesh join sovereign://join/cwth-a1b2-c3d4-e5f6",
             "Native app deep link"),
        ]),
    ],
};

const HELP_MESH_ROTATE: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign mesh rotate",
    summary: "Generate a new shareable join key (the previous key stops working for future joins).",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign mesh rotate"),
        crate::util::help::HelpSection::Notes(
            "Existing members keep their connections. If the daemon is running, restart it\n\
             so the new key is active in-memory (the persisted mesh.json is updated on disk).",
        ),
    ],
};

const HELP_CORPUS: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign corpus",
    summary: "Manage knowledge corpora shared across the mesh (install / remove / inspect).",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign corpus <subcommand> [args]"),
        crate::util::help::HelpSection::Subcommands(&[
            ("list",                      "List installed and available corpora"),
            ("install <id>",              "Install a corpus (e.g. 'wikipedia')"),
            ("remove <id>",               "Remove an installed corpus"),
            ("status",                    "Show shard status for all corpora"),
            ("diag <id>",                 "Audit an installed corpus: distinct-article count vs. recipe filter"),
            ("dedupe <id>",               "One-shot rescue: collapse duplicate-content rows from a resume-rewind ingest"),
            ("reconstruct-manifest <id>", "Rebuild source-file manifest (required before collaborative ingestion)"),
            ("migrate-to-partition <id>", "Rename a legacy canonical index into a partition-of-self so collaborative ingest can resume it"),
        ]),
        crate::util::help::HelpSection::Notes(
            "`reconstruct-manifest` accepts --source-dir <path> (default:\n\
             ~/.sovereign/indexes/_downloads/<id>) and --yes (skip confirmation).\n\
             `migrate-to-partition` accepts --dry-run to preview without touching disk.",
        ),
    ],
};

// ── Mesh subcommand implementations ──────────────────────

async fn cmd_create(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_MESH_CREATE);
        return 0;
    }
    let mut name = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--name" {
            if let Some(n) = iter.next() {
                name = Some(n.clone());
            }
        }
    }

    // If a mesh already exists (e.g. the silent solo mesh created by
    // `sovereign setup`), the join-key hash is stored but its plaintext
    // is gone — we can't re-show it. Direct the user to `mesh rotate`
    // instead of blindly attempting another create_mesh (which errors
    // with AlreadyRunning or leaves them confused).
    if sovereign_mesh::persist::load(&mesh_data_dir())
        .map(|opt| opt.is_some())
        .unwrap_or(false)
    {
        eprintln!("A mesh already exists (created during `sovereign setup`).");
        eprintln!("To generate a new shareable join key, run:");
        eprintln!();
        eprintln!("  sovereign mesh rotate");
        eprintln!();
        return 1;
    }

    let mesh_name = name.unwrap_or_else(|| {
        let host = hostname().unwrap_or_else(|| "sovereign".to_string());
        format!("{host}'s Mesh")
    });
    let node_name = hostname().unwrap_or_else(|| "sovereign-node".to_string());

    let daemon = EmbeddedDaemon::new(mesh_data_dir());
    match daemon.create_mesh(&mesh_name, &node_name).await {
        Ok(result) => {
            print_mesh_share(&result.mesh_name, &result.join_key);
            0
        }
        Err(e) => {
            eprintln!("Failed to create mesh: {e}");
            1
        }
    }
}

/// Spec-format banner for a freshly-created or freshly-rotated mesh.
/// Prints both the https share URL and the CLI form so the inviter
/// can pick whichever suits the invitee's environment.
fn print_mesh_share(mesh_name: &str, join_key: &str) {
    let app_link = build_https_join_link(join_key, None, Some(mesh_name));
    println!();
    println!("Mesh created.");
    println!();
    println!("  Join key:  {join_key}");
    println!();
    println!("Share with a friend:");
    println!("  App:  {app_link}");
    println!("  CLI:  sovereign mesh join {join_key}");
    println!();
}

async fn cmd_join(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_MESH_JOIN);
        return 0;
    }
    let Some(arg) = args.first() else {
        eprintln!("Missing join key.");
        eprintln!("Usage: sovereign mesh join <key-or-url>");
        eprintln!();
        eprintln!("Accepted forms:");
        eprintln!("  cwth-XXXX-XXXX-XXXX");
        eprintln!("  https://sovereign.dev/join/cwth-XXXX-XXXX-XXXX");
        eprintln!("  sovereign://join/cwth-XXXX-XXXX-XXXX");
        return 1;
    };

    let link = match parse_join_argument(arg) {
        Some(l) => l,
        None => {
            eprintln!("Invalid join argument: {arg}");
            eprintln!("Expected a bare key (cwth-XXXX-XXXX-XXXX), an https URL, or a sovereign:// link.");
            return 1;
        }
    };

    let node_name = hostname().unwrap_or_else(|| "sovereign-node".to_string());
    let daemon = EmbeddedDaemon::new(mesh_data_dir());

    println!();
    println!("Joining mesh...");
    match daemon.join_mesh(&link, &node_name).await {
        Ok(result) => {
            println!();
            println!("\u{2713} Connected to \"{}\"", result.mesh_name);
            println!("  Your node id: {}", result.node_id);
            println!();
            println!("Shared compute is now available.");
            println!();
            0
        }
        Err(e) => {
            eprintln!();
            eprintln!("Failed to join mesh: {e}");
            1
        }
    }
}

/// Rotate the join key on an existing mesh. Regenerates the plaintext
/// key + hash, writes the new hash back to `mesh.json`, and prints the
/// new shareable invite in the same format as `mesh create`.
async fn cmd_rotate(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP_MESH_ROTATE);
        return 0;
    }
    match sovereign_mesh::persist::rotate_join_key(&mesh_data_dir()) {
        Ok(Some(rotated)) => {
            eprintln!();
            eprintln!("Note: existing members stay connected. Only future joins need the new key.");
            eprintln!("If the daemon is currently running, restart it to load the new key.");
            print_mesh_share(&rotated.mesh_name, &rotated.join_key);
            0
        }
        Ok(None) => {
            eprintln!("No mesh to rotate — run `sovereign setup` or `sovereign mesh create` first.");
            1
        }
        Err(e) => {
            eprintln!("Failed to rotate join key: {e}");
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

/// `sovereign corpus install <id> [--params name=value,...] [--param key=value]...`
///
/// Submits an install request to the running daemon's
/// `/internal/corpus/install` endpoint. The daemon owns the actual
/// ingest task — this CLI command is a thin client so the install
/// runs in the background and the user can disconnect / re-attach
/// via `sovereign corpus status`.
///
/// Recipe parameters: when the recipe declares a
/// `[recipe.parameters]` block (e.g. `sec-filings` asking for an
/// entity list), supply values via either:
///
/// - `--params entities=NVDA,MSFT,GOOGL --params start_date=2022-01-01`
///   (each `--params` flag carries one comma-joined `key=value`)
/// - `--param entities=NVDA,MSFT --param start_date=2022-01-01`
///   (singular form, easier to remember; semantically identical)
/// - `--params-file <path>` for a JSON file containing the full
///   parameter map — handy for SEC investigations with dozens of
///   CIK numbers.
async fn cmd_corpus_install(args: &[String]) -> i32 {
    let mut positional: Vec<String> = Vec::new();
    let mut params: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    let mut params_file: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--params" | "--param" => {
                let Some(spec) = iter.next() else {
                    eprintln!("{a} requires a `key=value` argument");
                    return 1;
                };
                if let Err(e) = parse_param_spec(spec, &mut params) {
                    eprintln!("Invalid {a}: {e}");
                    return 1;
                }
            }
            "--params-file" => {
                let Some(p) = iter.next() else {
                    eprintln!("--params-file requires a path argument");
                    return 1;
                };
                params_file = Some(PathBuf::from(p));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: sovereign corpus install <id> [--params k=v[,k=v...]] \
                     [--params-file <path>]\n\n\
                     Submits an install request to the running daemon. Recipe \
                     parameters declared in the recipe's `[recipe.parameters]` block \
                     are validated by the daemon before ingest spawns; missing \
                     required parameters fail the request synchronously."
                );
                return 0;
            }
            other if !other.starts_with('-') => positional.push(other.to_string()),
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }

    let Some(id) = positional.first() else {
        eprintln!("Missing corpus ID. Usage: sovereign corpus install <id> [--params …]");
        return 1;
    };

    if let Some(path) = params_file {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to read --params-file {}: {e}", path.display());
                return 1;
            }
        };
        let from_file: std::collections::BTreeMap<String, serde_json::Value> =
            match serde_json::from_str(&raw) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "--params-file {} is not a JSON object of parameters: {e}",
                        path.display()
                    );
                    return 1;
                }
            };
        for (k, v) in from_file {
            params.entry(k).or_insert(v);
        }
    }

    let url = "http://127.0.0.1:9742/internal/corpus/install";
    let body = serde_json::json!({
        "corpus_id": id,
        "parameters": params,
    });
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    match client.post(url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            // The endpoint is fire-and-forget; surface the success
            // shape so users know the daemon picked it up.
            let body_text = resp.text().await.unwrap_or_default();
            println!("Install requested: {id}");
            if !body_text.is_empty() {
                println!("{body_text}");
            }
            println!("Watch progress: sovereign corpus status");
            0
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            eprintln!("Daemon rejected install ({status}): {body}");
            1
        }
        Err(e) => {
            eprintln!(
                "Failed to contact daemon at {url}: {e}\n\n\
                 Is `sovereign daemon` running? Try: sovereign daemon status"
            );
            1
        }
    }
}

/// Parse a single `--params` / `--param` value into the running
/// parameter map. Accepts:
///
/// - `key=value` — single string value
/// - `key=v1,v2,v3` — list of strings (comma-separated)
/// - `key=` — empty value (rare but useful for clearing a default)
///
/// The daemon does the type coercion (strings → ints / dates per
/// the recipe's declared `ParameterKind`), so the CLI just shapes
/// the JSON.
fn parse_param_spec(
    spec: &str,
    out: &mut std::collections::BTreeMap<String, serde_json::Value>,
) -> std::result::Result<(), String> {
    let (key, value) = spec
        .split_once('=')
        .ok_or_else(|| format!("expected `key=value`, got `{spec}`"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("empty parameter name".into());
    }
    let value = if value.contains(',') {
        let items: Vec<serde_json::Value> = value
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(serde_json::Value::String)
            .collect();
        serde_json::Value::Array(items)
    } else {
        serde_json::Value::String(value.trim().to_string())
    };
    out.insert(key.to_string(), value);
    Ok(())
}

#[cfg(test)]
mod install_tests {
    use super::*;

    #[test]
    fn parse_param_spec_string() {
        let mut params = std::collections::BTreeMap::new();
        parse_param_spec("start_date=2022-01-01", &mut params).unwrap();
        assert_eq!(
            params.get("start_date"),
            Some(&serde_json::Value::String("2022-01-01".into()))
        );
    }

    #[test]
    fn parse_param_spec_list() {
        let mut params = std::collections::BTreeMap::new();
        parse_param_spec("entities=NVDA,MSFT,GOOGL", &mut params).unwrap();
        match params.get("entities") {
            Some(serde_json::Value::Array(arr)) => {
                assert_eq!(arr.len(), 3);
                assert_eq!(arr[0], serde_json::Value::String("NVDA".into()));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn parse_param_spec_rejects_missing_equals() {
        let mut params = std::collections::BTreeMap::new();
        assert!(parse_param_spec("entities", &mut params).is_err());
    }

    #[test]
    fn parse_param_spec_rejects_empty_key() {
        let mut params = std::collections::BTreeMap::new();
        assert!(parse_param_spec("=NVDA", &mut params).is_err());
    }
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

/// `sovereign corpus diag <corpus_id> [--titles-file <path>]`
///
/// Audit an installed corpus's distinct-article coverage. Reads the
/// chunks table directly via `CorpusIndex::list_indexed_source_doc_ids`
/// — no daemon needed — and compares the article URL set against the
/// recipe's title filter. For Wikipedia (Vital Articles L5 Core scope)
/// this surfaces silent gaps caused by the resume-cursor bug where the
/// `committed_iter_pos` coordinate space shifted between runs as
/// `processed_shards` shrunk the assigned set.
///
/// Output: distinct articles in index, expected from filter, missing
/// titles count, plus a sample of up to 10 missing titles for spot-
/// checking. Non-zero exit when the gap exceeds 1% of the filter
/// expected size, so this is wireable into a CI / preflight check.
async fn cmd_corpus_diag(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut titles_file: Option<PathBuf> = None;
    let mut sample_size: usize = 10;
    let mut check_duplicates = false;
    let mut total_shards_override: Option<usize> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--titles-file" => {
                if let Some(p) = iter.next() {
                    titles_file = Some(PathBuf::from(p));
                } else {
                    eprintln!("--titles-file requires a path argument");
                    return 1;
                }
            }
            "--sample" => {
                if let Some(n) = iter.next() {
                    match n.parse::<usize>() {
                        Ok(v) => sample_size = v,
                        Err(_) => {
                            eprintln!("--sample requires a non-negative integer");
                            return 1;
                        }
                    }
                } else {
                    eprintln!("--sample requires an integer argument");
                    return 1;
                }
            }
            "--check-duplicates" => check_duplicates = true,
            "--total-shards" => {
                if let Some(n) = iter.next() {
                    match n.parse::<usize>() {
                        Ok(v) => total_shards_override = Some(v),
                        Err(_) => {
                            eprintln!("--total-shards requires a non-negative integer");
                            return 1;
                        }
                    }
                } else {
                    eprintln!("--total-shards requires an integer argument");
                    return 1;
                }
            }
            "--help" | "-h" => {
                println!(
                    "Usage: sovereign corpus diag <corpus_id> \
                     [--titles-file <path>] [--sample <n>] [--check-duplicates] \
                     [--total-shards <n>]\n\n\
                     Audit a corpus index against its filter title list. \
                     For wikipedia, --titles-file defaults to the bundled \
                     Vital Articles Level 5 list.\n\n\
                     --check-duplicates scans every chunk's content_hash to \
                     detect re-embedding (wasted work if a resume rewound \
                     past already-written rows). ~650MB transient RAM for a \
                     4M-chunk corpus.\n\n\
                     --total-shards overrides the meta-stored / inferred \
                     shard count when computing the missing-shards list. \
                     Useful for legacy indexes that pre-date the \
                     total_shards meta field."
                );
                return 0;
            }
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID. Usage: sovereign corpus diag <corpus_id>");
        return 1;
    };

    // Resolve the same indexes dir the daemon uses: read
    // `~/.config/sovereign/config.toml`'s `[data] dir` if present,
    // fall back to `~/.sovereign`. Diag is a read-only command so a
    // mis-resolution is recoverable by passing --titles-file later;
    // we still want it to "just work" against the live install
    // without operator config.
    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let index_dir = data_dir.join("indexes");

    // Resolve where this corpus actually lives. Three shapes are
    // valid in the wild:
    //
    // 1. **Canonical** — `<index_dir>/<corpus_id>/`. Produced by
    //    `coordinate_merge` after a queue-mode ingest finishes; this
    //    is the form peers fan-out queries to.
    // 2. **Self-partition** — `<index_dir>/<corpus_id>-partition-<self>/`.
    //    Active during ingest, and also the *terminal* state when a
    //    solo-node ingest never advances to merge (e.g. wikipedia
    //    here: 31/38 shards processed, indexes built, but no merge
    //    yet because the merge step waits on all-units-complete).
    // 3. **Peer-partition** — `<index_dir>/<corpus_id>-partition-<peer>/`.
    //    Foreign data the local node should not introspect.
    //
    // For diag we accept (1) and (2) via the file-system scan;
    // (3) is excluded by the `partition-<self>` suffix match. If
    // both exist we prefer canonical because it represents the
    // merged final state.
    let canonical_path = index_dir.join(&corpus_id);
    let (index_path, surface_label) = if canonical_path.exists() {
        (canonical_path, "canonical".to_string())
    } else if let Some((partition_path, node_id_label)) =
        find_self_partition(&index_dir, &corpus_id)
    {
        eprintln!(
            "  note: canonical `{corpus_id}/` is absent — diag is reading the self-partition\n  \
             at `{}/`. The partition contains everything ingested so far on this node;\n  \
             merging it into the canonical path is what peers (and `mesh_corpus.installed`)\n  \
             ultimately consume.\n",
            partition_path.display()
        );
        (partition_path, format!("partition-{node_id_label}"))
    } else {
        eprintln!(
            "Index not found at {} (and no self-partition either).\n  \
             Has this corpus been installed?",
            canonical_path.display()
        );
        return 1;
    };

    println!("Opening index at {} ({}) …", index_path.display(), surface_label);

    // If we're reading a partition, surface the processed-shards gap
    // up front. The whole point of diag is to answer "is this corpus
    // complete?" — the partition's `_corpus_meta.json` already tracks
    // this so we don't have to wait for the title-list comparison
    // below to discover an obvious gap.
    if surface_label.starts_with("partition-") {
        if let Some(shard_summary) =
            processed_shards_summary(&index_path, total_shards_override)
        {
            println!("  shard coverage: {shard_summary}");
        }
    }

    let index = match corpus_engine::CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open corpus index: {e}");
            return 1;
        }
    };

    let chunk_count = index.chunk_count().await.unwrap_or(0);
    println!("  chunks in table: {chunk_count}");

    println!("Scanning distinct source_doc_ids (this reads the full URL column)…");
    let indexed_ids = match index.list_indexed_source_doc_ids().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to list distinct source_doc_ids: {e}");
            return 1;
        }
    };

    // The Wikipedia extractor emits one ExtractedDoc per article
    // SECTION, not per article. So distinct source_doc_id URLs count
    // sections (and section URLs may include `#fragment` suffixes
    // from the streaming chunker). Strip the URL down to a normalized
    // article title so the comparison against `vital_articles_l5` is
    // honest — and report both numbers so an operator can spot the
    // distinction.
    let mut indexed_titles: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(indexed_ids.len());
    for id in &indexed_ids {
        let title = corpus_engine::extractors::wikipedia_types::wiki_title_from_url(id)
            .unwrap_or_else(|| id.clone());
        indexed_titles.insert(corpus_engine::filters::normalize_title(&title));
    }
    println!(
        "  distinct source_doc_id URLs (sections + fragments): {}",
        indexed_ids.len()
    );
    println!(
        "  distinct articles after url→title normalize:        {}",
        indexed_titles.len()
    );
    if !indexed_ids.is_empty() && !indexed_titles.is_empty() {
        let ratio = indexed_ids.len() as f64 / indexed_titles.len() as f64;
        println!(
            "  avg sections per article: {ratio:.1} \
             (Wikipedia-typical: 5–20, anomalously high suggests duplicate ingest)"
        );
    }
    if chunk_count > 0 && !indexed_ids.is_empty() {
        let cps = chunk_count as f64 / indexed_ids.len() as f64;
        println!(
            "  avg chunks per section:   {cps:.2} \
             (paragraph-chunked at 1024 chars; expect 1–10)"
        );
    }

    if check_duplicates {
        println!("Counting distinct content_hashes (this scans every chunk row)…");
        match index.count_distinct_content_hashes().await {
            Ok((distinct, with_hash, total)) => {
                println!("  total chunks:             {total}");
                println!("  with content_hash set:    {with_hash}");
                println!("  distinct content_hashes:  {distinct}");
                let hashless = total.saturating_sub(with_hash);
                if hashless > 0 {
                    println!(
                        "  hashless (legacy) rows:   {hashless} \
                         (predates content_hash population; cannot dedup-check these)"
                    );
                }
                if with_hash > 0 {
                    let dup = with_hash.saturating_sub(distinct);
                    if dup == 0 {
                        println!(
                            "  ✓ no duplicate chunks detected — embed-once invariant holds \
                             across the {with_hash} hashed rows."
                        );
                    } else {
                        let pct = dup as f64 / with_hash as f64 * 100.0;
                        println!(
                            "  ⚠ {dup} duplicate chunk rows ({pct:.2}% of hashed rows) — \
                             some chunks were embedded more than once. Likely cause: \
                             resume rewound the cursor past already-written rows."
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("  failed to count distinct content_hashes: {e}");
            }
        }
    }


    // Decide which title list to compare against. For wikipedia we
    // default to the bundled VITAL_ARTICLES_L5; --titles-file overrides.
    let (expected_titles, source_label) = match (titles_file.as_deref(), corpus_id.as_str()) {
        (Some(path), _) => {
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Failed to read --titles-file {}: {e}", path.display());
                    return 1;
                }
            };
            (load_title_set(&bytes), format!("{}", path.display()))
        }
        (None, "wikipedia") => (
            load_title_set(corpus_engine::filters::assets::VITAL_ARTICLES_L5),
            "bundled vital_articles_l5".to_string(),
        ),
        (None, _) => {
            println!(
                "\nNo title list specified and no default for corpus '{corpus_id}'. \
                 Pass --titles-file to compare against an expected set."
            );
            return 0;
        }
    };

    let expected_count = expected_titles.len();
    let intersect = indexed_titles.intersection(&expected_titles).count();
    let missing: Vec<&String> =
        expected_titles.difference(&indexed_titles).collect();
    let unexpected: Vec<&String> =
        indexed_titles.difference(&expected_titles).collect();

    println!("\nFilter list: {source_label}");
    println!("  titles in list:           {expected_count}");
    println!("  in list ∩ in index:       {intersect}");
    println!(
        "  in list, missing in index: {} ({:.2}%)",
        missing.len(),
        if expected_count > 0 {
            100.0 * missing.len() as f64 / expected_count as f64
        } else {
            0.0
        }
    );
    println!(
        "  in index, not in list:    {} (likely redirect / normalisation drift)",
        unexpected.len()
    );

    if sample_size > 0 && !missing.is_empty() {
        println!("\nSample of missing titles (up to {sample_size}):");
        let mut sorted_missing: Vec<&String> = missing.iter().copied().collect();
        sorted_missing.sort();
        for t in sorted_missing.iter().take(sample_size) {
            println!("  • {t}");
        }
    }
    if sample_size > 0 && !unexpected.is_empty() {
        println!("\nSample of unexpected titles (up to {sample_size}):");
        let mut sorted_unexpected: Vec<&String> = unexpected.iter().copied().collect();
        sorted_unexpected.sort();
        for t in sorted_unexpected.iter().take(sample_size) {
            println!("  • {t}");
        }
    }

    // Exit non-zero if the gap is material. 1% threshold is arbitrary
    // but above the noise floor for L5 normalization quirks (a few
    // dozen titles shift between curator pulls).
    let gap_pct = if expected_count > 0 {
        100.0 * missing.len() as f64 / expected_count as f64
    } else {
        0.0
    };
    if gap_pct > 1.0 {
        eprintln!(
            "\n⚠ Material gap detected: {} titles missing ({:.2}%). \
             This may indicate the resume-cursor coordinate-space bug \
             — see plan to re-ingest with shard-set-drift fix.",
            missing.len(),
            gap_pct
        );
        return 2;
    }

    0
}

/// Parse a newline-delimited title list (the same format
/// `TitleListFilter::from_bytes` accepts) into a normalized
/// `HashSet<String>`. Comments (`#…`) and blank lines are skipped.
fn load_title_set(bytes: &[u8]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in bytes.split(|&b| b == b'\n') {
        let line = std::str::from_utf8(line).unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.insert(corpus_engine::filters::normalize_title(line));
    }
    out
}

/// `sovereign corpus dedupe <corpus_id> [--yes]`
///
/// Run the one-shot rescue pass on an installed corpus: collapse
/// duplicate-content rows (same `content_hash`) so the index reflects
/// actual unique work. The cause this exists for: a resume-cursor-
/// rewind bug that re-embedded already-written content during a
/// long-running ingest, leaving up to ~65% of chunks as exact
/// duplicates of older rows. Reclaims disk and unblocks the
/// subsequent `build_indexes()` (which now runs a dedupe prelude
/// automatically — this command exists for partitions that already
/// completed their build over duplicated data, before the auto-dedup
/// landed).
///
/// Resolves both canonical and self-partition paths (mirrors
/// `corpus diag`'s resolution). Prints before/after counts and
/// duplication rate. `--yes` skips the y/N confirmation; default is
/// to confirm because the operation deletes rows.
async fn cmd_corpus_dedupe(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut yes = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            "--help" | "-h" => {
                println!(
                    "Usage: sovereign corpus dedupe <corpus_id> [--yes]\n\n\
                     Collapse duplicate-content rows in an installed corpus. \
                     Detected via the chunk's content_hash. Hashless legacy \
                     rows are preserved (no signal to compare). Resolves \
                     both canonical (<index_dir>/<corpus>/) and self-\
                     partition (<index_dir>/<corpus>-partition-<self>/) \
                     paths."
                );
                return 0;
            }
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID. Usage: sovereign corpus dedupe <corpus_id>");
        return 1;
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let index_dir = data_dir.join("indexes");

    // Same resolution as diag — canonical first, then self-partition.
    let canonical_path = index_dir.join(&corpus_id);
    let (index_path, surface_label) = if canonical_path.exists() {
        (canonical_path, "canonical".to_string())
    } else if let Some((partition_path, node_id_label)) =
        find_self_partition(&index_dir, &corpus_id)
    {
        (partition_path, format!("partition-{node_id_label}"))
    } else {
        eprintln!(
            "Index not found at {} (and no self-partition either).",
            canonical_path.display()
        );
        return 1;
    };

    println!("Opening index at {} ({})…", index_path.display(), surface_label);
    let index = match corpus_engine::CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to open corpus index: {e}");
            return 1;
        }
    };

    // Show the user what we're about to do BEFORE the destructive
    // call. The count_distinct_content_hashes scan is the same one
    // dedupe runs internally, but cheap enough to repeat — the
    // delete pass is the load-bearing part.
    println!("Scanning content_hashes (full table read)…");
    let (distinct, with_hash, total) = match index.count_distinct_content_hashes().await {
        Ok(triple) => triple,
        Err(e) => {
            eprintln!("Failed to count content_hashes: {e}");
            return 1;
        }
    };
    let dup_rows = with_hash.saturating_sub(distinct);
    let dup_pct = if with_hash > 0 {
        dup_rows as f64 / with_hash as f64 * 100.0
    } else {
        0.0
    };
    println!("  total chunks:             {total}");
    println!("  with content_hash set:    {with_hash}");
    println!("  distinct content_hashes:  {distinct}");
    println!("  duplicates to delete:     {dup_rows} ({dup_pct:.2}% of hashed)");

    if dup_rows == 0 {
        println!("\n✓ Nothing to do — index already deduped.");
        return 0;
    }

    if !yes {
        eprint!(
            "\nAbout to delete {dup_rows} duplicate row(s) from {}.\n\
             Existing chunk_ids will be preserved for the surviving (lowest-id) \
             row in each group. Vector + FTS indexes remain valid.\n\
             Proceed? [y/N] ",
            index_path.display()
        );
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            eprintln!("aborted (could not read stdin)");
            return 1;
        }
        let answer = line.trim().to_lowercase();
        if answer != "y" && answer != "yes" {
            println!("aborted.");
            return 0;
        }
    }

    println!("\nRunning dedupe…");
    match index.dedupe_by_content_hash().await {
        Ok(report) => {
            println!("  rows before:              {}", report.rows_before);
            println!("  rows after:               {}", report.rows_after);
            println!("  duplicates deleted:       {}", report.duplicates_deleted);
            println!(
                "  unique hashes preserved:  {}",
                report.unique_hashes_kept
            );
            println!(
                "  hashless rows preserved:  {}",
                report.hashless_rows_preserved
            );
            println!(
                "\n✓ Dedupe complete ({:.2}% duplication eliminated).",
                report.dup_fraction() * 100.0
            );
            0
        }
        Err(e) => {
            eprintln!("Dedupe failed: {e}");
            1
        }
    }
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

/// Migrate a pre-unified canonical index into a partition-of-self
/// dir so the daemon's auto-collaborate loop will pick it up and
/// participate in collaborative ingest alongside peers.
///
/// Before Layer 1's unified-ingest primitive, `engine.ingest()`
/// wrote directly into `<index_dir>/<corpus_id>/`. New code writes
/// into `<index_dir>/<corpus_id>-partition-<self_node_id>/` and
/// promotes to canonical via `finalise_solo_ingest` or
/// `coordinate_merge`. A user mid-ingest when they upgraded has a
/// populated canonical and no partition-of-self — so auto_ingest
/// skips spawning local work for them (`partition_path.exists()`
/// is false), and `coordinate_merge` from a peer would collide on
/// the output path.
///
/// This subcommand is the one-shot fix: it renames the canonical
/// into the partition-of-self path and rewrites the meta so the
/// new code treats it as "this node's share of a collaborative
/// ingest in progress". No data is copied; the `chunks.lance`
/// table is preserved verbatim.
async fn cmd_corpus_migrate_to_partition(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut dry_run = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                eprintln!(
                    "Usage: sovereign corpus migrate-to-partition <corpus_id> [--dry-run]\n\
                     \n\
                     Renames ~/.sovereign/indexes/<id>/ to\n\
                     ~/.sovereign/indexes/<id>-partition-<self_node_id>/ and\n\
                     flips the meta to partition shape so the daemon's\n\
                     auto-collaborate loop will resume the ingest and\n\
                     peers can participate.\n\
                     \n\
                     The canonical must have ingestion_in_progress=true\n\
                     (otherwise there's nothing to resume). Partition-of-self\n\
                     must not already exist."
                );
                return 0;
            }
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                eprintln!("Usage: sovereign corpus migrate-to-partition <corpus_id> [--dry-run]");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID");
        eprintln!("Usage: sovereign corpus migrate-to-partition <corpus_id> [--dry-run]");
        return 1;
    };

    // Resolve data_dir from the setup config so we read mesh.json
    // + indexes from exactly the same place the running daemon does.
    // Using `mesh_data_dir()` (platform data dir) would work for a
    // Desktop-only deployment but not for CLI-daemon setups where
    // `config.data.dir` commonly points at `~/.sovereign/`.
    let config = match sovereign_core::setup_config::SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Failed to load setup config ({e}).\n\
                 Run `sovereign setup` first so the migration knows which\n\
                 data_dir your daemon uses."
            );
            return 1;
        }
    };
    let data_dir = config.data.dir.clone();

    // Load the self_node_id the daemon uses so the partition path
    // matches. Prefer the explicit `<data_dir>/node_id` file; fall
    // back to mesh.json's `self_node_id` for deployments that never
    // materialised the separate file (the common path — the daemon
    // only writes node_id when it generates a fresh one, and existing
    // meshes carry the ID inside mesh.json).
    let self_node_id = match sovereign_mesh::persist::load_node_id(&data_dir) {
        Ok(Some(id)) => id,
        _ => match sovereign_mesh::persist::load(&data_dir) {
            Ok(Some(persisted)) => persisted.self_node_id,
            Ok(None) => {
                eprintln!(
                    "No mesh state at {} — run `sovereign mesh create` or\n\
                     `sovereign mesh join …` before migrating a corpus so the\n\
                     daemon has a stable node id.",
                    data_dir.display()
                );
                return 1;
            }
            Err(e) => {
                eprintln!("Failed to load mesh state from {}: {e}", data_dir.display());
                return 1;
            }
        },
    };
    let self_node_id_str = self_node_id.to_string();

    let index_dir = data_dir.join("indexes");
    let canonical = index_dir.join(&corpus_id);
    let partition = index_dir.join(format!("{corpus_id}-partition-{self_node_id_str}"));

    println!();
    println!("Migration plan for '{corpus_id}':");
    println!("  Canonical : {}", canonical.display());
    println!("  Partition : {}", partition.display());
    println!("  Node id   : {self_node_id_str}");

    if dry_run {
        println!();
        println!("Dry run — no changes made. Re-run without --dry-run to apply.");
        return 0;
    }

    // Engine just needs the directories + a no-op embed for this
    // file-moving operation; ingestion won't run during migration.
    let recipes_dir = data_dir.join("recipes");
    let noop_embed: corpus_engine::EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok(vec![0.0_f32; 0]) })
    });
    let engine = CorpusEngine::new(recipes_dir, index_dir, noop_embed)
        .with_self_node_id(self_node_id_str.clone());

    match engine.migrate_canonical_to_partition(&corpus_id) {
        Ok(new_path) => {
            println!();
            println!("✓ Migration complete. New partition-of-self: {}", new_path.display());
            println!();
            println!("Next steps:");
            println!(
                "  - If the daemon is running, its auto-collaborate loop will\n\
                   pick up the partition within 30 s and resume ingest."
            );
            println!(
                "  - If the daemon is not running, start it with `sovereign daemon start`\n\
                   (or reopen Sovereign Desktop)."
            );
            0
        }
        Err(e) => {
            eprintln!();
            eprintln!("Migration failed: {e}");
            1
        }
    }
}

// ── Helpers ──────────────────────────────────────────────

/// Locate `<index_dir>/<corpus_id>-partition-<self>/` when canonical
/// is absent. Returns the path and the truncated node-id label for
/// human-friendly logging.
///
/// We don't have direct access to the daemon's `self_node_id` from
/// the CLI (the cli is decoupled from any live mesh state — it can
/// run before the daemon does), so the "self" partition is
/// identified positively: scan the indexes dir for any directory
/// matching `<corpus_id>-partition-<NODE_HEX>` and prefer the one
/// where `_corpus_meta.json.indexes_built == true`. That's a
/// pragmatic stand-in for "the partition this machine actually
/// finished writing to" — peer-pulled partitions for OTHER nodes
/// have `indexes_built: false` until coordinate_merge promotes
/// them, so we don't accidentally read a peer's partial download.
fn find_self_partition(
    index_dir: &std::path::Path,
    corpus_id: &str,
) -> Option<(PathBuf, String)> {
    let prefix = format!("{corpus_id}-partition-");
    let mut best: Option<(PathBuf, String, bool)> = None;
    let entries = std::fs::read_dir(index_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        let Some(suffix) = name_str.strip_prefix(&prefix) else { continue };
        let meta_path = path.join("_corpus_meta.json");
        let Ok(content) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let built = meta["indexes_built"].as_bool().unwrap_or(false);
        // Prefer a built partition; if none built, fall back to any.
        match &best {
            Some((_, _, prior_built)) if *prior_built && !built => continue,
            _ => {
                best = Some((path, suffix.to_string(), built));
            }
        }
    }
    best.map(|(path, label, _)| (path, label))
}

/// Read the `processed_shards` array out of a partition's
/// `_corpus_meta.json` and produce a one-line summary.
///
/// Total-shard resolution priority:
/// 1. `--total-shards N` override (caller-supplied).
/// 2. `total_shards` field in `_corpus_meta.json` (stamped by the
///    extractor at ingest start; authoritative when present).
/// 3. `max(processed_shards) + 1` heuristic (legacy fallback;
///    silently undercounts trailing-missing shards — surface that
///    caveat in the output so operators don't trust it blindly).
///
/// Returns `None` only when there's no `processed_shards` array at
/// all (older schema or non-sharded corpus).
fn processed_shards_summary(
    index_path: &std::path::Path,
    total_override: Option<usize>,
) -> Option<String> {
    let meta = std::fs::read_to_string(index_path.join("_corpus_meta.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&meta).ok()?;
    let processed: Vec<u64> = v["processed_shards"]
        .as_array()?
        .iter()
        .filter_map(|x| x.as_u64())
        .collect();
    if processed.is_empty() && total_override.is_none() {
        return Some(
            "processed_shards present but empty (no shards finalized)".to_string(),
        );
    }
    let max_idx = processed.iter().copied().max().unwrap_or(0);
    let processed_set: std::collections::HashSet<u64> =
        processed.iter().copied().collect();

    // Resolve total shards via the priority chain.
    let total_meta = v["total_shards"].as_u64().map(|n| n as usize);
    let (total_inferred, total_source) = match (total_override, total_meta) {
        (Some(n), _) => (n, "--total-shards override"),
        (None, Some(n)) => (n, "stamped at extract start"),
        (None, None) => ((max_idx + 1) as usize, "inferred from max(processed)+1"),
    };

    let total = total_inferred as u64;
    let missing: Vec<u64> = (0..total)
        .filter(|i| !processed_set.contains(i))
        .collect();

    let trailing_caveat = matches!(
        total_source,
        "inferred from max(processed)+1"
    );

    if missing.is_empty() {
        Some(format!(
            "{} of {} shards processed (source: {total_source}; none missing)",
            processed.len(),
            total,
        ))
    } else {
        let preview: Vec<String> =
            missing.iter().take(8).map(|n| n.to_string()).collect();
        let suffix = if missing.len() > 8 {
            format!(" + {} more", missing.len() - 8)
        } else {
            String::new()
        };
        let caveat = if trailing_caveat {
            " (heuristic; trailing shards beyond max_idx may also be missing — \
             check daemon.log for `assigned … real shards` or pass --total-shards N)"
        } else {
            ""
        };
        Some(format!(
            "{} of {} shards processed (source: {total_source}); \
             missing: [{}]{}{caveat}",
            processed.len(),
            total,
            preview.join(", "),
            suffix,
        ))
    }
}

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
