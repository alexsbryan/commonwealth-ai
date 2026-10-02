// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon-backed HTTP provider over the daemon's resolved `(chat, embed)`
//! ids: probe `/v1/models`, resolve the ids, build a
//! [`SplitInferenceProvider`]. Moved from sovereign-cli-llm's
//! `chat_cmd::bootstrap` (pb-cli-llm-bench-move) so bench's subject dial and
//! svrn's chat bootstrap build it the same way; `chat_cmd::bootstrap::
//! build_inference` adapts `ChatGlobals` onto [`build_inference`].

use std::sync::Arc;
use std::time::Duration;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::InferenceProvider;

use crate::daemon_models::{looks_like_embed_model, resolve_embed_model};
use crate::SplitInferenceProvider;

/// Probe the daemon at `daemon_base`, resolve its `(chat, embed)` model ids,
/// and build the HTTP `InferenceProvider` over them. Returns `(inference,
/// daemon_base, embed_model_id)`. `bearer`, `guest_link_active` and
/// `guest_lender_url` are the guest-link facts (`None`/`false` for the
/// ordinary loopback caller); `chat_model`/`embed_model` are the explicit
/// `--chat-model`/`--embed-model` flags.
pub async fn build_inference(
    daemon_base: &str,
    bearer: Option<&str>,
    chat_model: Option<&str>,
    embed_model: Option<&str>,
    guest_link_active: bool,
    guest_lender_url: Option<&str>,
) -> Result<(Arc<dyn InferenceProvider>, String, String)> {
    // 1. Probe the daemon before we touch anything else. A fast fail
    //    here prints a clean "start the daemon" message instead of
    //    the cryptic timeout from the first real request.
    let base = daemon_base.to_string();
    let v1 = format!("{base}/v1");
    probe_or_bail(&base, bearer).await?;

    // 2. Resolve model IDs. Preference order:
    //       a) explicit `--chat-model` / `--embed-model` flag,
    //       b) the daemon's `SetupConfig.models.*` filename stems
    //          — this is what the daemon actually loaded, and the
    //          daemon advertises those IDs on `/v1/models`,
    //       c) fallback: probe `/v1/models` and pick the first
    //          chat- and first embed-shaped entries.
    //    The historical (c)-only path picked non-deterministically
    //    between a locally-loaded `qwen-embedding-0.6b` (1024-dim)
    //    and a mesh-peer-advertised `Qwen3-Embedding-0.6B-Q8_0`
    //    whose dimensionality didn't match any installed corpus —
    //    silently downgrading every retrieval to FTS-only. Reading
    //    the config directly removes that race.
    let (chat_model, embed_model) = resolve_model_ids(
        &v1,
        chat_model,
        embed_model,
        bearer,
        guest_link_active,
        guest_lender_url,
    )
    .await?;
    eprintln!("Daemon: {base}");
    eprintln!("Chat model:  {chat_model}");
    eprintln!("Embed model: {embed_model}");

    // B:P9a — prefer the daemon's own OICP capabilities manifest for the chat
    // slot's context window (v0.4 §7) and the embed slot's query-instruction
    // prefix (§4), so `Runtime`'s budget-aware compaction sees the host's REAL
    // window (e.g. 32768) instead of the historical 8192 approximation. On a
    // v0.3 host that doesn't serve `/oicp/v1/capabilities`, fall back to 8192 +
    // the `DEFAULT_MANIFEST`-derived prefix (the prior behavior, bit-identical).
    let inference: Arc<dyn InferenceProvider> = Arc::new(
        match crate::fetch_manifest(&base, bearer.map(str::to_string)).await {
            Some(manifest) => SplitInferenceProvider::from_manifest_with_bearer(
                &v1,
                bearer.map(str::to_string),
                &manifest,
                chat_model,
                embed_model.clone(),
            ),
            None => SplitInferenceProvider::new_with_bearer(
                &v1,
                bearer.map(str::to_string),
                chat_model,
                embed_model.clone(),
                8192,
                sovereign_contracts::models_manifest::DEFAULT_MANIFEST
                    .embed_query_instruction(&embed_model),
            ),
        },
    );

    Ok((inference, base, embed_model))
}

/// GET `/v1/models` with a 2s timeout. Any non-200 aborts bootstrap
/// with a clear remediation hint — the alternative is cryptic
/// "connection refused" errors minutes later, mid-retrieval.
/// `bearer` is `Some` only under a guest link. `/v1/models` is NOT in
/// `AUTH_EXEMPT_PATHS`, so probing it without the bearer against a lender's
/// node returns 401 and the message would blame the daemon rather than the
/// missing credential. (`/status` is exempt, but it does not prove the
/// inference surface is reachable, which is what this probe is for.)
async fn probe_or_bail(base: &str, bearer: Option<&str>) -> Result<()> {
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    let mut req = client.get(&url);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(Error::Serialization(format!(
            "daemon at {base} returned {} from /v1/models. \
             Is it really a svrn daemon? Try `svrn doctor`.",
            r.status()
        ))),
        Err(_) => Err(Error::Serialization(format!(
            "daemon unreachable at {base}. \
             Start it with `svrn daemon run`, or pass --daemon <url>. \
             (If this is a guest link, the lending node is down or unreachable — \
             `svrn mesh use --forget` returns you to your own daemon.)"
        ))),
    }
}

/// Resolve `(chat_model_id, embed_model_id)` against the daemon.
/// See the call-site comment in `build_session` for the preference
/// order — explicit flag → SetupConfig stem → `/v1/models` probe.
async fn resolve_model_ids(
    v1: &str,
    chat_model: Option<&str>,
    embed_model: Option<&str>,
    bearer: Option<&str>,
    guest_link_active: bool,
    guest_lender_url: Option<&str>,
) -> Result<(String, String)> {
    // (a) Explicit flags short-circuit everything.
    if let (Some(c), Some(e)) = (chat_model, embed_model) {
        return Ok((c.to_string(), e.to_string()));
    }

    // Under a guest link the local `SetupConfig` names THIS machine's models,
    // and the guest wants the LENT one. Reading config here would name a model
    // the turn was not borrowed for.
    //
    // Falling through to `/v1/models` is now the honest source in a second
    // sense: since 2026-08-28 that listing is our OWN daemon's, and it carries
    // the granted ids alongside local slots (`lender_manifest`). So the id
    // this resolves to is one the local daemon can actually route — which is
    // the whole point, because the turn runs there and only the completion
    // crosses.
    let guest = guest_link_active;

    // The EMBED id goes through the one decider
    // (`crate::daemon_models`, ARCH §10.6): explicit flag →
    // configured stem → embedding-like advertised id, then a
    // `/v1/embeddings` probe so a session never starts on an embed model the
    // daemon cannot answer with. This file used to hold its own copy of the
    // ladder (stem first, then an `embedding`/`-embed` substring), and
    // `recipe_cmd` + `workflow-host` each held a different one — which is how
    // one daemon could serve `svrn chat` and refuse `svrn corpus ingest`.
    //
    // Under a guest link the configured stem is THIS machine's and is
    // skipped; a grant with no embed model is tolerated with the sentinel
    // below rather than refused, so the guest branch keeps its own path
    // through the `/v1/models` loop.
    let mut embed_found = if guest {
        embed_model.map(str::to_string)
    } else {
        Some(
            resolve_embed_model(v1, embed_model)
                .await
                .map_err(Error::Serialization)?
                .id,
        )
    };

    // (b) The CHAT stem from SetupConfig. The daemon loads
    //     `config.models.primary` and advertises it on `/v1/models`
    //     under its filename stem. Preferring the stem over
    //     `/v1/models` iteration means we always reach the
    //     *local* slot, never a mesh-peer advertisement, and the
    //     answer is stable across invocations.
    let mut chat_found =
        chat_model
            .map(str::to_string)
            .or_else(|| if guest { None } else { chat_stem_from_config() });
    if let (Some(c), Some(e)) = (chat_found.as_ref(), embed_found.as_ref()) {
        return Ok((c.clone(), e.clone()));
    }

    // (c) Fallback: probe `/v1/models`. Used when SetupConfig is
    //     absent (fresh install, dev without setup) or when it
    //     lacks one of the two slots.
    let url = format!("{v1}/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    let mut req = client.get(&url);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| Error::Serialization(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Serialization(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| Error::Serialization(format!("parse /v1/models: {e}")))?;
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| Error::Serialization("/v1/models: no `data` array".into()))?;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        let is_embed = looks_like_embed_model(id);
        // Under a guest link the chat model must be one the LENDER
        // advertises. This listing carries local slots, mesh peers AND the
        // lender's granted ids, and taking whichever non-embed id comes
        // first means a guest borrows a model and then asks their own local
        // slot the question. That is exactly what the first live 3.3 did:
        // the turn served from `Qwen3.8-27B-UD-Q6_K_XL` on this machine
        // while a grant for the lender's model sat unused.
        //
        // `advertised_by` is the daemon's own answer to "who holds this",
        // so there is no second scope lookup here (§10.6).
        let lender_holds = |m: &serde_json::Value| -> bool {
            let Some(lender) = guest_lender_url else {
                return false;
            };
            m.get("advertised_by")
                .and_then(|a| a.as_array())
                .is_some_and(|rows| rows.iter().any(|h| h.as_str() == Some(lender)))
        };
        if is_embed {
            if embed_found.is_none() {
                embed_found = Some(id.to_string());
            }
        } else if chat_found.is_none() && (!guest || lender_holds(m)) {
            chat_found = Some(id.to_string());
        }
    }

    match (chat_found, embed_found) {
        (Some(c), Some(e)) => Ok((c, e)),
        // Under a guest link this branch has ONE cause and it is not the
        // local slots: the lender advertises nothing under this grant, so it
        // expired, was revoked, or the lending node restarted (grants are
        // held in memory). Sending the operator to `svrn setup` for that is
        // the B1 misattribution shape — a true statement about the wrong
        // subject. This is the surface live bar 3.6 reads.
        (None, _) if guest => Err(Error::Serialization(
            "the guest link is live but the lending node grants no chat model — the grant has \
             expired, been revoked, or the lender restarted (grants are held in memory). \
             Nothing was served from this node in its place; `svrn mesh use --forget` drops \
             the link."
                .into(),
        )),
        (None, _) => Err(Error::Serialization(
            "daemon lists no chat models — check `svrn setup` and the primary/fast slots".into(),
        )),
        // A guest grant may cover chat and no embedding model at all — today
        // `Scope::Models` unlocks `/v1/chat/completions`, not `/v1/embeddings`.
        // That is not a reason to refuse the whole session: chat works. It IS
        // a reason never to substitute a plausible-looking id, which would make
        // retrieval appear to work and quietly return nothing (§18.3). The
        // sentinel is unservable BY DESIGN — an embed call fails with the host
        // naming this exact string.
        (Some(c), None) if guest => {
            eprintln!(
                "Guest link: this grant covers chat only — no embedding model is in scope, \
                 so retrieval over local corpora will be refused by the lending node."
            );
            Ok((c, NO_EMBED_MODEL_IN_GRANT.to_string()))
        }
        (_, None) => Err(Error::Serialization(
            "daemon lists no embedding model — retrieval will fail. Set `[models] embed` in \
             ~/.svrnmesh/config.toml or pass --embed-model."
                .into(),
        )),
    }
}

/// Stand-in embed-model id when a guest grant names no embedding model.
///
/// Deliberately not a real name, and deliberately not the chat model's: any
/// embed request fails with the host quoting THIS string back, which says
/// exactly what happened. A plausible substitute would make the failure look
/// like a retrieval miss.
const NO_EMBED_MODEL_IN_GRANT: &str = "(no-embedding-model-in-this-guest-grant)";

/// Filename stem of `SetupConfig.models.primary`. The daemon advertises it
/// on `/v1/models` using exactly the file stem, so this is the stable
/// local chat id without a `/v1/models` round-trip. (The embed stem's
/// twin lives in `crate::daemon_models`.)
fn chat_stem_from_config() -> Option<String> {
    sovereign_contracts::setup_config::SetupConfig::load()
        .ok()?
        .primary_model_stem()
}
