// SPDX-License-Identifier: AGPL-3.0-or-later
//! Startup diagnostics and UX helpers.

/// Query the Commonwealth daemon for mesh peer count and print a one-line
/// status summary.  Logs a warning if the daemon isn't reachable — activity
/// reporting and mesh routing will be disabled for this server session.
pub async fn print_mesh_status(commonwealth_url: &str) {
    match mesh_status_line(commonwealth_url).await {
        Some(line) => {
            eprintln!("  ⬡ {line}");
            tracing::info!(url = %commonwealth_url, "Commonwealth reachable");
        }
        None => {
            tracing::warn!(
                url = %commonwealth_url,
                "Commonwealth not reachable. Activity reporting disabled. \
                 Run `commonwealth daemon start` to enable mesh routing."
            );
        }
    }
}

/// Returns a human-readable mesh status line, or None if the daemon is
/// unreachable or returns an unexpected shape.
pub async fn mesh_status_line(commonwealth_url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;

    // Commonwealth daemon API port is 9741; sovereign-server typically
    // points at the inference port (9742).  Normalise either way.
    let api_url = commonwealth_url
        .trim_end_matches('/')
        .replace(":9742", ":9741");

    let status: serde_json::Value = client
        .get(format!("{api_url}/status"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    parse_status_json(&status)
}

/// Parse a /status JSON response into a human-readable status line.
/// Extracted so unit tests can exercise the formatting logic without
/// spinning up an HTTP server.
fn parse_status_json(status: &serde_json::Value) -> Option<String> {
    let online = status["mesh"]["online_members"].as_u64()?;
    let peer_word = if online == 1 { "node" } else { "nodes" };

    let model_hint = best_available_model(status)
        .map(|m| format!(" · best model: {m}"))
        .unwrap_or_default();

    Some(format!("{online} {peer_word} online on mesh{model_hint}"))
}

/// Extract the highest-scoring available model name from the /status payload.
/// Expects `status.oicp.models[].{id, status.available}`.
fn best_available_model(status: &serde_json::Value) -> Option<String> {
    let models = status["oicp"]["models"].as_array()?;
    models
        .iter()
        .filter(|m| m["status"]["available"].as_bool().unwrap_or(false))
        .filter_map(|m| m["id"].as_str())
        .next()
        .map(|s| s.to_string())
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn status_json(online: u64) -> serde_json::Value {
        serde_json::json!({ "mesh": { "online_members": online } })
    }

    fn status_json_with_model(online: u64, model_id: &str, available: bool) -> serde_json::Value {
        serde_json::json!({
            "mesh": { "online_members": online },
            "oicp": {
                "models": [{ "id": model_id, "status": { "available": available } }]
            }
        })
    }

    #[test]
    fn singular_node_uses_node_not_nodes() {
        let line = parse_status_json(&status_json(1)).unwrap();
        assert_eq!(line, "1 node online on mesh");
    }

    #[test]
    fn plural_nodes_uses_nodes() {
        let line = parse_status_json(&status_json(5)).unwrap();
        assert_eq!(line, "5 nodes online on mesh");
    }

    #[test]
    fn model_hint_appended_when_available() {
        let json = status_json_with_model(3, "qwen3-coder-30b", true);
        let line = parse_status_json(&json).unwrap();
        assert_eq!(line, "3 nodes online on mesh · best model: qwen3-coder-30b");
    }

    #[test]
    fn unavailable_model_is_excluded_from_hint() {
        let json = status_json_with_model(2, "huge-model", false);
        let line = parse_status_json(&json).unwrap();
        assert!(
            !line.contains("best model"),
            "unavailable model must not appear in hint"
        );
    }

    #[test]
    fn missing_online_members_returns_none() {
        let json = serde_json::json!({ "mesh": {} });
        assert!(parse_status_json(&json).is_none());
    }

    #[test]
    fn best_available_skips_unavailable_entries() {
        let json = serde_json::json!({
            "oicp": {
                "models": [
                    { "id": "big-model", "status": { "available": false } },
                    { "id": "small-model", "status": { "available": true } }
                ]
            }
        });
        let model = best_available_model(&json).unwrap();
        assert_eq!(model, "small-model");
    }
}
