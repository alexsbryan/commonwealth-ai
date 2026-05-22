//! `knowledge_lookup` — Tool-Mastery Framework Phase 5.
//!
//! A unified evidence front-door that collapses corpus search,
//! memory recall, and note FTS behind a single tool surface. The
//! load-bearing assumption: one tool with a unified evidence
//! envelope is easier for a 27B model than three tools with three
//! shapes. Each returned `Evidence` row carries a stable
//! `EvidenceId` (e.g. `ev-0001`) so the model can cite back into
//! the call's result without paraphrasing or fabricating handles.
//!
//! The tool fans out to all three channels in parallel and merges
//! the results by `confidence` (best-effort: corpus scores are
//! normalised at the SQL/Lance layer; memory/note "confidence" is
//! a synthetic value derived from recency + cosine similarity).
//! Empty channels stay empty — the model sees `evidence: []` and
//! the absence is the honest signal.
//!
//! Asset files live alongside this module per ARCH §6.2 — `data,
//! not program`. Edit the markdown when you want to change what
//! the model reads.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use corpus_engine::NoteStore;

use sovereign_core::error::{Error, Result};
use sovereign_core::memory;
use sovereign_core::traits::{InferenceProvider, MemoryScope, StateStore, Tool};
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, RetryConfig, Scope, StepOutput, ToolContext,
    ToolDescriptor,
};

// ─── Public types ──────────────────────────────────────────────

/// Stable per-call evidence handle (e.g. `ev-0001`). The id space
/// is the position of the row in the returned `evidence` array,
/// zero-padded so lexicographic ordering matches array ordering
/// for the first 9,999 rows. Stable means the model can cite an id
/// it saw mid-stream after subsequent tokens; ephemeral means a
/// later `knowledge_lookup` call gets a fresh `ev-0000` numbering
/// — the rows are different evidence, the handles can't conflate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceId(pub String);

impl EvidenceId {
    /// Build a deterministic id from a 0-based row index.
    /// Back-compat wrapper around [`Self::from_index_with_turn`]
    /// for fixtures and tests that don't carry turn context —
    /// renders as `ev-T0-NNNN` so the cross-turn renderer doesn't
    /// have to special-case the turnless shape.
    pub fn from_index(idx: usize) -> Self {
        Self::from_index_with_turn(idx, 0)
    }

    /// Build a turn-prefixed id from `(idx, turn)` (Tier 1
    /// result-memory). Renders as `ev-Tn-NNNN`. The dossier's
    /// outcome-history renderer surfaces these so the model can
    /// reference `[ev-T2-0001]` in turn N+1 and the runtime can
    /// dereference without re-calling the tool. `turn` is the
    /// 0-based count of prior user turns in the conversation.
    pub fn from_index_with_turn(idx: usize, turn: usize) -> Self {
        Self(format!("ev-T{turn}-{idx:04}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which evidence channel this row came from. Closed set per ARCH
/// §2.1 — adding a fifth channel (e.g. `Catalog` for unindexed
/// recipe metadata) is one variant + one render arm, not a new
/// string convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    /// A chunk from an installed knowledge corpus (wiki, SEP,
    /// research collection).
    Corpus,
    /// A long-term memory the user has shared with the assistant
    /// across past conversations.
    Memory,
    /// A working note (decision, invariant, todo) written via the
    /// `note` tool or harvested from commits.
    Note,
    /// A web search result (Tier 3 of tool-framework expansion).
    /// Only present when the tool was constructed with
    /// `with_auto_escalate(true)` AND a `SearchOrchestrator` AND
    /// the local channels returned thin/empty results. The
    /// `source_id` for these rows is the URL; the
    /// `retrieval_context` notes the backend that served the
    /// result.
    Web,
}

/// One row of evidence returned to the model. `source_id` is the
/// underlying back-end identifier (`corpus_id::chunk_id`,
/// `memory_id`, `note_id`) — useful for downstream attribution
/// audits but NOT the handle the model should cite (that's `id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub source_kind: EvidenceKind,
    pub source_id: String,
    pub title: String,
    pub content: String,
    pub confidence: f32,
    /// Free-text breadcrumb explaining WHY this row matched
    /// (e.g. `"matched on 'M5 Mac Studio' (cosine 0.81)"`).
    /// The model can echo it to the user when summarising
    /// evidence quality.
    pub retrieval_context: String,
}

/// The envelope returned by the tool. Carries the original query
/// + the merged evidence list + a per-channel hit count so the
/// model can tell at a glance whether one channel was thin (and
/// therefore whether falling back to gap-check is appropriate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeLookupResponse {
    pub query: String,
    pub evidence: Vec<Evidence>,
    pub by_kind_counts: KindCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KindCounts {
    pub corpus: usize,
    pub memory: usize,
    pub note: usize,
    /// Tier 3 web-escalation row count. Stays 0 when the tool is
    /// not configured for auto-escalation OR when local channels
    /// returned satisfactory results.
    #[serde(default)]
    pub web: usize,
}

// ─── Asset descriptors ─────────────────────────────────────────

pub const TOOL_DESCRIPTION: &str = include_str!("assets/tool_description.md");
pub const SYSTEM_PROMPT: &str = include_str!("assets/system_prompt.md");

// ─── Tool impl ─────────────────────────────────────────────────

const CORPUS_LIMIT_DEFAULT: usize = 8;
const MEMORY_LIMIT_DEFAULT: usize = 4;
const NOTE_LIMIT_DEFAULT: usize = 4;
/// Hard cap on the returned envelope size. Big-K calls bloat the
/// model's context and rarely improve answer quality — the model
/// uses the top 3-5 rows in practice.
const MAX_EVIDENCE_RETURNED: usize = 12;

pub struct KnowledgeLookupTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    notes: Option<Arc<NoteStore>>,
    /// Tier 3: optional web-search orchestrator. When `Some` AND
    /// `auto_escalate_to_web` is true AND the local channels return
    /// thin/empty results, the tool internally calls web search and
    /// merges the results in as `EvidenceKind::Web` rows.
    web: Option<Arc<crate::web::search::orchestrator::SearchOrchestrator>>,
    auto_escalate_to_web: bool,
}

impl KnowledgeLookupTool {
    /// Construct a corpus+memory-only knowledge_lookup (no notes
    /// channel). Useful for the test/CLI paths where a `NoteStore`
    /// isn't wired.
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self {
            store,
            inference,
            notes: None,
            web: None,
            auto_escalate_to_web: false,
        }
    }

    /// Wire the web-escalation channel (Tier 3). Pass an
    /// already-constructed `SearchOrchestrator` (typically the same
    /// instance the desktop's `submit_information_search` uses).
    /// Escalation only fires when `with_auto_escalate(true)` is
    /// ALSO set — having an orchestrator wired but the setting
    /// off keeps the user-in-loop card path as the only web
    /// surface.
    pub fn with_web_orchestrator(
        mut self,
        orchestrator: Arc<crate::web::search::orchestrator::SearchOrchestrator>,
    ) -> Self {
        self.web = Some(orchestrator);
        self
    }

    /// Toggle the operator setting (Tier 3). True ⇒ thin local
    /// results trigger an internal web search. False (default) ⇒
    /// the tool returns whatever local channels produced; the user
    /// can still escalate to web manually via the INFORMATION
    /// REQUEST card.
    pub fn with_auto_escalate(mut self, enabled: bool) -> Self {
        self.auto_escalate_to_web = enabled;
        self
    }

    /// Add a `NoteStore` so the third evidence channel (notes) is
    /// queried. Without this, the tool returns corpus + memory
    /// evidence only.
    pub fn with_notes(mut self, notes: Arc<NoteStore>) -> Self {
        self.notes = Some(notes);
        self
    }

    async fn corpus_evidence(&self, query: &str, limit: usize) -> Vec<Evidence> {
        let embedding = self.inference.embed_query(query).await.ok();
        let emb_slice = embedding.as_deref().unwrap_or(&[]);
        let chunks = self
            .store
            .search_documents_scored(emb_slice, query, limit)
            .await
            .unwrap_or_default();
        chunks
            .into_iter()
            .map(|sc| {
                let corpus = match &sc.chunk.source_type {
                    sovereign_core::types::SourceType::Corpus { corpus_id } => corpus_id.clone(),
                    sovereign_core::types::SourceType::WebSearch { .. } => "web".to_string(),
                    sovereign_core::types::SourceType::UserDocument => "user_document".to_string(),
                };
                let source_id = format!("{corpus}::{}", sc.chunk.id);
                Evidence {
                    // id assigned after merge so ordering is stable
                    id: EvidenceId::from_index(0),
                    source_kind: EvidenceKind::Corpus,
                    source_id,
                    title: sc.chunk.source.clone(),
                    content: truncate(&sc.chunk.content, 1200),
                    confidence: sc.score.clamp(0.0, 1.0),
                    retrieval_context: format!("corpus chunk (score {:.2})", sc.score),
                }
            })
            .collect()
    }

    async fn memory_evidence(&self, query: &str, limit: usize) -> Vec<Evidence> {
        let recalled = match memory::recall_relevant_memories_embed(
            self.inference.as_ref(),
            self.store.as_ref(),
            &MemoryScope::General,
            query,
            limit,
        )
        .await
        {
            Ok(mems) => mems,
            Err(_) => return Vec::new(),
        };
        recalled
            .into_iter()
            .map(|m| {
                // Memory recall doesn't return per-row similarity
                // scores, so use the stored confidence as a stand-in.
                let conf = m.confidence as f32;
                Evidence {
                    id: EvidenceId::from_index(0),
                    source_kind: EvidenceKind::Memory,
                    source_id: m.id.clone(),
                    title: memory_title(&m.content),
                    content: truncate(&m.content, 800),
                    confidence: conf,
                    retrieval_context: format!("user memory (confidence {conf:.2})"),
                }
            })
            .collect()
    }

    async fn note_evidence(&self, query: &str, limit: usize) -> Vec<Evidence> {
        let Some(notes) = &self.notes else {
            return Vec::new();
        };
        let rows = notes
            .read_notes(Some(query), &[], &[], &[], limit, false)
            .await
            .unwrap_or_default();
        rows.into_iter()
            .map(|row| {
                let title = format!("{} note", row.kind);
                Evidence {
                    id: EvidenceId::from_index(0),
                    source_kind: EvidenceKind::Note,
                    source_id: row.id.clone(),
                    title,
                    content: truncate(&row.content, 800),
                    // FTS results don't carry bm25 scores out at
                    // this layer; default to a moderate
                    // confidence so notes don't drown out
                    // higher-confidence corpus hits.
                    confidence: 0.55,
                    retrieval_context: format!("note (kind={})", row.kind),
                }
            })
            .collect()
    }

    /// Tier 3 web-escalation helper. Called from `execute()` when
    /// the operator setting is on AND local channels returned
    /// thin/empty results. Failures fall through to an empty Vec
    /// — the model sees the local channels and an empty web row
    /// set; that's a degraded but valid response (Tier 1 prompt
    /// discipline still keeps the model honest about the absence).
    async fn web_search_evidence(
        &self,
        orchestrator: &crate::web::search::orchestrator::SearchOrchestrator,
        query: &str,
    ) -> Vec<Evidence> {
        use crate::web::search::backend_trait::SearchPrivacy;
        use crate::web::search::orchestrator::{BudgetView, SelectInputs};
        let client = reqwest::Client::new();
        let budget = BudgetView::default();
        let prefer: Vec<&str> = Vec::new();
        let inputs = SelectInputs {
            query,
            max_results: 5,
            // External: the operator opted in, so escalation may
            // reach public-API backends (DDG, Brave, Tavily). The
            // orchestrator's per-backend filter still applies; if
            // only Local backends are configured this falls back
            // to an empty result set.
            max_privacy: SearchPrivacy::External { provider: "any" },
            budget: &budget,
            prefer: &prefer,
        };
        let result = orchestrator.search(&client, inputs).await;
        let backend_id = result.backend_id;
        result
            .results
            .into_iter()
            .map(|sr| Evidence {
                // id reassigned post-merge — placeholder here.
                id: EvidenceId::from_index(0),
                source_kind: EvidenceKind::Web,
                source_id: sr.url.clone(),
                title: sr.title,
                content: {
                    let mut c = sr.snippet;
                    c.truncate(800);
                    c
                },
                // Synthetic confidence below the corpus floor so
                // genuine local matches still rank above web hits
                // when both are present.
                confidence: 0.55,
                retrieval_context: format!(
                    "web search (operator-enabled escalation, backend={backend_id})"
                ),
            })
            .collect()
    }
}

#[async_trait]
impl Tool for KnowledgeLookupTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "knowledge_lookup".to_string(),
            name: "Knowledge lookup".to_string(),
            description: TOOL_DESCRIPTION.trim().to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "A short focused query (≤ 8 words is plenty)."
                    },
                    "kinds": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["corpus", "memory", "note"] },
                        "description": "Optional channel filter. Omit to fan out to all three."
                    }
                },
                "required": ["query"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "evidence": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Stable handle (e.g. ev-0001)" },
                                "source_kind": { "type": "string", "enum": ["corpus", "memory", "note"] },
                                "source_id": { "type": "string" },
                                "title": { "type": "string" },
                                "content": { "type": "string" },
                                "confidence": { "type": "number" },
                                "retrieval_context": { "type": "string" }
                            }
                        }
                    },
                    "by_kind_counts": {
                        "type": "object",
                        "properties": {
                            "corpus": { "type": "integer" },
                            "memory": { "type": "integer" },
                            "note": { "type": "integer" }
                        }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn retry_config(&self) -> Option<RetryConfig> {
        None
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let query = params.get("query").and_then(|v| v.as_str());
        let Some(query) = query else {
            return Err(Error::InvalidInput(
                "knowledge_lookup requires a 'query' string parameter".to_string(),
            ));
        };
        if query.trim().is_empty() {
            return Err(Error::InvalidInput(
                "knowledge_lookup 'query' cannot be empty".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'query' parameter".to_string()))?
            .trim()
            .to_string();

        let requested_kinds: Option<Vec<String>> = params
            .get("kinds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            });
        let want_corpus = requested_kinds
            .as_ref()
            .map_or(true, |ks| ks.iter().any(|k| k == "corpus"));
        let want_memory = requested_kinds
            .as_ref()
            .map_or(true, |ks| ks.iter().any(|k| k == "memory"));
        let want_note = requested_kinds
            .as_ref()
            .map_or(true, |ks| ks.iter().any(|k| k == "note"));

        // Fan-out in parallel — three reads against independent
        // SQLite tables / Lance indexes, no cross-channel data
        // dependency. Failures on any single channel return Vec::new()
        // so a degraded channel doesn't tank the whole call.
        let (corpus, memories, note_rows) = tokio::join!(
            async {
                if want_corpus {
                    self.corpus_evidence(&query, CORPUS_LIMIT_DEFAULT).await
                } else {
                    Vec::new()
                }
            },
            async {
                if want_memory {
                    self.memory_evidence(&query, MEMORY_LIMIT_DEFAULT).await
                } else {
                    Vec::new()
                }
            },
            async {
                if want_note {
                    self.note_evidence(&query, NOTE_LIMIT_DEFAULT).await
                } else {
                    Vec::new()
                }
            }
        );

        // Capture channel counts before consuming the vecs into
        // the merge — needed for the `KindCounts` aggregate below
        // (and for the Tier 3 thinness gate, which inspects
        // confidence rather than count but uses counts for
        // logging when escalation fires).
        let corpus_count = corpus.len();
        let memory_count = memories.len();
        let note_count = note_rows.len();

        // Merge by confidence, descending. Stable sort so
        // within-channel ordering (which is already best-first
        // from the back-ends) is preserved on confidence ties.
        let mut merged: Vec<Evidence> = corpus
            .into_iter()
            .chain(memories)
            .chain(note_rows)
            .collect();
        merged.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Tier 3: web-escalation branch. Fires only when:
        //   - operator opted in (`auto_escalate_to_web = true`),
        //   - an orchestrator is wired (`web.is_some()`),
        //   - AND the local channels returned thin/empty results.
        // The thinness gate uses the same shape as the existing
        // EvidenceShape::is_off_target predicate (top-3 confidence
        // all below 0.4 or no rows at all). When the gate trips,
        // we call web search with the same query, format each
        // result as an `EvidenceKind::Web` row at a synthetic
        // confidence (0.55, below the corpus floor so genuine
        // corpus matches still rank above web), and re-sort.
        let mut web_count = 0usize;
        if self.auto_escalate_to_web {
            if let Some(orchestrator) = self.web.as_ref() {
                let local_thin = merged.is_empty()
                    || merged.iter().take(3).all(|e| e.confidence < 0.4);
                if local_thin {
                    let web_evidence =
                        self.web_search_evidence(orchestrator, &query).await;
                    web_count = web_evidence.len();
                    merged.extend(web_evidence);
                    merged.sort_by(|a, b| {
                        b.confidence
                            .partial_cmp(&a.confidence)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
            }
        }

        merged.truncate(MAX_EVIDENCE_RETURNED);

        let counts = KindCounts {
            corpus: corpus_count,
            memory: memory_count,
            note: note_count,
            web: web_count,
        };

        // Assign stable per-call ids in final post-merge order
        // (NOT pre-merge channel order). This is the id the model
        // sees and cites; ev-T<turn>-0001 is the top-ranked
        // evidence across all channels for THIS turn.
        // Tier 1: ids carry the turn index so cross-turn
        // citation handles disambiguate when two turns return
        // the same evidence array.
        let turn_index = ctx.turn_index;
        for (idx, ev) in merged.iter_mut().enumerate() {
            ev.id = EvidenceId::from_index_with_turn(idx, turn_index);
        }

        let response = KnowledgeLookupResponse {
            query,
            evidence: merged,
            by_kind_counts: counts,
        };

        let value = serde_json::to_value(&response)
            .map_err(|e| Error::Execution(format!("knowledge_lookup: serialize: {e}")))?;
        Ok(StepOutput::Json(value))
    }
}

// ─── Helpers ────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = s.char_indices().take(max).last().map(|(i, _)| i).unwrap_or(max);
    let mut out = s[..cut].to_string();
    out.push_str(" …");
    out
}

fn memory_title(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or(content);
    let trimmed: String = first_line.chars().take(80).collect();
    if trimmed.len() < first_line.len() {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;

    use futures::Stream;
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
    };

    /// Bare-minimum InferenceProvider for the unit tests. Embeds
    /// return an empty vector — `corpus_evidence` interprets that
    /// as "embedding unavailable; fall through to FTS-only", and
    /// memory recall similarly degrades to FTS. Other inference
    /// paths panic so a regression that adds an inference
    /// dependency to the tool's hot path surfaces immediately.
    struct MockInference;

    #[async_trait]
    impl InferenceProvider for MockInference {
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
            unreachable!("knowledge_lookup tool should not call complete()")
        }

        async fn complete_stream(
            &self,
            _: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unreachable!("knowledge_lookup tool should not stream")
        }

        async fn embed(&self, _: &str) -> Result<Vec<f32>> {
            Ok(Vec::new())
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 8192,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    fn mock_tool() -> KnowledgeLookupTool {
        let store: Arc<dyn StateStore> =
            Arc::new(sovereign_store::memory::InMemoryStateStore::new());
        let inference: Arc<dyn InferenceProvider> = Arc::new(MockInference);
        KnowledgeLookupTool::new(store, inference)
    }

    #[test]
    fn evidence_id_zero_pad() {
        // Post-Tier-1: ids carry a turn prefix. `from_index(n)` is
        // a thin wrapper that defaults to turn 0 — the rendered
        // shape is `ev-T0-NNNN` even on the back-compat path.
        assert_eq!(EvidenceId::from_index(0).as_str(), "ev-T0-0000");
        assert_eq!(EvidenceId::from_index(7).as_str(), "ev-T0-0007");
        assert_eq!(EvidenceId::from_index(123).as_str(), "ev-T0-0123");
        // Turn-aware constructor uses an explicit turn index.
        assert_eq!(EvidenceId::from_index_with_turn(1, 2).as_str(), "ev-T2-0001");
        assert_eq!(EvidenceId::from_index_with_turn(0, 22).as_str(), "ev-T22-0000");
    }

    #[test]
    fn descriptor_uses_short_id_and_loads_asset() {
        let tool = mock_tool();
        let desc = tool.descriptor();
        assert_eq!(desc.id, "knowledge_lookup");
        assert!(desc.description.len() > 50, "tool description must come from asset");
        // The descriptor should warn against fabrication — load-
        // bearing for the "no ev-xxx fabrication" structural
        // predicate in the knowledge-gym. The vocabulary may
        // shift between asset revisions; we accept either the
        // "NEVER cite" / "fabricat" wording or the
        // "did not come back" / "invent" shape from the
        // small-first rewrite (Tier 1b).
        let d = &desc.description;
        assert!(
            d.contains("fabricat")
                || d.contains("NEVER cite")
                || d.contains("did not come back")
                || d.contains("invent"),
            "descriptor must include no-fabrication guidance"
        );
    }

    #[test]
    fn validate_rejects_empty_query() {
        let tool = mock_tool();
        let err = tool
            .validate(&serde_json::json!({ "query": "" }))
            .err()
            .expect("should reject empty query");
        let msg = format!("{err}");
        assert!(msg.contains("empty"));
    }

    #[test]
    fn validate_rejects_missing_query() {
        let tool = mock_tool();
        let err = tool
            .validate(&serde_json::json!({ }))
            .err()
            .expect("should reject missing query");
        let msg = format!("{err}");
        assert!(msg.contains("requires"));
    }

    #[tokio::test]
    async fn execute_returns_empty_envelope_on_empty_state() {
        // No corpus, no memories, no notes — must still return a
        // shaped envelope (so the model sees `evidence: []` and
        // can render the honest "I don't know" rather than
        // confabulating).
        let tool = mock_tool();
        let out = tool
            .execute(
                &serde_json::json!({ "query": "what is recursion" }),
                &sovereign_core::types::ToolContext {
                    conversation_id: "test".into(),
                    task_id: None,
                    working_directory: None,
                    in_reasoning_loop: false,
                    agent_session_token: None,
                    turn_index: 0,
                },
            )
            .await
            .unwrap();
        let StepOutput::Json(value) = out else {
            panic!("expected JSON output, got {out:?}");
        };
        let parsed: KnowledgeLookupResponse = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.query, "what is recursion");
        assert_eq!(parsed.evidence.len(), 0);
        assert_eq!(parsed.by_kind_counts.corpus, 0);
        assert_eq!(parsed.by_kind_counts.memory, 0);
        assert_eq!(parsed.by_kind_counts.note, 0);
    }
}
