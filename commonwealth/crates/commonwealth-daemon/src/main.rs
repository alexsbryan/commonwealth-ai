use clap::Parser;

#[derive(Parser)]
#[command(
    name = "commonwealth",
    about = "Coordination daemon for community-owned distributed inference and knowledge"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
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
    },
    /// Show mesh status
    Status,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Init { name }) => {
            println!("commonwealth init: mesh={name} (not yet implemented)");
        }
        Some(Commands::Join { key }) => {
            println!("commonwealth join: key={key} (not yet implemented)");
        }
        Some(Commands::Status) => {
            println!("commonwealth status (not yet implemented)");
        }
        None => {
            println!("Commonwealth v{}", env!("CARGO_PKG_VERSION"));
            println!("Run 'commonwealth --help' for usage.");
        }
    }

    Ok(())
}
