// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which models the daemon serves — and, for the embed slot, whether it
//! actually answers.
//!
//! # Why one module (ARCH §10.6)
//!
//! Until 2026-09-01 "which embed model does the daemon have" was decided in
//! three places that disagreed: `chat_cmd::bootstrap` read the configured
//! GGUF stem first and fell back to an `embedding`/`-embed` substring over
//! `/v1/models`; `recipe_cmd` and this crate's `discover_models` looked
//! only for an `embed` substring in `/v1/models`. A daemon whose model
//! listing carries only chat ids — the shape observed on 2026-09-01, where
//! `/v1/models` listed two chat models and `POST /v1/embeddings` returned a
//! 1024-dim vector — therefore made `svrn chat` work and `svrn corpus
//! ingest` refuse with "advertises no embedding model". Same daemon, two
//! verdicts, and the refusing one named a test (the id substring) that was
//! never the capability in question.
//!
//! This module is the one decider. The candidate order is chat's —
//! explicit flag, then the configured stem, then an embedding-like
//! advertised id — and the VERDICT is a `/v1/embeddings` probe, because an
//! id substring is a guess about a name and the probe is the capability
//! itself (§18.4: validate the instrument). A refusal says what was probed
//! and what failed.

use std::time::Duration;

use sovereign_contracts::setup_config::SetupConfig;

/// The chat + embed model ids the daemon advertises.
pub struct DaemonModels {
    /// The first advertised id that is not embedding-like.
    pub chat: Option<String>,
    /// The embed id [`resolve_embed_model`] WOULD try — configured stem
    /// first, else an embedding-like advertised id. Named a candidate
    /// because nothing here has proved it answers; a caller that is about
    /// to embed calls [`resolve_embed_model`], which does.
    pub embed_candidate: Option<String>,
}

/// Where a resolved embed id came from — named in the probe's failure so
/// the operator knows which knob to turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedSource {
    /// A `--embed-model` flag or an equivalent caller-supplied id.
    Explicit,
    /// `[models] embed` in `~/.svrnmesh/config.toml` (or, on a terminal,
    /// the entry node's recorded embed id).
    Configured,
    /// An embedding-like id on `/v1/models`.
    Advertised,
}

impl std::fmt::Display for EmbedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EmbedSource::Explicit => "the --embed-model flag",
            EmbedSource::Configured => "`[models] embed` in ~/.svrnmesh/config.toml",
            EmbedSource::Advertised => "an embedding-like id on /v1/models",
        })
    }
}

/// An embed model the daemon has been SEEN to answer with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEmbedModel {
    /// The id to send as `model` on every `/v1/embeddings` call.
    pub id: String,
    /// Vector width the probe came back with.
    pub dimensions: usize,
    /// Which rung of the ladder named the id.
    pub source: EmbedSource,
}

/// The one id-shape test left in the workspace, kept only to CHOOSE a
/// candidate and to keep the chat pick off embedding ids — never to decide
/// whether the daemon can embed.
pub fn looks_like_embed_model(id: &str) -> bool {
    id.to_ascii_lowercase().contains("embed")
}

/// The candidate ladder, pure: explicit → configured → advertised.
pub fn embed_candidate(
    explicit: Option<&str>,
    configured: Option<&str>,
    advertised: &[String],
) -> Option<(String, EmbedSource)> {
    if let Some(id) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Some((id.to_string(), EmbedSource::Explicit));
    }
    if let Some(stem) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        return Some((stem.to_string(), EmbedSource::Configured));
    }
    advertised
        .iter()
        .find(|id| looks_like_embed_model(id))
        .map(|id| (id.clone(), EmbedSource::Advertised))
}

/// The configured embed id, when the config can name one.
///
/// `local_embed_model_id`, not `embed_model_stem`: a terminal embeds by
/// forwarding to its entry node, and that node's recorded id is the space
/// its vectors land in (the distinction `build_daemon_embed_fn` documents).
pub fn configured_embed_model() -> Option<String> {
    SetupConfig::load().ok()?.local_embed_model_id()
}

/// GET `<v1>/models` — the ids the daemon advertises. Doubles as the
/// liveness probe: a connection error is "start the daemon".
async fn advertised_ids(v1: &str) -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{v1}/models");
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("/v1/models → HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let models = body
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| "malformed /v1/models response".to_string())?;
    Ok(models
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect())
}

/// GET `<v1>/models` and split the advertised ids into the chat pick and
/// the embed CANDIDATE. See [`DaemonModels`] for why the embed half is not
/// a verdict.
pub async fn discover_models(v1: &str) -> Result<DaemonModels, String> {
    let ids = advertised_ids(v1).await?;
    if ids.is_empty() {
        return Err("the daemon advertises no models".to_string());
    }
    let chat = ids.iter().find(|id| !looks_like_embed_model(id)).cloned();
    let embed_candidate =
        embed_candidate(None, configured_embed_model().as_deref(), &ids).map(|(id, _)| id);
    Ok(DaemonModels {
        chat,
        embed_candidate,
    })
}

/// Resolve the embed model the daemon at `v1` answers with, and PROVE it:
/// explicit → configured → advertised for the candidate, then one
/// `POST /v1/embeddings` with that id. `Err` names what was tried and what
/// failed, so the operator is never told "no embedding model" by a check
/// that never asked for an embedding.
pub async fn resolve_embed_model(
    v1: &str,
    explicit: Option<&str>,
) -> Result<ResolvedEmbedModel, String> {
    resolve_embed_model_with(v1, explicit, configured_embed_model().as_deref()).await
}

/// [`resolve_embed_model`] with the configured id passed in, so the ladder
/// and the probe are testable without a `~/.svrnmesh/config.toml`.
pub async fn resolve_embed_model_with(
    v1: &str,
    explicit: Option<&str>,
    configured: Option<&str>,
) -> Result<ResolvedEmbedModel, String> {
    let advertised = advertised_ids(v1)
        .await
        .map_err(|e| format!("daemon at {v1} unreachable ({e}); is the daemon running?"))?;
    let Some((id, source)) = embed_candidate(explicit, configured, &advertised) else {
        let listed = if advertised.is_empty() {
            "nothing".to_string()
        } else {
            advertised.join(", ")
        };
        return Err(format!(
            "no embedding model to probe: `[models] embed` is not set in \
             ~/.svrnmesh/config.toml (and this node records no entry-node embed model), \
             and {v1}/models advertises no embedding-like id (advertised: {listed}). \
             Set `[models] embed` (svrn setup) or pass --embed-model."
        ));
    };
    let dimensions = probe_embeddings(v1, &id, source).await?;
    tracing::info!(
        target: "daemon_models",
        embed_model = %id,
        source = ?source,
        dimensions,
        "embed model resolved and probed"
    );
    Ok(ResolvedEmbedModel {
        id,
        dimensions,
        source,
    })
}

/// One real embedding call — the verdict. Returns the vector width.
async fn probe_embeddings(v1: &str, id: &str, source: EmbedSource) -> Result<usize, String> {
    let url = format!("{v1}/embeddings");
    let client = reqwest::Client::builder()
        // The embed slot can sit behind a busy chat slot on a loaded host;
        // a probe that gives up in 3s would refuse a daemon that works.
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "model": id, "input": "sovereign embed probe" }))
        .send()
        .await
        .map_err(|e| format!("embed probe POST {url} (model `{id}`, from {source}) failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let excerpt: String = body.trim().chars().take(300).collect();
        return Err(format!(
            "embed probe POST {url} (model `{id}`, from {source}) returned {status}: {excerpt}"
        ));
    }
    let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        format!(
            "embed probe POST {url} (model `{id}`, from {source}) returned a non-JSON body: {e}"
        )
    })?;
    let dimensions = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|d| d.first())
        .and_then(|d| d.get("embedding"))
        .and_then(|e| e.as_array())
        .map(|e| e.len())
        .unwrap_or(0);
    if dimensions == 0 {
        return Err(format!(
            "embed probe POST {url} (model `{id}`, from {source}) returned {status} but no \
             embedding vector"
        ));
    }
    Ok(dimensions)
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::routing::{get, post};
    use axum::{Json, Router};

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // ── The ladder, pure ────────────────────────────────────────────────

    #[test]
    fn explicit_beats_configured_beats_advertised() {
        let adv = ids(&["chat-a", "bge-embed"]);
        assert_eq!(
            embed_candidate(Some("mine"), Some("cfg"), &adv),
            Some(("mine".into(), EmbedSource::Explicit))
        );
        assert_eq!(
            embed_candidate(None, Some("cfg"), &adv),
            Some(("cfg".into(), EmbedSource::Configured))
        );
        assert_eq!(
            embed_candidate(None, None, &adv),
            Some(("bge-embed".into(), EmbedSource::Advertised))
        );
    }

    /// THE issue-#57 shape: `/v1/models` lists only chat ids. Chat resolved
    /// the configured stem and worked; ingest looked only at the listing and
    /// refused. One ladder now, and the listing is the LAST rung.
    #[test]
    fn a_chat_only_listing_still_yields_the_configured_stem() {
        let adv = ids(&["Qwen3.6-35B-A3B-UD-MTP-IQ4_NL", "Qwopus3.5-4B-v3-MTP-Q8_0"]);
        assert_eq!(
            embed_candidate(None, Some("qwen-embedding-0.6b"), &adv),
            Some(("qwen-embedding-0.6b".into(), EmbedSource::Configured))
        );
        assert_eq!(embed_candidate(None, None, &adv), None);
    }

    #[test]
    fn blank_explicit_and_configured_are_treated_as_absent() {
        let adv = ids(&["x-embedding"]);
        assert_eq!(
            embed_candidate(Some("  "), Some(""), &adv),
            Some(("x-embedding".into(), EmbedSource::Advertised))
        );
    }

    #[test]
    fn looks_like_embed_model_is_case_insensitive_and_substring() {
        assert!(looks_like_embed_model("Qwen3-Embedding-0.6B"));
        assert!(looks_like_embed_model("bge-embed"));
        assert!(!looks_like_embed_model("Qwopus3.5-4B"));
    }

    // ── The probe, against a fake daemon ────────────────────────────────

    /// A fake `/v1` that advertises `models` and answers `/embeddings` with
    /// `embed_status` — `Some(dim)` returns a vector of that width, `None`
    /// returns 503 with a reason, the way a daemon without an embed backend
    /// does (`routes_inference::embeddings`).
    async fn fake_daemon(models: &[&str], embed: Option<usize>) -> String {
        let listing = serde_json::json!({
            "object": "list",
            "data": models.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>(),
        });
        let app = Router::new()
            .route(
                "/v1/models",
                get(move || {
                    let listing = listing.clone();
                    async move { Json(listing) }
                }),
            )
            .route(
                "/v1/embeddings",
                post(move |Json(req): Json<serde_json::Value>| async move {
                    match embed {
                        Some(dim) => (
                            axum::http::StatusCode::OK,
                            Json(serde_json::json!({
                                "object": "list",
                                "model": req.get("model").cloned().unwrap_or_default(),
                                "data": [{ "object": "embedding", "index": 0,
                                           "embedding": vec![0.5_f32; dim] }],
                            })),
                        ),
                        None => (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({
                                "error": "this daemon has no local embedding backend"
                            })),
                        ),
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1")
    }

    /// The daemon observed on 2026-09-01: a chat-only listing, a working
    /// embed slot, a configured stem. Before this the ingest path refused
    /// it; now the configured id is probed and found alive.
    #[tokio::test]
    async fn configured_stem_over_a_chat_only_listing_resolves_when_the_probe_answers() {
        let v1 = fake_daemon(&["Qwen3.6-35B", "Qwopus3.5-4B"], Some(1024)).await;
        let r = resolve_embed_model_with(&v1, None, Some("qwen-embedding-0.6b"))
            .await
            .expect("the probe is the verdict, not the listing");
        assert_eq!(r.id, "qwen-embedding-0.6b");
        assert_eq!(r.dimensions, 1024);
        assert_eq!(r.source, EmbedSource::Configured);
    }

    #[tokio::test]
    async fn an_advertised_embed_id_is_used_when_nothing_is_configured() {
        let v1 = fake_daemon(&["chat-a", "Qwen3-Embedding-0.6B"], Some(8)).await;
        let r = resolve_embed_model_with(&v1, None, None).await.unwrap();
        assert_eq!(r.id, "Qwen3-Embedding-0.6B");
        assert_eq!(r.source, EmbedSource::Advertised);
        assert_eq!(r.dimensions, 8);
    }

    /// No candidate anywhere: the refusal names every rung it checked and
    /// what the listing DID carry, so the operator can tell "misconfigured"
    /// from "daemon down".
    #[tokio::test]
    async fn no_candidate_is_refused_naming_what_was_checked() {
        let v1 = fake_daemon(&["chat-a", "chat-b"], Some(8)).await;
        let err = resolve_embed_model_with(&v1, None, None)
            .await
            .expect_err("nothing to probe");
        assert!(err.contains("[models] embed"), "{err}");
        assert!(err.contains("/v1/models"), "{err}");
        assert!(err.contains("advertised: chat-a, chat-b"), "{err}");
        assert!(err.contains("--embed-model"), "{err}");
    }

    /// A candidate the daemon cannot serve: the refusal names the probe URL,
    /// the id, where the id came from, and the daemon's own words.
    #[tokio::test]
    async fn a_dead_embed_slot_is_refused_with_the_probe_named() {
        let v1 = fake_daemon(&["chat-a"], None).await;
        let err = resolve_embed_model_with(&v1, None, Some("qwen-embedding-0.6b"))
            .await
            .expect_err("the slot returned 503");
        assert!(err.contains("/v1/embeddings"), "{err}");
        assert!(err.contains("model `qwen-embedding-0.6b`"), "{err}");
        assert!(err.contains("[models] embed"), "{err}");
        assert!(err.contains("503"), "{err}");
        assert!(err.contains("no local embedding backend"), "{err}");
    }

    #[tokio::test]
    async fn an_unreachable_daemon_is_named_as_such() {
        let err = resolve_embed_model_with("http://127.0.0.1:9/v1", None, Some("x"))
            .await
            .expect_err("nothing listens on port 9");
        assert!(err.contains("unreachable"), "{err}");
        assert!(err.contains("is the daemon running"), "{err}");
    }

    #[tokio::test]
    async fn discover_models_splits_chat_from_the_embed_candidate() {
        let v1 = fake_daemon(&["chat-a", "bge-embed"], Some(8)).await;
        let m = discover_models(&v1).await.unwrap();
        assert_eq!(m.chat.as_deref(), Some("chat-a"));
        // With no config on this test host the candidate is the advertised
        // id; with one, the configured stem — either way it is a candidate,
        // and `resolve_embed_model` is the door for anyone who will embed.
        assert!(m.embed_candidate.is_some());
    }
}
