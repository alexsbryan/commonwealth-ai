use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;

use commonwealth_core::config::DaemonConfig;
use commonwealth_discovery::membership;

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
    /// List available and hosted knowledge bases
    Corpora,
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
    /// Stop the daemon
    Stop,
    /// Show daemon status
    Status,
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
        Commands::Corpora => cmd_corpora(&config),
        Commands::Logs { follow } => cmd_logs(follow, &config),
        Commands::Mesh { command } => match command {
            MeshCommands::Set { key, value } => cmd_mesh_set(&key, &value, &config),
            MeshCommands::Members => cmd_mesh_members(&config),
            MeshCommands::Revoke { node } => cmd_mesh_revoke(&node, &config),
            MeshCommands::Peer { key } => cmd_mesh_peer(&key, &config),
        },
        Commands::Daemon { command } => match command {
            DaemonCommands::Start => cmd_daemon_start(&config),
            DaemonCommands::Stop => cmd_daemon_stop(&config),
            DaemonCommands::Status => cmd_daemon_status(&config),
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
    println!("(In production, this would read the contribution ledger and display balances.)");
    println!();
    println!("Contribution Balance (last 30 days)");
    println!("{}", "─".repeat(60));
    println!(
        "{:<20} {:>10} {:>10} {:>12} {:>10}",
        "Node", "Compute", "Storage", "Bandwidth", "Balance"
    );
    println!("{}", "─".repeat(60));
    println!(
        "{:<20} {:>10} {:>10} {:>12} {:>10}",
        "(no data)", "—", "—", "—", "—"
    );
    Ok(())
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

fn cmd_corpora(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("(In production, this would list hosted knowledge corpora from the daemon.)");
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
    println!("Proposing revocation of node: {node}");
    println!("(In production, this would broadcast a revocation proposal via gossip.)");
    Ok(())
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

    println!("Starting Commonwealth daemon...");
    println!("  Client API:   http://127.0.0.1:{api_port}");
    println!("  Internal API: http://127.0.0.1:{internal_port}");
    println!();
    println!(
        "(In production, this would fork a background process or be managed by systemd/launchd.)"
    );

    Ok(())
}

fn cmd_daemon_stop(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("Stopping Commonwealth daemon...");
    println!("(In production, this would send SIGTERM to the daemon process.)");
    Ok(())
}

fn cmd_daemon_status(_config: &Option<DaemonConfig>) -> Result<()> {
    println!("(In production, this would check if the daemon process is running.)");
    Ok(())
}
