// SPDX-License-Identifier: AGPL-3.0-or-later
//! Is the daemon up, and what will it answer with?
//!
//! Liveness probe plus default chat/embed model resolution off `/v1/models`.
//! Free functions, not methods: every caller runs these BEFORE it has a
//! client to ask.

use std::time::Duration;

/// Readiness probe — returns `true` iff `GET /v1/models` responds
/// 200 within 500ms. Used by `enrich init` / `extract` to fail early
/// if the daemon isn't running.
pub async fn probe_daemon(base_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    let url = if base_url.ends_with("/v1/models") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1/models")
    };
    client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Enumerate the daemon's registered models. Returns `(chat_model, embed_model)`
/// heuristically — the first chat-capable ID and the first embedding ID — or
/// `(None, None)` on any failure.
///
/// The `/v1/models` endpoint doesn't carry capability tags consistently across
/// backends, so we fall back to name-pattern matching: anything containing
/// `"embedding"` or `"-embed"` is classed as embed; everything else is chat.
pub async fn resolve_default_models(base_url: &str) -> (Option<String>, Option<String>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    // The CONFIGURED client port when the caller supplied no base — the
    // compiled default reached the wrong daemon (or the operator's) on any
    // host that moved `client_port`.
    let url = format!(
        "{}/v1/models",
        sovereign_contracts::setup_config::client_daemon_base()
    );
    // If caller gave us a non-default base, use their URL.
    let url = if base_url.contains("://") && !base_url.ends_with("/v1/models") {
        format!("{}/v1/models", base_url.trim_end_matches('/'))
    } else {
        url
    };
    let Ok(resp) = client.get(&url).send().await else {
        return (None, None);
    };
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return (None, None);
    };
    pick_default_models_from_v1(&v)
}

/// Pure parser over a `/v1/models` payload — split out from the
/// HTTP path so the alias-preference logic is unit-testable.
///
/// Resolves aliases first so the LOCAL primary always wins over
/// mesh-advertised peer models. `/v1/models` aggregates local +
/// peer manifests, so the first non-embed id may be a peer's
/// primary (alphabetical or order-of-arrival) and baking that
/// into a corpus config makes every subsequent `enrich build`
/// request a model this node can't serve.
///
/// The daemon exposes `commonwealth/primary` and
/// `commonwealth/embed` as stable aliases pointing at the
/// local-only models. We pick those first, then walk the rest
/// as a fallback (e.g. a peer-only mesh where the local slot
/// isn't loaded, or an older daemon without the alias surface).
fn pick_default_models_from_v1(v: &serde_json::Value) -> (Option<String>, Option<String>) {
    let Some(arr) = v.get("data").and_then(|d| d.as_array()) else {
        return (None, None);
    };

    let mut chat = None;
    let mut embed = None;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        // Return the alias itself, NOT its resolved GGUF target.
        // Storing `commonwealth/primary` in corpus configs lets
        // the daemon route across the mesh (any peer with a Slow
        // slot loaded can serve `commonwealth/primary`) and makes
        // local model swaps transparent — no per-corpus config
        // rewrites when the underlying GGUF changes.
        if id == "commonwealth/primary" && chat.is_none() {
            chat = Some(id.to_string());
        } else if id == "commonwealth/embed" && embed.is_none() {
            embed = Some(id.to_string());
        }
    }
    if chat.is_none() || embed.is_none() {
        for m in arr {
            let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
                continue;
            };
            if id.starts_with("commonwealth/") {
                continue;
            }
            let lower = id.to_lowercase();
            let is_embed = lower.contains("embedding") || lower.contains("-embed");
            if is_embed {
                if embed.is_none() {
                    embed = Some(id.to_string());
                }
            } else if chat.is_none() {
                chat = Some(id.to_string());
            }
        }
    }
    (chat, embed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_daemon_returns_false_for_unreachable_host() {
        // Port 1 is reserved and never listening.
        assert!(!probe_daemon("http://127.0.0.1:1").await);
    }

    #[tokio::test]
    async fn resolve_default_models_returns_none_on_unreachable() {
        let (chat, embed) = resolve_default_models("http://127.0.0.1:1").await;
        assert!(chat.is_none());
        assert!(embed.is_none());
    }

    #[test]
    fn pick_default_returns_alias_not_underlying_gguf() {
        // The whole point: enrich corpus configs should store
        // the mesh-stable alias, not the resolved GGUF. Peers
        // advertise `commonwealth/primary` from their own
        // self-manifest (each pointing at its own local Slow
        // slot), so a request for `commonwealth/primary` can
        // route to either node. If we stored the GGUF id here,
        // every model swap on either machine would invalidate
        // every corpus config — that's the brittleness we're
        // fixing.
        let payload = serde_json::json!({
            "data": [
                {"id": "FINAL-Bench_Darwin-36B-Opus-Q6_K", "owned_by": "mesh"},
                {"id": "FINAL-Bench_Darwin-36B-Opus-Q4_K_L", "owned_by": "mesh"},
                {"id": "Qwen3-Embedding-0.6B-Q8_0", "owned_by": "mesh"},
                {"id": "commonwealth/primary", "owned_by": "alias→FINAL-Bench_Darwin-36B-Opus-Q4_K_L"},
                {"id": "commonwealth/embed", "owned_by": "alias→Qwen3-Embedding-0.6B-Q8_0"},
            ]
        });
        let (chat, embed) = pick_default_models_from_v1(&payload);
        assert_eq!(chat.as_deref(), Some("commonwealth/primary"));
        assert_eq!(embed.as_deref(), Some("commonwealth/embed"));
    }

    #[test]
    fn pick_default_falls_back_to_first_non_embed_without_alias() {
        // Older daemon / minimal config: no `commonwealth/*`
        // aliases present. Resolver walks the list and grabs
        // the first non-embed for chat, first embed for embed.
        // The GGUF id is the right answer in this case — the
        // daemon doesn't know about the alias namespace at all.
        let payload = serde_json::json!({
            "data": [
                {"id": "Qwen3-Embedding-0.6B-Q8_0", "owned_by": "mesh"},
                {"id": "Darwin-9B-Opus.Q8_0", "owned_by": "mesh"},
            ]
        });
        let (chat, embed) = pick_default_models_from_v1(&payload);
        assert_eq!(chat.as_deref(), Some("Darwin-9B-Opus.Q8_0"));
        assert_eq!(embed.as_deref(), Some("Qwen3-Embedding-0.6B-Q8_0"));
    }

    #[test]
    fn pick_default_skips_commonwealth_namespace_in_fallback() {
        // Edge case: `commonwealth/fast` is present but
        // `commonwealth/primary` is not (operator misconfig).
        // The fallback should still skip every `commonwealth/*`
        // id rather than picking `commonwealth/fast` as chat —
        // we only know `commonwealth/primary` and `commonwealth/embed`
        // are the canonical chat/embed aliases.
        let payload = serde_json::json!({
            "data": [
                {"id": "commonwealth/fast", "owned_by": "alias→some-fast-model"},
                {"id": "Darwin-36B-Opus", "owned_by": "mesh"},
            ]
        });
        let (chat, _) = pick_default_models_from_v1(&payload);
        assert_eq!(chat.as_deref(), Some("Darwin-36B-Opus"));
    }
}
