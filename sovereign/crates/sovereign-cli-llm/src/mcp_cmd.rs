// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mcp` subcommand handlers.
//!
//! Manage and inspect configured MCP servers — `add` / `remove` edit the
//! canonical `[[mcp_servers]]` list in `~/.sovereign/config.toml` (the same
//! list every chat surface loads via
//! `sovereign_tools::mcp::load_from_setup_config`); `list` / `test` / `tools`
//! connect and report. All lightweight: no model load.

use sovereign_core::setup_config::SetupConfig;
use sovereign_tools::mcp::auth::{secret_env_var, McpAuth};
use sovereign_tools::mcp::config::{McpAuthConfig, McpServerConfig, McpTransportConfig};

/// Run an MCP subcommand. Returns the exit code.
pub async fn run_mcp(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    }

    match args[0].as_str() {
        "list" => cmd_list().await,
        "add" => cmd_add(&args[1..]).await,
        "remove" | "rm" => cmd_remove(&args[1..]).await,
        "test" => cmd_test(&args[1..]).await,
        "tools" => cmd_tools(&args[1..]).await,
        "demo-server" => crate::mcp_demo_server::run_demo_server(&args[1..]).await,
        other => {
            eprintln!("Unknown mcp subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mcp",
    summary: "Inspect and test configured MCP (Model Context Protocol) servers.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn mcp <command> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            ("list", "List configured MCP servers with status"),
            (
                "add <name> --url <url>",
                "Add an HTTP MCP server [--description <t>] [--bearer] [--disabled]",
            ),
            ("remove <name>", "Remove a configured MCP server"),
            ("test <server>", "Test connection to a named server"),
            (
                "tools [server]",
                "List available MCP tools (optionally for one server)",
            ),
            (
                "demo-server [--port N]",
                "Run a local reference MCP server (sealed-fact demo)",
            ),
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
        let desc = config.description.as_deref().unwrap_or("");
        eprintln!("  {:<16} {status}  {desc}", config.name);
    }
    0
}

async fn cmd_test(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: svrn mcp test <server-name>");
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

/// `svrn mcp add <name> --url <url> [--description <t>] [--bearer] [--disabled]`
///
/// Writes an HTTP MCP server into the canonical `[[mcp_servers]]` list and
/// probes it once for immediate feedback. HTTP-only by design — Sovereign does
/// not spawn/supervise stdio subprocesses.
async fn cmd_add(args: &[String]) -> i32 {
    let usage =
        "Usage: svrn mcp add <name> --url <https://host/mcp> [--description <text>] [--bearer] [--disabled]";
    let Some(name) = args.first().cloned() else {
        eprintln!("{usage}");
        return 1;
    };
    if name.starts_with('-') {
        eprintln!("The first argument must be the server name.\n{usage}");
        return 1;
    }

    let mut url: Option<String> = None;
    let mut description: Option<String> = None;
    let mut bearer = false;
    let mut enabled = true;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                i += 1;
                url = args.get(i).cloned();
            }
            "--description" | "--desc" => {
                i += 1;
                description = args.get(i).cloned();
            }
            "--bearer" => bearer = true,
            "--disabled" => enabled = false,
            other => {
                eprintln!("Unknown flag: {other}\n{usage}");
                return 1;
            }
        }
        i += 1;
    }

    let Some(url) = url else {
        eprintln!("Missing required --url.\n{usage}");
        return 1;
    };

    let mut cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not load ~/.sovereign/config.toml ({e}).");
            eprintln!("Run `svrn setup` first, then add MCP servers.");
            return 1;
        }
    };

    let auth = if bearer {
        McpAuthConfig::Bearer
    } else {
        McpAuthConfig::None
    };
    let entry = McpServerConfig {
        name: name.clone(),
        description,
        enabled,
        transport: McpTransportConfig::Http {
            url: url.clone(),
            auth,
        },
        global: true,
    };

    // Replace any existing entry with the same name (idempotent re-add).
    cfg.mcp_servers.retain(|s| s.name != name);
    cfg.mcp_servers.push(entry);

    match cfg.save() {
        Ok(path) => {
            eprintln!("Added MCP server '{name}' → {url}");
            eprintln!("  config: {}", path.display());
            if bearer {
                eprintln!(
                    "  bearer auth: export the token in `{}` before connecting.",
                    secret_env_var(&name)
                );
            }
            if enabled {
                eprint!("  probing… ");
                // Safe: we just pushed this entry.
                match test_connection(cfg.mcp_servers.last().unwrap()).await {
                    Ok(n) => eprintln!("connected, {n} tools."),
                    Err(e) => eprintln!("not reachable yet ({e})."),
                }
            }
            eprintln!("Available in `svrn chat` and the desktop on next start.");
            0
        }
        Err(e) => {
            eprintln!("Failed to save config: {e}");
            1
        }
    }
}

/// `svrn mcp remove <name>` — drop a server from the canonical list.
async fn cmd_remove(args: &[String]) -> i32 {
    let Some(name) = args.first() else {
        eprintln!("Usage: svrn mcp remove <server-name>");
        return 1;
    };
    let mut cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not load config: {e}");
            return 1;
        }
    };
    let before = cfg.mcp_servers.len();
    cfg.mcp_servers.retain(|s| s.name != *name);
    if cfg.mcp_servers.len() == before {
        eprintln!("No MCP server named '{name}' in config.");
        return 1;
    }
    match cfg.save() {
        Ok(path) => {
            eprintln!("Removed MCP server '{name}'. ({})", path.display());
            0
        }
        Err(e) => {
            eprintln!("Failed to save config: {e}");
            1
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────

fn load_mcp_configs() -> Vec<McpServerConfig> {
    // Canonical source of truth: the `[[mcp_servers]]` array of
    // `~/.sovereign/config.toml`. `SetupConfig::load` handles the legacy-path
    // migration, and this is the *same* typed list the chat surfaces load via
    // `sovereign_tools::mcp::load_from_setup_config` — no second parser to
    // drift out of sync.
    SetupConfig::load().map(|c| c.mcp_servers).unwrap_or_default()
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
        McpTransportConfig::Http {
            url,
            auth: auth_config,
        } => {
            let auth = McpAuth::resolve(&config.name, auth_config);
            let tools = sovereign_tools::mcp::connect_http_mcp_server(url, auth, &config.name)
                .await
                .map_err(|e| e.to_string())?;
            Ok(tools.iter().map(|t| t.descriptor().id).collect())
        }
    }
}
