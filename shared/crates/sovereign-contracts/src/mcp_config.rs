// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP server configuration schema (pure data).
//!
//! Parsed from the `[[mcp_servers]]` array of `~/.svrnmesh/config.toml`
//! (via [`crate::setup_config::SetupConfig`]) and written by the desktop
//! settings pane / `sovereign mcp add`. Credentials are **not** stored here —
//! only the auth *type* is named; the actual secret is resolved at connect
//! time (see `sovereign_tools::mcp::auth`).
//!
//! ## Why this lives in `sovereign-core`
//!
//! The MCP *client engine* (transports, adapter, discovery) lives in
//! `sovereign-tools`, which depends on `sovereign-core`. For `SetupConfig`
//! (a core type) to carry a typed `mcp_servers` field, the config DTO must be
//! reachable from core without a crate cycle — so the data half lives here and
//! `sovereign_tools::mcp` re-exports it for back-compat. This is the SICP
//! data/program split (ARCH §6): the schema is data (core), the connection
//! machinery is program (tools).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One MCP server entry in the config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name. Used as the credential lookup key and tool name prefix
    /// (`mcp_<name>_<tool>`).
    pub name: String,

    /// Human-readable description shown in the settings UI.
    #[serde(default)]
    pub description: Option<String>,

    /// Whether this server is active.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Transport configuration.
    pub transport: McpTransportConfig,

    /// If true, tools are available to all skills.
    /// If false, skills must explicitly list this server's tools.
    #[serde(default = "default_true")]
    pub global: bool,
}

/// Transport-specific configuration.
///
/// The surfaced path (CLI `mcp add`, desktop settings) only offers `Http` —
/// Sovereign deliberately does not supervise subprocesses, so `Stdio` is
/// latent: it parses and the client can drive it, but no UI creates it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportConfig {
    /// Spawn a subprocess and speak MCP over stdio. Latent — parses and the client can drive it, but no UI creates it (see type doc).
    Stdio {
        /// Executable to spawn.
        command: String,
        /// Command-line arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Extra environment variables for the subprocess.
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Connect to a streamable-HTTP MCP endpoint — the only transport the CLI/desktop surfaces offer.
    Http {
        /// Endpoint URL.
        url: String,
        /// How to authenticate; the credential itself is resolved at connect time (see `McpAuthConfig`).
        #[serde(default)]
        auth: McpAuthConfig,
    },
}

/// Auth type descriptor. The actual credential is resolved at connect time
/// (from an env var today; a keychain later) — never persisted here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpAuthConfig {
    /// No authentication.
    #[default]
    None,
    /// `Authorization: Bearer <token>`; the token is resolved at connect time.
    Bearer,
    /// Token sent in a custom header.
    ApiKey {
        /// Header name carrying the key (e.g. `X-Api-Key`).
        header: String,
    },
    /// HTTP Basic auth; credentials resolved at connect time.
    Basic,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stdio_config() {
        let toml = r#"
name = "filesystem"
description = "Local filesystem access"

[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/alice"]
"#;
        let config: McpServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.name, "filesystem");
        assert!(config.enabled); // default true
        assert!(config.global); // default true
        assert!(matches!(
            config.transport,
            McpTransportConfig::Stdio { ref command, .. } if command == "npx"
        ));
    }

    #[test]
    fn parse_http_bearer_config() {
        let toml = r#"
name = "github"
description = "GitHub — issues, pull requests"

[transport]
type = "http"
url = "https://api.githubcopilot.com/mcp/"

[transport.auth]
type = "bearer"
"#;
        let config: McpServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.name, "github");
        assert!(matches!(
            config.transport,
            McpTransportConfig::Http { ref url, .. } if url.contains("github")
        ));
    }

    #[test]
    fn parse_http_api_key_config() {
        let toml = r#"
name = "notion"
description = "Notion pages and databases"
global = false

[transport]
type = "http"
url = "https://api.notion.com/mcp"

[transport.auth]
type = "api_key"
header = "Notion-Version"
"#;
        let config: McpServerConfig = toml::from_str(toml).unwrap();
        assert!(!config.global);
        assert!(matches!(
            config.transport,
            McpTransportConfig::Http {
                auth: McpAuthConfig::ApiKey { ref header },
                ..
            } if header == "Notion-Version"
        ));
    }

    #[test]
    fn parse_disabled_server() {
        let toml = r#"
name = "test"
enabled = false

[transport]
type = "http"
url = "https://example.com/mcp"
"#;
        let config: McpServerConfig = toml::from_str(toml).unwrap();
        assert!(!config.enabled);
    }

    #[test]
    fn parse_http_no_auth() {
        let toml = r#"
name = "local"

[transport]
type = "http"
url = "http://localhost:3000/mcp"
"#;
        let config: McpServerConfig = toml::from_str(toml).unwrap();
        assert!(matches!(
            config.transport,
            McpTransportConfig::Http {
                auth: McpAuthConfig::None,
                ..
            }
        ));
    }
}
