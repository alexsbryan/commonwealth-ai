use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::Result;
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

    /// Return the model ID that will be selected for a request at the given
    /// speed tier, without running inference. Used to populate provenance on
    /// streaming responses (where `complete_stream` returns no metadata).
    /// Default returns `"unknown"` — override in providers that know their
    /// loaded model names without blocking on a lock.
    fn model_id_for(&self, speed: Speed) -> String {
        let _ = speed;
        "unknown".to_string()
    }

    fn capabilities(&self) -> ProviderCapabilities;
}

// ─── 2. Routing ────────────────────────────────────────────────

#[async_trait]
pub trait Router: Send + Sync {
    async fn classify(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<RoutingOutcome>;
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
}

#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn save_task(&self, task: &Task) -> Result<()>;
    async fn get_task(&self, id: &str) -> Result<Task>;
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn save_memory(&self, memory: &Memory) -> Result<()>;
    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>>;
    async fn get_all_memories(&self) -> Result<Vec<Memory>>;
    async fn delete_memory(&self, id: &str) -> Result<()>;
    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()>;
    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()>;
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
