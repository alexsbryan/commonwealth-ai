// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP authentication — credential types and header injection.
//!
//! Credentials are resolved by [`McpAuth::resolve`] from a file-backed secret
//! store ([`super::secret_store`], `~/.sovereign/secrets/`) first, then the
//! `SOVEREIGN_MCP_TOKEN_<NAME>` env var (headless nodes / CI) — never from the
//! on-disk `config.toml` (ARCH §7), so the secret never rides along with
//! shared / synced / gossiped state. Both are read through the single `resolve`
//! seam; every caller picks up the precedence automatically.

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
    /// Resolve an `McpAuthConfig` (the *type* of auth, from on-disk config)
    /// into a concrete `McpAuth` (carrying the *secret*), reading the secret
    /// from the `SOVEREIGN_MCP_TOKEN_<NAME>` env var. A credentialed server
    /// whose env var is unset connects unauthenticated with a loud warning
    /// naming the exact variable to set — never a silent failure.
    pub fn resolve(server_name: &str, config: &McpAuthConfig) -> Self {
        match config {
            McpAuthConfig::None => McpAuth::None,
            McpAuthConfig::Bearer => match read_secret(server_name) {
                Some(token) => McpAuth::BearerToken(token),
                None => {
                    warn_missing_secret(server_name);
                    McpAuth::None
                }
            },
            McpAuthConfig::ApiKey { header } => match read_secret(server_name) {
                Some(value) => McpAuth::ApiKey {
                    header: header.clone(),
                    value,
                },
                None => {
                    warn_missing_secret(server_name);
                    McpAuth::None
                }
            },
            McpAuthConfig::Basic => {
                tracing::warn!(
                    server = server_name,
                    "MCP basic auth is not yet wired — connecting unauthenticated"
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

/// The environment variable a server's credential is read from:
/// `SOVEREIGN_MCP_TOKEN_<NAME>`, where NAME is the server name uppercased with
/// every non-alphanumeric character folded to `_` (so server `my-vision`
/// reads `SOVEREIGN_MCP_TOKEN_MY_VISION`). Keeps the secret out of the
/// on-disk config without a keychain dependency. `pub` so `sovereign mcp add`
/// can name the exact variable back to the user from this single definition.
pub fn secret_env_var(server_name: &str) -> String {
    format!("SOVEREIGN_MCP_TOKEN_{}", sanitized_name(server_name))
}

/// The server name folded to a slug shared by the env var and the file store:
/// every non-alphanumeric char becomes `_`, then uppercased (`my-vision` →
/// `MY_VISION`). One definition so the two stores never disagree on the key.
pub fn sanitized_name(server_name: &str) -> String {
    server_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// A server's secret, file store first (what the desktop writes) then the
/// `SOVEREIGN_MCP_TOKEN_<NAME>` env var (headless / CI). The single read seam.
fn read_secret(server_name: &str) -> Option<String> {
    super::secret_store::read_token(server_name).or_else(|| read_secret_env(server_name))
}

fn read_secret_env(server_name: &str) -> Option<String> {
    std::env::var(secret_env_var(server_name))
        .ok()
        .filter(|s| !s.is_empty())
}

fn warn_missing_secret(server_name: &str) {
    tracing::warn!(
        server = server_name,
        env_var = %secret_env_var(server_name),
        "MCP server needs a credential but none is stored in the app and its env \
         var is unset — connecting unauthenticated"
    );
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
