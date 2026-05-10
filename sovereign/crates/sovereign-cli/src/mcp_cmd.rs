//! `sovereign mcp` subcommand handlers.
//!
//! Read-only commands for inspecting and testing configured MCP servers.
//! These are lightweight: they don't require loading a full model.

use sovereign_tools::mcp::auth::McpAuth;
use sovereign_tools::mcp::config::{McpServerConfig, McpTransportConfig};

/// Run an MCP subcommand. Returns the exit code.
pub async fn run_mcp(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    if args.is_empty() {
        crate::util::help::print(&HELP);
        return 1;
    }

    match args[0].as_str() {
        "list" => cmd_list().await,
        "test" => cmd_test(&args[1..]).await,
        "tools" => cmd_tools(&args[1..]).await,
        other => {
            eprintln!("Unknown mcp subcommand: {other}");
            crate::util::help::print(&HELP);
            1
        }
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign mcp",
    summary: "Inspect and test configured MCP (Model Context Protocol) servers.",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign mcp <command> [args]"),
        crate::util::help::HelpSection::Subcommands(&[
            ("list",               "List configured MCP servers with status"),
            ("test <server>",      "Test connection to a named server"),
            ("tools [server]",     "List available MCP tools (optionally for one server)"),
        ]),
    ],
};

async fn cmd_list() -> i32 {
    let configs = load_mcp_configs();
    if configs.is_empty() {
        eprintln!("No MCP servers configured.");
        eprintln!("Add servers to your config file under [[mcp_servers]].");
        return 0;
    }

    for config in &configs {
        let status = if !config.enabled {
            "[disabled]".to_string()
        } else {
            // Try to connect and count tools.
            match test_connection(config).await {
                Ok(count) => format!("[connected]   {count} tools"),
                Err(e) => format!("[error]       {e}"),
            }
        };
        let desc = config
            .description
            .as_deref()
            .unwrap_or("");
        eprintln!("  {:<16} {status}  {desc}", config.name);
    }
    0
}

async fn cmd_test(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: sovereign mcp test <server-name>");
        return 1;
    }
    let name = &args[0];
    let configs = load_mcp_configs();
    let config = match configs.iter().find(|c| c.name == *name) {
        Some(c) => c,
        None => {
            eprintln!("Server '{name}' not found in config.");
            return 1;
        }
    };

    eprint!("Connecting to {name}... ");
    match test_connection_verbose(config).await {
        Ok(tools) => {
            eprintln!("connected! {} tools available:", tools.len());
            for tool in &tools {
                eprintln!("  - mcp_{}_{}", name, tool);
            }
            0
        }
        Err(e) => {
            eprintln!("failed: {e}");
            1
        }
    }
}

async fn cmd_tools(args: &[String]) -> i32 {
    let filter = args.first().map(|s| s.as_str());
    let configs = load_mcp_configs();
    let mut found_any = false;

    for config in &configs {
        if !config.enabled {
            continue;
        }
        if let Some(f) = filter {
            if config.name != f {
                continue;
            }
        }
        match test_connection_verbose(config).await {
            Ok(tools) => {
                found_any = true;
                eprintln!("[{}] {} tools:", config.name, tools.len());
                for tool in &tools {
                    eprintln!("  mcp_{}_{tool}", config.name);
                }
            }
            Err(e) => {
                eprintln!("[{}] connection failed: {e}", config.name);
            }
        }
    }

    if !found_any {
        eprintln!("No tools found. Check server connections with: sovereign mcp list");
    }
    0
}

// ─── Helpers ──────────────────────────────────────────────────

fn load_mcp_configs() -> Vec<McpServerConfig> {
    // Try to load from the standard config file locations.
    let config_paths: [Option<std::path::PathBuf>; 2] = [
        dirs::config_dir()
            .map(|d| d.join("sovereign").join("config.toml")),
        dirs::home_dir()
            .map(|d| d.join(".sovereign").join("config.toml")),
    ];

    for path in config_paths.iter().flatten() {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                // Parse just the mcp_servers array from the config.
                #[derive(serde::Deserialize, Default)]
                struct PartialConfig {
                    #[serde(default)]
                    mcp_servers: Vec<McpServerConfig>,
                }
                if let Ok(config) = toml::from_str::<PartialConfig>(&content) {
                    return config.mcp_servers;
                }
            }
        }
    }

    Vec::new()
}

async fn test_connection(config: &McpServerConfig) -> std::result::Result<usize, String> {
    let tools = test_connection_verbose(config).await?;
    Ok(tools.len())
}

async fn test_connection_verbose(
    config: &McpServerConfig,
) -> std::result::Result<Vec<String>, String> {
    match &config.transport {
        McpTransportConfig::Stdio { command, args, .. } => {
            let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let tools = sovereign_tools::mcp::connect_mcp_server(command, &args_refs, &config.name)
                .await
                .map_err(|e| e.to_string())?;
            Ok(tools.iter().map(|t| t.descriptor().id).collect())
        }
        McpTransportConfig::Http { url, auth: auth_config } => {
            let auth = McpAuth::resolve(&config.name, auth_config);
            let tools =
                sovereign_tools::mcp::connect_http_mcp_server(url, auth, &config.name)
                    .await
                    .map_err(|e| e.to_string())?;
            Ok(tools.iter().map(|t| t.descriptor().id).collect())
        }
    }
}
