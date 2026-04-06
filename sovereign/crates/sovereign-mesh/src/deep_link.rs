//! Deep link parsing for `sovereign://join/...` URLs.
//!
//! Links carry the join key, optionally a relay hint for remote discovery,
//! and are registered as a URL scheme on install (macOS, Windows, Linux).

use crate::types::JoinConfirmation;

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
