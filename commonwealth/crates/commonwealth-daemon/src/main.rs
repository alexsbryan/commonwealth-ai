// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use corpus_engine::{CorpusEngine, EmbedFn, TestOptions};
use tracing::info;

use commonwealth_app::manifest::{AppPermissions, MeshAppManifest, RequiredCapabilities};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::config::{DaemonConfig, InferenceConfig};
use commonwealth_discovery::membership;
use commonwealth_state::{MeshStore, RetentionGc};

#[derive(Parser)]
#[command(
    name = "commonwealth",
    about = "Coordination daemon for community-owned distributed inference and knowledge",
    version
)]
struct Cli {
    /// Path to config file. Defaults to ~/.commonwealth/config.toml
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new mesh
    Init {
        /// Human-readable mesh name
        #[arg(long)]
        name: String,
    },
    /// Join an existing mesh
    Join {
        /// Join key shared by a mesh member
        key: String,
        /// Addresses to advertise to the mesh
        #[arg(long)]
        address: Vec<String>,
    },
    /// Show mesh status
    Status,
    /// Show contribution balance
    Balance,
    /// Pause this node (graceful departure)
    Pause,
    /// Resume this node after pause
    Resume,
    /// Permanently leave the mesh
    Leave,
    /// List available and loaded models
    Models,
    /// Manage knowledge corpora
    Corpus {
        #[command(subcommand)]
        command: CorpusCommands,
    },
    /// Show daemon logs
    Logs {
        /// Follow log output
        #[arg(long, short)]
        follow: bool,
    },
    /// Mesh management commands
    Mesh {
        #[command(subcommand)]
        command: MeshCommands,
    },
    /// Daemon lifecycle management
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    /// Test or validate a community recipe file
    Recipe {
        #[command(subcommand)]
        command: RecipeCommands,
    },
    /// Manage per-peer affinity preferences (Ostrom sanctions).
    ///
    /// Lets the operator privately tell their daemon to advertise
    /// reduced affinity to a specific peer — a local-only,
    /// reversible adjustment that the manifest endpoint applies on
    /// every fetch. Never gossiped, never visible to the
    /// penalized peer as a distinct signal.
    PeerPreference {
        #[command(subcommand)]
        command: PeerPreferenceCommands,
    },
}

#[derive(Subcommand)]
enum PeerPreferenceCommands {
    /// Set a preference. Multiplier is `0.5` (=50%) or `50%`.
    Set {
        /// Peer node id (32-hex-char form) or name.
        node: String,
        /// Multiplier in `(0.0, 1.0]`. Accepts `0.5` or `50%`.
        multiplier: String,
        /// Optional local-only annotation.
        #[arg(long)]
        reason: Option<String>,
    },
    /// List all current preferences.
    List,
    /// Clear a peer's preference.
    Clear {
        /// Peer node id (32-hex-char form) or name.
        node: String,
    },
}

#[derive(Subcommand)]
enum CorpusCommands {
    /// List installed and available corpora
    List,
    /// Install a corpus
    Install {
        /// Corpus ID (e.g., "wikipedia")
        id: String,
        /// Install from a recipe file
        #[arg(long)]
        recipe: Option<PathBuf>,
    },
    /// Remove an installed corpus
    Remove { id: String },
    /// Check for corpus updates
    Update { id: String },
    /// Show shard status for all corpora
    Status,
    /// Merge shard files into a complete index
    Consolidate { id: String },
    /// Recruit mesh peers to share a mid-flight ingestion.
    ///
    /// Requires a source-file manifest (`sovereign corpus
    /// reconstruct-manifest <id>`).  Divides the remaining parquet
    /// files across compatible peers and prints the partition plan.
    Collaborate {
        /// Corpus ID (e.g., "wikipedia")
        id: String,
        /// Recipe ID if different from corpus ID
        #[arg(long)]
        recipe: Option<String>,
    },
    /// Monitor collaborative ingestion progress
    CollaborateStatus {
        /// Corpus ID to monitor
        id: String,
    },
}

#[derive(Subcommand)]
enum MeshCommands {
    /// Propose a mesh-wide config change
    Set { key: String, value: String },
    /// List members with status
    Members,
    /// Propose revoking a member
    Revoke {
        /// Node ID or name to revoke
        node: String,
    },
    /// Establish peering with another mesh
    Peer {
        /// Peering key shared by the other mesh
        key: String,
    },
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Start the daemon
    Start,
}

#[derive(Subcommand)]
enum RecipeCommands {
    /// Run the full test harness against a recipe file
    Test {
        /// Path to the recipe.toml file
        path: PathBuf,
        /// Number of source records to sample
        #[arg(long, default_value = "100")]
        sample_size: usize,
        /// Skip the embedding and search test
        #[arg(long)]
        no_embed: bool,
        /// Where to write the report (default: <recipe_dir>/TEST_REPORT.md)
        #[arg(long)]
        output: Option<PathBuf>,
        /// Skip the source URL reachability check
        #[arg(long)]
        offline: bool,
        /// Print per-record extraction outcome
        #[arg(long, short)]
        verbose: bool,
    },
    /// Validate a recipe's fields without downloading data
    Validate {
        /// Path to the recipe.toml file
        path: PathBuf,
        /// Skip the source URL reachability check
        #[arg(long)]
        offline: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Load config if it exists.
    let config = load_config(cli.config.as_deref())?;

    match cli.command {
        Commands::Init { name } => cmd_init(&name, &config),
        Commands::Join { key, address } => cmd_join(&key, &address, &config),
        Commands::Status => cmd_status(&config),
        Commands::Balance => cmd_balance(&config),
        Commands::Pause => cmd_pause(&config),
        Commands::Resume => cmd_resume(&config),
        Commands::Leave => cmd_leave(&config),
        Commands::Models => cmd_models(&config),
        Commands::Corpus { command } => match command {
            CorpusCommands::List => cmd_corpus_list(&config),
            CorpusCommands::Install { id, recipe } => {
                cmd_corpus_install(&id, recipe.as_deref(), &config)
            }
            CorpusCommands::Remove { id } => cmd_corpus_remove(&id, &config),
            CorpusCommands::Update { id } => cmd_corpus_update(&id, &config),
            CorpusCommands::Status => cmd_corpus_status(&config),
            CorpusCommands::Consolidate { id } => cmd_corpus_consolidate(&id, &config),
            CorpusCommands::Collaborate { id, recipe } => {
                cmd_corpus_collaborate(&id, recipe.as_deref(), &config)
            }
            CorpusCommands::CollaborateStatus { id } => cmd_corpus_collaborate_status(&id, &config),
        },
        Commands::Logs { follow } => cmd_logs(follow, &config),
        Commands::Mesh { command } => match command {
            MeshCommands::Set { key, value } => cmd_mesh_set(&key, &value, &config),
            MeshCommands::Members => cmd_mesh_members(&config),
            MeshCommands::Revoke { node } => cmd_mesh_revoke(&node, &config),
            MeshCommands::Peer { key } => cmd_mesh_peer(&key, &config),
        },
        Commands::Daemon { command } => match command {
            DaemonCommands::Start => cmd_daemon_start(&config),
        },
        Commands::Recipe { command } => match command {
            RecipeCommands::Test {
                path,
                sample_size,
                no_embed,
                output,
                offline,
                verbose,
            } => cmd_recipe_test(
                &path,
                sample_size,
                !no_embed,
                output.as_deref(),
                offline,
                verbose,
            ),
            RecipeCommands::Validate { path, offline } => cmd_recipe_validate(&path, offline),
        },
        Commands::PeerPreference { command } => match command {
            PeerPreferenceCommands::Set {
                node,
                multiplier,
                reason,
            } => cmd_peer_preference_set(&node, &multiplier, reason.as_deref()),
            PeerPreferenceCommands::List => cmd_peer_preference_list(),
            PeerPreferenceCommands::Clear { node } => cmd_peer_preference_clear(&node),
        },
    }
}

fn load_config(path: Option<&std::path::Path>) -> Result<Option<DaemonConfig>> {
    let config_path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_path().join("config.toml"));

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read config from {}", config_path.display()))?;
        let config: DaemonConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse config from {}", config_path.display()))?;
        Ok(Some(config))
    } else {
        Ok(None)
    }
}

fn dirs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".commonwealth")
}

// ============================================================================
// Command implementations
// ============================================================================

fn cmd_init(name: &str, config: &Option<DaemonConfig>) -> Result<()> {
    let node_name = config
        .as_ref()
        .map(|c| c.node.name.clone())
        .unwrap_or_else(|| {
            hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "commonwealth-node".into())
        });

    let internal_port = config
        .as_ref()
        .map(|c| c.node.internal_port)
        .unwrap_or(9742);

    let addr: SocketAddr = format!("0.0.0.0:{internal_port}").parse()?;
    let (mesh, join_key) = membership::init_mesh(name, &node_name, vec![addr]);

    println!();
    println!("Mesh created: {name}");
    println!("Join key: {join_key}");
    println!();
    println!("Share this key with people you want in the mesh.");
    println!("They run: commonwealth join {join_key}");
    println!();

    // Save mesh state.
    let data_dir = config
        .as_ref()
        .map(|c| c.node.data_dir.clone())
        .unwrap_or_else(|| "~/.commonwealth".into())
        .replace('~', &std::env::var("HOME").unwrap_or_default());

    std::fs::create_dir_all(&data_dir)?;
    // Serialize mesh state. Note: HashMap<NodeId, _> can't directly serialize
    // to JSON map keys, so we save as a list of members alongside mesh metadata.
    let save_data = serde_json::json!({
        "id": mesh.id,
        "name": mesh.name,
        "members": mesh.members.values().collect::<Vec<_>>(),
        "peers": mesh.peers,
    });
    let mesh_json = serde_json::to_string_pretty(&save_data)?;
    std::fs::write(PathBuf::from(&data_dir).join("mesh.json"), mesh_json)?;

    info!(mesh_name = name, "mesh initialized");
    Ok(())
}

fn cmd_join(key: &str, _addresses: &[String], _config: &Option<DaemonConfig>) -> Result<()> {
    membership::validate_join_key_format(key)
        .map_err(|e| anyhow::anyhow!("Invalid join key: {e}"))?;

    println!("Joining mesh with key: {key}");
    println!("(In production, this would contact a mesh member to complete the join handshake.)");

    Ok(())
}

fn cmd_status(config: &Option<DaemonConfig>) -> Result<()> {
    let api_port = config.as_ref().map(|c| c.node.api_port).unwrap_or(9741);

    println!("Checking daemon at localhost:{api_port}...");
    println!("(In production, this would GET http://localhost:{api_port}/status and display the result.)");

    Ok(())
}

fn cmd_balance(_config: &Option<DaemonConfig>) -> Result<()> {
    use commonwealth_state::{current_contributions, MeshStore};
    use std::collections::HashMap;

    let store_path = dirs_path().join("store.db");
    let store = match MeshStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            println!("Contribution Ledger");
            println!("{}", "─".repeat(60));
            println!(
                "(no store at {} — daemon hasn't run yet: {e})",
                store_path.display()
            );
            return Ok(());
        }
    };

    let contribs = current_contributions(
        &store,
        &HashMap::new(),
        commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
    )
    .with_context(|| "reading contribution ledger from store")?;

    if contribs.is_empty() {
        println!("Contribution Ledger (last 30 days)");
        println!("{}", "─".repeat(72));
        println!("(no events recorded yet)");
        return Ok(());
    }

    println!("Contribution Ledger (last 30 days)");
    println!("{}", "─".repeat(72));
    println!(
        "{:<20} {:>22} {:>14} {:>12}",
        "Node", "Inference (s/r)", "Knowledge", "Network (s/r)"
    );
    println!("{}", "─".repeat(72));
    let mut entries: Vec<_> = contribs.iter().collect();
    // Stable display ordering — sort by hex prefix of NodeId so two
    // operators reading on different nodes see the same row order.
    entries.sort_by_key(|(id, _)| id.as_bytes().to_vec());
    for (node_id, c) in entries {
        let node_label = short_node_id(node_id);
        let served = c.inference_served.requests;
        let received = c.inference_consumed.requests;
        let corpora = c.corpora_hosted.len();
        let queries: u64 = c.corpora_hosted.iter().map(|h| h.queries_served).sum();
        let gb_served = c.bytes_served as f64 / 1e9;
        let gb_received = c.bytes_received as f64 / 1e9;
        println!(
            "{:<20} {:>10} / {:<8} {:>3} corp/{:>4} q  {:>5.1} / {:>5.1} GB",
            node_label, served, received, corpora, queries, gb_served, gb_received,
        );
        // Sole-host annotations, one per line under the row.
        for hosting in &c.corpora_hosted {
            if hosting.is_sole_host {
                println!(
                    "  {:<18} sole host of {} ({:.1} GB)",
                    "", hosting.corpus_id, hosting.size_gb,
                );
            }
        }
    }
    println!("{}", "─".repeat(72));
    println!(
        "(units are dimensional — compute time, storage, and bandwidth are not collapsed into a single score)"
    );
    Ok(())
}

/// 12-hex-char prefix for human-friendly display.
fn short_node_id(id: &commonwealth_core::ids::NodeId) -> String {
    id.as_bytes()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ============================================================================
// Peer-preference commands
// ============================================================================

fn cmd_peer_preference_set(node: &str, multiplier_arg: &str, reason: Option<&str>) -> Result<()> {
    use commonwealth_state::{MeshStore, PeerPreference, PeerPreferenceStore};

    let multiplier = parse_multiplier(multiplier_arg)?;
    let pref = PeerPreference::new(multiplier, reason.map(|s| s.to_string()))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let target = parse_peer_node_id(node)?;

    let store = open_local_store()?;
    let prefs = PeerPreferenceStore::new(store, fallback_self_id());
    prefs
        .set(&target, pref.clone())
        .with_context(|| format!("setting preference for {node}"))?;
    println!(
        "set peer preference: {} → {:.0}%{}",
        node,
        multiplier * 100.0,
        reason
            .map(|r| format!(" (reason: {r})"))
            .unwrap_or_default()
    );
    let _ = prefs; // silence the unused-import lint when the binary is stripped
    let _ = MeshStore::in_memory; // ditto
    Ok(())
}

fn cmd_peer_preference_list() -> Result<()> {
    use commonwealth_state::PeerPreferenceStore;
    let store = open_local_store()?;
    let prefs = PeerPreferenceStore::new(store, fallback_self_id());
    let entries = prefs.list().with_context(|| "listing peer preferences")?;
    if entries.is_empty() {
        println!("(no peer preferences set)");
        return Ok(());
    }
    println!("{:<32} {:>10}  Reason", "Peer node id", "Multiplier");
    println!("{}", "─".repeat(64));
    for (id, pref) in entries {
        let id_hex: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        println!(
            "{:<32} {:>9.0}%  {}",
            id_hex,
            pref.multiplier() * 100.0,
            pref.reason().unwrap_or("—"),
        );
    }
    Ok(())
}

fn cmd_peer_preference_clear(node: &str) -> Result<()> {
    use commonwealth_state::PeerPreferenceStore;
    let target = parse_peer_node_id(node)?;
    let store = open_local_store()?;
    let prefs = PeerPreferenceStore::new(store, fallback_self_id());
    let removed = prefs
        .clear(&target)
        .with_context(|| format!("clearing preference for {node}"))?;
    if removed {
        println!("cleared preference for {node}");
    } else {
        println!("no preference to clear for {node}");
    }
    Ok(())
}

fn parse_multiplier(s: &str) -> Result<f64> {
    let trimmed = s.trim();
    let (number, divisor) = if let Some(stripped) = trimmed.strip_suffix('%') {
        (stripped, 100.0_f64)
    } else {
        (trimmed, 1.0_f64)
    };
    let parsed: f64 = number.parse().with_context(|| {
        format!("multiplier must be a decimal (0.5) or percentage (50%), got '{s}'")
    })?;
    Ok(parsed / divisor)
}

fn parse_peer_node_id(s: &str) -> Result<commonwealth_core::ids::NodeId> {
    let trimmed = s.trim();
    if trimmed.len() != 32 {
        anyhow::bail!(
            "expected 32-hex-char node id, got '{}' ({} chars). \
             Use `commonwealth mesh members` to find peer ids.",
            trimmed,
            trimmed.len()
        );
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        let pair = trimmed
            .get(i * 2..i * 2 + 2)
            .with_context(|| format!("invalid hex id '{trimmed}'"))?;
        *b = u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex id '{trimmed}'"))?;
    }
    Ok(commonwealth_core::ids::NodeId::from_u128(
        u128::from_be_bytes(bytes),
    ))
}

fn open_local_store() -> Result<commonwealth_state::MeshStore> {
    let store_path = dirs_path().join("store.db");
    commonwealth_state::MeshStore::open(&store_path)
        .with_context(|| format!("opening local store at {}", store_path.display()))
}

/// CLI subcommands operate on the local store directly without
/// going through the running daemon. Without a daemon there's no
/// authoritative `self_node_id`, but `PeerPreferenceStore::set`
/// only uses it as the gossip-origin (and gossip is excluded for
/// this namespace anyway). A zeroed sentinel is fine.
fn fallback_self_id() -> commonwealth_core::ids::NodeId {
    commonwealth_core::ids::NodeId::from_u128(0)
}

fn cmd_pause(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("Announcing departure to mesh...");
    println!("(In production, this would initiate a graceful departure with 30s countdown.)");
    println!("Your node is paused. Run 'commonwealth resume' to rejoin.");
    Ok(())
}

fn cmd_resume(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("Resuming node...");
    println!("(In production, this would announce the node's return to the mesh.)");
    Ok(())
}

fn cmd_leave(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("Leaving mesh permanently...");
    println!("(In production, this would initiate graceful departure, then remove membership.)");
    Ok(())
}

fn cmd_models(config: &Option<DaemonConfig>) -> Result<()> {
    let api_port = config.as_ref().map(|c| c.node.api_port).unwrap_or(9741);
    println!("(In production, this would GET http://localhost:{api_port}/v1/models.)");
    Ok(())
}

fn cmd_corpus_list(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("(In production, this would list installed and available knowledge corpora.)");
    Ok(())
}

fn cmd_corpus_install(
    id: &str,
    recipe: Option<&std::path::Path>,
    _config: &Option<DaemonConfig>,
) -> Result<()> {
    if let Some(path) = recipe {
        println!("Installing corpus '{id}' from recipe: {}", path.display());
    } else {
        println!("Installing builtin corpus '{id}'...");
    }
    println!("(In production, this would ingest the corpus via corpus-engine.)");
    Ok(())
}

fn cmd_corpus_remove(id: &str, _config: &Option<DaemonConfig>) -> Result<()> {
    println!("Removing corpus '{id}'...");
    println!("(In production, this would remove the index file and update the mesh.)");
    Ok(())
}

fn cmd_corpus_update(id: &str, _config: &Option<DaemonConfig>) -> Result<()> {
    println!("Checking for updates to corpus '{id}'...");
    println!("(In production, this would check the source for newer data.)");
    Ok(())
}

fn cmd_corpus_status(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("(In production, this would show shard status for all corpora on this node.)");
    Ok(())
}

fn cmd_corpus_consolidate(id: &str, _config: &Option<DaemonConfig>) -> Result<()> {
    println!("Consolidating shards for corpus '{id}'...");
    println!("(In production, this would merge all local shard files into a complete index.)");
    Ok(())
}

fn cmd_corpus_collaborate(
    id: &str,
    recipe: Option<&str>,
    config: &Option<DaemonConfig>,
) -> Result<()> {
    let internal_port = config
        .as_ref()
        .map(|c| c.node.internal_port)
        .unwrap_or(9742);
    let url = format!("http://127.0.0.1:{internal_port}/internal/corpus/collaborate");

    println!("Planning collaborative ingestion for corpus '{id}'...");
    println!("Contacting daemon at {url}");
    println!();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build async runtime")?;

    rt.block_on(async move {
        let client = reqwest::Client::new();
        let mut payload = serde_json::json!({ "corpus_id": id });
        if let Some(r) = recipe {
            payload["recipe_id"] = serde_json::json!(r);
        }

        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("failed to reach daemon at {url}\nIs it running? Try: commonwealth daemon start"))?;

        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();

        if status.is_success() {
            let handoff = &body;
            let partitions = handoff["partitions"].as_array().cloned().unwrap_or_default();
            println!("Collaborative ingestion planned.");
            println!();
            println!("  Corpus:     {id}");
            println!("  Handoff ID: {}", handoff["handoff_id"].as_str().unwrap_or("?"));
            println!("  Partitions: {}", partitions.len());
            println!();
            println!("  {:<40} {:>10} {:>12}", "Node", "Files", "Status");
            println!("  {}", "─".repeat(66));
            for p in &partitions {
                let node = p["node_id"].as_str().unwrap_or("?");
                let files = p["file_indices"].as_array().map(|a| a.len()).unwrap_or(0);
                let status = p["status"]["state"].as_str().unwrap_or("?");
                println!("  {:<40} {:>10} {:>12}", node, files, status);
            }
            println!();
            println!("Peers have been notified. Monitor progress with:");
            println!("  commonwealth corpus collaborate-status {id}");
        } else {
            let error = body["error"].as_str().unwrap_or("unknown error");
            eprintln!("Error: {error}");
            eprintln!("HTTP {status}");
            std::process::exit(1);
        }
        Ok::<_, anyhow::Error>(())
    })
}

fn cmd_corpus_collaborate_status(id: &str, config: &Option<DaemonConfig>) -> Result<()> {
    let internal_port = config
        .as_ref()
        .map(|c| c.node.internal_port)
        .unwrap_or(9742);
    println!("Checking collaborative ingestion status for corpus '{id}'...");
    println!("(In production, this would read the gossip handoff state from the daemon at localhost:{internal_port}.)");
    Ok(())
}

fn cmd_logs(follow: bool, config: &Option<DaemonConfig>) -> Result<()> {
    let data_dir = config
        .as_ref()
        .map(|c| c.node.data_dir.clone())
        .unwrap_or_else(|| "~/.commonwealth".into());

    if follow {
        println!("(In production, this would tail -f {data_dir}/daemon.log.)");
    } else {
        println!("(In production, this would cat {data_dir}/daemon.log.)");
    }
    Ok(())
}

fn cmd_mesh_set(key: &str, value: &str, _config: &Option<DaemonConfig>) -> Result<()> {
    println!("Proposing mesh config change: {key} = {value}");
    println!("(In production, this would broadcast the proposal via gossip for majority vote.)");
    Ok(())
}

fn cmd_mesh_members(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("(In production, this would list all mesh members with status and balance.)");
    Ok(())
}

fn cmd_mesh_revoke(node: &str, _config: &Option<DaemonConfig>) -> Result<()> {
    // HONESTY: revocation is not wired. `Mesh::merge_from` is grow-only with no
    // tombstone, so a "revoked" node is resurrected on the next gossip round —
    // returning Ok here would report false success on a security action. Fail
    // loudly until revocation actually propagates.
    anyhow::bail!(
        "mesh revoke is not implemented: revoking '{node}' would not propagate \
         (membership merge is grow-only, no tombstone). Refusing to report false success."
    )
}

fn cmd_mesh_peer(key: &str, _config: &Option<DaemonConfig>) -> Result<()> {
    membership::validate_join_key_format(key)
        .map_err(|e| anyhow::anyhow!("Invalid peering key: {e}"))?;

    println!("Establishing peering with key: {key}");
    println!("(In production, this would initiate the peering handshake.)");
    Ok(())
}

fn cmd_daemon_start(config: &Option<DaemonConfig>) -> Result<()> {
    let api_port = config.as_ref().map(|c| c.node.api_port).unwrap_or(9741);
    let internal_port = config
        .as_ref()
        .map(|c| c.node.internal_port)
        .unwrap_or(9742);
    let data_dir = config
        .as_ref()
        .map(|c| c.node.data_dir.clone())
        .unwrap_or_else(|| "~/.commonwealth".into())
        .replace('~', &std::env::var("HOME").unwrap_or_default());

    println!("Starting Commonwealth daemon...");
    println!("  Client API:   http://127.0.0.1:{api_port}");
    println!("  Internal API: http://127.0.0.1:{internal_port}");
    println!();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build async runtime")?;

    rt.block_on(async move {
        // 1. Ensure data directory exists.
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create data dir: {data_dir}"))?;

        // 2. Open the mesh store.
        let store_path = PathBuf::from(&data_dir).join("store.db");
        let mesh_store =
            Arc::new(MeshStore::open(&store_path).with_context(|| {
                format!("failed to open MeshStore at {}", store_path.display())
            })?);
        info!(path = %store_path.display(), "MeshStore opened");

        // 3. Init the app registry and register built-in apps.
        let app_registry = Arc::new(AppRegistry::new());

        app_registry
            .register(MeshAppManifest {
                app_id: "inference".into(),
                name: "Inference Engine".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                entrypoint: "embedded".into(),
                permissions: AppPermissions {
                    mesh_store_read: true,
                    mesh_store_write: true,
                    inference_access: true,
                    knowledge_access: false,
                },
                required_capabilities: RequiredCapabilities::default(),
            })
            .await;

        app_registry
            .register(MeshAppManifest {
                app_id: "knowledge".into(),
                name: "Knowledge Engine".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                entrypoint: "embedded".into(),
                permissions: AppPermissions {
                    mesh_store_read: true,
                    mesh_store_write: true,
                    inference_access: false,
                    knowledge_access: true,
                },
                required_capabilities: RequiredCapabilities::default(),
            })
            .await;

        info!("AppRegistry initialized with inference and knowledge apps");

        // 4. Shutdown channel for background tasks.
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // 5. Start RetentionGc (7-day TTL, hourly GC).
        let gc = RetentionGc::new(
            mesh_store.clone(),
            86_400 * 7,                 // 7 days
            Duration::from_secs(3_600), // run every hour
        );
        tokio::spawn(gc.run(shutdown_rx));
        info!("RetentionGc started");

        // 6. Probe inference capability before announcing to the mesh.
        // This must run before mDNS announce and the first gossip round so the
        // node never transiently appears as an inference candidate it cannot fill.
        let inference_config = config
            .as_ref()
            .map(|c| c.inference.clone())
            .unwrap_or_default();
        let inference_capable = probe_inference_capability(&inference_config).await;
        if inference_capable {
            info!("Inference probe passed — node will participate in inference routing.");
        } else {
            info!("Inference probe failed — node will join as storage-only.");
        }

        // 7. Build AppState with platform components.
        // (Mesh state is loaded from disk in a full implementation;
        //  here we construct a minimal mesh for the daemon to serve.)
        use commonwealth_core::ids::{MeshId, NodeId};
        use commonwealth_core::mesh::Mesh;
        use std::collections::HashMap;

        let self_node_id = NodeId::generate();
        let mesh = Mesh {
            id: MeshId::from_u128(0),
            name: "commonwealth".into(),
            join_key_hash: [0u8; 32],
            members: HashMap::new(),
            peers: vec![],
        };

        let state = commonwealth_api::state::AppState::new_with_platform(
            self_node_id,
            mesh,
            mesh_store,
            app_registry,
        );
        state.set_local_inference_capable(inference_capable);

        // The standalone daemon always binds `0.0.0.0` (it exists to
        // serve a mesh), so non-loopback callers are expected — the
        // client API therefore requires a bearer token of every remote
        // caller (the `client_auth` layer; loopback stays exempt).
        // Precedence: env → auto-generate+persist under data_dir. If a
        // token can't be obtained, install None → the layer fails
        // closed for remote callers rather than serving unauthenticated.
        let client_token = std::env::var("SOVEREIGN_CLIENT_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                commonwealth_api::client_auth::load_or_create_client_token(std::path::Path::new(
                    &data_dir,
                ))
                .map_err(|e| tracing::warn!("client-token persistence failed: {e}"))
                .ok()
            });
        match &client_token {
            Some(_) => {
                info!(
                    "client API requires a bearer token for remote callers \
                     (token at {data_dir}/client-token; or set SOVEREIGN_CLIENT_TOKEN)"
                );
            }
            None => {
                tracing::warn!(
                    "no client token could be resolved/generated — remote callers \
                     will be REFUSED (fail-closed); fix data-dir perms or set \
                     SOVEREIGN_CLIENT_TOKEN"
                );
            }
        }
        state.install_client_token(client_token.map(std::sync::Arc::<str>::from));

        // Hourly StorageSnapshot ledger emission. Runs alongside
        // RetentionGc (same shutdown channel, same cadence). The
        // walker closure pulls installed-and-mesh-shared corpora
        // out of the engine; on the standalone daemon (no engine
        // wired) it returns an empty list and the loop emits
        // nothing — safe degradation.
        let snapshot_emitter = state.inner.contribution_emitter.clone();
        let snapshot_engine = state.inner.corpus_engine.clone();
        let snapshot_shutdown = shutdown_tx.subscribe();
        tokio::spawn(
            commonwealth_state::contributions::run_storage_snapshot_loop(
                snapshot_emitter,
                move || {
                    let engine = snapshot_engine.clone();
                    async move {
                        let Some(engine) = engine else {
                            return Vec::new();
                        };
                        let installed = match engine.installed_indexes().await {
                            Ok(list) => list,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "storage_snapshot: installed_indexes failed"
                                );
                                return Vec::new();
                            }
                        };
                        installed
                            .into_iter()
                            .filter(|i| i.mesh_sharing)
                            .map(|i| (i.corpus_id, i.index_size_bytes as f64 / 1e9))
                            .collect()
                    }
                },
                commonwealth_state::contributions::STORAGE_SNAPSHOT_INTERVAL,
                snapshot_shutdown,
            ),
        );
        info!("StorageSnapshot loop started");

        // 7. Start both API servers.
        let client_addr: SocketAddr = format!("0.0.0.0:{api_port}").parse()?;
        let internal_addr: SocketAddr = format!("0.0.0.0:{internal_port}").parse()?;

        info!(
            client = %client_addr,
            internal = %internal_addr,
            "Commonwealth daemon starting"
        );

        // Handle SIGTERM/Ctrl-C.
        let shutdown_tx_clone = shutdown_tx.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Shutdown signal received");
            let _ = shutdown_tx_clone.send(true);
        });

        commonwealth_api::server::serve(state, client_addr, internal_addr)
            .await
            .map_err(|e| anyhow::anyhow!("daemon exited with error: {e}"))?;

        Ok::<_, anyhow::Error>(())
    })
}

/// Probe whether this node can serve inference requests.
///
/// The probe runs BEFORE mDNS announce and the first gossip round so the
/// node never transiently advertises capability it cannot fulfill.
///
/// Strategy:
/// - If `llama_server` looks like a URL (`http://` prefix or contains `:`),
///   attempt a TCP connect + `GET /health` with a 3-second timeout.
/// - Otherwise treat it as a binary name and check PATH via `which`.
///
/// Returns `true` only on confirmed reachability. Never panics.
// TODO: re-probe on config file change; for now, daemon restart is required.
async fn probe_inference_capability(config: &InferenceConfig) -> bool {
    let addr = &config.llama_server;

    if addr.starts_with("http://") || addr.starts_with("https://") || addr.contains(':') {
        // Looks like an address — try a health check.
        let url = if addr.starts_with("http") {
            format!("{addr}/health")
        } else {
            format!("http://{addr}/health")
        };
        match reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                tracing::info!(url = %url, "Inference probe: llama-server healthy");
                true
            }
            Ok(r) => {
                tracing::warn!(
                    url = %url, status = %r.status(),
                    "Inference probe: llama-server returned non-success status. \
                     Joining mesh as storage-only."
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    url = %url, error = %e,
                    "Inference probe: llama-server unreachable. \
                     Joining mesh as storage-only. \
                     Start llama-server or fix the address in config to enable inference routing."
                );
                false
            }
        }
    } else {
        // Looks like a binary name — check if it exists in PATH.
        let found = std::process::Command::new("which")
            .arg(addr.as_str())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if found {
            tracing::info!(binary = %addr, "Inference probe: binary found in PATH");
        } else {
            tracing::warn!(
                binary = %addr,
                "Inference probe: binary not found in PATH. \
                 Joining mesh as storage-only. \
                 Install llama-server or set [inference].llama_server in config."
            );
        }
        found
    }
}

// ── Recipe commands ─────────────────────────────────────────────────────────

fn cmd_recipe_test(
    path: &std::path::Path,
    sample_size: usize,
    embed: bool,
    output: Option<&std::path::Path>,
    offline: bool,
    verbose: bool,
) -> Result<()> {
    let output = output.map(PathBuf::from).unwrap_or_else(|| {
        path.parent()
            .unwrap_or(std::path::Path::new("."))
            .join("TEST_REPORT.md")
    });

    eprintln!("Testing recipe: {}", path.display());
    eprintln!("Sample size:    {sample_size}");
    eprintln!("Output:         {}", output.display());

    let engine = build_stub_engine();
    let options = TestOptions {
        sample_size,
        embed,
        queries: None,
        output: Some(output.clone()),
        offline,
        verbose,
        parameters: Default::default(),
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build async runtime")?;

    let report = rt
        .block_on(engine.test_recipe(path, &options))
        .context("recipe test failed")?;

    let markdown = report.to_markdown();
    std::fs::write(&output, &markdown)
        .with_context(|| format!("failed to write report to {}", output.display()))?;

    eprintln!();
    eprintln!("Report written to: {}", output.display());

    let warnings = report.warnings();
    if !warnings.is_empty() {
        eprintln!();
        eprintln!("Warnings:");
        for w in &warnings {
            eprintln!("  ⚠  {w}");
        }
    }

    if !report.validation.errors.is_empty() {
        eprintln!();
        eprintln!("Errors:");
        for e in &report.validation.errors {
            eprintln!("  ✗  {e}");
        }
    }

    eprintln!();
    if report.passed() {
        eprintln!("Result: PASS");
        Ok(())
    } else {
        eprintln!("Result: FAIL");
        std::process::exit(1);
    }
}

fn cmd_recipe_validate(path: &std::path::Path, offline: bool) -> Result<()> {
    eprintln!("Validating recipe: {}", path.display());

    let engine = build_stub_engine();
    let options = TestOptions {
        sample_size: 0,
        embed: false,
        offline,
        ..Default::default()
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build async runtime")?;

    let report = rt
        .block_on(engine.test_recipe(path, &options))
        .context("recipe validation failed")?;

    if report.validation.errors.is_empty() {
        eprintln!("✓ Validation passed");
        for w in &report.validation.warnings {
            eprintln!("  ⚠  {w}");
        }
        Ok(())
    } else {
        eprintln!("✗ Validation failed:");
        for e in &report.validation.errors {
            eprintln!("  - {e}");
        }
        std::process::exit(1);
    }
}

fn build_stub_engine() -> CorpusEngine {
    let stub_embed: EmbedFn = Arc::new(|_text| Box::pin(async { Ok(vec![0f32; 768]) }));
    let tmp = std::env::temp_dir().join("commonwealth-recipe-test");
    CorpusEngine::new(tmp.clone(), tmp, stub_embed)
}
