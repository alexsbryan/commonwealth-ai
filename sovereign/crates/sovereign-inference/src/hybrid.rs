use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;

use crate::remote::RemoteApiProvider;
use crate::selector::{BackendEntry, BackendSelector, CapabilityAwareSelector, PrioritySelector};

/// Multi-backend inference provider that routes requests using a BackendSelector.
///
/// On failure, retries with the next-best backend (up to 2 retries).
/// Tracks health per-backend and optionally polls OICP manifests.
pub struct HybridProvider {
    /// Providers indexed the same as `entries`.
    providers: Vec<Arc<dyn InferenceProvider>>,
    /// Backend metadata indexed the same as `providers`.
    entries: Vec<BackendEntry>,
    selector: Box<dyn BackendSelector>,
}

impl HybridProvider {
    pub fn new(
        backends: Vec<(Arc<dyn InferenceProvider>, BackendEntry)>,
        selector: Box<dyn BackendSelector>,
    ) -> Self {
        let (providers, entries): (Vec<_>, Vec<_>) = backends.into_iter().unzip();
        Self {
            providers,
            entries,
            selector,
        }
    }

    /// Create with default selector: CapabilityAwareSelector → PrioritySelector fallback.
    pub fn with_defaults(backends: Vec<(Arc<dyn InferenceProvider>, BackendEntry)>) -> Self {
        let selector = Box::new(CapabilityAwareSelector {
            fallback: Box::new(PrioritySelector),
        });
        Self::new(backends, selector)
    }

    /// Start a background health check loop.
    /// Every `interval_secs`, pings each backend and updates OICP manifests
    /// for remote backends.
    pub fn start_health_loop(self: &Arc<Self>, interval_secs: u64) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                for (i, provider) in this.providers.iter().enumerate() {
                    let entry = &this.entries[i];
                    let probe = CompletionRequest {
                        prompt: "ping".to_string(),
                        system_message: None,
                        preferred_speed: Speed::Fast,
                        max_tokens: Some(1),
                        temperature: Some(0.0),
                        structured_output: None,
            think_budget: None,
                        oicp: None,
                    };

                    match provider.complete(&probe).await {
                        Ok(resp) => entry.health.record_success(resp.latency_ms),
                        Err(_) => entry.health.record_failure(),
                    }

                    // Update OICP manifest for remote backends.
                    if !entry.is_local {
                        if let Some(remote) = as_remote(provider) {
                            if let Some(manifest) = remote.fetch_oicp_manifest().await {
                                *entry.oicp_manifest.write().await = Some(manifest);
                            }
                        }
                    }
                }
            }
        });
    }
}

/// Try to downcast an InferenceProvider to RemoteApiProvider.
fn as_remote(_provider: &Arc<dyn InferenceProvider>) -> Option<&RemoteApiProvider> {
    // Downcasting trait objects requires `Any`. For now, OICP manifest polling
    // is a no-op. A future version can add `fn as_any(&self)` to InferenceProvider.
    None
}

#[async_trait]
impl InferenceProvider for HybridProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let max_retries = 2.min(self.providers.len().saturating_sub(1));
        let mut excluded: Vec<usize> = Vec::new();

        for attempt in 0..=max_retries {
            // Build a filtered entries list for the selector.
            let filtered: Vec<(usize, &BackendEntry)> = self
                .entries
                .iter()
                .enumerate()
                .filter(|(i, e)| !excluded.contains(i) && e.health.is_healthy())
                .collect();

            if filtered.is_empty() {
                return Err(Error::Inference("No healthy backends available".to_string()));
            }

            // Build a contiguous slice for the selector.
            let selector_entries: Vec<BackendEntry> = filtered
                .iter()
                .map(|(_, e)| BackendEntry {
                    name: e.name.clone(),
                    health: Arc::clone(&e.health),
                    priority: e.priority,
                    cost_per_token: e.cost_per_token,
                    is_local: e.is_local,
                    oicp_manifest: Arc::clone(&e.oicp_manifest),
                })
                .collect();

            let selected = self.selector.select(request, &selector_entries).await?;
            let original_idx = filtered[selected].0;
            let entry = &self.entries[original_idx];

            match self.providers[original_idx].complete(request).await {
                Ok(response) => {
                    entry.health.record_success(response.latency_ms);
                    return Ok(response);
                }
                Err(e) => {
                    entry.health.record_failure();
                    eprintln!(
                        "[hybrid] Backend {} failed (attempt {}): {e}",
                        entry.name,
                        attempt + 1,
                    );
                    excluded.push(original_idx);
                }
            }
        }

        Err(Error::Inference("All backends failed".to_string()))
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let idx = self.selector.select(request, &self.entries).await?;
        self.providers[idx].complete_stream(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        for provider in &self.providers {
            match provider.embed(text).await {
                Ok(embedding) => return Ok(embedding),
                Err(Error::NotImplemented(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(Error::NotImplemented(
            "No backend supports embeddings".to_string(),
        ))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let mut max_context = 0;
        let mut supports_structured = false;

        for provider in &self.providers {
            let caps = provider.capabilities();
            max_context = max_context.max(caps.max_context_tokens);
            supports_structured = supports_structured || caps.supports_structured_output;
        }

        ProviderCapabilities {
            max_context_tokens: max_context,
            supports_structured_output: supports_structured,
            relative_speed: Speed::Medium,
            relative_reasoning: Depth::Deep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::HealthTracker;
    use std::sync::Arc;

    struct MockProvider {
        response: String,
        should_fail: bool,
    }

    #[async_trait]
    impl InferenceProvider for MockProvider {
        async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
            if self.should_fail {
                return Err(Error::Inference("mock failure".to_string()));
            }
            Ok(CompletionResponse {
                text: self.response.clone(),
                tokens_used: 5,
                model_id: "mock".to_string(),
                latency_ms: 10,
                oicp_meta: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("no stream".to_string()))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Err(Error::NotImplemented("no embed".to_string()))
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    fn mock_backend(
        name: &str,
        response: &str,
        should_fail: bool,
        priority: u32,
    ) -> (Arc<dyn InferenceProvider>, BackendEntry) {
        let provider: Arc<dyn InferenceProvider> = Arc::new(MockProvider {
            response: response.to_string(),
            should_fail,
        });
        let entry = BackendEntry::new_local(name, Arc::new(HealthTracker::new()), priority);
        (provider, entry)
    }

    #[tokio::test]
    async fn routes_to_primary() {
        let hybrid = HybridProvider::with_defaults(vec![
            mock_backend("primary", "from primary", false, 1),
            mock_backend("secondary", "from secondary", false, 2),
        ]);

        let request = CompletionRequest::new("test");
        let response = hybrid.complete(&request).await.unwrap();
        assert_eq!(response.text, "from primary");
    }

    #[tokio::test]
    async fn falls_back_on_failure() {
        let hybrid = HybridProvider::with_defaults(vec![
            mock_backend("primary", "primary", true, 1),
            mock_backend("secondary", "from secondary", false, 2),
        ]);

        let request = CompletionRequest::new("test");
        let response = hybrid.complete(&request).await.unwrap();
        assert_eq!(response.text, "from secondary");
    }

    #[tokio::test]
    async fn all_fail_returns_error() {
        let hybrid = HybridProvider::with_defaults(vec![
            mock_backend("a", "", true, 1),
            mock_backend("b", "", true, 2),
        ]);

        let request = CompletionRequest::new("test");
        assert!(hybrid.complete(&request).await.is_err());
    }

    #[tokio::test]
    async fn capabilities_merged() {
        let hybrid = HybridProvider::with_defaults(vec![
            mock_backend("a", "", false, 1),
            mock_backend("b", "", false, 2),
        ]);

        let caps = hybrid.capabilities();
        assert_eq!(caps.max_context_tokens, 4096);
    }
}
