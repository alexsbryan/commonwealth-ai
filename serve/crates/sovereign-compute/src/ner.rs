// SPDX-License-Identifier: AGPL-3.0-or-later
//! The NER served kind: named-entity extraction, registered once, loaded once
//! per process, and handed to every reader as the same handle.
//!
//! NER is served on [`NER_PATH`] from the process's one handle
//! ([`served_ner`]), not from the host's provider: the kind loads its own
//! model by id ([`KindLoader::Ner`]). Its first callers are the svrn daemon's
//! readers (the NoteStore T2 hook, the tiered chunk adapter, the turn's
//! entity-aware retrieval) on the path where serving lives in `serve`: the
//! daemon installs a [`RemoteNer`] dialing serve's route as its handle
//! ([`install_ner`]) before any reader asks (pb-svrn-dials-serve).
//!
//! The kind also owns its asset: which model it loads, whether that model is
//! installed, where it lives and how it is fetched, re-exported below from
//! the one crate that implements them.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use sovereign_contracts::ner::{generation_wire, to_wire, LabeledEntityExtractor};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_inference::served_kind::{
    self, KindChild, KindClient, KindLoader, KindRoute, KindServeError, ServedKind,
};

pub use sovereign_gliner::configured_model_id;
pub use sovereign_gliner::gliner_ner::{download_model, models_root, probe_model_available};

// The wire and the client moved out (pb-cli-llm): the wire to
// sovereign-contracts, the client to oicp-client beside rerank, because two
// programs speak them (FIVE_PROGRAMS §12 3a). Reachable here as before.
pub use oicp_client::RemoteNer;
pub use sovereign_contracts::ner::{
    NerExtractor, NerMention, NerPass, NerRequest, NerResponse, NER_PATH,
};

/// Why no compute child hosts NER.
const NO_CHILD: &str = "no config hosts NER out of process: a `[[compute.slot]]` \
     child loads a GGUF provider (`ServedKind::load_provider`), and NER loads an \
     extractor by model id";

/// The NER kind.
pub const NER: ServedKind = ServedKind {
    role: "ner",
    env_path: None,
    loader: KindLoader::Ner(sovereign_gliner::load_gliner_extractor),
    route: KindRoute::Served {
        path: NER_PATH,
        serve: serve_ner,
    },
    client: KindClient::Provided {
        method: "LabeledEntityExtractor::extract_mentions_batch",
    },
    child: KindChild::Absent { reason: NO_CHILD },
};

/// This process's NER handle: loaded by [`served_ner`] or installed by
/// [`install_ner`], whichever comes first, once.
static HANDLE: OnceLock<Option<Arc<dyn LabeledEntityExtractor>>> = OnceLock::new();

/// This process's one NER handle. The first call registers the kind and
/// loads through its registered loader; every later call, from any reader,
/// gets the same `Arc`. `None` is a node without the model installed (the
/// loader says so at `info`) or a kind that could not register (said here).
pub fn served_ner() -> Option<Arc<dyn LabeledEntityExtractor>> {
    load_once(&HANDLE, NER)
}

/// Make `handle` this process's NER handle instead of loading one here: the
/// svrn daemon on the dialing path, whose handle is a [`RemoteNer`] on serve.
/// `None` names a serve with no NER model. An Err when a handle was already
/// loaded or installed, because two handles in one process is the fork
/// `served_ner` exists to prevent.
pub fn install_ner(
    handle: Option<Arc<dyn LabeledEntityExtractor>>,
) -> std::result::Result<(), String> {
    ensure_registered(NER)?;
    let installed = handle.as_ref().map(|h| h.model_id().to_string());
    HANDLE.set(handle).map_err(|_| {
        "this process already holds a NER handle; install before the first reader asks".to_string()
    })?;
    tracing::info!(target: "served_kind", kind = NER.role, installed = ?installed, "NER handle installed for this process");
    Ok(())
}

/// Register the NER kind, so a host mounting kind routes serves its route.
/// Idempotent: a host may register it and then load it.
pub fn register() -> std::result::Result<(), String> {
    ensure_registered(NER)
}

/// Register `kind` unless its role already is.
fn ensure_registered(kind: ServedKind) -> std::result::Result<(), String> {
    let registered = || {
        served_kind::served_kinds()
            .iter()
            .any(|k| k.role == kind.role)
    };
    if registered() {
        return Ok(());
    }
    match served_kind::register_kind(kind) {
        Ok(()) => Ok(()),
        // Another caller registered it between the check and the write.
        Err(_) if registered() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Register `kind`, then load it through the loader the registry holds for
/// its role, at most once per `cell`.
fn load_once(
    cell: &OnceLock<Option<Arc<dyn LabeledEntityExtractor>>>,
    kind: ServedKind,
) -> Option<Arc<dyn LabeledEntityExtractor>> {
    cell.get_or_init(|| {
        if let Err(e) = ensure_registered(kind) {
            tracing::warn!(target: "served_kind", kind = kind.role, error = %e, "NER kind did not register — no entity extractor this process");
            return None;
        }
        let registered = served_kind::served_kinds()
            .into_iter()
            .find(|k| k.role == kind.role)?;
        match registered.loader {
            KindLoader::Ner(load) => {
                let handle = load();
                tracing::info!(target: "served_kind", kind = kind.role, loaded = handle.is_some(), "NER kind loaded for this process");
                handle
            }
            KindLoader::Provider(_) => {
                tracing::warn!(target: "served_kind", kind = kind.role, "registered kind loads a provider, not an entity extractor");
                None
            }
        }
    })
    .clone()
}

/// Answer `request` from `handle`. Blocking: the extractor runs its model on
/// this thread.
fn answer(
    handle: Option<Arc<dyn LabeledEntityExtractor>>,
    request: NerRequest,
) -> std::result::Result<NerResponse, KindServeError> {
    let Some(handle) = handle else {
        if request.texts.is_empty() {
            return Ok(NerResponse {
                extractor: None,
                mentions: Vec::new(),
            });
        }
        return Err(KindServeError::Backend(
            "no NER model is installed on this node".to_string(),
        ));
    };
    let mentions = match request.pass {
        NerPass::Entities => {
            let texts: Vec<&str> = request.texts.iter().map(String::as_str).collect();
            handle.extract_mentions_batch(&texts)
        }
        NerPass::Concepts => request
            .texts
            .iter()
            .map(|t| handle.extract_concept_mentions(t))
            .collect(),
    }
    .map_err(|e| KindServeError::Backend(format!("ner failed: {e}")))?;
    Ok(NerResponse {
        extractor: Some(NerExtractor {
            model_id: handle.model_id().to_string(),
            labels: handle.labels(),
            threshold: handle.threshold(),
            generation: generation_wire(handle.generation()).to_string(),
        }),
        mentions: mentions
            .into_iter()
            .map(|ms| ms.into_iter().map(to_wire).collect())
            .collect(),
    })
}

/// The route's handler: served from this process's NER handle, loaded (or
/// installed) once, on the blocking pool. The host's provider is not NER's
/// backend.
fn serve_ner(
    _host: Arc<dyn InferenceProvider>,
    body: serde_json::Value,
) -> Pin<Box<dyn Future<Output = std::result::Result<serde_json::Value, KindServeError>> + Send>> {
    Box::pin(async move {
        let request: NerRequest = serde_json::from_value(body)
            .map_err(|e| KindServeError::BadRequest(format!("ner request: {e}")))?;
        let texts = request.texts.len();
        let response = tokio::task::spawn_blocking(move || answer(served_ner(), request))
            .await
            .map_err(|e| KindServeError::Backend(format!("ner worker: {e}")))??;
        tracing::debug!(target: "served_kind", kind = NER.role, texts, installed = response.extractor.is_some(), "ner request answered");
        serde_json::to_value(response).map_err(|e| KindServeError::Backend(e.to_string()))
    })
}

#[cfg(test)]
#[path = "ner_tests.rs"]
mod tests;
