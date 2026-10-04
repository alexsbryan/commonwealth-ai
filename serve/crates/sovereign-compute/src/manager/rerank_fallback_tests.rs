// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `ComputeRoutedProvider::rerank_batch`'s fallback — see `manager.rs`.

use super::*;
use sovereign_contracts::{Depth, Error};

/// An in-process engine whose only answer is rerank.
struct InProcess;

#[async_trait]
impl InferenceProvider for InProcess {
    async fn complete(&self, _: &CompletionRequest) -> Result<CompletionResponse> {
        Err(Error::NotImplemented("rerank only".into()))
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("rerank only".into()))
    }
    async fn embed(&self, _: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("rerank only".into()))
    }
    async fn rerank_batch(&self, _: &str, docs: &[String]) -> Result<Vec<f32>> {
        Ok(vec![0.0; docs.len()])
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 0,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

/// A rerank child that is not serving hands the call to the in-process slot,
/// and says so where the daemon's filter admits it.
#[test]
fn a_rerank_that_falls_back_from_its_child_is_logged() {
    let (_tx, rx) = watch::channel(ChildRuntimeState::starting());
    let facade = ComputeRoutedProvider {
        inner: Arc::new(InProcess),
        routes: HashMap::new(),
        embed_child: None,
        rerank_child: Some(Arc::new(ChildProvider::new("reranker".into(), rx))),
        manager: None,
        distributed_primary: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let (scores, log) = crate::logged_under("compute_child=info", || {
        runtime.block_on(facade.rerank_batch("q", &["d".to_string()]))
    });
    assert_eq!(scores.expect("the in-process slot answers"), vec![0.0]);
    assert!(
        log.contains("rerank child not serving"),
        "no fallback event at compute_child: {log:?}"
    );
}
