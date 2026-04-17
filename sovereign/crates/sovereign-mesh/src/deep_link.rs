//! Deep link parsing for `sovereign://join/...` URLs.
//!
//! Links carry the join key, optionally a relay hint for remote discovery,
//! and are registered as a URL scheme on install (macOS, Windows, Linux).
//!
//! Also accepts user-facing forms the CLI might receive:
//! - Bare join key: `cwth-XXXX-XXXX-XXXX` (typed into a terminal)
//! - HTTPS URL: `https://sovereign.dev/join/<key>` (click-from-email)
//! - Deep link: `sovereign://join/<key>` (click-from-native app)
//!
//! Use [`parse_join_argument`] when accepting user input; it tries all
//! three forms in order. The host for the HTTPS form is overridable via
//! the `SOVEREIGN_JOIN_HOST` environment variable (useful for dev /
//! testing against a staging domain).

use crate::types::JoinConfirmation;

/// Default host for the HTTPS join link form. Overridable via the
/// `SOVEREIGN_JOIN_HOST` environment variable.
const DEFAULT_HTTPS_HOST: &str = "sovereign.dev";

/// A parsed deep link.
#[derive(Debug, Clone)]
pub enum DeepLink {
    /// Join a mesh: `sovereign://join/<join-key>[?relay=<host>&name=<mesh-name>]`
    Join {
        join_key: String,
        relay_hint: Option<String>,
        mesh_name: Option<String>,
    },
}

/// Parse a `sovereign://` deep link URL.
///
/// Supported formats:
/// - `sovereign://join/cwth-7f3a-9b2e-4d1c`
/// - `sovereign://join/cwth-7f3a-9b2e-4d1c?relay=192.168.1.100&name=Lab+Squad`
pub fn parse_deep_link(url: &str) -> Option<DeepLink> {
    let stripped = url
        .strip_prefix("sovereign://")
        .or_else(|| url.strip_prefix("sovereign:"))?;
    let stripped = stripped.trim_start_matches('/');

    // Split path and query.
    let (path, query) = match stripped.find('?') {
        Some(idx) => (&stripped[..idx], Some(&stripped[idx + 1..])),
        None => (stripped, None),
    };

    let parts: Vec<&str> = path.split('/').collect();
    if parts.is_empty() {
        return None;
    }

    match parts[0] {
        "join" if parts.len() >= 2 => {
            let join_key = parts[1].to_string();
            let params = parse_query_params(query);
            let relay_hint = params.get("relay").cloned();
            let mesh_name = params
                .get("name")
                .map(|n| n.replace('+', " "));

            Some(DeepLink::Join {
                join_key,
                relay_hint,
                mesh_name,
            })
        }
        _ => None,
    }
}

/// Parse an HTTPS join URL: `https://sovereign.dev/join/<key>[?...]`.
///
/// The host is validated against `SOVEREIGN_JOIN_HOST` (or the
/// default) so pasting a random https URL doesn't silently mis-route.
/// Query parameters (`relay`, `name`) share semantics with the
/// `sovereign://` scheme.
pub fn parse_https_join(url: &str) -> Option<DeepLink> {
    let expected_host =
        std::env::var("SOVEREIGN_JOIN_HOST").unwrap_or_else(|_| DEFAULT_HTTPS_HOST.to_string());
    let prefix = format!("https://{expected_host}/join/");
    let stripped = url.strip_prefix(&prefix)?;

    // Reuse the same path/query split as parse_deep_link so semantics
    // stay consistent across transports.
    let (path, query) = match stripped.find('?') {
        Some(idx) => (&stripped[..idx], Some(&stripped[idx + 1..])),
        None => (stripped, None),
    };

    // The key may end at the first `/` if the URL has extra path
    // segments (defensive; the canonical form has exactly one segment).
    let join_key = path.split('/').next()?;
    if join_key.is_empty() {
        return None;
    }

    let params = parse_query_params(query);
    Some(DeepLink::Join {
        join_key: join_key.to_string(),
        relay_hint: params.get("relay").cloned(),
        mesh_name: params.get("name").map(|n| n.replace('+', " ")),
    })
}

/// Accept any of the three user-facing join forms (bare key, HTTPS URL,
/// or `sovereign://` deep link) and return a [`DeepLink::Join`]. Returns
/// `None` if the input doesn't match any known form.
///
/// Order of attempt:
/// 1. `parse_deep_link` (covers the `sovereign://` scheme)
/// 2. `parse_https_join` (covers the https URL form)
/// 3. `validate_join_key_format` (covers a bare `cwth-XXXX-XXXX-XXXX`)
pub fn parse_join_argument(arg: &str) -> Option<DeepLink> {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(link) = parse_deep_link(trimmed) {
        return Some(link);
    }
    if let Some(link) = parse_https_join(trimmed) {
        return Some(link);
    }

    // Bare key fallback: validate format to avoid silently accepting
    // arbitrary strings.
    if commonwealth_discovery::membership::validate_join_key_format(trimmed).is_ok() {
        return Some(DeepLink::Join {
            join_key: trimmed.to_string(),
            relay_hint: None,
            mesh_name: None,
        });
    }
    None
}

/// Build an HTTPS join link for the shareable URL form.
/// Host defaults to `sovereign.dev` (override via `SOVEREIGN_JOIN_HOST`).
pub fn build_https_join_link(
    join_key: &str,
    relay_hint: Option<&str>,
    mesh_name: Option<&str>,
) -> String {
    let host =
        std::env::var("SOVEREIGN_JOIN_HOST").unwrap_or_else(|_| DEFAULT_HTTPS_HOST.to_string());
    let mut url = format!("https://{host}/join/{join_key}");
    let mut params = Vec::new();
    if let Some(relay) = relay_hint {
        params.push(format!("relay={relay}"));
    }
    if let Some(name) = mesh_name {
        let encoded = name.replace(' ', "+");
        params.push(format!("name={encoded}"));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

/// Build a deep link URL for joining a mesh.
pub fn build_join_link(
    join_key: &str,
    relay_hint: Option<&str>,
    mesh_name: Option<&str>,
) -> String {
    let mut url = format!("sovereign://join/{join_key}");
    let mut params = Vec::new();

    if let Some(relay) = relay_hint {
        params.push(format!("relay={relay}"));
    }
    if let Some(name) = mesh_name {
        let encoded = name.replace(' ', "+");
        params.push(format!("name={encoded}"));
    }

    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }

    url
}

/// Build a join confirmation from a parsed deep link.
pub fn join_confirmation_from_link(link: &DeepLink) -> Option<JoinConfirmation> {
    match link {
        DeepLink::Join {
            join_key,
            relay_hint,
            mesh_name,
        } => Some(JoinConfirmation {
            mesh_name: mesh_name
                .clone()
                .unwrap_or_else(|| "Unknown Mesh".to_string()),
            invited_by: None,
            join_key: join_key.clone(),
            relay_hint: relay_hint.clone(),
        }),
    }
}

fn parse_query_params(query: Option<&str>) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                map.insert(key.to_string(), value.to_string());
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_join_link() {
        let link = parse_deep_link("sovereign://join/cwth-7f3a-9b2e-4d1c").unwrap();
        match link {
            DeepLink::Join {
                join_key,
                relay_hint,
                mesh_name,
            } => {
                assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
                assert!(relay_hint.is_none());
                assert!(mesh_name.is_none());
            }
        }
    }

    #[test]
    fn parse_join_link_with_params() {
        let link = parse_deep_link(
            "sovereign://join/cwth-7f3a-9b2e-4d1c?relay=192.168.1.100&name=Lab+Squad",
        )
        .unwrap();
        match link {
            DeepLink::Join {
                join_key,
                relay_hint,
                mesh_name,
            } => {
                assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
                assert_eq!(relay_hint.as_deref(), Some("192.168.1.100"));
                assert_eq!(mesh_name.as_deref(), Some("Lab Squad"));
            }
        }
    }

    #[test]
    fn parse_invalid_link() {
        assert!(parse_deep_link("https://example.com").is_none());
        assert!(parse_deep_link("sovereign://unknown/foo").is_none());
        assert!(parse_deep_link("sovereign://join").is_none());
    }

    #[test]
    fn build_and_parse_round_trip() {
        let url = build_join_link(
            "cwth-abcd-efgh-ijkl",
            Some("10.0.0.5"),
            Some("My Mesh"),
        );
        assert_eq!(
            url,
            "sovereign://join/cwth-abcd-efgh-ijkl?relay=10.0.0.5&name=My+Mesh"
        );

        let link = parse_deep_link(&url).unwrap();
        match link {
            DeepLink::Join {
                join_key,
                relay_hint,
                mesh_name,
            } => {
                assert_eq!(join_key, "cwth-abcd-efgh-ijkl");
                assert_eq!(relay_hint.as_deref(), Some("10.0.0.5"));
                assert_eq!(mesh_name.as_deref(), Some("My Mesh"));
            }
        }
    }

    #[test]
    fn parse_https_simple() {
        let link = parse_https_join("https://sovereign.dev/join/cwth-7f3a-9b2e-4d1c").unwrap();
        let DeepLink::Join { join_key, relay_hint, mesh_name } = link;
        assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
        assert!(relay_hint.is_none());
        assert!(mesh_name.is_none());
    }

    #[test]
    fn parse_https_with_params() {
        let link = parse_https_join(
            "https://sovereign.dev/join/cwth-7f3a-9b2e-4d1c?relay=10.0.0.5&name=Lab+Squad",
        )
        .unwrap();
        let DeepLink::Join { join_key, relay_hint, mesh_name } = link;
        assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
        assert_eq!(relay_hint.as_deref(), Some("10.0.0.5"));
        assert_eq!(mesh_name.as_deref(), Some("Lab Squad"));
    }

    #[test]
    fn parse_https_rejects_wrong_host() {
        // Host must match SOVEREIGN_JOIN_HOST (or default). A random
        // https URL on a different domain must not be accepted.
        assert!(parse_https_join("https://evil.example.com/join/cwth-0000-1111-2222").is_none());
    }

    #[test]
    fn parse_join_argument_accepts_bare_key() {
        let link = parse_join_argument("cwth-7f3a-9b2e-4d1c").unwrap();
        let DeepLink::Join { join_key, .. } = link;
        assert_eq!(join_key, "cwth-7f3a-9b2e-4d1c");
    }

    #[test]
    fn parse_join_argument_accepts_https() {
        let link =
            parse_join_argument("https://sovereign.dev/join/cwth-abcd-ef01-2345").unwrap();
        let DeepLink::Join { join_key, .. } = link;
        assert_eq!(join_key, "cwth-abcd-ef01-2345");
    }

    #[test]
    fn parse_join_argument_accepts_scheme() {
        let link = parse_join_argument("sovereign://join/cwth-1111-2222-3333").unwrap();
        let DeepLink::Join { join_key, .. } = link;
        assert_eq!(join_key, "cwth-1111-2222-3333");
    }

    #[test]
    fn parse_join_argument_rejects_garbage() {
        assert!(parse_join_argument("").is_none());
        assert!(parse_join_argument("not-a-key").is_none());
        assert!(parse_join_argument("https://example.com/other").is_none());
    }

    #[test]
    fn build_https_join_link_round_trip() {
        let url = build_https_join_link(
            "cwth-abcd-ef01-2345",
            Some("10.0.0.5"),
            Some("My Mesh"),
        );
        assert_eq!(
            url,
            "https://sovereign.dev/join/cwth-abcd-ef01-2345?relay=10.0.0.5&name=My+Mesh"
        );
        let link = parse_https_join(&url).unwrap();
        let DeepLink::Join { join_key, relay_hint, mesh_name } = link;
        assert_eq!(join_key, "cwth-abcd-ef01-2345");
        assert_eq!(relay_hint.as_deref(), Some("10.0.0.5"));
        assert_eq!(mesh_name.as_deref(), Some("My Mesh"));
    }

    #[test]
    fn join_confirmation_from_parsed_link() {
        let link = DeepLink::Join {
            join_key: "cwth-test".into(),
            relay_hint: Some("relay.example.com".into()),
            mesh_name: Some("Test Mesh".into()),
        };
        let confirm = join_confirmation_from_link(&link).unwrap();
        assert_eq!(confirm.mesh_name, "Test Mesh");
        assert_eq!(confirm.join_key, "cwth-test");
        assert_eq!(confirm.relay_hint.as_deref(), Some("relay.example.com"));
    }
}
