// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use sovereign_core::registry::ToolRegistry;
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::StateStore;

use crate::approval::ServerApprovalChannel;
use crate::auth::TenantId;
use crate::busy::busy_response_hint;
use crate::reciprocity::{user_key, ReciprocityTable};
use crate::scheduler::FairScheduler;
use crate::tenant::TenantRuntime;
use sovereign_contracts::types::projection::{
    project_epistemic_state, project_message_metadata, Citation, Provenance,
};

// ─── Request/Response Types ───────────────────────────────────

#[derive(serde::Deserialize)]
pub struct SendMessageRequest {
    pub content: String,
}

/// Optional body for `POST /v1/conversations`. `skill_id =
/// "recipe-author"` tags the conversation so subsequent messages route
/// into the recipe-author agent loop. Body is optional — an empty POST
/// keeps the pre-existing "untagged conversation" behaviour.
#[derive(serde::Deserialize, Default)]
pub struct CreateConversationRequest {
    #[serde(default)]
    pub skill_id: Option<String>,
}

#[derive(serde::Serialize)]
pub struct CreateConversationResponse {
    pub id: String,
    pub created_at: i64,
}

#[derive(serde::Serialize)]
pub struct MessageResponse {
    pub message_id: String,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskSummary>,
    /// Host-side provenance (model + serving node, routing tier,
    /// latency). `None` on turns whose handler doesn't persist
    /// provenance. Projected from `Message.metadata` — see
    /// `sovereign_contracts::types::projection`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Corpus-grounded citations carrying the host's
    /// `(corpus_id, chunk_id)` handle. Empty when the answer wasn't
    /// grounded in an installed corpus.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// The typed epistemic ledger (EPISTEMIC_STATE.md), when the turn
    /// stamped one. `None` on old messages / kill switch off. I2-C
    /// closes the wire gap; mobile rendering stays deferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epistemic_state: Option<sovereign_core::types::EpistemicState>,
}

#[derive(serde::Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub status: String,
    pub steps_completed: usize,
}

#[derive(serde::Serialize)]
pub struct ConversationResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub messages: Vec<MessageEntry>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(serde::Serialize)]
pub struct MessageEntry {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// The typed epistemic ledger (EPISTEMIC_STATE.md); see
    /// [`MessageResponse::epistemic_state`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epistemic_state: Option<sovereign_core::types::EpistemicState>,
}

#[derive(serde::Serialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationListEntry>,
}

#[derive(serde::Serialize)]
pub struct ConversationListEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(serde::Deserialize)]
pub struct ListQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(serde::Deserialize)]
pub struct ApproveRequest {
    pub step_id: usize,
    pub approved: bool,
}

#[derive(serde::Serialize)]
pub struct ApproveResponse {
    pub task_id: String,
    pub accepted: bool,
}

#[derive(serde::Deserialize)]
pub struct SearchRequest {
    pub query: String,
}

#[derive(serde::Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultEntry>,
}

#[derive(serde::Serialize)]
pub struct SearchResultEntry {
    pub r#type: String,
    pub content: String,
    pub conversation_id: String,
}

#[derive(serde::Serialize)]
pub struct ToolListResponse {
    pub tools: Vec<ToolEntry>,
}

#[derive(serde::Serialize)]
pub struct ToolEntry {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(serde::Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;

fn api_error(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
}

fn tenant_runtime(
    runtime: &Arc<Runtime>,
    store: &Arc<dyn StateStore>,
    tenant: &TenantId,
) -> TenantRuntime {
    TenantRuntime::new(Arc::clone(runtime), Arc::clone(store), tenant.0.clone())
}

// ─── Handlers ─────────────────────────────────────────────────

/// POST /v1/conversations
pub async fn create_conversation(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    body: axum::body::Bytes,
) -> ApiResult<CreateConversationResponse> {
    let tr = tenant_runtime(&runtime, &store, &tenant);
    // Body is optional + best-effort: an empty or malformed POST yields
    // an untagged conversation (the prior behaviour) rather than a 4xx.
    let req: CreateConversationRequest = if body.is_empty() {
        CreateConversationRequest::default()
    } else {
        serde_json::from_slice(&body).unwrap_or_default()
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Seed the row now so the skill tag is set before the first message
    // (mirrors the desktop "new chat" create flow). Without this,
    // `resolve_active_mode` can't route the conversation into a
    // workspace agent loop.
    if let Err(e) = tr
        .seed_conversation(&id, now, req.skill_id.as_deref())
        .await
    {
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("seed conversation: {e}"),
        ));
    }

    Ok(Json(CreateConversationResponse {
        id,
        created_at: now,
    }))
}

/// POST /v1/conversations/:id/messages
///
/// Returns an `axum::response::Response` (not `ApiResult`) so the busy
/// path can attach a `Retry-After` header to its `503` — the host-busy
/// acceptance criterion.
pub async fn send_message(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    Extension(approval): Extension<Arc<ServerApprovalChannel>>,
    Extension(sched): Extension<FairScheduler>,
    Extension(reciprocity): Extension<Arc<ReciprocityTable>>,
    headers: HeaderMap,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Response {
    // Fair scheduler — REST is one-shot: grant if a slot is free, else shed
    // immediately with a queue-position hint (no long-poll). The permit is
    // held for the turn and dropped when this fn returns.
    let key = user_key(&tenant, &headers);
    let weight = reciprocity.weight_for(&key);
    let _permit = match sched.try_grant(key, weight) {
        Ok(permit) => permit,
        Err(shed) => {
            tracing::warn!(
                conversation_id = %conversation_id,
                available = sched.available(),
                would_be_position = shed.would_be_position,
                "host_busy: shedding send_message"
            );
            return busy_response_hint(shed.retry_after_secs, shed.would_be_position);
        }
    };

    let tr = tenant_runtime(&runtime, &store, &tenant);

    // Set task_id context for approval channel (will be updated by executor if needed).
    approval.set_task_id(&conversation_id).await;

    match tr.handle_message_any(&body.content, &conversation_id).await {
        Ok(response) => {
            let task_summary = response.task.map(|t| TaskSummary {
                id: t.id,
                status: format!("{:?}", t.status),
                steps_completed: t.completed_steps.len(),
            });

            let role = response.message.role_str().to_string();
            let (provenance, citations) = project_message_metadata(&response.message.metadata);
            let epistemic_state = project_epistemic_state(&response.message.metadata);
            Json(MessageResponse {
                message_id: response.message.id,
                role,
                content: response.message.content,
                task: task_summary,
                provenance,
                citations,
                epistemic_state,
            })
            .into_response()
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()).into_response(),
    }
}

/// GET /v1/conversations/:id
pub async fn get_conversation(
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    Path(conversation_id): Path<String>,
) -> ApiResult<ConversationResponse> {
    let scoped_id = format!("{}:{conversation_id}", tenant.0);

    match store.get_conversation(&scoped_id).await {
        Ok(convo) => Ok(Json(ConversationResponse {
            id: conversation_id,
            title: convo.title,
            messages: convo
                .messages
                .into_iter()
                .map(|m| {
                    let role = m.role_str().to_string();
                    let (provenance, citations) = project_message_metadata(&m.metadata);
                    let epistemic_state = project_epistemic_state(&m.metadata);
                    MessageEntry {
                        id: m.id,
                        role,
                        content: m.content,
                        created_at: m.created_at,
                        provenance,
                        citations,
                        epistemic_state,
                    }
                })
                .collect(),
            created_at: convo.created_at,
            updated_at: convo.updated_at,
        })),
        Err(sovereign_core::Error::NotFound(_)) => {
            Err(api_error(StatusCode::NOT_FOUND, "Conversation not found"))
        }
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

/// GET /v1/conversations
pub async fn list_conversations(
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    Query(params): Query<ListQuery>,
) -> ApiResult<ConversationListResponse> {
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);

    // Conversations are stored tenant-scoped as `tenant:id`. Two things must
    // happen here that previously didn't: (1) filter to THIS tenant so we
    // don't leak other tenants' conversations, and (2) strip the `tenant:`
    // prefix so the client sees the bare id it created. Returning the scoped
    // id made the client re-scope it on open — `GET /v1/conversations/
    // {tenant:id}` becomes `tenant:tenant:id`, which matches nothing, so
    // every existing conversation opened empty.
    let prefix = format!("{}:", tenant.0);
    match store.list_conversations(limit, offset).await {
        Ok(convos) => Ok(Json(ConversationListResponse {
            conversations: convos
                .into_iter()
                .filter(|c| c.id.starts_with(&prefix))
                .map(|c| ConversationListEntry {
                    id: c.id.strip_prefix(&prefix).unwrap_or(&c.id).to_string(),
                    title: c.title,
                    created_at: c.created_at,
                    updated_at: c.updated_at,
                })
                .collect(),
        })),
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

/// DELETE /v1/conversations/:id
pub async fn delete_conversation(
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    Path(conversation_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let scoped_id = format!("{}:{conversation_id}", tenant.0);

    match store.delete_conversation(&scoped_id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

/// POST /v1/tasks/:id/approve
pub async fn approve_task(
    Extension(approval): Extension<Arc<ServerApprovalChannel>>,
    Path(task_id): Path<String>,
    Json(body): Json<ApproveRequest>,
) -> ApiResult<ApproveResponse> {
    let key = format!("{task_id}:{}", body.step_id);
    let accepted = approval.submit_approval(&key, body.approved).await;

    Ok(Json(ApproveResponse { task_id, accepted }))
}

/// GET /v1/tools
pub async fn list_tools(
    Extension(tools): Extension<Arc<ToolRegistry>>,
) -> ApiResult<ToolListResponse> {
    let descriptors = tools.descriptors();

    Ok(Json(ToolListResponse {
        tools: descriptors
            .into_iter()
            .map(|d| ToolEntry {
                id: d.id,
                name: d.name,
                description: d.description,
            })
            .collect(),
    }))
}

/// POST /v1/search
pub async fn search(
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    Json(body): Json<SearchRequest>,
) -> ApiResult<SearchResponse> {
    // Scope the cross-conversation search to THIS tenant: every row a tenant
    // owns is stored under a `"{tenant}:"`-prefixed conversation id (see
    // `TenantRuntime::scoped_id`). Without this filter the search runs over
    // every tenant's messages — the cross-tenant leak that
    // `http_tests::search_does_not_leak_across_tenants` guards against.
    let prefix = format!("{}:", tenant.0);
    match store.search_messages(&body.query).await {
        Ok(messages) => Ok(Json(SearchResponse {
            results: messages
                .into_iter()
                .filter(|m| m.conversation_id.starts_with(&prefix))
                .take(50)
                .map(|m| SearchResultEntry {
                    r#type: "message".to_string(),
                    content: m.content,
                    conversation_id: m.conversation_id,
                })
                .collect(),
        })),
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

// ─── Corpora (CORPUS_REF) ─────────────────────────────────────

#[derive(serde::Serialize)]
pub struct CorpusListResponse {
    pub corpora: Vec<CorpusRefEntry>,
}

/// One installed knowledge corpus — the spec's `CORPUS_REF`. Metadata
/// only; the corpus chunks/vectors never leave the host.
#[derive(serde::Serialize)]
pub struct CorpusRefEntry {
    pub corpus_id: String,
    pub display_name: String,
    /// `[display] category` (e.g. `"conversation"`, `"reference"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `[display] icon` hint; the client maps known values to its glyphs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub chunk_count: u64,
    /// Privacy posture (`MOBILE.md`): `"local"` (never leaves this host)
    /// vs `"mesh"` (eligible for shard distribution / knowledge
    /// fan-out). Derived from `IndexInfo.mesh_sharing`. The phone badges
    /// `local` sources as private-to-this-host (acceptance §7); the
    /// per-identity conversation corpus is always `local`.
    pub scope: String,
    /// `false` = never sharded or gossiped to peers. Mirrors
    /// `IndexInfo.mesh_sharing`; pairs with `scope`.
    pub mesh_shared: bool,
}

/// GET /v1/corpora
///
/// Surfaces the host's installed **knowledge** corpora as `CORPUS_REF`
/// records so the thin client can render the corpus list and resolve
/// `(corpus_id, chunk_id)` citations against it. Code-intelligence
/// corpora (`CorpusKind::Code`) are filtered out — they aren't chat
/// knowledge sources. Returns an empty list (not an error) when no
/// corpus engine is wired or none are installed.
pub async fn list_corpora(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
) -> ApiResult<CorpusListResponse> {
    let Some(engine) = runtime.corpus_engine.as_ref() else {
        return Ok(Json(CorpusListResponse {
            corpora: Vec::new(),
        }));
    };

    // Hide corpora this tenant may not see (another principal's Private
    // uploads). Fail closed: a store error rejects the request rather than
    // listing everything.
    let forbidden = tenant_runtime(&runtime, &store, &tenant)
        .forbidden_corpora()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    match engine.installed_indexes().await {
        Ok(indexes) => {
            let corpora = indexes
                .into_iter()
                .filter(|i| matches!(i.kind, corpus_engine::CorpusKind::Knowledge))
                .filter(|i| !forbidden.contains(&i.corpus_id))
                .map(|i| {
                    let (category, icon) = i
                        .display
                        .as_ref()
                        .map(|d| (d.category.clone(), d.icon.clone()))
                        .unwrap_or((None, None));
                    let mesh_shared = i.mesh_sharing;
                    let scope = if mesh_shared { "mesh" } else { "local" }.to_string();
                    CorpusRefEntry {
                        corpus_id: i.corpus_id,
                        display_name: i.corpus_name,
                        category,
                        icon,
                        chunk_count: i.chunk_count,
                        scope,
                        mesh_shared,
                    }
                })
                .collect();
            Ok(Json(CorpusListResponse { corpora }))
        }
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}

// ─── Reading view — fetch a cited passage + surrounding context ──
//
// The thin mobile client holds only the truncated citation snippet. This
// serves the real passage from the host's corpus engine — the cited
// chunk plus a window of neighbouring chunks — mirroring the desktop's
// `read_get_chunk_neighbors`, so the phone can render a proper reader.

#[derive(serde::Deserialize)]
pub struct ReadingQuery {
    pub radius: Option<usize>,
}

#[derive(serde::Serialize)]
pub struct ReadChunkEntry {
    pub chunk_id: u64,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ReadingWindowResponse {
    pub corpus_id: String,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<ReadChunkEntry>,
    pub prev: Vec<ReadChunkEntry>,
    pub next: Vec<ReadChunkEntry>,
}

/// GET /v1/corpora/:corpus_id/chunks/:chunk_id?radius=1
pub async fn read_chunk(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    Path((corpus_id, chunk_id)): Path<(String, u64)>,
    Query(params): Query<ReadingQuery>,
) -> ApiResult<ReadingWindowResponse> {
    let radius = params.radius.unwrap_or(1).min(5);
    // A tenant may only read chunks from corpora it can see. Treat a
    // forbidden (another principal's Private) corpus as not-found — don't
    // reveal that it exists. Fail closed on a store error.
    let forbidden = tenant_runtime(&runtime, &store, &tenant)
        .forbidden_corpora()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    if forbidden.contains(&corpus_id) {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            &format!("corpus '{corpus_id}' not found"),
        ));
    }
    let Some(engine) = runtime.corpus_engine.as_ref() else {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "corpus engine not available",
        ));
    };
    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| {
            api_error(
                StatusCode::NOT_FOUND,
                &format!("open index '{corpus_id}': {e}"),
            )
        })?;
    let window = index
        .neighbors(chunk_id, radius)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let entry = |r: corpus_engine::EnrichmentChunkRow| ReadChunkEntry {
        chunk_id: r.id,
        content: r.content,
        title: r.title,
        url: r.url,
    };
    match window {
        None => Ok(Json(ReadingWindowResponse {
            corpus_id,
            found: false,
            center: None,
            prev: vec![],
            next: vec![],
        })),
        Some(w) => Ok(Json(ReadingWindowResponse {
            corpus_id,
            found: true,
            center: Some(entry(w.center)),
            prev: w.prev.into_iter().map(entry).collect(),
            next: w.next.into_iter().map(entry).collect(),
        })),
    }
}
