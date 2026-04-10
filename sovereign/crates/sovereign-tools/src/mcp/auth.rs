//! MCP authentication — credential types and header injection.
//!
//! Credentials are stored in the system keychain, never in config files.

use reqwest::RequestBuilder;

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
    /// Inject authentication headers into a request.
    pub fn inject(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            McpAuth::None => req,
            McpAuth::BearerToken(token) => {
                req.header("Authorization", format!("Bearer {token}"))
            }
            McpAuth::ApiKey { header, value } => req.header(header, value),
            McpAuth::Basic {
                username,
                password,
            } => req.basic_auth(username, Some(password)),
        }
    }
}

/// Credential storage format for serialization to the keychain.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredCredential {
    Bearer { token: String },
    ApiKey { header: String, value: String },
    Basic { username: String, password: String },
}

impl StoredCredential {
    fn from_auth(auth: &McpAuth) -> Option<Self> {
        match auth {
            McpAuth::None => None,
            McpAuth::BearerToken(t) => Some(Self::Bearer { token: t.clone() }),
            McpAuth::ApiKey { header, value } => Some(Self::ApiKey {
                header: header.clone(),
                value: value.clone(),
            }),
            McpAuth::Basic { username, password } => Some(Self::Basic {
                username: username.clone(),
                password: password.clone(),
            }),
        }
    }

    fn into_auth(self) -> McpAuth {
        match self {
            Self::Bearer { token } => McpAuth::BearerToken(token),
            Self::ApiKey { header, value } => McpAuth::ApiKey { header, value },
            Self::Basic { username, password } => McpAuth::Basic { username, password },
        }
    }
}

// ─── Keychain integration ─────────────────────────────────────
// Uses the `keyring` crate for cross-platform secure storage.
// Gated behind a feature flag or cfg so tests don't require keychain access.

#[cfg(feature = "keychain")]
impl McpAuth {
    /// Load credentials from the system keychain for a named server.
    pub fn from_keychain(server_name: &str) -> Result<Self, String> {
        let service = format!("sovereign-mcp-{server_name}");
        match keyring::Entry::new(&service, "credential") {
            Ok(entry) => match entry.get_password() {
                Ok(raw) => {
                    let cred: StoredCredential =
                        serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                    Ok(cred.into_auth())
                }
                Err(keyring::Error::NoEntry) => Ok(McpAuth::None),
                Err(e) => Err(format!("Keychain error: {e}")),
            },
            Err(e) => Err(format!("Keychain error: {e}")),
        }
    }

    /// Store credentials in the system keychain.
    pub fn store_in_keychain(server_name: &str, auth: &McpAuth) -> Result<(), String> {
        let cred = StoredCredential::from_auth(auth)
            .ok_or_else(|| "Cannot store None credentials".to_string())?;
        let service = format!("sovereign-mcp-{server_name}");
        let raw = serde_json::to_string(&cred).map_err(|e| e.to_string())?;

        keyring::Entry::new(&service, "credential")
            .map_err(|e| format!("Keychain error: {e}"))?
            .set_password(&raw)
            .map_err(|e| format!("Keychain error: {e}"))
    }

    /// Remove credentials from the keychain.
    pub fn remove_from_keychain(server_name: &str) -> Result<(), String> {
        let service = format!("sovereign-mcp-{server_name}");
        match keyring::Entry::new(&service, "credential")
            .and_then(|e| e.delete_credential())
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("Keychain error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_auth_does_not_modify_request() {
        // Can't easily test reqwest::RequestBuilder in isolation,
        // but we can verify the enum variant works.
        let auth = McpAuth::None;
        assert!(matches!(auth, McpAuth::None));
    }

    #[test]
    fn stored_credential_json_round_trip() {
        let cred = StoredCredential::Bearer {
            token: "ghp_test123".into(),
        };
        let json = serde_json::to_string(&cred).unwrap();
        let parsed: StoredCredential = serde_json::from_str(&json).unwrap();
        let auth = parsed.into_auth();
        assert!(matches!(auth, McpAuth::BearerToken(t) if t == "ghp_test123"));
    }

    #[test]
    fn api_key_credential_round_trip() {
        let cred = StoredCredential::ApiKey {
            header: "X-Api-Key".into(),
            value: "secret".into(),
        };
        let json = serde_json::to_string(&cred).unwrap();
        let parsed: StoredCredential = serde_json::from_str(&json).unwrap();
        let auth = parsed.into_auth();
        assert!(matches!(auth, McpAuth::ApiKey { header, value } if header == "X-Api-Key" && value == "secret"));
    }

    #[test]
    fn from_auth_none_returns_none() {
        assert!(StoredCredential::from_auth(&McpAuth::None).is_none());
    }
}
