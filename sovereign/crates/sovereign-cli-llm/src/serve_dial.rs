// SPDX-License-Identifier: AGPL-3.0-or-later
//! cli-llm loads no model: serve owns the weights (FIVE_PROGRAMS §2). The
//! embed, rerank and NER calls that loaded one in this process dial serve on
//! this host (`venue::serve_port()`; `SOVEREIGN_SERVE_PORT` overrides)
//! through the daemon's serve client, the one reader of serve's self-report
//! and its NER route (pb-cli-llm). No serve at the base is a refusal naming
//! serve and the base, never a local load and never an empty result
//! (principle 6); cli-llm starts nothing.

use std::path::Path;
use std::sync::Arc;

use sovereign_contracts::engine_state::ServedSelf;
use sovereign_contracts::ner::LabeledEntityExtractor;
use sovereign_contracts::rerank_kind::serves_rerank;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_daemon::serve_client::{self, ServeBase, ServeBaseSource};
use sovereign_turn_client::serve_self::{default_serve_base, read_served_self};

/// serve on this host, at the one port serve listens on.
fn this_hosts_serve() -> ServeBase {
    ServeBase {
        base: default_serve_base(),
        source: ServeBaseSource::Default,
    }
}

/// The refusal when serve does not answer: what `verb` needed it for, and
/// where it looked.
fn absent(verb: &str, what: &str, serve: &ServeBase, why: &str) -> String {
    tracing::warn!(verb, what, serve_base = %serve.base, error = why, "serve did not answer");
    format!(
        "{verb}: {what} is serve's, and serve did not answer at {} ({why}). \
         Start a serve holding the model (the stock `svrn daemon run`, or \
         `sovereign-serve`); SOVEREIGN_SERVE_PORT names another port.",
        serve.base
    )
}

async fn served(verb: &str, what: &str, serve: &ServeBase) -> Result<ServedSelf, String> {
    read_served_self(&serve.base)
        .await
        .map_err(|e| absent(verb, what, serve, &e))
}

/// An embedder on serve's `/v1/embeddings`, refused unless serve embeds
/// with `model`: the prescribed slot's file or `--embed-model`'s, compared
/// by file stem, which is serve's `embed_model_id`.
pub async fn serve_embedder(
    verb: &str,
    model: &Path,
) -> Result<Arc<dyn InferenceProvider>, String> {
    let serve = this_hosts_serve();
    let served = served(verb, "the embed model", &serve).await?;
    let want = model
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if served.embed_model != want {
        tracing::warn!(verb, serve_base = %serve.base, serving = %served.embed_model, want = %want, "serve embeds with another model");
        return Err(format!(
            "{verb}: serve at {} embeds with `{}`, not `{want}` ({}). To \
             measure another model, start a serve holding it and point \
             SOVEREIGN_SERVE_PORT at it.",
            serve.base,
            served.embed_model,
            model.display()
        ));
    }
    tracing::info!(verb, serve_base = %serve.base, embed_model = %want, "embedding on serve");
    Ok(Arc::new(serve_client::loopback_provider(&serve, served, 0)))
}

/// serve's cross-encoder, reranking over its `/v1/rerank`. `Ok(None)` when
/// serve answered and holds no rerank kind (`serves_rerank`, the one
/// decider); an Err when serve did not answer.
pub async fn serve_reranker(verb: &str) -> Result<Option<Arc<dyn InferenceProvider>>, String> {
    let serve = this_hosts_serve();
    let served = served(verb, "the reranker", &serve).await?;
    let provider = serve_client::loopback_provider(&serve, served, 0);
    let holds = serves_rerank(&provider);
    tracing::info!(verb, serve_base = %serve.base, holds, "serve's rerank kind");
    Ok(holds.then(|| Arc::new(provider) as Arc<dyn InferenceProvider>))
}

/// serve's NER handle on its `/v1/ner`. `Ok(None)` when serve answered and
/// has no NER model; an Err when it did not answer. One probe: a CLI waits
/// for no bring-up.
pub async fn serve_ner(verb: &str) -> Result<Option<Arc<dyn LabeledEntityExtractor>>, String> {
    let serve = this_hosts_serve();
    serve_client::resolve_serve_ner(&serve.base, std::time::Duration::ZERO)
        .await
        .map_err(|e| absent(verb, "the NER model", &serve, &e))
}

#[cfg(test)]
#[path = "serve_dial_tests.rs"]
mod tests;
