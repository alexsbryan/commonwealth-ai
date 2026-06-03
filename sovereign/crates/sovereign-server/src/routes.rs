use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use sovereign_core::runtime::Runtime;

use crate::approval::ServerApprovalChannel;
use crate::auth::TenantId;
use crate::busy::{busy_response, BusyGuard};
use crate::projection::{project_message_metadata, Citation, Provenance};
use crate::tenant::TenantRuntime;

// ─── Request/Response Types ───────────────────────────────────

#[derive(serde::Deserialize)]
pub struct SendMessageRequest {
    pub content: String,
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
    /// `crate::projection`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Corpus-grounded citations carrying the host's
    /// `(corpus_id, chunk_id)` handle. Empty when the answer wasn't
    /// grounded in an installed corpus.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
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

fn tenant_runtime(runtime: &Arc<Runtime>, tenant: &TenantId) -> TenantRuntime {
    TenantRuntime::new(Arc::clone(runtime), tenant.0.clone())
}

// ─── Handlers ─────────────────────────────────────────────────

/// POST /v1/conversations
pub async fn create_conversation(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
) -> ApiResult<CreateConversationResponse> {
    let _ = tenant_runtime(&runtime, &tenant);
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

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
    Extension(tenant): Extension<TenantId>,
    Extension(approval): Extension<Arc<ServerApprovalChannel>>,
    Extension(busy): Extension<BusyGuard>,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Response {
    // Busy guard — held for the turn, dropped when this fn returns.
    let Some(_permit) = busy.try_enter() else {
        tracing::warn!(
            conversation_id = %conversation_id,
            available = busy.available(),
            "host_busy: rejecting send_message"
        );
        return busy_response(busy.retry_after_secs());
    };

    let tr = tenant_runtime(&runtime, &tenant);

    // Set task_id context for approval channel (will be updated by executor if needed).
    approval.set_task_id(&conversation_id).await;

    match tr.handle_message(&body.content, &conversation_id).await {
        Ok(response) => {
            let task_summary = response.task.map(|t| TaskSummary {
                id: t.id,
                status: format!("{:?}", t.status),
                steps_completed: t.completed_steps.len(),
            });

            let role = response.message.role_str().to_string();
            let (provenance, citations) = project_message_metadata(&response.message.metadata);
            Json(MessageResponse {
                message_id: response.message.id,
                role,
                content: response.message.content,
                task: task_summary,
                provenance,
                citations,
            })
            .into_response()
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()).into_response(),
    }
}

/// GET /v1/conversations/:id
pub async fn get_conversation(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Path(conversation_id): Path<String>,
) -> ApiResult<ConversationResponse> {
    let scoped_id = format!("{}:{conversation_id}", tenant.0);

    match runtime.store.get_conversation(&scoped_id).await {
        Ok(convo) => Ok(Json(ConversationResponse {
            id: conversation_id,
            title: convo.title,
            messages: convo
                .messages
                .into_iter()
                .map(|m| {
                    let role = m.role_str().to_string();
                    let (provenance, citations) = project_message_metadata(&m.metadata);
                    MessageEntry {
                        id: m.id,
                        role,
                        content: m.content,
                        created_at: m.created_at,
                        provenance,
                        citations,
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
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(_tenant): Extension<TenantId>,
    Query(params): Query<ListQuery>,
) -> ApiResult<ConversationListResponse> {
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);

    match runtime.store.list_conversations(limit, offset).await {
        Ok(convos) => Ok(Json(ConversationListResponse {
            conversations: convos
                .into_iter()
                .map(|c| ConversationListEntry {
                    id: c.id,
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
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Path(conversation_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let scoped_id = format!("{}:{conversation_id}", tenant.0);

    match runtime.store.delete_conversation(&scoped_id).await {
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
    Extension(runtime): Extension<Arc<Runtime>>,
) -> ApiResult<ToolListResponse> {
    let descriptors = runtime.tools.descriptors();

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
    Extension(runtime): Extension<Arc<Runtime>>,
    Json(body): Json<SearchRequest>,
) -> ApiResult<SearchResponse> {
    match runtime.store.search_messages(&body.query).await {
        Ok(messages) => Ok(Json(SearchResponse {
            results: messages
                .into_iter()
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
    Extension(_tenant): Extension<TenantId>,
) -> ApiResult<CorpusListResponse> {
    let Some(engine) = runtime.corpus_engine.as_ref() else {
        return Ok(Json(CorpusListResponse {
            corpora: Vec::new(),
        }));
    };

    match engine.installed_indexes().await {
        Ok(indexes) => {
            let corpora = indexes
                .into_iter()
                .filter(|i| matches!(i.kind, corpus_engine::CorpusKind::Knowledge))
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
