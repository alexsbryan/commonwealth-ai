//! `sovereign mcp` subcommand handlers.
//!
//! Manages MCP server connections, credentials, and tool discovery.
//! These are lightweight commands that don't require loading a full model.

use sovereign_tools::mcp::auth::McpAuth;
use sovereign_tools::mcp::config::{McpAuthConfig, McpServerConfig, McpTransportConfig};

/// Run an MCP subcommand. Returns the exit code.
pub async fn run_mcp(args: &[String]) -> i32 {
    if args.is_empty() {
        print_mcp_usage();
        return 1;
    }

    match args[0].as_str() {
        "list" => cmd_list().await,
        "test" => cmd_test(&args[1..]).await,
        "tools" => cmd_tools(&args[1..]).await,
        "add-credential" => cmd_add_credential(&args[1..]).await,
        "remove-credential" => cmd_remove_credential(&args[1..]).await,
        "help" | "--help" | "-h" => {
            print_mcp_usage();
            0
        }
        other => {
            eprintln!("Unknown mcp subcommand: {other}");
            print_mcp_usage();
            1
        }
    }
}

fn print_mcp_usage() {
    eprintln!(
        "Usage: sovereign mcp <command>

Commands:
  list                     List configured MCP servers with status
  test <server-name>       Test connection to a server
  tools [server-name]      List available MCP tools
  add-credential <name>    Store credentials in system keychain
  remove-credential <name> Remove credentials from keychain
  help                     Show this help"
    );
}

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

async fn cmd_add_credential(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: sovereign mcp add-credential <server-name> [--bearer|--api-key|--basic]");
        return 1;
    }
    let name = &args[0];
    let auth_type = args.get(1).map(|s| s.as_str()).unwrap_or("--bearer");

    let auth = match auth_type {
        "--bearer" => {
            eprint!("Token: ");
            let token = read_hidden_input();
            McpAuth::BearerToken(token)
        }
        "--api-key" => {
            eprint!("Header name: ");
            let header = read_line_input();
            eprint!("API key: ");
            let value = read_hidden_input();
            McpAuth::ApiKey { header, value }
        }
        "--basic" => {
            eprint!("Username: ");
            let username = read_line_input();
            eprint!("Password: ");
            let password = read_hidden_input();
            McpAuth::Basic { username, password }
        }
        other => {
            eprintln!("Unknown auth type: {other}. Use --bearer, --api-key, or --basic.");
            return 1;
        }
    };

    #[cfg(feature = "keychain")]
    {
        match McpAuth::store_in_keychain(name, &auth) {
            Ok(()) => {
                eprintln!("Stored in keychain as \"sovereign-mcp-{name}\"");
                0
            }
            Err(e) => {
                eprintln!("Failed to store credential: {e}");
                1
            }
        }
    }
    #[cfg(not(feature = "keychain"))]
    {
        let _ = (name, auth);
        eprintln!("Keychain support not compiled in. Rebuild with --features keychain.");
        1
    }
}

async fn cmd_remove_credential(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("Usage: sovereign mcp remove-credential <server-name>");
        return 1;
    }
    let name = &args[0];

    #[cfg(feature = "keychain")]
    {
        match McpAuth::remove_from_keychain(name) {
            Ok(()) => {
                eprintln!("Removed credential for \"{name}\"");
                0
            }
            Err(e) => {
                eprintln!("Failed: {e}");
                1
            }
        }
    }
    #[cfg(not(feature = "keychain"))]
    {
        let _ = name;
        eprintln!("Keychain support not compiled in.");
        1
    }
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
            let auth = resolve_auth_for_test(&config.name, auth_config);
            let tools =
                sovereign_tools::mcp::connect_http_mcp_server(url, auth, &config.name)
                    .await
                    .map_err(|e| e.to_string())?;
            Ok(tools.iter().map(|t| t.descriptor().id).collect())
        }
    }
}

fn resolve_auth_for_test(server_name: &str, config: &McpAuthConfig) -> McpAuth {
    match config {
        McpAuthConfig::None => McpAuth::None,
        _ => {
            #[cfg(feature = "keychain")]
            {
                McpAuth::from_keychain(server_name).unwrap_or(McpAuth::None)
            }
            #[cfg(not(feature = "keychain"))]
            {
                let _ = server_name;
                McpAuth::None
            }
        }
    }
}

fn read_line_input() -> String {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap_or(0);
    input.trim().to_string()
}

fn read_hidden_input() -> String {
    // Simple hidden input — on Unix, disable echo. On other platforms, fall back.
    #[cfg(unix)]
    {
        if let Ok(()) = disable_echo() {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).unwrap_or(0);
            let _ = enable_echo();
            eprintln!(); // newline after hidden input
            return input.trim().to_string();
        }
    }
    // Fallback: read normally (visible).
    read_line_input()
}

#[cfg(unix)]
fn disable_echo() -> std::result::Result<(), ()> {
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) != 0 {
            return Err(());
        }
        termios.c_lflag &= !libc::ECHO;
        if libc::tcsetattr(0, libc::TCSANOW, &termios) != 0 {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn enable_echo() -> std::result::Result<(), ()> {
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) != 0 {
            return Err(());
        }
        termios.c_lflag |= libc::ECHO;
        if libc::tcsetattr(0, libc::TCSANOW, &termios) != 0 {
            return Err(());
        }
    }
    Ok(())
}
