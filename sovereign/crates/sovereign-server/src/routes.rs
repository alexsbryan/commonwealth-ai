use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::Json;

use sovereign_core::runtime::Runtime;

use crate::approval::ServerApprovalChannel;
use crate::auth::TenantId;
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
pub async fn send_message(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Extension(approval): Extension<Arc<ServerApprovalChannel>>,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> ApiResult<MessageResponse> {
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
            Ok(Json(MessageResponse {
                message_id: response.message.id,
                role,
                content: response.message.content,
                task: task_summary,
            }))
        }
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
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
                    MessageEntry {
                        id: m.id,
                        role,
                        content: m.content,
                        created_at: m.created_at,
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

    Ok(Json(ApproveResponse {
        task_id,
        accepted,
    }))
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
