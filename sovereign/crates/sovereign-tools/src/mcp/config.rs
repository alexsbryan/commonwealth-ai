// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP server configuration schema.
//!
//! Parsed from TOML config files. Credentials are NOT stored here —
//! only the auth type is specified; actual secrets live in the keychain.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One MCP server entry in the config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name. Used as the keychain service identifier and tool name prefix.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        auth: McpAuthConfig,
    },
}

/// Auth type descriptor. The actual credential is in the keychain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpAuthConfig {
    #[default]
    None,
    Bearer,
    ApiKey {
        header: String,
    },
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
