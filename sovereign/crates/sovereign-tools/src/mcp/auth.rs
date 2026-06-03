//! MCP authentication — credential types and header injection.
//!
//! Secure persistence (e.g. system keychain) is intentionally out of scope
//! right now. Callers construct `McpAuth` values from config or prompts and
//! pass them through for the lifetime of a request.

use reqwest::RequestBuilder;

use super::config::McpAuthConfig;

/// Credential type for an MCP server connection.
#[derive(Debug, Clone)]
pub enum McpAuth {
    /// No authentication required.
    None,
    /// Bearer token in Authorization header.
    BearerToken(String),
    /// API key in a named header (e.g. X-Api-Key).
    ApiKey { header: String, value: String },
    /// HTTP Basic Auth.
    Basic { username: String, password: String },
}

impl McpAuth {
    /// Resolve an `McpAuthConfig` from user config into a concrete `McpAuth`.
    ///
    /// There is no secret store wired up yet, so any config that needs a
    /// credential resolves to `None` with a warning. When a keychain or
    /// file-backed store is added, extend this single function — every
    /// caller (discovery, CLI test commands, future callers) will pick up
    /// the new behavior automatically.
    pub fn resolve(server_name: &str, config: &McpAuthConfig) -> Self {
        match config {
            McpAuthConfig::None => McpAuth::None,
            McpAuthConfig::Bearer | McpAuthConfig::ApiKey { .. } | McpAuthConfig::Basic => {
                tracing::warn!(
                    server = server_name,
                    "MCP credential store not configured — falling back to unauthenticated access"
                );
                McpAuth::None
            }
        }
    }

    /// Inject authentication headers into a request.
    pub fn inject(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            McpAuth::None => req,
            McpAuth::BearerToken(token) => req.header("Authorization", format!("Bearer {token}")),
            McpAuth::ApiKey { header, value } => req.header(header, value),
            McpAuth::Basic { username, password } => req.basic_auth(username, Some(password)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_auth_is_constructible() {
        let auth = McpAuth::None;
        assert!(matches!(auth, McpAuth::None));
    }
}
