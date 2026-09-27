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

use serde::{Deserialize, Serialize};
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::ner::{EntityMention, GlinerGeneration, LabeledEntityExtractor};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_inference::served_kind::{
    self, KindChild, KindClient, KindLoader, KindRoute, KindServeError, ServedKind,
};

pub use sovereign_gliner::configured_model_id;
pub use sovereign_gliner::gliner_ner::{download_model, models_root, probe_model_available};

/// The NER kind's route.
pub const NER_PATH: &str = "/v1/ner";

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

// ── The wire ────────────────────────────────────────────────────────────────

/// Which pass a request runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NerPass {
    /// `extract_mentions_batch`.
    #[default]
    Entities,
    /// `extract_concept_mentions`, per text.
    Concepts,
}

/// `POST /v1/ner`. Empty `texts` asks only which extractor answers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NerRequest {
    /// The texts, one mention list each in the answer.
    pub texts: Vec<String>,
    /// Which pass runs; entities when absent.
    #[serde(default)]
    pub pass: NerPass,
}

/// The extractor that answered, as the port reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NerExtractor {
    /// `LabeledEntityExtractor::model_id`.
    pub model_id: String,
    /// `LabeledEntityExtractor::labels`.
    pub labels: Vec<String>,
    /// `LabeledEntityExtractor::threshold`.
    pub threshold: f32,
    /// `v1` or `v2` ([`GlinerGeneration`]).
    pub generation: String,
}

/// One mention on the wire ([`EntityMention`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NerMention {
    /// `EntityMention::text`.
    pub text: String,
    /// `EntityMention::label`.
    pub label: String,
    /// `EntityMention::char_start`.
    pub char_start: usize,
    /// `EntityMention::char_end`.
    pub char_end: usize,
    /// `EntityMention::score`.
    pub score: f32,
}

/// The answer: `extractor` is `None` on a node with no NER model installed,
/// which answers only an empty request; `mentions` has one entry per text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NerResponse {
    /// The extractor that answered; `None` without a model.
    pub extractor: Option<NerExtractor>,
    /// One mention list per requested text, in order.
    pub mentions: Vec<Vec<NerMention>>,
}

fn generation_wire(g: GlinerGeneration) -> &'static str {
    match g {
        GlinerGeneration::V1 => "v1",
        GlinerGeneration::V2 => "v2",
    }
}

fn to_wire(m: EntityMention) -> NerMention {
    let EntityMention {
        text,
        label,
        char_start,
        char_end,
        score,
    } = m;
    NerMention {
        text,
        label,
        char_start,
        char_end,
        score,
    }
}

fn from_wire(m: NerMention) -> EntityMention {
    let NerMention {
        text,
        label,
        char_start,
        char_end,
        score,
    } = m;
    EntityMention {
        text,
        label,
        char_start,
        char_end,
        score,
    }
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

// ── The client ──────────────────────────────────────────────────────────────

/// How long one request waits on the route. The in-process extractor had no
/// bound; a batch is bounded by its callers (corpus-engine's
/// `chunk_ner_bound`), and this caps a route that stopped answering.
const NER_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

/// The NER port answered by another process's [`NER_PATH`]: the svrn
/// daemon's handle when serving lives in `serve`. The port is synchronous
/// and its callers run inside the async runtime (the turn's retrieval calls
/// it inline, as it called the in-process model), so each call runs its
/// request on a thread of its own, which blocks the caller as the model did.
/// The route must not be served by the caller's own single-threaded runtime,
/// which the blocked caller would starve; serve is another process.
#[derive(Debug, Clone)]
pub struct RemoteNer {
    url: String,
    extractor: NerExtractor,
    generation: GlinerGeneration,
}

impl RemoteNer {
    /// Ask the route at `base` which extractor answers. `Ok(None)` when that
    /// node has no NER model installed; an Err names an unreachable or
    /// unreadable route.
    pub async fn connect(base: &str) -> std::result::Result<Option<Self>, String> {
        let url = format!("{}{NER_PATH}", base.trim_end_matches('/'));
        let response = post(&url, &NerRequest::default()).await?;
        let Some(extractor) = response.extractor else {
            return Ok(None);
        };
        let generation = match extractor.generation.as_str() {
            "v1" => GlinerGeneration::V1,
            "v2" => GlinerGeneration::V2,
            other => {
                return Err(format!(
                    "{url} named an unknown GLiNER generation `{other}`"
                ))
            }
        };
        Ok(Some(Self {
            url,
            extractor,
            generation,
        }))
    }

    fn run(&self, texts: &[&str], pass: NerPass) -> Result<Vec<Vec<EntityMention>>> {
        let url = self.url.clone();
        let request = NerRequest {
            texts: texts.iter().map(|t| t.to_string()).collect(),
            pass,
        };
        let expected = request.texts.len();
        let response = std::thread::Builder::new()
            .name("ner-client".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("ner client runtime: {e}"))?;
                rt.block_on(post(&url, &request))
            })
            .map_err(|e| Error::Inference(format!("ner client thread: {e}")))?
            .join()
            .map_err(|_| Error::Inference("ner client thread panicked".to_string()))?
            .map_err(Error::Inference)?;
        if response.mentions.len() != expected {
            return Err(Error::Inference(format!(
                "{} answered {} mention lists for {expected} texts",
                self.url,
                response.mentions.len()
            )));
        }
        Ok(response
            .mentions
            .into_iter()
            .map(|ms| ms.into_iter().map(from_wire).collect())
            .collect())
    }
}

impl LabeledEntityExtractor for RemoteNer {
    fn model_id(&self) -> &str {
        &self.extractor.model_id
    }

    fn labels(&self) -> Vec<String> {
        self.extractor.labels.clone()
    }

    fn threshold(&self) -> f32 {
        self.extractor.threshold
    }

    fn extract_mentions(&self, text: &str) -> Result<Vec<EntityMention>> {
        Ok(self
            .run(&[text], NerPass::Entities)?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    fn extract_mentions_batch(&self, texts: &[&str]) -> Result<Vec<Vec<EntityMention>>> {
        self.run(texts, NerPass::Entities)
    }

    fn extract_concept_mentions(&self, text: &str) -> Result<Vec<EntityMention>> {
        Ok(self
            .run(&[text], NerPass::Concepts)?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    fn generation(&self) -> GlinerGeneration {
        self.generation
    }
}

/// One request to the route, its refusal named.
async fn post(url: &str, request: &NerRequest) -> std::result::Result<NerResponse, String> {
    let resp = reqwest::Client::new()
        .post(url)
        .json(request)
        .timeout(NER_WINDOW)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                format!("{url} did not answer within {NER_WINDOW:?}")
            } else {
                format!("{url} is not reachable: {e}")
            }
        })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("{url} refused (HTTP {status}): {body}"));
    }
    let response = resp
        .json::<NerResponse>()
        .await
        .map_err(|e| format!("{url} answered unreadably: {e}"))?;
    tracing::debug!(target: "served_kind", url, texts = request.texts.len(), "ner request dialled");
    Ok(response)
}

#[cfg(test)]
#[path = "ner_tests.rs"]
mod tests;
