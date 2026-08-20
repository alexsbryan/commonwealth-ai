// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// Every `interval_secs`, refreshes OICP manifests and re-probes any
    /// backend currently marked unavailable. See [`Self::health_sweep`] for
    /// why healthy backends are deliberately left alone.
    pub fn start_health_loop(self: &Arc<Self>, interval_secs: u64) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                this.health_sweep().await;
            }
        });
    }

    /// One tick of the health loop. Public within the crate so the sweep can
    /// be driven directly by tests rather than by waiting on a wall clock.
    ///
    /// ## Why this only probes UNHEALTHY backends
    ///
    /// The probe is a real completion, and against a `remote` backend it is an
    /// ordinary `POST /v1/chat/completions` to that peer's daemon. At the
    /// receiving end nothing distinguishes it from a person typing: the
    /// handler calls `bump_foreground_active()` unconditionally
    /// (`commonwealth-api/src/routes_inference.rs:43`), which holds that
    /// node's foreground-yield window open for `yield_to_foreground_secs`.
    ///
    /// This loop ran at 30s against a 60s default window, so the window on the
    /// probed daemon never lapsed and its ingest enrichment was starved
    /// indefinitely — measured 2026-08-18 across three runs
    /// (`runs/sec-filings-ship-e2e/evidence/real-daemon.log`), with a live
    /// `sovereign-server` mobile host (`~/.svrnmesh/mobile-host-server.toml`,
    /// `[[inference.backends]] type = "remote"` → `127.0.0.1:9741`) as the
    /// caller. A synthetic user turn on a fixed cadence is not a liveness
    /// check; it is a denial-of-service with good intentions.
    ///
    /// Nothing needed those probes. [`Self::complete`] already records success
    /// and failure on the very same [`HealthTracker`](crate::health::HealthTracker)
    /// from real traffic, so a backend carrying traffic keeps its health
    /// current for free. The job left for this loop is **repair**: an entry
    /// that three consecutive failures marked unavailable is filtered out of
    /// the selector's candidate set, so no real request will ever reach it
    /// again and only a probe can bring it back.
    ///
    /// What this gives up, stated plainly: a backend that dies silently while
    /// carrying zero traffic is no longer discovered before the next real
    /// request. Nothing consumes that fact while there is no traffic, and the
    /// first real request already retries the next-best backend
    /// ([`Self::complete`] retries up to twice), so the cost is one extra hop
    /// on one request — paid only in the case where the old behaviour's cost
    /// was a permanently starved peer.
    pub(crate) async fn health_sweep(&self) {
        for (i, provider) in self.providers.iter().enumerate() {
            let entry = &self.entries[i];

            // Manifest refresh is not gated on health: an entry's advertised
            // capabilities should stay fresh whether or not it is currently
            // serving. (`as_remote` is a documented stub today, so this is a
            // no-op — kept unconditional so it stays correct when it isn't.)
            if !entry.is_local {
                if let Some(remote) = as_remote(provider) {
                    if let Some(manifest) = remote.fetch_oicp_manifest().await {
                        *entry.oicp_manifest.write().await = Some(manifest);
                    }
                }
            }

            if entry.health.is_healthy() {
                tracing::trace!(
                    backend = %entry.name,
                    "health: backend is available — no probe sent (real traffic maintains it)"
                );
                continue;
            }

            let probe = CompletionRequest {
                prompt: "ping".to_string(),
                system_message: None,
                preferred_speed: Speed::Fast,
                max_tokens: Some(1),
                temperature: Some(0.0),
                structured_output: None,
                think_budget: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
                prompt_shape: None,
                stable_prefix_len: None,
            };

            match provider.complete(&probe).await {
                Ok(resp) => {
                    entry.health.record_success(resp.latency_ms);
                    tracing::info!(
                        backend = %entry.name,
                        latency_ms = resp.latency_ms,
                        "health: repair probe succeeded — backend returned to the candidate set"
                    );
                }
                Err(e) => {
                    entry.health.record_failure();
                    tracing::debug!(
                        backend = %entry.name,
                        error = %e,
                        "health: repair probe failed — backend stays unavailable"
                    );
                }
            }
        }
    }

    /// Return the `HealthTracker` for the primary (first) backend, if any.
    /// Used by `RouterCircuitChecker` to monitor circuit state.
    pub fn primary_health_tracker(&self) -> Option<Arc<crate::health::HealthTracker>> {
        self.entries.first().map(|e| e.health.clone())
    }

    /// Return the `HealthTracker` for every registered backend.
    pub fn all_health_trackers(&self) -> Vec<Arc<crate::health::HealthTracker>> {
        self.entries.iter().map(|e| e.health.clone()).collect()
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
                return Err(Error::Inference(
                    "No healthy backends available".to_string(),
                ));
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
                    inference_availability: e.inference_availability,
                    observations: Arc::clone(&e.observations),
                    locality: e.locality,
                    benchmark: Arc::clone(&e.benchmark),
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

    // Runtime slot management delegates to the first local backend.
    // Without this override the trait's default-impl returns a generic
    // "this inference provider does not support runtime slot load"
    // error — confirmed 2026-05-20: `POST /internal/models/load`
    // failed with the default error even though the daemon had a
    // working EmbeddedLlamaCpp behind the HybridProvider, because the
    // hybrid wrapper didn't delegate. Mesh peers can't load extras
    // into a remote node anyway; local is the only meaningful target.
    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.is_local {
                return self.providers[idx].load_extra_slot(slot_name, path, context_size);
            }
        }
        Err(Error::Inference(
            "hybrid provider has no local backend — runtime slot load \
             requires an embedded llama.cpp provider on this node"
                .to_string(),
        ))
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.is_local {
                return self.providers[idx].unload_extra_slot(slot_name);
            }
        }
        Err(Error::Inference(
            "hybrid provider has no local backend — runtime slot unload \
             requires an embedded llama.cpp provider on this node"
                .to_string(),
        ))
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.is_local {
                return self.providers[idx].extras_inventory();
            }
        }
        Vec::new()
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

    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockProvider {
        response: String,
        should_fail: bool,
        /// Every `complete` this provider is asked for, probe or not. The
        /// health-loop tests assert on this, because "did the peer daemon see
        /// a chat completion?" is the whole question.
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl InferenceProvider for MockProvider {
        async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.should_fail {
                return Err(Error::Inference("mock failure".to_string()));
            }
            Ok(CompletionResponse {
                text: self.response.clone(),
                tokens_used: 5,
                prompt_tokens: 0,
                model_id: "mock".to_string(),
                latency_ms: 10,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
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
        counted_backend(name, response, should_fail, priority).0
    }

    #[allow(clippy::type_complexity)]
    fn counted_backend(
        name: &str,
        response: &str,
        should_fail: bool,
        priority: u32,
    ) -> (
        (Arc<dyn InferenceProvider>, BackendEntry),
        Arc<AtomicUsize>,
        Arc<HealthTracker>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let health = Arc::new(HealthTracker::new());
        let provider: Arc<dyn InferenceProvider> = Arc::new(MockProvider {
            response: response.to_string(),
            should_fail,
            calls: calls.clone(),
        });
        let entry = BackendEntry::new_local(name, health.clone(), priority);
        ((provider, entry), calls, health)
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

    // ─── health sweep: the probe that starved a peer's enrichment ────────
    //
    // The defect these pin: this loop's probe is a real completion, and
    // against a remote backend it lands on the peer daemon's
    // `/v1/chat/completions` where it is indistinguishable from a user turn
    // and bumps that node's foreground-yield window. At 30s against a 60s
    // window the window never lapsed. See `HybridProvider::health_sweep`.

    /// A healthy backend must see ZERO completions from the health loop, no
    /// matter how many times it ticks. This is the arm that fixes the
    /// starvation: with no probe there is no foreground bump on the peer.
    #[tokio::test]
    async fn a_healthy_backend_is_never_probed() {
        let (backend, calls, health) = counted_backend("daemon", "pong", false, 1);
        let hybrid = Arc::new(HybridProvider::with_defaults(vec![backend]));
        assert!(health.is_healthy(), "premise: the backend starts healthy");

        // Ten ticks — i.e. five minutes at the shipped 30s cadence, which is
        // longer than the 60s yield window it used to hold open.
        for _ in 0..10 {
            hybrid.health_sweep().await;
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a healthy backend must receive no synthetic user turns"
        );
    }

    /// The WATCHED PASS for the same gate, and the reason it is not simply
    /// "delete the probe": a backend three failures have taken out of the
    /// candidate set can only come back via a probe, and it must.
    #[tokio::test]
    async fn an_unhealthy_backend_is_probed_and_repaired() {
        let (backend, calls, health) = counted_backend("daemon", "pong", false, 1);
        let hybrid = Arc::new(HybridProvider::with_defaults(vec![backend]));

        // Drive it unavailable exactly the way real traffic would
        // (`health.rs`: three consecutive failures).
        health.record_failure();
        health.record_failure();
        health.record_failure();
        assert!(!health.is_healthy(), "premise: three failures take it out");

        hybrid.health_sweep().await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an unavailable backend must be probed — nothing else can revive it"
        );
        assert!(
            health.is_healthy(),
            "and the successful probe must actually return it to service"
        );

        // ...and having been repaired, it goes quiet again.
        hybrid.health_sweep().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a repaired backend must stop being probed"
        );
    }

    /// A backend that is still down stays down and keeps being retried —
    /// the repair path must not mark it healthy on a failed probe.
    #[tokio::test]
    async fn a_still_dead_backend_stays_out_and_keeps_being_retried() {
        let (backend, calls, health) = counted_backend("daemon", "", true, 1);
        let hybrid = Arc::new(HybridProvider::with_defaults(vec![backend]));
        health.record_failure();
        health.record_failure();
        health.record_failure();

        hybrid.health_sweep().await;
        hybrid.health_sweep().await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!health.is_healthy());
    }

    /// Real traffic is what keeps a healthy backend's health current — the
    /// premise the "don't probe healthy backends" decision rests on. If this
    /// ever stops being true, the sweep change above loses its justification.
    #[tokio::test]
    async fn real_traffic_records_health_so_the_probe_is_redundant() {
        let (backend, _calls, health) = counted_backend("primary", "ok", false, 1);
        let hybrid = HybridProvider::with_defaults(vec![backend]);

        assert_eq!(health.latency_ms(), 0, "premise: no observations yet");
        let _ = hybrid
            .complete(&CompletionRequest::new("test"))
            .await
            .unwrap();
        assert!(
            health.latency_ms() > 0,
            "a real request must feed the same tracker the probe used to feed"
        );
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
