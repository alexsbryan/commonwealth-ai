use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{Error, Result};
use crate::types::*;

// Re-export observer types so `sovereign_core::StateStoreObserver`
// works alongside `sovereign_core::StateStore`.
pub use crate::observer::{noop_observer, NoopObserver, SharedStateStoreObserver, StateStoreObserver};

/// Produces `knowledge_view_digests` for a
/// [`ConversationContext`][crate::types::ConversationContext] after
/// skill routing has resolved.
///
/// Defined in `sovereign-core` so [`Runtime`][crate::runtime::Runtime]
/// can splice digests without depending on `sovereign-tools` (which
/// would create a circular dependency). `KnowledgeViewManager` in
/// `sovereign-tools` is the canonical implementation.
///
/// See `ConversationContext.knowledge_view_digests` for the invariant:
/// a `None` value reaching the prompt-assembly site is a bug. Runtime
/// calls `splice_landscape_digests` after resolving the active skill.
#[async_trait]
pub trait LandscapeDigestProvider: Send + Sync {
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
    async fn entity_inventory(&self) -> Option<crate::memory::EntityInventory> {
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
/// Defined in `sovereign-core` so [`Runtime`][crate::runtime::Runtime]
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

// ─── 1. Inference ──────────────────────────────────────────────

#[async_trait]
pub trait InferenceProvider: Send + Sync {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse>;

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>>;

    /// Streaming variant that also returns the model id actually
    /// chosen to serve this request. Exists because
    /// `complete_stream` itself returns only a `Stream<String>` —
    /// there's nowhere to attach "I routed this to peer BeefyMac's
    /// Qwen3.5-9B" to, so streaming provenance has historically
    /// fallen back to the synchronous `model_id_for(Speed)` which
    /// can't see any routing decision made inside the async call.
    ///
    /// Default implementation preserves the legacy behaviour:
    /// delegate to `complete_stream` and stamp the model_id with
    /// whatever `model_id_for` reports for the request's speed.
    /// Mesh-aware wrappers override this to return the peer-
    /// attributed id (e.g. `"Qwen3.5-9B @ peer BeefyMac"`). All
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
    /// overrides this when a `[rerank]` slot is configured in
    /// `models.toml`. The `CorpusIndex::search_with_rerank` path
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
}

// ─── 2. Routing ────────────────────────────────────────────────

#[async_trait]
pub trait Router: Send + Sync {
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
    async fn emit_interpretation_proposed(&self, payload: InterpretationProposed);
    async fn emit_clarification_request(&self, payload: ClarificationRequest);
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

#[async_trait]
pub trait Planner: Send + Sync {
    async fn plan(
        &self,
        goal: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Plan>;

    async fn replan(
        &self,
        original: &Plan,
        completed: &[(usize, StepOutput)],
        failure: &StepError,
    ) -> Result<Plan>;
}

// ─── 4. Tool Execution ────────────────────────────────────────

#[async_trait]
pub trait Tool: Send + Sync {
    fn descriptor(&self) -> ToolDescriptor;
    fn required_permissions(&self) -> Vec<Permission>;

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput>;

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
}

// ─── 5. Storage (sub-traits) ──────────────────────────────────

#[async_trait]
pub trait ConversationStore: Send + Sync {
    async fn save_message(&self, msg: &Message) -> Result<()>;
    async fn get_conversation(&self, id: &str) -> Result<Conversation>;
    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>>;
    async fn search_messages(&self, query: &str) -> Result<Vec<Message>>;
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
}

#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn save_task(&self, task: &Task) -> Result<()>;
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
    pub fn from_conversation_skill(skill_id: Option<&str>) -> Self {
        match skill_id {
            Some(id) if !id.is_empty() => Self::Scoped(id.to_string()),
            _ => Self::General,
        }
    }
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn save_memory(&self, memory: &Memory) -> Result<()>;
    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>>;
    async fn get_all_memories(&self) -> Result<Vec<Memory>>;
    async fn delete_memory(&self, id: &str) -> Result<()>;
    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()>;
    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()>;

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
    async fn list_memories_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<Memory>> {
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
    async fn mark_superseded(
        &self,
        memory_id: &str,
        summary_id: &str,
    ) -> Result<()> {
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

#[async_trait]
pub trait RoutingStore: Send + Sync {
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
    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>>;
    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()>;
    /// PR4 — record an explicit user redirect away from a
    /// Propose-tier commit. Sets `routing_log.was_redirected = 1`
    /// and `routing_log.redirect_to = <intent_hint>` for the row
    /// previously written by `log_routing`. A future calibration
    /// job tunes confidence thresholds from the aggregate of these
    /// signals. Default no-op so legacy implementations compile.
    async fn mark_routing_redirected(
        &self,
        message_hash: &str,
        redirect_to: &str,
    ) -> Result<()> {
        let _ = (message_hash, redirect_to);
        Ok(())
    }
}

#[async_trait]
pub trait DocumentStore: Send + Sync {
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()>;
    async fn search_documents(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<DocumentChunk>>;
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
            .map(|c| ScoredChunk { chunk: c, score: 0.0 })
            .collect())
    }
    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>>;
    async fn delete_chunks_by_corpus(&self, corpus_id: &str) -> Result<u64>;
    async fn list_sources(&self) -> Result<Vec<String>>;
}

#[async_trait]
pub trait CorpusStateStore: Send + Sync {
    async fn save_corpus_state(&self, state: &CorpusState) -> Result<()>;
    async fn get_corpus_state(&self, corpus_id: &str) -> Result<CorpusState>;
    async fn list_corpus_states(&self) -> Result<Vec<CorpusState>>;
    async fn delete_corpus_state(&self, corpus_id: &str) -> Result<()>;
    async fn set_vector_index_ready(&self, corpus_id: &str, ready: bool) -> Result<()>;
    async fn get_vector_index_ready(&self, corpus_id: &str) -> Result<bool>;
}

#[async_trait]
pub trait BudgetStore: Send + Sync {
    async fn get_search_budget(&self, backend: &str) -> Result<Option<SearchBudget>>;
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
// can annotate provenance ("sep (6) via BeefyMac") without having to
// know anything about mesh topology itself.

/// A single retrieval hit from the mesh, possibly tagged with the
/// peer that served it. `peer_name` is `None` when the hit came
/// from our own local index served via `/v1/knowledge/search` — a
/// consequence of fan-out also searching locally.
#[derive(Debug, Clone)]
pub struct MeshScoredChunk {
    pub content: String,
    pub title: Option<String>,
    pub corpus_id: String,
    pub url: Option<String>,
    pub score: f32,
    pub peer_name: Option<String>,
    /// Stable chunk id from the producing peer's index. Forwarded
    /// from `KnowledgeResult.chunk_id` so the desktop reading
    /// surface can deref a peer-served citation back to that peer
    /// (deref still requires reaching the peer's chunk endpoint —
    /// pre-built only for local citations in v1, but the id needs
    /// to round-trip so we don't silently lose it).
    pub chunk_id: Option<u64>,
    pub source_doc_id: Option<String>,
}

#[async_trait]
pub trait MeshKnowledgeSource: Send + Sync {
    /// Query the mesh for knowledge. Returns an empty vec when the
    /// mesh is unreachable, has no corpora, or hasn't converged yet —
    /// *never* propagates a network error up into query preparation,
    /// because a broken mesh should degrade gracefully to local-only
    /// search rather than fail the whole user request.
    async fn search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Vec<MeshScoredChunk>;
}

#[async_trait]
pub trait PermissionStore: Send + Sync {
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>>;
    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()>;
}

#[async_trait]
pub trait HealthStore: Send + Sync {
    async fn save_health_report(
        &self,
        report: &crate::health::HealthReport,
    ) -> Result<()> {
        let _ = report;
        Ok(())
    }
    async fn save_pending_decision(
        &self,
        d: &crate::health::PendingDecision,
    ) -> Result<()> {
        let _ = d;
        Ok(())
    }
    async fn list_pending_decisions(
        &self,
    ) -> Result<Vec<crate::health::PendingDecision>> {
        Ok(vec![])
    }
    async fn resolve_pending_decision(
        &self,
        id: i64,
        chosen: crate::health::RepairKind,
    ) -> Result<()> {
        let _ = (id, chosen);
        Ok(())
    }
}

#[async_trait]
pub trait DocumentSessionStore: Send + Sync {
    async fn create_document_session(&self, session: &DocumentSession) -> Result<()>;
    async fn get_document_session(&self, session_id: &str) -> Result<Option<DocumentSession>>;
    async fn get_document_session_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<DocumentSession>>;
    async fn update_document_session(&self, session: &DocumentSession) -> Result<()>;
}

// ─── Document Asset Store ────────────────────────────────────

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
    async fn save_raptor_nodes(
        &self,
        asset_id: &str,
        nodes: &[RaptorNode],
    ) -> Result<()>;

    /// All RAPTOR nodes for an asset, ordered by level ascending
    /// (leaves first).
    async fn list_raptor_nodes(&self, asset_id: &str) -> Result<Vec<RaptorNode>>;

    /// Fetch a single node by its node_id. Used by the granularity-
    /// aware retrieval tool when the model drills from a parent to
    /// its evidence chunks.
    async fn get_raptor_node(&self, node_id: &str) -> Result<Option<RaptorNode>>;

    /// Persist the motif index for an asset. Replaces any existing
    /// motifs for the same asset.
    async fn save_asset_motifs(
        &self,
        asset_id: &str,
        motifs: &[AssetMotif],
    ) -> Result<()>;

    /// Motif index for an asset, distinctive motifs first.
    async fn list_asset_motifs(&self, asset_id: &str) -> Result<Vec<AssetMotif>>;
}

// ─── 6. Storage (supertrait) ──────────────────────────────────

#[async_trait]
pub trait StateStore:
    ConversationStore + TaskStore + MemoryStore + RoutingStore
    + DocumentStore + CorpusStateStore + BudgetStore + PermissionStore
    + HealthStore + DocumentSessionStore + DocumentAssetStore
{}

// ─── Approval Channel ─────────────────────────────────────────

#[async_trait]
pub trait ApprovalChannel: Send + Sync {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool>;
    async fn ask_user(&self, question: &str) -> Result<String>;
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
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    async fn is_connected(&self) -> bool;
    async fn push(&self, node: &InsightNode) -> Result<()>;
    async fn push_batch(&self, nodes: &[InsightNode]) -> Result<()>;
}
