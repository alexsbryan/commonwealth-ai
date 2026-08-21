// SPDX-License-Identifier: AGPL-3.0-or-later
//! The behavioural contracts of the system: inference, routing, planning,
//! tools, the storage sub-traits (+ `StateStore` supertrait), the approval
//! channel, and the optional oracles a `Runtime` can be wired with. Every
//! item is declared explicitly (no glob re-exports) — see lib.rs.
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{Error, Result};
use crate::types::*;

// Re-export observer types so `sovereign_core::StateStoreObserver`
// works alongside `sovereign_core::StateStore`.
pub use crate::observer::{
    noop_observer, NoopObserver, SharedStateStoreObserver, StateStoreObserver,
};

/// Produces `knowledge_view_digests` for a
/// [`ConversationContext`][crate::types::ConversationContext] after
/// skill routing has resolved.
///
/// Defined here in the contract crate so `sovereign-core`'s `Runtime`
/// can splice digests without depending on `sovereign-tools` (which
/// would create a circular dependency). `KnowledgeViewManager` in
/// `sovereign-tools` is the canonical implementation.
///
/// See `ConversationContext.knowledge_view_digests` for the invariant:
/// a `None` value reaching the prompt-assembly site is a bug. Runtime
/// calls `splice_landscape_digests` after resolving the active skill.
#[async_trait]
pub trait LandscapeDigestProvider: Send + Sync {
    /// Compute and attach the per-view digests to `ctx.knowledge_view_digests`
    /// for the resolved `active_skill`. Must leave the field `Some` (possibly
    /// empty) — see the type-level invariant on `ConversationContext`.
    async fn splice_landscape_digests(
        &self,
        ctx: &mut ConversationContext,
        active_skill: Option<&str>,
    );

    /// Snapshot of canonical entity names + aliases extracted from
    /// the live atlases. Feeds the relationship-weighted memory
    /// decay path (`memory::prune_decayed_memories_with_config`):
    /// memories that mention any name in the inventory decay at
    /// half rate.
    ///
    /// Default impl returns `None` — uniform decay applies. The
    /// `KnowledgeViewManager` implementation overrides this to
    /// expose its on-disk atom inventory.
    async fn entity_inventory(&self) -> Option<EntityInventory> {
        None
    }
}

/// Reports which `corpus_id`s are flagged as sensitive and must be
/// excluded from the agent's *ambient* situated-context assembly.
///
/// Folder-ingest v1 §3.4: a watched-folder corpus marked sensitive
/// (e.g. journal, therapy notes, legal-privileged material) stays
/// available for explicit search and Inner Work mode but is
/// structurally absent from the pre-turn ambient retrieval that
/// answers "what does the user know about X?". Per ARCH §7.4, this
/// is one layer of defence — the recipe-level invariants
/// (`scope=Local`, `mesh_sharing=false`) are the others.
///
/// Defined here in the contract crate so `sovereign-core`'s `Runtime`
/// can apply the filter without depending on `sovereign-tools`
/// (which holds the canonical `WatchedFolderConfig.sensitive` flag).
/// The `LocalCorpusManager` in `sovereign-tools` is the canonical
/// implementation; tests get the default no-op (`is_sensitive ⇒
/// false` for every corpus).
#[async_trait]
pub trait SensitiveCorpusOracle: Send + Sync {
    /// Snapshot the set of sensitive `corpus_id`s. Read once per
    /// query and intersected with the candidate corpus list — the
    /// implementation must be cheap (a `HashSet` clone or an
    /// `RwLock` read). Returning an empty set means "no corpus is
    /// sensitive", which is the default-pre-v1 behaviour.
    async fn sensitive_corpus_ids(&self) -> std::collections::HashSet<String>;
}

/// No-op oracle — every corpus is non-sensitive. Used by tests and
/// any caller that hasn't wired sovereign-tools' watched-folder
/// manager into the runtime.
pub struct NoSensitiveCorpora;

#[async_trait]
impl SensitiveCorpusOracle for NoSensitiveCorpora {
    async fn sensitive_corpus_ids(&self) -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }
}

/// Resolves an opaque per-request **principal** from a conversation id, so
/// the retrieval seam (`build_context`) can scope corpus visibility without
/// the Runtime ever knowing what a "tenant" is. The server — which owns the
/// tenant-prefix convention on conversation ids — provides the
/// implementation; desktop / CLI / tests leave it unset, and then no corpus
/// is ever hidden (single-user behaviour, unchanged).
///
/// The returned string is compared against `CorpusVisibility::Private {
/// owner }`: equality means "this principal owns it" (visible), inequality
/// means "another principal's private corpus" (hidden).
pub trait PrincipalResolver: Send + Sync {
    /// The principal that owns this conversation, or `None` when there is no
    /// tenancy (single-user) — in which case no corpus is hidden.
    fn principal_for(&self, conversation_id: &str) -> Option<String>;
}

/// Snapshot of one watched-folder corpus's user-facing metadata.
///
/// Fed by [`FolderMetadataOracle`] into the prompt-assembly seam
/// (`format_scored_chunks_with_kinds`) so the model sees "your
/// case-files folder" instead of an opaque `corpus_id`, and so the
/// "what I don't have" prompt-time note can enumerate gaps without
/// the runtime depending on `sovereign-tools`.
///
/// Folder-ingest v1 §3.7 (glassbox) and the §6.3 attribution
/// requirement: any folder corpus that contributes retrieval should
/// carry its display name + a coverage signal back to the user.
#[derive(Debug, Clone, Default)]
pub struct FolderMetadata {
    /// Operator-facing display name. The label the user typed in the
    /// register-flow ("case files", "research notes"), NOT the
    /// `corpus_id` slug.
    pub display_name: String,
    /// Count of files the watcher tried and failed to extract
    /// (encrypted PDFs, malformed DOCX, etc.). Sourced from
    /// `WatchedFolderState.failed_files.len()`.
    pub failed_count: usize,
    /// Count of files skipped because their extension isn't in the
    /// extractor's accept-list. Sum of
    /// `WatchedFolderState.skipped_by_extension` values.
    pub skipped_count: usize,
    /// Up to two extensions, by descending count, that drove
    /// `skipped_count`. Surfaced verbatim in the prompt note
    /// (e.g. ".pages, .key") so the user sees concrete formats.
    pub top_skipped_extensions: Vec<String>,
}

/// Reports the user-facing metadata for the watched-folder corpora
/// installed locally. Lives in `sovereign-core` so [`Runtime`] can
/// thread folder display names into the prompt without depending
/// on `sovereign-tools`.
///
/// `LocalCorpusManager` in `sovereign-tools` is the canonical
/// implementation; tests get the default no-op
/// ([`NoFolderMetadata`]) which returns an empty map and so leaves
/// today's `corpus_id`-as-label behaviour untouched.
#[async_trait]
pub trait FolderMetadataOracle: Send + Sync {
    /// Snapshot the watched-folder metadata keyed by `corpus_id`.
    /// Read once per knowledge-query plan and intersected with the
    /// corpora that actually contributed retrieval. Implementations
    /// must be cheap (cloning a small HashMap) — this runs on every
    /// turn that hits a knowledge route.
    ///
    /// `corpus_id`s NOT in the returned map are treated as
    /// non-folder corpora (SEP, Wikipedia, mesh hits) and keep their
    /// existing label rendering.
    async fn folder_metadata(&self) -> std::collections::HashMap<String, FolderMetadata>;
}

/// No-op oracle — no folder corpora are known. Used by tests and
/// any caller that hasn't wired sovereign-tools' watched-folder
/// manager into the runtime. Falling back here preserves the
/// pre-Phase-F label rendering exactly.
pub struct NoFolderMetadata;

#[async_trait]
impl FolderMetadataOracle for NoFolderMetadata {
    async fn folder_metadata(&self) -> std::collections::HashMap<String, FolderMetadata> {
        std::collections::HashMap::new()
    }
}

/// Entity extractor for entity-aware retrieval-over-history.
/// `sovereign-gliner::GlinerExtractor` is the canonical
/// production impl (GLiNER ONNX, 5-label tag set). Defined here as a
/// trait so `sovereign-core` can hold an `Option<Arc<dyn EntityExtractor>>`
/// on Runtime without depending on sovereign-tools (cycle: tools →
/// core already exists).
///
/// Returns the unique set of entity texts (lower-cased) found in
/// `text`. The label is intentionally elided — for retrieval scoring
/// we only need the entity STRING for set-overlap (jaccard), not its
/// type. Implementations should dedupe before returning.
pub trait EntityExtractor: Send + Sync {
    /// Unique, lower-cased entity strings found in `text`, deduped; labels are elided (see trait doc).
    fn extract_entities(&self, text: &str) -> Vec<String>;

    /// Unique, lower-cased CONCEPT strings (abstract nouns / -isms like
    /// "determinism", "colonialism", "uncertainty principle") found in
    /// `text`, deduped.
    ///
    /// Separate from [`extract_entities`](Self::extract_entities) on
    /// purpose: the proper-noun heuristic that seeds retrieval is
    /// uppercase-only and structurally cannot surface these, and the
    /// tagged-entity `Concept` class is deliberately kept out of the
    /// 5-label confirmation pass (its precision is only recoverable
    /// downstream, at the FTS-exact-title obligation gate). Retrieval's
    /// entity-obligation lane uses these to pin exact-title concept
    /// articles that would otherwise never be fetched.
    ///
    /// Default returns empty — only the GLiNER-backed extractor runs the
    /// extra `Concept` inference pass; every other impl (and the
    /// not-yet-warm lazy loader) degrades to no concepts, exactly like a
    /// machine without the model installed.
    fn extract_concepts(&self, _text: &str) -> Vec<String> {
        Vec::new()
    }
}

// ─── Corpus install (the `recipe:` workflow stage) ─────────────

/// Installs/updates a corpus from a recipe. The `recipe:` workflow stage
/// delegates to this instead of reimplementing ingest — implementations route
/// through the EXISTING corpus-install path (the daemon's mesh-coordinated
/// `/internal/corpus/install`), so a `recipe:` step never bypasses the work-queue
/// / partition lock. Lives on core so `sovereign-workflow` (core-only) can hold a
/// `dyn CorpusInstaller` without depending on the engine or the install crate; the
/// concrete HTTP-client impl lives at the daemon/CLI layer.
#[async_trait]
pub trait CorpusInstaller: Send + Sync {
    /// Install or update the corpus produced by recipe `id`, threading the
    /// recipe's `[parameters]` values. Idempotent (a no-op when already installed
    /// and fresh), and blocks until the install reaches a terminal state so a
    /// downstream workflow step can consume the corpus.
    async fn ensure_installed(
        &self,
        id: &str,
        params: &std::collections::BTreeMap<String, String>,
    ) -> Result<InstallOutcome>;
}

/// Result of [`CorpusInstaller::ensure_installed`].
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Id of the installed/updated corpus — what downstream workflow steps consume.
    pub corpus_id: String,
    /// A short status token: `complete` | `already_installed` | `installing`.
    pub status: String,
}

// ─── 1. Inference ──────────────────────────────────────────────

// Slot residency and placement are what a HOST reports about itself, so they
// are protocol vocabulary and live in `oicp-types` (layer 0). Re-exported here
// at their historical paths — `InferenceProvider::resident_slots` and
// `compute_children` still return them (noun-convergence rung 2b).
pub use crate::oicp::{ComputeChildStatus, ResidentSlot, SlotPlacement, WorkerPlacement};

/// The inference backend contract: completions (one-shot, streaming, batch),
/// embeddings, optional rerank, plus slot/model introspection. Implemented by
/// the embedded llama.cpp engine, remote HTTP forwarders, mesh-peer wrappers,
/// and test stubs — the many defaulted methods keep those impls minimal.
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// One-shot completion (no streaming).
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse>;

    /// Stream the completion as plain text chunks. Carries no metadata — the
    /// `_with_id` / `_with_finish` variants below add provenance and finish reasons.
    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>>;

    /// Streaming variant that also returns the model id actually
    /// chosen to serve this request. Exists because
    /// `complete_stream` itself returns only a `Stream<String>` —
    /// there's nowhere to attach "I routed this to peer mac-peer's
    /// Qwen3.5-9B" to, so streaming provenance has historically
    /// fallen back to the synchronous `model_id_for(Speed)` which
    /// can't see any routing decision made inside the async call.
    ///
    /// Default implementation preserves the legacy behaviour:
    /// delegate to `complete_stream` and stamp the model_id with
    /// whatever `model_id_for` reports for the request's speed.
    /// Mesh-aware wrappers override this to return the peer-
    /// attributed id (e.g. `"Qwen3.5-9B @ peer mac-peer"`). All
    /// pre-existing providers and test mocks keep working
    /// unmodified.
    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        let stream = self.complete_stream(request).await?;
        Ok((stream, self.model_id_for(request.preferred_speed)))
    }

    /// Streaming variant that yields typed [`StreamFrame`]s and ends
    /// with a terminal [`StreamFrame::Finish`] carrying the
    /// [`FinishReason`] the provider observed (`Stop` for EOS,
    /// `Length` for `max_tokens`, `Cancelled` for receiver-drop, etc.).
    ///
    /// The default implementation wraps [`complete_stream`] and
    /// synthesises `Finish { reason: Stop, usage: None }` after the
    /// underlying stream closes — adequate for providers that don't
    /// observe truncation themselves (remote APIs that flatten the
    /// signal away, deterministic test stubs). Providers that DO know
    /// why generation stopped (`EmbeddedLlamaCpp`) override this method
    /// so the SSE bridge can emit an accurate OpenAI `finish_reason`.
    ///
    /// Receivers MUST treat a closed channel without any terminal frame
    /// as `Cancelled` rather than `Stop` — silent truncation is the bug
    /// this method exists to make impossible.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        use futures::StreamExt;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let inner = self.complete_stream(request).await?;
        // Shared between the body map and the tail-once future so the
        // tail can suppress the synthetic Stop when the body already
        // emitted a terminal Error frame.
        let terminal_emitted = Arc::new(AtomicBool::new(false));
        let body_flag = Arc::clone(&terminal_emitted);
        let mapped = inner.flat_map(move |item| {
            let frames: Vec<StreamFrame> = match item {
                Ok(text) => vec![StreamFrame::Token(text)],
                Err(e) => {
                    body_flag.store(true, Ordering::Relaxed);
                    vec![StreamFrame::Finish {
                        reason: FinishReason::Error(format!("{e}")),
                        usage: None,
                    }]
                }
            };
            futures::stream::iter(frames)
        });
        // Append a synthetic Stop frame after the underlying stream
        // closes (unless an Error already terminated it). This is a
        // legacy-bridge default — overrides should emit Length /
        // Cancelled / etc. with real fidelity.
        let tail_flag = terminal_emitted;
        let tail = futures::stream::once(async move {
            if tail_flag.load(Ordering::Relaxed) {
                None
            } else {
                Some(StreamFrame::Finish {
                    reason: FinishReason::Stop,
                    usage: None,
                })
            }
        })
        .filter_map(|f| async move { f });
        Ok(Box::pin(mapped.chain(tail)))
    }

    /// Combined variant: typed `StreamFrame` stream plus the mesh-
    /// attributed `model_id` of the provider that actually served the
    /// request. Equivalent to calling both
    /// [`complete_stream_with_finish`] (for the typed terminal Finish
    /// frame) and [`complete_stream_with_id`] (for the peer-attributed
    /// id) but in a single round-trip so the routing decision and
    /// stream initiation can't drift.
    ///
    /// Default implementation: call `complete_stream_with_finish` and
    /// stamp the synchronous `model_id_for(speed)` (no peer
    /// attribution). Mesh-aware wrappers override to return the real
    /// peer-attributed id alongside the typed stream.
    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String)> {
        let stream = self.complete_stream_with_finish(request).await?;
        Ok((stream, self.model_id_for(request.preferred_speed)))
    }

    /// Document-side embedding of `text`. Query-side callers should use
    /// `embed_query`, which instruction-aware models prefix differently.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts in a single forward pass when the backend supports it.
    /// The default implementation falls back to sequential single-text embedding.
    /// Override in providers that can batch (e.g. `EmbeddedLlamaCpp` with llama.cpp
    /// multi-sequence decoding) for significantly higher throughput on corpus ingest.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    /// Complete a batch of requests. The default implementation runs them
    /// sequentially. Remote providers (HTTP-based) override this to dispatch
    /// concurrently via `join_all`, achieving parallelism when the server
    /// supports `--parallel N --cont-batching`.
    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        let mut results = Vec::with_capacity(requests.len());
        for req in requests {
            results.push(self.complete(req).await?);
        }
        Ok(results)
    }

    /// Embed a query string, applying any model-specific query instruction prefix.
    ///
    /// Default implementation calls `embed()` — override in providers that support
    /// asymmetric instruction-aware models (e.g. Qwen3-Embedding) where the query
    /// side gets a different prefix than the document side. The distinction yields
    /// 1–5% retrieval improvement on instruction-aware models.
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.embed(query).await
    }

    /// Cross-encoder rerank. Score each `doc` against `query`; higher
    /// score = more relevant. Returns one score per doc, in the same
    /// order as input. Implementations should batch internally — the
    /// caller passes a single multi-doc request to amortise the
    /// model-load + tokenisation cost.
    ///
    /// Default returns `Err(Error::NotImplemented)` so providers
    /// without a reranker (remote API, mesh peer, stubs) satisfy the
    /// trait without lying about capability. `EmbeddedLlamaCpp`
    /// overrides this when a rerank slot is configured — env-var only
    /// (`SOVEREIGN_RERANK_MODEL_PATH` + `SOVEREIGN_RERANK_*`); there is
    /// no `[rerank]` models.toml section.
    /// The `CorpusIndex::search_with_rerank` path
    /// catches the error and falls back to the un-reranked fusion
    /// result — enabling rerank is purely additive.
    ///
    /// Score semantics are model-specific. bge-reranker-v2-m3 returns
    /// raw rank logits in roughly `[-10, +10]`; conventional
    /// relevance threshold is `0.0` (sigmoid → 0.5). Callers must
    /// not compare scores across providers.
    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let _ = (query, docs);
        Err(Error::NotImplemented(
            "rerank_batch not supported by this provider".to_string(),
        ))
    }

    /// Return the model ID that will be selected for a request at the given
    /// speed tier, without running inference. Used to populate provenance on
    /// streaming responses (where `complete_stream` returns no metadata).
    /// Default returns `"unknown"` — override in providers that know their
    /// loaded model names without blocking on a lock.
    fn model_id_for(&self, speed: Speed) -> String {
        let _ = speed;
        "unknown".to_string()
    }

    /// Stable identifier of the embedding model behind `embed` /
    /// `embed_batch`. Persisted alongside cached embeddings (e.g.
    /// `memories.embedding_model`) as a staleness guard: a stored vector is
    /// only reusable when the model that produced it matches the model that
    /// would embed the query — a same-dimension different-model vector would
    /// silently mis-rank.
    ///
    /// Default returns `"unknown"`. Callers MUST treat `"unknown"` as
    /// "cannot verify" — never match a stored embedding against it and never
    /// persist embeddings under it. Override in providers that know their
    /// embed model (`SplitInferenceProvider`, `EmbeddedLlamaCpp`).
    fn embed_model_id(&self) -> String {
        "unknown".to_string()
    }

    /// The actual context window size the chat slots are currently
    /// configured with, after any llama.cpp-side padding /
    /// `n_ctx_train` capping. This is the value the runtime should
    /// budget prompts against — NOT the configured-but-not-yet-active
    /// value from `SetupConfig`. Returns `None` on providers without
    /// a meaningful slot context (remote API forwards over HTTP and
    /// doesn't own a local ctx).
    ///
    /// Used by the KnowledgeQuery handler's pre-flight retrieval-bundle
    /// truncation: if the assembled prompt would exceed
    /// `effective_context_size() - reserved_output`, drop the
    /// lowest-score chunks before synthesis. Also surfaced in
    /// `ResponseProvenance.context_window` so the desktop chat bubble
    /// can render a "X / N tokens" budget indicator.
    fn effective_context_size(&self) -> Option<u32> {
        None
    }

    /// The gguf-trained context window of the primary slot's model
    /// (the `n_ctx_train` metadata field). This is the ceiling
    /// llama.cpp will cap `effective_context_size()` at — useful for
    /// the Settings UI to show "you can bump configured ctx up to N
    /// without RoPE scaling." Returns `None` on providers that don't
    /// own a local model (remote API).
    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        None
    }

    /// Estimate the token count of `text` for the active chat slot's
    /// tokenizer. Used by the runtime's chat-context-management layer
    /// (compaction trigger, retrieval-bundle budget) to decide when
    /// the prompt is approaching the slot's `effective_context_size`.
    ///
    /// Default implementation uses the project-wide
    /// `~4 chars/token` heuristic — accurate within ±15% on Latin-1
    /// prose for the GGUFs we ship; less accurate on CJK / code /
    /// heavy punctuation. Providers that own a real tokenizer
    /// (`EmbeddedLlamaCpp`) override with the slot's own BPE vocab
    /// for an exact count.
    ///
    /// Sync method. Implementors must not block on a `Mutex` —
    /// the chat-context-management path calls this in hot loops
    /// (per-message during prompt assembly). `EmbeddedLlamaCpp`'s
    /// override reads from the always-resident `Arc<LlamaModel>` on
    /// the fast slot, no lock needed.
    fn count_tokens(&self, text: &str) -> u32 {
        (text.chars().count() / 4) as u32
    }

    /// Return the model ID of a configured Code specialist slot, if the
    /// provider has one separately from its primary slot. Returns `None`
    /// on providers that collapse all chat work into `model_id_for(Slow)`
    /// — that's the pre-PR-E2 behaviour and still correct for remote
    /// providers, stubs, and single-model test harnesses.
    ///
    /// The mesh advertiser (`build_self_manifest`) consults this to emit
    /// a third `ProviderModel` entry with a `CapabilityHint::code` claim
    /// so peer schedulers can route code-hinted requests at this peer
    /// without first having to elicit a swap. Overriding providers
    /// (currently only `EmbeddedLlamaCpp`) return the filename stem of
    /// the configured code GGUF.
    fn code_model_id(&self) -> Option<String> {
        None
    }

    /// Live description of the code-editing serving arrangement —
    /// which model serves editing assistance and which of the two
    /// lanes it can actually serve (`sovereign/docs/NEXT_EDIT.md`,
    /// `sovereign/docs/INLINE_COMPLETION.md`).
    ///
    /// `None` means no editing model is available at all. A `Some`
    /// whose [`EditSlotInfo::fim`] is `None` is the ordinary case for
    /// a general chat model: next-edit serves, `/v1/completions`
    /// 503s. **Ask the lane** — never re-derive capability from the
    /// model id or a marker enum, or the answer grows a second decider
    /// (ARCH §10.6).
    ///
    /// Default returns `None` — remote providers, stubs, and test
    /// harnesses satisfy the trait unchanged. `EmbeddedLlamaCpp`
    /// overrides after `install_edit_slot` or
    /// `install_fallback_next_edit_slot`. Consumed by the daemon's
    /// `POST /v1/completions` route (503 gate), `POST
    /// /v1/edit_predictions`, and `GET /status` (`inference.edit`).
    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        None
    }

    /// Trigger any deferred slot loads so the next `complete()` /
    /// `complete_stream()` call doesn't pay the lazy-load tax.
    /// Specifically, eagerly load the primary chat slot on
    /// providers that lazy-load it (default behaviour for
    /// `EmbeddedLlamaCpp`). Idempotent — calling on an
    /// already-warm provider returns immediately.
    ///
    /// The default impl is a no-op so providers that don't manage
    /// local slots (remote API, mesh peer, deterministic test stub)
    /// satisfy the trait without behaviour changes.
    ///
    /// Wired into the desktop's foreground/focus events so the
    /// primary slot is hot by the time the user hits send. Without
    /// this, every conversation that pauses past
    /// `primary_idle_secs` (default 300s) re-pays the model-load
    /// wait — which on a 35B Q6 is ~10–20s on Metal and an order of
    /// magnitude worse on CPU.
    async fn warmup_primary(&self) -> Result<()> {
        Ok(())
    }

    /// Descriptive metadata (models, relative speed/reasoning, feature support) for display and coarse routing.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Add (or replace) an operator-declared additional chat slot at
    /// runtime. Returns the `model_id` (advertised name) loaded into
    /// the slot — what callers send in `request.model_id` to land
    /// here. Default returns an error: providers that don't manage
    /// local slots (remote API, mesh peer, deterministic test stub)
    /// have nothing to load.
    ///
    /// `EmbeddedLlamaCpp` overrides this to delegate to its concrete
    /// `load_extra` method. The HTTP `/internal/models/load` handler
    /// surfaces the error verbatim when a non-embedded provider is
    /// the active local inference service.
    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        let _ = (slot_name, path, context_size);
        Err(crate::error::Error::Inference(
            "this inference provider does not support runtime slot \
             load — only the embedded llama.cpp provider does"
                .to_string(),
        ))
    }

    /// Drop an operator-declared additional chat slot. Returns
    /// `Some(model_id)` when a slot was removed, `None` if the slot
    /// wasn't loaded. Default returns an error for non-embedded
    /// providers — same rationale as `load_extra_slot`.
    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        let _ = slot_name;
        Err(crate::error::Error::Inference(
            "this inference provider does not support runtime slot \
             unload — only the embedded llama.cpp provider does"
                .to_string(),
        ))
    }

    /// List currently-loaded extras as `(slot_name, model_id)` pairs
    /// in deterministic order. Default empty list — non-embedded
    /// providers have no extras concept.
    fn extras_inventory(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Report the *actual* in-memory residency of every slot this
    /// provider owns — the ground truth behind `/status`'s `loaded`
    /// flag and the `ollama ps` analog. Enumeration MUST be
    /// non-blocking and MUST NOT force-load a lazy slot just to report
    /// it (a contended slot reports `transitioning: true` instead).
    ///
    /// Default empty: only the embedded engine holds resident weights
    /// it can introspect. Remote/mesh forwarders return `[]` — the
    /// daemon they forward to answers for its own residency.
    fn resident_slots(&self) -> Vec<ResidentSlot> {
        Vec::new()
    }

    /// State of the deep-reasoning ("primary") slot — the answer to
    /// "will the next synthesis pay a model load before its first
    /// token?". `None` means unknown, and callers MUST NOT narrate a
    /// load they cannot confirm.
    ///
    /// Exists separately from [`Self::resident_slots`] because that one
    /// is sync and contractually non-blocking, which a remote forwarder
    /// cannot satisfy while also telling the truth: it owns no weights,
    /// so the only honest answer requires asking the node that does.
    /// This method is async precisely so such a provider can ask.
    ///
    /// Called once per deep turn, immediately before a multi-second
    /// synthesis, so one localhost round-trip is noise against the wait
    /// it explains. Implementors must still fail soft — a provider that
    /// cannot answer returns `None` rather than blocking the turn.
    ///
    /// The default serves providers that own their weights: read the
    /// sync report and pick the primary row.
    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        self.resident_slots()
            .into_iter()
            .find(|s| s.role == "primary")
    }

    /// Live status of any supervised compute children this provider routes to
    /// (DISTRIBUTED_PILOT_READINESS.md P1). Default empty — only the
    /// compute-routing facade has children; every other provider inherits
    /// this. Rendered on `/status`.
    fn compute_children(&self) -> Vec<ComputeChildStatus> {
        Vec::new()
    }
}

/// Adapt a typed [`StreamFrame`] stream down to the legacy
/// `Result<String>` surface — the exact inverse of
/// [`InferenceProvider::complete_stream_with_finish`]'s default, and
/// the ONE implementation of that direction (ARCH §10.6, §7.5).
///
/// It lives here rather than at a call site because it had been
/// hand-written three times and the copies had DRIFTED on the question
/// that matters: **which frame carries a mid-stream failure.** There
/// are two terminal error shapes on this surface and a provider emits
/// only one of them —
///
/// - `StreamFrame::Error(msg)` is the WIRE shape. Only the compute
///   child client (`sovereign-compute/src/client.rs`) and the mesh
///   `inference_adapter` produce it.
/// - `Finish { reason: FinishReason::Error(msg), .. }` is the
///   IN-PROCESS shape. `EmbeddedLlamaCpp` and this trait's own default
///   `complete_stream_with_finish` produce only this one, never the
///   other.
///
/// Adapters written against the wire shape alone therefore compile,
/// pass, and silently truncate every embedded-engine stream that fails
/// mid-generation: the terminal frame matches `Finish { .. }`, gets
/// dropped, and the consumer sees a clean end of stream instead of an
/// error. That is what `sovereign-core`'s presenter path did until
/// this function replaced its inline copy. **Handle both, or an error
/// becomes a short answer.**
///
/// Non-error `Finish` frames close the stream and yield nothing —
/// end-of-stream on the legacy surface has no representation other
/// than the stream ending.
pub fn frames_to_text_stream(
    frames: Pin<Box<dyn Stream<Item = StreamFrame> + Send>>,
) -> Pin<Box<dyn Stream<Item = Result<String>> + Send>> {
    use futures::StreamExt;
    Box::pin(frames.filter_map(|frame| async move {
        match frame {
            StreamFrame::Token(text) => Some(Ok(text)),
            StreamFrame::Error(msg) => Some(Err(Error::Inference(msg))),
            StreamFrame::Finish {
                reason: FinishReason::Error(msg),
                ..
            } => Some(Err(Error::Inference(msg))),
            StreamFrame::Finish { .. } => None,
        }
    }))
}

// ─── 2. Routing ────────────────────────────────────────────────

/// Intent classification: turns a user message (plus conversation context and
/// the tool inventory) into a `RouterClassification`. Pure witness — acting on
/// the classification is `decide_policy`'s job.
#[async_trait]
pub trait Router: Send + Sync {
    /// Classify `message` into intent candidates with confidences. Must not mutate state or enact anything.
    async fn classify(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<RouterClassification>;
}

/// Emit-only channel for antifragile-routing user-facing events that
/// are *not* request/response.
///
/// - Interpretation-proposed: moderate-confidence classifications
///   stream normally but surface an inline banner so the user can
///   cheaply redirect.
/// - Clarification-request: low-confidence classifications suppress
///   synthesis and ask the user to pick an alternative or type
///   freeform input.
/// - Turn-narration: model-voice narration at phase boundaries for
///   long turns (suppressed under 5s, capped at 3 per turn).
///
/// Desktop implements this to emit Tauri events; CLI/server can
/// implement as loggers or no-ops. The default — an `Arc` around
/// [`NoOpRoutingEventSink`] — drops every event silently so
/// headless test harnesses and the CLI preserve exactly today's
/// surface.
#[async_trait]
pub trait RoutingEventSink: Send + Sync {
    /// Moderate-confidence banner: "interpreting as X", streamed alongside the answer so the user can cheaply redirect.
    async fn emit_interpretation_proposed(&self, payload: InterpretationProposed);
    /// Low-confidence ask: synthesis is suppressed until the user picks an alternative or types freeform.
    async fn emit_clarification_request(&self, payload: ClarificationRequest);
    /// Model-voice narration at a phase boundary of a long turn.
    async fn emit_turn_narration(&self, payload: TurnNarration);
}

/// No-op implementation. Returned by `Arc::new(NoOpRoutingEventSink)`
/// for Runtime builders that don't install a desktop sink. Keeps the
/// runtime's dispatcher branches branchless (no `if let Some(...)`
/// wrapping every emit site).
pub struct NoOpRoutingEventSink;

#[async_trait]
impl RoutingEventSink for NoOpRoutingEventSink {
    async fn emit_interpretation_proposed(&self, _payload: InterpretationProposed) {}
    async fn emit_clarification_request(&self, _payload: ClarificationRequest) {}
    async fn emit_turn_narration(&self, _payload: TurnNarration) {}
}

// ─── 3. Planning ───────────────────────────────────────────────

/// Decomposes a goal into an executable `Plan`, and repairs plans after step failures.
#[async_trait]
pub trait Planner: Send + Sync {
    /// Produce a step DAG for `goal`, choosing among `available_tools`.
    async fn plan(
        &self,
        goal: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Plan>;

    /// Produce a recovery plan after `failure`, given the original plan and the step outputs already banked in `completed`.
    ///
    /// `available_tools` is the same list `plan` was given. It is not
    /// derivable from `original` — a recovery plan routinely needs a
    /// tool the failed plan never used — and an implementation that
    /// constrains decoding needs the vocabulary to constrain against.
    async fn replan(
        &self,
        original: &Plan,
        completed: &[(usize, StepOutput)],
        failure: &StepError,
        available_tools: &[ToolDescriptor],
    ) -> Result<Plan>;
}

// ─── 4. Tool Execution ────────────────────────────────────────

/// An invocable capability registered in the `ToolRegistry`: a descriptor for
/// routing/planning, permissions for the consent layer, `execute` for the work.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Static metadata (id, parameter schema, behavioural properties) used for registry listing, routing, and planning.
    fn descriptor(&self) -> ToolDescriptor;
    /// Permissions the consent layer must hold before `execute` may run.
    fn required_permissions(&self) -> Vec<Permission>;

    /// Run the tool. `params` should already have passed `validate`; `ctx` carries conversation-scoped context.
    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput>;

    /// Cheap pre-execution parameter check. Default accepts everything; override to reject malformed params before any side effect.
    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let _ = params;
        Ok(())
    }

    /// Retry configuration for transient failures.
    /// Returns None for tools that should not retry (e.g., email send).
    fn retry_config(&self) -> Option<RetryConfig> {
        None
    }

    /// Cheap summary of salient current state. Returns `None` when the
    /// tool has no stateful signal to broadcast (the common case —
    /// stateless query tools always return None).
    ///
    /// Called by the context assembler every turn during
    /// `ReasonWithTools`. Must be fast (~ms), must not block, must not
    /// mutate state. Tools that read watcher stores, NoteStore digests,
    /// etc. can implement this to give the agent peripheral awareness
    /// of "is there something salient here?" without the agent having
    /// to poll each tool explicitly.
    ///
    /// Default returns `None`; the overwhelming majority of tools
    /// have nothing to signal and keep the default.
    async fn signal(&self) -> Option<String> {
        None
    }

    /// Deterministic authority claims over `question`
    /// (FINANCIAL_CORPORA.md §7.3). A tool backed by a typed
    /// authoritative store — where the same corpus's prose carries
    /// lookalike values that are NOT authoritative, and confusing the
    /// two causes material harm — answers from its own enumerable
    /// domain: does this question name an entity and an assertion class
    /// the store covers, for a corpus whose recipe DECLARED this tool
    /// authoritative (`[authority]` block)?
    ///
    /// Contract: pure, cheap (~µs after a lazily cached store load), no
    /// inference, no network, no threshold. The router consults this
    /// BEFORE intent classification; a claim routes the turn to the
    /// agentic path where the tool runs and the numeric audit applies.
    /// Over-claiming fails safe (an honest refusal naming what IS
    /// available), so implementations should prefer recall over
    /// precision — but must never claim without an entity match.
    ///
    /// Default: no claims — the overwhelming majority of tools are not
    /// authoritative stores and keep the default.
    fn claims(&self, question: &str) -> Vec<crate::types::AuthorityClaim> {
        let _ = question;
        Vec::new()
    }

    /// The corpora this tool declares authority OVER, question-independent
    /// (order authority-guard-at-exit, 2026-08-17). Same declaration index
    /// as [`Self::claims`] read at CORPUS granularity: `claims` asks "does
    /// this tool serve this question?" (a routing decision, deliberately
    /// narrowed — e.g. explanation-shaped questions are declined so they
    /// reach the prose path); this asks "does this tool vouch for figures
    /// over this corpus at all?" (a provenance decision, which must NOT
    /// inherit the routing narrowing — the answer-exit numeric guard arms
    /// off it precisely for the questions `claims` declines).
    ///
    /// One `AuthorityClaim` per declared corpus; `matched` describes the
    /// declaration, not a question match. Same purity contract as
    /// `claims`. Default: none — tools that are not authoritative stores
    /// keep the default, which is what makes corpora with no declaration
    /// structurally invisible to the exit guard.
    fn authority_domains(&self) -> Vec<crate::types::AuthorityClaim> {
        Vec::new()
    }
}

// ─── 5. Storage (sub-traits) ──────────────────────────────────

/// Persistence for conversations and their messages, plus the per-conversation
/// settings columns (skill tag, corpus allow-list, searched-sources registry).
#[async_trait]
pub trait ConversationStore: Send + Sync {
    /// Append `msg` to its conversation.
    async fn save_message(&self, msg: &Message) -> Result<()>;
    /// Load a conversation, with its message history, by id.
    async fn get_conversation(&self, id: &str) -> Result<Conversation>;
    /// Page through stored conversations (`limit`/`offset`).
    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>>;
    /// Full-text search across messages in all conversations.
    async fn search_messages(&self, query: &str) -> Result<Vec<Message>>;
    /// Soft-delete a conversation (tombstoned for sync-readiness, not physically removed).
    async fn delete_conversation(&self, id: &str) -> Result<()>;
    /// Update the conversation's display title and bump `updated_at`.
    /// Used by both auto-title generation and user rename actions.
    async fn update_conversation_title(&self, id: &str, title: &str) -> Result<()>;

    /// Tag a conversation with the skill that was active when it
    /// started. **Only sets the value when `skill_id` is currently
    /// NULL** — a conversation never changes the skill it was
    /// started under, even if skill activation shifts mid-session.
    ///
    /// Used by the Runtime on first message to populate
    /// `conversations.skill_id`, which the conversational
    /// KnowledgeView acquirer uses to exclude `privacy = local_only`
    /// conversations from the shared corpus.
    ///
    /// Default impl is a no-op so existing `ConversationStore`
    /// implementations (test doubles, in-memory stores) keep
    /// compiling. Real backends override.
    #[allow(unused_variables)]
    async fn set_conversation_skill_if_unset(
        &self,
        conversation_id: &str,
        skill_id: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Read this conversation's rendered `conversation-frame/v1`
    /// document — the named-section summary of what the conversation
    /// established outside its visible turn window. `None` = no frame
    /// yet (new conversation, or one that has not compacted).
    ///
    /// The fold watermark rides in the document's own frontmatter (see
    /// [`crate::frame`]), so this single accessor pair is the whole
    /// persistence surface: no side table, and a resumed conversation
    /// continues folding incrementally instead of re-summarising its
    /// entire history.
    ///
    /// Default impl returns `None` so existing `ConversationStore`
    /// implementations keep compiling. A store that does not override
    /// this is not broken, just amnesiac across processes — it pays one
    /// bounded cold fold per conversation per process.
    #[allow(unused_variables)]
    async fn get_conversation_frame(&self, conversation_id: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Persist this conversation's rendered frame document, replacing
    /// any previous one. See [`Self::get_conversation_frame`].
    ///
    /// Default impl is a no-op so existing `ConversationStore`
    /// implementations keep compiling. Real backends override.
    #[allow(unused_variables)]
    async fn set_conversation_frame(&self, conversation_id: &str, frame: &str) -> Result<()> {
        Ok(())
    }

    /// Persist the user-controlled per-conversation corpus allow-list.
    /// `None` clears the column ("all installed corpora", the default
    /// state); `Some(vec)` writes the explicit subset. Empty vec is
    /// structurally valid (stored as `[]`) and means "search nothing"
    /// — desktop UI is expected to guard against sending in that
    /// state, but the store does not.
    ///
    /// Default impl is a no-op so existing `ConversationStore`
    /// implementations (test doubles, in-memory stores) keep
    /// compiling. Real backends override.
    #[allow(unused_variables)]
    async fn set_conversation_enabled_corpora(
        &self,
        conversation_id: &str,
        enabled_corpora: Option<Vec<String>>,
    ) -> Result<()> {
        Ok(())
    }

    /// Marathon-graceful M3 — replace the cumulative web-source
    /// registry for this conversation. The `submit_information_search`
    /// Tauri command loads the current set, dedupes new URLs against
    /// it (bumping `last_referenced_turn` on duplicates, appending new
    /// entries), then writes the merged list back through this method.
    /// `None` clears the column entirely (re-creates the
    /// pre-migration state).
    ///
    /// Default `Ok(())` no-op lets test doubles / in-memory stores
    /// keep compiling without re-implementing the storage layer.
    #[allow(unused_variables)]
    async fn set_conversation_searched_sources(
        &self,
        conversation_id: &str,
        entries: Option<Vec<crate::types::SearchedSourceEntry>>,
    ) -> Result<()> {
        Ok(())
    }

    /// Create an empty conversation row if one doesn't already exist
    /// (INSERT OR IGNORE semantics). Needed by surfaces that must set
    /// per-conversation state — `skill_id`, `enabled_corpora` — *before*
    /// the first message is processed (the desktop "new chat" flow, and
    /// the eval harness's per-corpus isolation mode). A no-op default
    /// keeps test doubles / in-memory stores compiling; real backends
    /// override.
    #[allow(unused_variables)]
    async fn insert_empty_conversation(
        &self,
        id: &str,
        created_at: i64,
        surface_skill_id: Option<&str>,
    ) -> Result<()> {
        Ok(())
    }
}

/// Whole-task snapshot persistence. Contrast `StepExecutionStore`, the per-attempt ledger (ARCH §5.3).
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Insert or update the full task snapshot.
    async fn save_task(&self, task: &Task) -> Result<()>;
    /// Load a task by id.
    async fn get_task(&self, id: &str) -> Result<Task>;
}

/// Scope filter for memory recall. Enforces the inner-work memory
/// wall: scoped pools never recall outside their scope, and general
/// recall never sees scoped memories. The wall is bidirectional and
/// applied at the SQL layer.
///
/// Construct from a conversation's `skill_id`: `Some("inner-work")`
/// → `Scoped("inner-work")`; `None` → `General`. The runtime calls
/// `MemoryScope::from_conversation` and threads the result through
/// to the *_for_scope methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryScope {
    /// Pool that excludes any scoped memories. The default for chat,
    /// research, and any non-scoped surface. A memory whose
    /// `source_skill_id` is `None` recalls here; one whose
    /// `source_skill_id` is `Some(_)` does NOT.
    General,
    /// Pool restricted to memories tagged with this skill_id. Other
    /// memories (general or other-scope) do NOT recall here.
    Scoped(String),
}

impl MemoryScope {
    /// Derive the scope from a conversation's `skill_id`: non-empty `Some` → `Scoped`, otherwise `General`.
    pub fn from_conversation_skill(skill_id: Option<&str>) -> Self {
        match skill_id {
            Some(id) if !id.is_empty() => Self::Scoped(id.to_string()),
            _ => Self::General,
        }
    }

    /// Grouping key for per-scope memory enrichment artifacts (the
    /// `mem_raptor_nodes.scope` column). The `mem:` namespace keeps
    /// these keys collision-free with document/conversation/vault
    /// source ids should the stores ever share a table.
    pub fn atlas_key(&self) -> String {
        match self {
            Self::General => "mem:general".to_string(),
            Self::Scoped(id) => format!("mem:{id}"),
        }
    }
}

/// Persistence and recall for extracted memories, including the scoped-recall
/// wall and the memory-RAPTOR tree tiers (all tree methods default so
/// non-persistent stores compile).
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Insert or update a memory row.
    async fn save_memory(&self, memory: &Memory) -> Result<()>;
    /// Recall up to `limit` memories relevant to `context`. Unscoped — the wall-enforcing variant is `get_relevant_memories_for_scope`.
    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>>;
    /// Every stored memory row, unfiltered.
    async fn get_all_memories(&self) -> Result<Vec<Memory>>;
    /// Soft-delete a memory (user revocation; tombstoned for sync-readiness).
    async fn delete_memory(&self, id: &str) -> Result<()>;
    /// Overwrite a memory's `confidence` value.
    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()>;
    /// Refresh `last_used` to `timestamp` — resets the decay clock after a recall.
    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()>;

    /// Persist a memory's content embedding (T1 tier of the memory-pool
    /// tiered-retrieval port). Called best-effort by the lazy backfill in
    /// `recall_relevant_memories_embed` after it re-embeds a row that had no
    /// usable stored vector — failures degrade to re-embedding next turn,
    /// never to a recall error.
    ///
    /// `model` is the `InferenceProvider::embed_model_id()` that produced
    /// `embedding`; recall treats a model mismatch as "no stored embedding".
    /// Default is a no-op so non-persistent impls (mocks, stubs) compile.
    async fn update_memory_embedding(
        &self,
        id: &str,
        embedding: &[f32],
        model: &str,
    ) -> Result<()> {
        let _ = (id, embedding, model);
        Ok(())
    }

    /// Replace the memory-RAPTOR node set for one scope (T3 tier of the
    /// tiered-retrieval memory port). Atomic delete + insert so a
    /// crashed builder never leaves a half tree. `scope_key` is
    /// `MemoryScope::atlas_key()`.
    ///
    /// Default errors — a builder must notice a store that can't
    /// persist its output rather than silently dropping the tree.
    /// Read-side (`list_`) defaults to empty so recall on such stores
    /// simply stays flat.
    async fn save_mem_raptor_nodes(
        &self,
        scope_key: &str,
        nodes: &[MemRaptorNodeRow],
    ) -> Result<()> {
        let _ = (scope_key, nodes);
        Err(Error::NotImplemented(
            "save_mem_raptor_nodes not supported by this store".to_string(),
        ))
    }

    /// All memory-RAPTOR nodes for one scope, highest level first.
    /// Empty = no tree (never built, pool too small, or store doesn't
    /// persist trees) — recall treats that as flat T1.
    async fn list_mem_raptor_nodes(&self, scope_key: &str) -> Result<Vec<MemRaptorNodeRow>> {
        let _ = scope_key;
        Ok(Vec::new())
    }

    /// Write ONE tree node (insert or replace). The incremental path
    /// (`mem_tree`) touches O(path) rows per memory insert — this is
    /// its single-row write, in contrast to `save_mem_raptor_nodes`'s
    /// whole-scope replace. Default errors like the batch save: a
    /// mutation path must notice a store that drops its writes.
    async fn upsert_mem_raptor_node(&self, node: &MemRaptorNodeRow) -> Result<()> {
        let _ = node;
        Err(Error::NotImplemented(
            "upsert_mem_raptor_node not supported by this store".to_string(),
        ))
    }

    /// Remove ONE tree node (split replaced it, or eviction emptied
    /// it). Idempotent.
    async fn delete_mem_raptor_node(&self, node_id: &str) -> Result<()> {
        let _ = node_id;
        Err(Error::NotImplemented(
            "delete_mem_raptor_node not supported by this store".to_string(),
        ))
    }

    /// Drop the tree for one scope (pool shrank below the build
    /// threshold, or a rebuild is invalidating). Idempotent.
    async fn delete_mem_raptor_nodes_for_scope(&self, scope_key: &str) -> Result<()> {
        let _ = scope_key;
        Ok(())
    }

    /// Scoped variant of `get_relevant_memories`. Default impl filters
    /// the unscoped result in-process so existing impls compile, but
    /// SQL backends should override with a server-side filter for the
    /// wall to be a real privacy guarantee (in-process filtering still
    /// loads scoped rows into memory before discarding them, which
    /// the postgres path leaks via observers and replication).
    async fn get_relevant_memories_for_scope(
        &self,
        scope: &MemoryScope,
        context: &str,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        let raw = self.get_relevant_memories(context, limit * 4).await?;
        Ok(filter_memories_for_scope(raw, scope, limit))
    }

    /// Scoped variant of `get_all_memories`. Same default-impl caveat.
    async fn get_all_memories_for_scope(&self, scope: &MemoryScope) -> Result<Vec<Memory>> {
        let raw = self.get_all_memories().await?;
        Ok(raw
            .into_iter()
            .filter(|m| matches_scope(m, scope))
            .collect())
    }

    /// All memories whose `source_conversation_id` matches and which
    /// have not been superseded. Used by the rolling-summary
    /// compaction worker to enumerate candidates per conversation.
    /// Returns memories ordered by `created_at` ascending so the
    /// caller can pick "oldest M" deterministically.
    ///
    /// Default impl loads everything and filters in-process so
    /// existing impls compile without a server-side override; the
    /// production sqlite/postgres impls override for efficiency.
    async fn list_memories_for_conversation(&self, conversation_id: &str) -> Result<Vec<Memory>> {
        let mut all = self.get_all_memories().await?;
        all.retain(|m| {
            m.source_conversation_id.as_deref() == Some(conversation_id)
                && m.superseded_by.is_none()
                && m.deleted_at.is_none()
        });
        all.sort_by_key(|m| m.created_at);
        Ok(all)
    }

    /// Mark `memory_id` as folded into `summary_id`. Called by the
    /// compaction worker after a new Summary row has been saved.
    /// Subsequent retrieval excludes the superseded memory; the body
    /// is preserved so `sovereign memory expand <summary-id>` can
    /// walk the chain.
    ///
    /// Idempotent: marking a row already superseded by the same
    /// summary is a no-op. Marking with a different summary id
    /// overwrites — last-writer-wins, matching the single-threaded
    /// compaction worker contract.
    async fn mark_superseded(&self, memory_id: &str, summary_id: &str) -> Result<()> {
        let _ = (memory_id, summary_id);
        Err(crate::error::Error::NotImplemented(
            "mark_superseded not implemented for this store".into(),
        ))
    }
}

/// In-process scope filter. Used by the default impls above and as
/// the in-memory store's enforcement point.
pub fn matches_scope(memory: &Memory, scope: &MemoryScope) -> bool {
    match scope {
        MemoryScope::General => memory.source_skill_id.is_none(),
        MemoryScope::Scoped(id) => memory.source_skill_id.as_deref() == Some(id.as_str()),
    }
}

fn filter_memories_for_scope(
    memories: Vec<Memory>,
    scope: &MemoryScope,
    limit: usize,
) -> Vec<Memory> {
    memories
        .into_iter()
        .filter(|m| matches_scope(m, scope))
        .take(limit)
        .collect()
}

/// Persistence for the routing log: per-message classifications, correctness
/// feedback, and redirect signals that feed threshold calibration.
#[async_trait]
pub trait RoutingStore: Send + Sync {
    /// Record one classification: the message's hash, the chosen intent label, and classification latency.
    async fn log_routing(
        &self,
        message_hash: &str,
        classified_as: &str,
        latency_ms: i64,
    ) -> Result<()>;
    /// Attach metacognition fields to a routing_log row written by `log_routing`.
    /// Default no-op so existing implementations compile without changes.
    async fn log_routing_meta(
        &self,
        message_hash: &str,
        coarse_intent: &str,
        self_assessment: Option<&str>,
    ) -> Result<()> {
        let _ = (message_hash, coarse_intent, self_assessment);
        Ok(())
    }
    /// Most recent user-flagged misclassifications (rows with `was_correct = false`) — the router's avoid-list.
    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>>;
    /// Record the user's verdict on the classification previously logged for `message_hash`.
    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()>;
    /// PR4 — record an explicit user redirect away from a
    /// Propose-tier commit. Sets `routing_log.was_redirected = 1`
    /// and `routing_log.redirect_to = <intent_hint>` for the row
    /// previously written by `log_routing`. A future calibration
    /// job tunes confidence thresholds from the aggregate of these
    /// signals. Default no-op so legacy implementations compile.
    async fn mark_routing_redirected(&self, message_hash: &str, redirect_to: &str) -> Result<()> {
        let _ = (message_hash, redirect_to);
        Ok(())
    }
}

/// Persistence and hybrid retrieval for `DocumentChunk`s.
#[async_trait]
pub trait DocumentStore: Send + Sync {
    /// Persist a batch of chunks.
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()>;
    /// Retrieve the `limit` best chunks for a query, given both its embedding (vector side) and raw text (FTS side).
    async fn search_documents(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<DocumentChunk>>;
    /// Scored variant of `search_documents`. The default impl returns the
    /// same hits with a placeholder `score: 0.0` — score-aware callers only
    /// see real relevance on stores that override this.
    async fn search_documents_scored(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        let chunks = self
            .search_documents(query_embedding, query_text, limit)
            .await?;
        Ok(chunks
            .into_iter()
            .map(|c| ScoredChunk {
                chunk: c,
                score: 0.0,
            })
            .collect())
    }
    /// All chunks belonging to one document, by its `source` key.
    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>>;
    /// Remove every chunk of a corpus; returns how many rows were removed.
    async fn delete_chunks_by_corpus(&self, corpus_id: &str) -> Result<u64>;
    /// Distinct `source` keys currently stored.
    async fn list_sources(&self) -> Result<Vec<String>>;
}

/// Persistence for `CorpusState` rows (the `corpus_state` table).
#[async_trait]
pub trait CorpusStateStore: Send + Sync {
    /// Insert or update a corpus's state row.
    async fn save_corpus_state(&self, state: &CorpusState) -> Result<()>;
    /// Load one corpus's state by id.
    async fn get_corpus_state(&self, corpus_id: &str) -> Result<CorpusState>;
    /// State rows for every installed corpus.
    async fn list_corpus_states(&self) -> Result<Vec<CorpusState>>;
    /// Soft-delete a corpus's state row (uninstall).
    async fn delete_corpus_state(&self, corpus_id: &str) -> Result<()>;
    /// Flip `CorpusState::vector_index_ready` once an IVF-PQ build finishes (or back to false on invalidation).
    async fn set_vector_index_ready(&self, corpus_id: &str, ready: bool) -> Result<()>;
    /// Whether vector search may be used for this corpus; false = FTS-only fallback.
    async fn get_vector_index_ready(&self, corpus_id: &str) -> Result<bool>;
}

/// Persistence for per-backend web-search quotas.
#[async_trait]
pub trait BudgetStore: Send + Sync {
    /// Budget row for `backend`; `None` when none has been created yet.
    async fn get_search_budget(&self, backend: &str) -> Result<Option<SearchBudget>>;
    /// Upsert the budget row (usage increments and monthly resets).
    async fn update_search_budget(&self, budget: &SearchBudget) -> Result<()>;
}

// ─── Mesh knowledge (optional, injected by sovereign-mesh) ────
//
// Decouples `sovereign-core::Runtime` from `sovereign-mesh` so the
// standalone (no-mesh) configuration keeps zero mesh dependencies.
// `Runtime` accepts an `Option<Arc<dyn MeshKnowledgeSource>>`; the
// desktop populates it with a client that POSTs to its own local
// Commonwealth daemon at `127.0.0.1:9741/v1/knowledge/search`.
//
// The return type carries peer attribution so `prepare_knowledge_context`
// can annotate provenance ("sep (6) via mac-peer") without having to
// know anything about mesh topology itself.

/// A single retrieval hit from the mesh, possibly tagged with the
/// peer that served it. `peer_name` is `None` when the hit came
/// from our own local index served via `/v1/knowledge/search` — a
/// consequence of fan-out also searching locally.
#[derive(Debug, Clone)]
pub struct MeshScoredChunk {
    /// Chunk text as returned by the serving index.
    pub content: String,
    /// Document/article title, when the producing index knows it.
    pub title: Option<String>,
    /// Corpus the hit came from, as named on the serving peer.
    pub corpus_id: String,
    /// Source URL, when the corpus carries one.
    pub url: Option<String>,
    /// Relevance score assigned by the serving index.
    pub score: f32,
    /// Peer that served the hit; `None` = our own local index (see type doc).
    pub peer_name: Option<String>,
    /// Stable chunk id from the producing peer's index. Forwarded
    /// from `KnowledgeResult.chunk_id` so the desktop reading
    /// surface can deref a peer-served citation back to that peer
    /// (deref still requires reaching the peer's chunk endpoint —
    /// pre-built only for local citations in v1, but the id needs
    /// to round-trip so we don't silently lose it).
    pub chunk_id: Option<u64>,
    /// Stable id of the chunk's parent document on the producing peer, when known.
    pub source_doc_id: Option<String>,
}

/// Why a corpus the turn would have searched could not serve it.
///
/// A CLOSED set (ARCH §2) — every way retrieval can lose a corpus, named once.
/// The first three are LOCAL readiness losses, decided by
/// `runtime::retrieval::corpus_search::corpus_unavailability`; the last is the
/// MESH loss, decided by the fan-out (peer refused / unreachable / the
/// daemon's own `corpora_unavailable`). Both families are the same defect —
/// the signal exists at the point of loss and used to die before the answer
/// surface (`MESH_SCALE_100_USERS_1000_CORPORA.md` §9.6, note 89d5f75a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailabilityReason {
    /// The index build never finished (ingest stalled / sync paused).
    NotBuilt,
    /// The build finished but the vector index was never written.
    NoVectorIndex,
    /// Built with a different embedding model than the one now loaded, so
    /// its vectors cannot be compared to the query's.
    DimMismatch {
        /// Dimensionality the corpus was built at.
        built: usize,
    },
    /// A peer hosts the corpus and could not serve it this turn — it
    /// refused (503, yielding to its local user), timed out, or the
    /// fan-out never reached it.
    PeerUnreachable,
}

impl UnavailabilityReason {
    /// Stable log tag — the glassbox axis (`reason=` on the trace line).
    /// Never shown to a user.
    pub fn log_tag(&self) -> &'static str {
        match self {
            Self::NotBuilt => "index_not_built",
            Self::NoVectorIndex => "vector_index_missing",
            Self::DimMismatch { .. } => "dim_mismatch",
            Self::PeerUnreachable => "peer_unreachable",
        }
    }

    /// Plain-language cause, deliberately free of "index", "vector",
    /// "embedding" and "dimensions" — those leaked into answers verbatim and
    /// read as a cold, broken refusal (see the readiness-disclosure step).
    pub fn user_phrase(&self) -> &'static str {
        match self {
            Self::NotBuilt => "hasn't finished building yet (a sync or import may have paused)",
            Self::NoVectorIndex => "isn't fully indexed for search yet",
            Self::DimMismatch { .. } => "needs a quick rebuild first",
            Self::PeerUnreachable => "is on another machine that couldn't be reached just now",
        }
    }

    /// What the user can DO about it, in the same plain register. The three
    /// local losses are all fixed by a rebuild; a peer loss is not the user's
    /// machine to fix, and telling them to rebuild would be a wrong
    /// instruction — which is why this is a `match` and not one string.
    pub fn user_remedy(&self) -> &'static str {
        match self {
            Self::NotBuilt | Self::NoVectorIndex | Self::DimMismatch { .. } => {
                "rebuilding it in Settings → Knowledge → Rebuild will fix it"
            }
            Self::PeerUnreachable => {
                "it should come back on its own once that machine is available"
            }
        }
    }
}

/// One corpus the turn would have searched, and why it could not.
///
/// THE one unavailability record. Every loss site writes this type and
/// nothing else; the answer surface renders from it. ARCH §18.3 — absence is
/// reported, never defaulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusUnavailable {
    /// Corpus id as the request named it.
    pub corpus_id: String,
    /// Why it could not serve this turn.
    pub reason: UnavailabilityReason,
}

impl CorpusUnavailable {
    /// Construct a record. The only constructor — loss sites name the corpus
    /// and the reason, nothing else.
    pub fn new(corpus_id: impl Into<String>, reason: UnavailabilityReason) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            reason,
        }
    }
}

/// What a mesh fan-out produced: the hits, AND the corpora it could not
/// reach.
///
/// The second field is the point of this type. Before 2026-08-14 the seam
/// returned a bare `Vec<MeshScoredChunk>`, so the daemon's own
/// `corpora_unavailable` — computed one function away — was discarded at the
/// client and a peer-only question came back answered from an unrelated local
/// corpus with nothing saying anything was missing (§9.6).
#[derive(Debug, Clone, Default)]
pub struct MeshSearchOutcome {
    /// Hits that were actually served.
    pub chunks: Vec<MeshScoredChunk>,
    /// Corpora the fan-out was asked for and could not deliver. EMPTY means
    /// "nothing was lost", never "we didn't look" — a transport failure
    /// reports the corpora it was asked for rather than collapsing to an
    /// empty vec (ARCH §18.3).
    pub unavailable: Vec<CorpusUnavailable>,
}

/// Optional mesh retrieval seam — injected by `sovereign-mesh` so the no-mesh
/// build keeps zero mesh dependencies (see the section comment above).
#[async_trait]
pub trait MeshKnowledgeSource: Send + Sync {
    /// Query the mesh for knowledge. Returns an outcome with no hits when the
    /// mesh is unreachable, has no corpora, or hasn't converged yet —
    /// *never* propagates a network error up into query preparation,
    /// because a broken mesh should degrade gracefully to local-only
    /// search rather than fail the whole user request. Degrading is not the
    /// same as going quiet: whatever could not be served is named in
    /// [`MeshSearchOutcome::unavailable`], and the answer surface renders it.
    ///
    /// `corpora` carries the conversation's `enabled_corpora` seal.
    /// When `Some`, the fan-out (this node's local view **and** peers)
    /// is scoped to those corpus ids at the SOURCE — not merely
    /// filtered out of the result set afterwards. This is load-bearing
    /// for memory safety: an unsealed fan-out opens every hosted index,
    /// and on a node with a large corpus installed (e.g. a 1.9M-row
    /// `wikipedia`) that search OOM-kills the daemon. `None` means
    /// "search everything hosted" (the unsealed, broad-research case).
    async fn search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
        corpora: Option<&[String]>,
    ) -> MeshSearchOutcome;
}

/// Persistence for per-tool consent grants.
#[async_trait]
pub trait PermissionStore: Send + Sync {
    /// Stored grant for `(tool_id, scope)`: `Some(true/false)` = the user decided, `None` = never asked.
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>>;
    /// Record the user's grant or denial for `(tool_id, scope)`.
    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()>;
}

/// Durable ledger of step-execution *attempts* — the replay-safety and
/// audit surface behind the agent control loop. A sibling of
/// [`TaskStore`], not a widening of it (ARCH §5.3): this is a distinct
/// concern (per-attempt history vs whole-task snapshot) that most
/// callers and test mocks don't need.
///
/// Every method defaults to a no-op so non-durable contexts (test mocks,
/// automation without a real store) are unaffected — absent a backing
/// store the executor's resume guard simply sees no prior attempts and
/// behaves exactly as it did before this ledger existed. The real stores
/// (`SqliteStateStore`, `PostgresStateStore`, `InMemoryStateStore`)
/// override with durable behaviour.
#[async_trait]
pub trait StepExecutionStore: Send + Sync {
    /// Record an attempt as `Started` — written *before* the side effect so
    /// a crash mid-step leaves a durable "may have run" marker.
    async fn record_started(&self, _execution: &StepExecution) -> Result<()> {
        Ok(())
    }

    /// Flip an attempt to `Completed`, attaching its compressed summary and
    /// any anomalies. Written *after* the side effect returns.
    async fn mark_completed(
        &self,
        _execution_id: &str,
        _summary: Option<String>,
        _anomalies: Option<String>,
    ) -> Result<()> {
        Ok(())
    }

    /// Flip an attempt to `Failed` with a message. Terminal and replay-safe.
    async fn mark_failed(&self, _execution_id: &str, _message: &str) -> Result<()> {
        Ok(())
    }

    /// The idempotency guard's query: the most recent attempt recorded for
    /// `idempotency_key` (any status), or `None` if this action has never
    /// run. The executor branches on the returned status — `Completed` →
    /// the action already succeeded (a replan/duplicate; skip and reuse the
    /// prior result); `Started` → a crash interrupted it mid-flight (do not
    /// blind-replay a `NonIdempotent` side-effect — halt and surface). The
    /// key is content-derived (`task:tool:hash(params)`) so it matches
    /// across a replan that re-issues the same action under a new `step_id`,
    /// not merely a same-plan resume.
    async fn find_execution(&self, _idempotency_key: &str) -> Result<Option<StepExecution>> {
        Ok(None)
    }
}

/// Persistence for health reports and pending repair decisions. Every method
/// defaults to a no-op so stores without health tables compile unchanged.
#[async_trait]
pub trait HealthStore: Send + Sync {
    /// Persist the latest health report for a component.
    async fn save_health_report(&self, report: &crate::health::HealthReport) -> Result<()> {
        let _ = report;
        Ok(())
    }
    /// Persist a repair decision awaiting user input; the store assigns
    /// `PendingDecision::id`.
    async fn save_pending_decision(&self, d: &crate::health::PendingDecision) -> Result<()> {
        let _ = d;
        Ok(())
    }
    /// All decisions still awaiting user input.
    async fn list_pending_decisions(&self) -> Result<Vec<crate::health::PendingDecision>> {
        Ok(vec![])
    }
    /// Record the user's choice for decision `id` and clear it from the pending set.
    async fn resolve_pending_decision(
        &self,
        id: i64,
        chosen: crate::health::RepairKind,
    ) -> Result<()> {
        let _ = (id, chosen);
        Ok(())
    }
}

/// Persistence for `DocumentSession`s (uploaded-document map/reduce sessions).
#[async_trait]
pub trait DocumentSessionStore: Send + Sync {
    /// Persist a new session after upload + planning.
    async fn create_document_session(&self, session: &DocumentSession) -> Result<()>;
    /// Load a session by its id.
    async fn get_document_session(&self, session_id: &str) -> Result<Option<DocumentSession>>;
    /// Look up the session belonging to `conversation_id`, if any.
    async fn get_document_session_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<DocumentSession>>;
    /// Overwrite a session (new `last_output`, appended `history`).
    async fn update_document_session(&self, session: &DocumentSession) -> Result<()>;
}

// ─── Document Asset Store ────────────────────────────────────

/// Persistence for document assets and their derived artifacts: skeletons, RAPTOR trees, motif indexes, operation records.
#[async_trait]
pub trait DocumentAssetStore: Send + Sync {
    /// Persist a new document asset. Called immediately after parsing
    /// so the asset appears in the library while processing continues.
    async fn save_document_asset(&self, asset: &DocumentAsset) -> Result<()>;

    /// Update just the processing state. Called frequently during
    /// ingest to drive UI progress.
    async fn update_asset_state(&self, id: &str, state: &AssetState) -> Result<()>;

    /// Store the completed skeleton and the detected document type.
    /// Called once when skeleton extraction finishes. The two fields are
    /// persisted atomically since they come from the same pipeline and
    /// should never disagree.
    async fn save_asset_skeleton(
        &self,
        id: &str,
        skeleton: &DocumentSkeleton,
        document_type: &DocumentTypeTag,
    ) -> Result<()>;

    /// Retrieve a single asset by ID.
    async fn get_document_asset(&self, id: &str) -> Result<Option<DocumentAsset>>;

    /// List all assets, ordered by ingested_at descending.
    async fn list_document_assets(&self) -> Result<Vec<DocumentAsset>>;

    /// Delete an asset and its associated data.
    async fn delete_document_asset(&self, id: &str) -> Result<()>;

    /// Record which operation was used for a document response.
    /// Stored alongside message metadata for the operation badge
    /// and for analytics.
    async fn save_document_operation(
        &self,
        message_id: &str,
        asset_id: &str,
        operation: &DocumentAssetOperation,
        duration_ms: u64,
    ) -> Result<()>;

    /// Persist all RAPTOR nodes for an asset in one transaction.
    /// Replaces any existing nodes for the same asset — the tree is
    /// rebuilt atomically, not incrementally.
    async fn save_raptor_nodes(&self, asset_id: &str, nodes: &[RaptorNode]) -> Result<()>;

    /// All RAPTOR nodes for an asset, ordered by level ascending
    /// (leaves first).
    async fn list_raptor_nodes(&self, asset_id: &str) -> Result<Vec<RaptorNode>>;

    /// Fetch a single node by its node_id. Used by the granularity-
    /// aware retrieval tool when the model drills from a parent to
    /// its evidence chunks.
    async fn get_raptor_node(&self, node_id: &str) -> Result<Option<RaptorNode>>;

    /// Persist the motif index for an asset. Replaces any existing
    /// motifs for the same asset.
    async fn save_asset_motifs(&self, asset_id: &str, motifs: &[AssetMotif]) -> Result<()>;

    /// Motif index for an asset, distinctive motifs first.
    async fn list_asset_motifs(&self, asset_id: &str) -> Result<Vec<AssetMotif>>;
}

// ─── 6. Storage (supertrait) ──────────────────────────────────

/// The whole-store supertrait: one object implementing every storage sub-trait.
/// The production stores (`SqliteStateStore`, `PostgresStateStore`,
/// `InMemoryStateStore`) implement it; callers that need everything hold
/// `Arc<dyn StateStore>`, narrower callers name just the sub-trait they use.
#[async_trait]
pub trait StateStore:
    ConversationStore
    + TaskStore
    + MemoryStore
    + RoutingStore
    + DocumentStore
    + CorpusStateStore
    + BudgetStore
    + PermissionStore
    + StepExecutionStore
    + HealthStore
    + DocumentSessionStore
    + DocumentAssetStore
{
}

// ─── Approval Channel ─────────────────────────────────────────

/// The executor's line to the user: approval prompts, questions, progress, and
/// UI-refresh notifications. Implemented per surface (desktop Tauri events,
/// server SSE, CLI prompt); dropped prompts surface as `Error::Cancelled`.
#[async_trait]
pub trait ApprovalChannel: Send + Sync {
    /// Ask the user to approve `step` before it runs, showing `preview`. `Ok(false)` = declined.
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool>;
    /// Put a `UserInput` step's question to the user and return their free-form reply.
    async fn ask_user(&self, question: &str) -> Result<String>;
    /// Fire-and-forget per-step progress notification. Sync — must not block the executor.
    fn emit_progress(&self, step: &Step, output: &StepOutput);

    /// Surface a structured information request to the user and wait
    /// asynchronously for them to either paste content or skip.
    ///
    /// `Some(content)` — user pasted a passage / paragraph / source.
    /// `None` — user pressed skip; the caller should proceed with current
    /// knowledge.
    ///
    /// Default impl returns `None` so non-interactive contexts (tests,
    /// automation, server runs without a UI) don't block.
    async fn request_information(&self, _request: &InformationRequest) -> Option<String> {
        None
    }

    /// Notify the UI that an already-streamed assistant message has
    /// been re-synthesised (see `Runtime::maybe_collaborate`). The
    /// default impl is a no-op — non-UI surfaces simply let the
    /// new content land in the store on the next read.
    fn emit_message_refined(&self, _payload: MessageRefinedPayload) {}

    /// Surface a drafted lesson card (TEACHABLE P0). Fire-and-forget —
    /// no pending map, no reply channel; the user's Save lands through
    /// a separate desktop command and dismissal calls nothing, so the
    /// originating turn never blocks. Default no-op keeps every non-UI
    /// impl (CLI, server, tests, automation) unchanged.
    fn emit_lesson_proposed(&self, _payload: LessonProposedPayload) {}
}

// ─── 7. Insight Storage ──────────────────────────────────────

/// Persistence for insight nodes. Implemented by SqliteInsightStore.
/// Standalone trait — not part of StateStore — to avoid cascading changes.
#[async_trait]
pub trait InsightStore: Send + Sync {
    /// Save a new insight node.
    async fn save(&self, node: &InsightNode) -> Result<()>;

    /// Retrieve a single node by ID.
    async fn get(&self, id: uuid::Uuid) -> Result<InsightNode>;

    /// Retrieve all nodes, newest first.
    async fn list(&self, limit: usize) -> Result<Vec<InsightNode>>;

    /// Retrieve nodes by a list of IDs.
    async fn list_by_ids(&self, ids: &[uuid::Uuid]) -> Result<Vec<InsightNode>>;

    /// Full-text search over clipped text.
    async fn search_text(&self, query: &str, limit: usize) -> Result<Vec<InsightNode>>;

    /// Retrieve nodes most similar to a given embedding (cosine similarity).
    async fn adjacent_by_embedding(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<InsightNode>>;

    /// Update the sink state of a node (called by sync machinery).
    async fn update_sink_state(
        &self,
        node_id: uuid::Uuid,
        sink_state: InsightSinkState,
    ) -> Result<()>;

    /// Soft-delete a node by ID.
    async fn delete(&self, node_id: uuid::Uuid) -> Result<()>;
}

// ─── 8. Insight Sink ─────────────────────────────────────────

/// An external destination for insight nodes.
/// The native SQLite store is not a sink — it's always present.
/// Sinks are optional additional destinations.
///
/// Currently: zero implementations. The trait is defined now so
/// the sync architecture is in place when Obsidian sync is built.
#[async_trait]
pub trait InsightSink: Send + Sync {
    /// Stable machine id of the sink (its config key).
    fn id(&self) -> &str;
    /// Human-readable sink name for the settings UI.
    fn display_name(&self) -> &str;
    /// Whether the sink is currently reachable/authorised.
    async fn is_connected(&self) -> bool;
    /// Send one node to the sink.
    async fn push(&self, node: &InsightNode) -> Result<()>;
    /// Send many nodes in one operation (sync catch-up).
    async fn push_batch(&self, nodes: &[InsightNode]) -> Result<()>;
}
