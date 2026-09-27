// SPDX-License-Identifier: AGPL-3.0-or-later
//! The NER kind's client: [`RemoteNer`] over a serving node's `/v1/ner`
//! (sovereign-compute's NER kind serves it). Moved beside `rerank.rs` from
//! `sovereign_compute::ner` (pb-cli-llm), which re-exports it.

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::ner::{
    from_wire, EntityMention, GlinerGeneration, LabeledEntityExtractor, NerExtractor, NerPass,
    NerRequest, NerResponse, NER_PATH,
};

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
