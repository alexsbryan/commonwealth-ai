// SPDX-License-Identifier: AGPL-3.0-or-later
//! Is the daemon up, and what will it answer with?
//!
//! Liveness probe plus default chat/embed model resolution off `/v1/models`.
//! Free functions, not methods: every caller runs these BEFORE it has a
//! client to ask.

use std::time::Duration;

/// The ONE budget this module allows for `GET /v1/models` — the same
/// figure `resolve_default_models` below has always used. The daemon
/// answers `/v1/models` in 0.80–0.86 s under a 2-lane pool load
/// (measured 2026-09-17, three probes through the ersilia proxy on
/// 9740; ~0.80 s direct before that), so 5 s is >5x headroom. The
/// readiness probe's old 500 ms killed cold `enrich build` runs at
/// extract while the daemon was in fact serving (order
/// enrich-probe-timeout).
pub(crate) const V1_MODELS_TIMEOUT: Duration = Duration::from_secs(5);

/// What a readiness probe learned about the daemon. Split so a SLOW
/// daemon is never reported to the user as a DOWN one — "did not
/// answer within the budget" is not "not responding" (order
/// enrich-probe-timeout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonProbe {
    /// Answered 200 within the budget.
    Up,
    /// No answer within the budget — the daemon may be up under load.
    Slow,
    /// Connection refused (or another immediate failure) — treat as
    /// not running.
    Down,
}

/// Classification core. `budget` is a plain parameter, NOT a config
/// surface: it exists so tests can watch the timeout path in
/// milliseconds instead of waiting the production budget. The one
/// production budget is [`V1_MODELS_TIMEOUT`], applied by
/// [`probe_daemon_status`].
async fn probe_with_budget(base_url: &str, budget: Duration) -> DaemonProbe {
    let Ok(client) = reqwest::Client::builder()
        .timeout(budget)
        .build()
    else {
        return DaemonProbe::Down;
    };
    let url = if base_url.ends_with("/v1/models") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1/models")
    };
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => DaemonProbe::Up,
        // Answered, but not 200 — unusable, and not slow: same
        // not-up verdict the bool probe has always folded this into.
        Ok(_) => DaemonProbe::Down,
        // The budget elapsed with no answer at all.
        Err(e) if e.is_timeout() => DaemonProbe::Slow,
        // Refused, DNS, TLS — everything that fails fast is down.
        Err(_) => DaemonProbe::Down,
    }
}

/// Three-way readiness probe for this crate's callers that report the
/// failure to a human: `Up` iff `GET /v1/models` answers 200 within
/// [`V1_MODELS_TIMEOUT`], `Slow` if the budget elapsed with no answer,
/// `Down` for refused/immediate failures.
pub(crate) async fn probe_daemon_status(base_url: &str) -> DaemonProbe {
    probe_with_budget(base_url, V1_MODELS_TIMEOUT).await
}

/// Readiness probe — returns `true` iff `GET /v1/models` responds
/// 200 within [`V1_MODELS_TIMEOUT`]. Used by `enrich init` / `extract`
/// to fail early if the daemon isn't running.
///
/// The bool folds `Slow` into `false`: roughly twenty call sites in
/// `sovereign-cli-llm` reach this function through the
/// `pub use sovereign_enrichment_build::{… inference_client …}`
/// re-export, so its signature is frozen — callers that need to name
/// slow-vs-down use [`probe_daemon_status`] instead.
pub async fn probe_daemon(base_url: &str) -> bool {
    matches!(probe_daemon_status(base_url).await, DaemonProbe::Up)
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
        .timeout(V1_MODELS_TIMEOUT)
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
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    /// A TCP server that accepts one connection and answers a valid
    /// empty HTTP 200 — just enough HTTP for the probe's status check.
    fn http_200_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                // Read the request head first, then answer — answering
                // before the request arrives is a race some clients
                // reject, and a real daemon never runs it.
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                // Hold the connection briefly so reqwest can finish
                // reading the response before the socket closes.
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        format!("http://{addr}")
    }

    /// A TCP server that accepts and then never writes — the
    /// daemon-under-load shape: connection up, no answer within any
    /// budget we'd pass.
    fn hanging_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((sock, _)) = listener.accept() {
                // Hold the accepted socket open across the sleep —
                // closing it would end the connection early.
                let _held = sock;
                std::thread::sleep(Duration::from_secs(10));
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn probe_daemon_returns_false_for_unreachable_host() {
        // Port 1 is reserved and never listening.
        assert!(!probe_daemon("http://127.0.0.1:1").await);
    }

    #[tokio::test]
    async fn status_refused_host_is_down() {
        assert_eq!(
            probe_daemon_status("http://127.0.0.1:1").await,
            DaemonProbe::Down
        );
    }

    #[tokio::test]
    async fn status_hanging_server_is_slow() {
        // Short budget so the timeout path is watched in milliseconds;
        // the production budget is pinned separately, below.
        let base = hanging_server();
        assert_eq!(
            probe_with_budget(&base, Duration::from_millis(250)).await,
            DaemonProbe::Slow
        );
    }

    #[tokio::test]
    async fn status_http_200_is_up() {
        let base = http_200_server();
        assert_eq!(
            probe_with_budget(&base, Duration::from_secs(2)).await,
            DaemonProbe::Up
        );
    }

    #[test]
    fn v1_models_budget_is_five_seconds() {
        // The module's ONE /v1/models budget: the resolve_default_models
        // precedent, >5x headroom over the measured 0.86 s latency.
        assert_eq!(V1_MODELS_TIMEOUT, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn status_uses_the_module_budget_not_the_old_500ms() {
        // Against the old 500 ms budget the probe returns at ~0.5 s and
        // the 2 s outer timeout does NOT fire; against the 5 s budget
        // the probe is still waiting at 2 s. This is the assertion the
        // timeout raise was watched red against.
        let base = hanging_server();
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            probe_daemon_status(&base),
        )
        .await;
        assert!(
            outcome.is_err(),
            "probe returned before 2s — the module budget is not applied"
        );
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
