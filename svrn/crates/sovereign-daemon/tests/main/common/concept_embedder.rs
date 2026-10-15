// SPDX-License-Identifier: AGPL-3.0-or-later
//! An embedder whose meaning is a word list, for the verify routes' e2e tests
//! (OICP v0.6 §2.15): each of seven dimensions counts the words of one topic
//! of the fixture libraries, and every other word adds a fifth to the eighth.
//! So a claim sharing a sentence's topic words scores near 1 against it, and
//! one sharing none scores under 0.25, with the data file's floor (0.5)
//! between them by construction rather than by a hash's luck.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use futures::Stream;
use kernel_types::NodeId;
use oicp_types::{KnowledgeSearchResponse, SentenceRoute};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common::fixture_library::{digest_of, DIM};
use crate::common::service_double::ProviderService;

pub const EMBED_MODEL: &str = "fake-concepts";

/// The topics, one per dimension; the last dimension is every other word.
const TOPICS: [&[&str]; 7] = [
    &[
        "port", "harbour", "sailings", "sailing", "recorded", "ferry", "master",
    ],
    &["late", "delays", "delay", "tide", "tables", "blamed"],
    &["percent", "total", "rose"],
    &["lighthouse", "keepers", "paid", "shillings"],
    &[
        "survey",
        "surveyed",
        "agreements",
        "gauges",
        "river",
        "rivers",
        "basin",
        "treaty",
        "disputes",
        "shared",
    ],
    &[
        "archive",
        "archives",
        "reading",
        "rooms",
        "folios",
        "catalogues",
    ],
    &["gulls", "breakwater"],
];

/// `text`'s vector: its topic words counted, every other word a fifth.
pub fn concepts(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        if w.is_empty() {
            continue;
        }
        let w = w.to_lowercase();
        match TOPICS.iter().position(|t| t.contains(&w.as_str())) {
            Some(d) => v[d] += 1.0,
            None => v[DIM - 1] += 0.2,
        }
    }
    v
}

/// Every text an embedder was asked for, in order (test-only record).
pub type AskedLog = Arc<std::sync::Mutex<Vec<String>>>;

#[derive(Default)]
pub struct Concepts(pub Option<AskedLog>);

#[async_trait::async_trait]
impl InferenceProvider for Concepts {
    async fn complete(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<CompletionResponse> {
        unimplemented!("the embedder never completes")
    }

    async fn complete_stream(
        &self,
        _r: &CompletionRequest,
    ) -> sovereign_core::error::Result<
        Pin<Box<dyn Stream<Item = sovereign_core::error::Result<String>> + Send>>,
    > {
        unimplemented!("the embedder never streams")
    }

    async fn embed(&self, text: &str) -> sovereign_core::error::Result<Vec<f32>> {
        if let Some(log) = &self.0 {
            log.lock().unwrap().push(text.to_string());
        }
        Ok(concepts(text))
    }

    fn embed_model_id(&self) -> String {
        EMBED_MODEL.into()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        unimplemented!()
    }
}

/// A host over the corpora installed under `indexes`, embedding by
/// [`Concepts`] on its local inference.
pub async fn state_with_concepts(indexes: std::path::PathBuf) -> AppState {
    state_with_concepts_logged(indexes, None).await
}

/// [`state_with_concepts`], every text either embedder is asked for pushed
/// onto `log`.
pub async fn state_with_concepts_logged(
    indexes: std::path::PathBuf,
    log: Option<AskedLog>,
) -> AppState {
    let seen = log.clone();
    let embed: corpus_index::types::EmbedFn = Arc::new(move |t: &str| {
        if let Some(log) = &seen {
            log.lock().unwrap().push(t.to_string());
        }
        let v = concepts(t);
        Box::pin(async move { Ok(v) })
    });
    let engine = crate::common::reading_double(indexes, embed).on_texts_digest(digest_of);
    AppState::new_with_platform_and_engine_and_serving(
        NodeId::from_u128(1),
        Some(Arc::new(engine)),
        sovereign_daemon::state::serving::ServingSeed {
            local_inference: Some(ProviderService::new(Arc::new(Concepts(log)))),
        },
    )
}

/// Search `corpus` by sentence until the index route answers: its sentences
/// are embedded, so every verify after reads the index, never the passages
/// route a still-indexing corpus is answered by.
pub async fn sentences_indexed(state: &AppState, corpus: &str) {
    for _ in 0..400 {
        let mut req = Request::post("/v1/knowledge/search")
            .header("host", "127.0.0.1:9741")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"query": "harbour", "corpora": [corpus], "granularity": "sentence"})
                    .to_string(),
            ))
            .unwrap();
        let addr: SocketAddr = "127.0.0.1:55007".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        let resp = client_router(state.clone()).oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let r: KnowledgeSearchResponse = serde_json::from_slice(&bytes).unwrap_or_default();
        if r.route == Some(SentenceRoute::Index) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("`{corpus}` was never sentence-indexed")
}
