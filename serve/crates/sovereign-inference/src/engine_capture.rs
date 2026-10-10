// SPDX-License-Identifier: AGPL-3.0-or-later
//! Records what an engine is asked, at the provider boundary, for the engine
//! conformance battery's case bank (`bench/lanes/engine-swap/`).
//!
//! `[engine] capture = "<path>"` wraps the built engine in a
//! [`RecordingProvider`], which appends one JSON line per model call: the
//! method, its input (the `CompletionRequest` itself for completions) and the
//! innermost tracing span, which stands in for the caller. It records only;
//! every call is forwarded unchanged, defaults included.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;
use serde_json::{json, Value};
use sovereign_contracts::traits::{InferenceProvider, ResidentSlot, ServingLocus};
use sovereign_contracts::types::*;
use sovereign_contracts::{oicp, Result};

/// An engine whose model calls are appended to a capture file.
pub struct RecordingProvider {
    inner: Arc<dyn InferenceProvider>,
    file: Mutex<File>,
}

/// Wrap `provider` when `[engine] capture` names a file; otherwise return
/// it as it is. A file that cannot be opened is reported and capture stays
/// off: the engine serves either way.
pub fn wrap_if_requested(
    provider: Arc<dyn InferenceProvider>,
    capture: Option<&Path>,
) -> Arc<dyn InferenceProvider> {
    let Some(path) = capture else {
        return provider;
    };
    match RecordingProvider::open(provider.clone(), path) {
        Ok(recording) => {
            tracing::info!(target: "engine_capture", path = %path.display(), "engine calls are being captured");
            Arc::new(recording)
        }
        Err(e) => {
            tracing::warn!(target: "engine_capture", path = %path.display(), error = %e, "capture file could not be opened; capture is off");
            provider
        }
    }
}

impl RecordingProvider {
    /// Append to `path`, creating it if needed.
    pub fn open(inner: Arc<dyn InferenceProvider>, path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            inner,
            file: Mutex::new(file),
        })
    }

    fn record(&self, method: &str, input: Value) {
        let span = tracing::Span::current()
            .metadata()
            .map(|m| m.name())
            .unwrap_or("");
        let line = json!({ "method": method, "span": span, "input": input });
        let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = writeln!(file, "{line}") {
            tracing::warn!(target: "engine_capture", method, error = %e, "a capture line was not written");
        }
    }

    fn record_request(&self, method: &str, request: &CompletionRequest) {
        let mut input = match serde_json::to_value(request) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "engine_capture", method, error = %e, "a request did not serialize; its line names the error");
                json!({ "unserializable": e.to_string() })
            }
        };
        // `admission` is not serialized; whether the call was admitted is
        // part of what the case replays.
        input["admitted"] = json!(request.admission.is_some());
        self.record(method, input);
    }
}

/// Every method forwards, defaults included: a default left in place here
/// would answer for the recorder instead of the engine behind it.
#[async_trait]
impl InferenceProvider for RecordingProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.record_request("complete", request);
        self.inner.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.record_request("complete_stream", request);
        self.inner.complete_stream(request).await
    }

    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        self.record_request("complete_stream_with_id", request);
        self.inner.complete_stream_with_id(request).await
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        self.record_request("complete_stream_with_finish", request);
        self.inner.complete_stream_with_finish(request).await
    }

    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String)> {
        self.record_request("complete_stream_with_id_and_finish", request);
        self.inner.complete_stream_with_id_and_finish(request).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.record("embed", json!({ "text": text }));
        self.inner.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.record("embed_batch", json!({ "texts": texts }));
        self.inner.embed_batch(texts).await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        for request in requests {
            self.record_request("complete_batch", request);
        }
        self.inner.complete_batch(requests).await
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.record("embed_query", json!({ "text": query }));
        self.inner.embed_query(query).await
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.record("rerank_batch", json!({ "query": query, "docs": docs }));
        self.inner.rerank_batch(query, docs).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.inner.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.inner.embed_model_id()
    }

    fn serving_locus(&self) -> ServingLocus {
        self.inner.serving_locus()
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.inner.effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.inner.n_ctx_train_for_primary()
    }

    fn count_tokens(&self, text: &str) -> u32 {
        self.record("count_tokens", json!({ "text": text }));
        self.inner.count_tokens(text)
    }

    fn code_model_id(&self) -> Option<String> {
        self.inner.code_model_id()
    }

    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.inner.edit_slot_info()
    }

    async fn warmup_primary(&self) -> Result<()> {
        self.inner.warmup_primary().await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.inner.load_extra_slot(slot_name, path, context_size)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.inner.unload_extra_slot(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.inner.extras_inventory()
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.inner.resident_slots()
    }

    fn decode_evidence(&self) -> Vec<oicp::SlotDecodeEvidence> {
        self.inner.decode_evidence()
    }

    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        self.inner.primary_slot_status().await
    }

    fn compute_children(&self) -> Vec<oicp::ComputeChildStatus> {
        self.inner.compute_children()
    }

    async fn peer_manifests(&self) -> Vec<(String, oicp::ProviderManifest)> {
        self.inner.peer_manifests().await
    }

    async fn lender_manifest(&self) -> Option<(String, Vec<String>)> {
        self.inner.lender_manifest().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sovereign_contracts::double::TestProvider;
    use sovereign_contracts::reloadable_provider::unforwarded_methods;
    use sovereign_contracts::traits::InferenceProvider;
    use sovereign_contracts::types::CompletionRequest;

    use super::RecordingProvider;

    #[tokio::test]
    async fn a_call_is_recorded_whole_and_answered_by_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.jsonl");
        let engine = Arc::new(TestProvider::new().with_complete_text("from the engine"));
        let recorder = RecordingProvider::open(engine, &path).unwrap();
        let mut request = CompletionRequest::new("a question");
        request.url_allowlist = Some(vec!["https://a.org".into()]);
        request.think_budget = Some(0);

        let answer = recorder.complete(&request).await.unwrap();
        assert_eq!(answer.text, "from the engine");
        let _ = recorder.count_tokens("four");

        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["method"], "complete");
        let replayed: CompletionRequest =
            serde_json::from_value(lines[0]["input"].clone()).unwrap();
        assert_eq!(replayed.prompt, "a question");
        assert_eq!(replayed.url_allowlist, request.url_allowlist);
        assert_eq!(replayed.think_budget, Some(0));
        assert_eq!(lines[0]["input"]["admitted"], false);
        assert_eq!(lines[1]["method"], "count_tokens");
        assert_eq!(lines[1]["input"]["text"], "four");
    }

    #[test]
    fn only_a_named_file_that_opens_turns_capture_on() {
        let engine: Arc<dyn InferenceProvider> = Arc::new(TestProvider::new());
        let unset = super::wrap_if_requested(engine.clone(), None);
        assert!(
            Arc::ptr_eq(&unset, &engine),
            "unset leaves the engine unwrapped"
        );
        let unopenable = super::wrap_if_requested(
            engine.clone(),
            Some(std::path::Path::new("/nonexistent-dir/capture.jsonl")),
        );
        assert!(
            Arc::ptr_eq(&unopenable, &engine),
            "a file that cannot open leaves capture off"
        );
        let dir = tempfile::tempdir().unwrap();
        let on = super::wrap_if_requested(engine.clone(), Some(&dir.path().join("c.jsonl")));
        assert!(!Arc::ptr_eq(&on, &engine), "a named file wraps the engine");
    }

    #[test]
    fn the_recorder_forwards_every_inference_provider_method() {
        let missing = unforwarded_methods(
            include_str!("engine_capture.rs"),
            "impl InferenceProvider for RecordingProvider",
        );
        assert!(
            missing.is_empty(),
            "RecordingProvider does not forward: {missing:?}"
        );
    }
}
