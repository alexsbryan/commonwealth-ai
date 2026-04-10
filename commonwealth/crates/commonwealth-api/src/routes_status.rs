use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

/// GET /status — mesh and node status summary.
pub async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let mesh = state.inner.mesh.read().await;
    let plan = state.inner.inference_store.get_plan().unwrap_or_default();

    let members_online = mesh
        .members
        .values()
        .filter(|m| {
            m.status == commonwealth_core::mesh::NodeStatus::Online
                || m.status == commonwealth_core::mesh::NodeStatus::Busy
        })
        .count();

    let pooled_vram_gb: f32 = mesh
        .members
        .values()
        .filter(|m| m.status == commonwealth_core::mesh::NodeStatus::Online)
        .map(|m| m.capabilities.available.free_vram_gb)
        .sum();

    let pooled_storage_gb: f32 = mesh
        .members
        .values()
        .filter(|m| m.status == commonwealth_core::mesh::NodeStatus::Online)
        .map(|m| m.capabilities.available.free_storage_gb)
        .sum();

    let loaded_models: Vec<LoadedModelStatus> = plan
        .model_plans
        .iter()
        .map(|p| LoadedModelStatus {
            model: format!("{}", p.model),
            nodes: p.assignments.len(),
            tps: p.estimated_tokens_per_sec,
            loaded: state.inner.inference_store.get_llama_address(p.model).is_some(),
        })
        .collect();

    Json(StatusResponse {
        node_id: format!("{}", state.inner.self_node_id),
        mesh: MeshStatus {
            name: mesh.name.clone(),
            members_online,
            members_total: mesh.members.len(),
            pooled_vram_gb,
            pooled_storage_gb,
        },
        inference: InferenceStatus { loaded_models },
        knowledge: KnowledgeStatus {
            hosted_corpora: vec![],
            total_chunks_searchable: 0,
        },
    })
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub node_id: String,
    pub mesh: MeshStatus,
    pub inference: InferenceStatus,
    pub knowledge: KnowledgeStatus,
}

#[derive(Debug, Serialize)]
pub struct MeshStatus {
    pub name: String,
    pub members_online: usize,
    pub members_total: usize,
    pub pooled_vram_gb: f32,
    pub pooled_storage_gb: f32,
}

#[derive(Debug, Serialize)]
pub struct InferenceStatus {
    pub loaded_models: Vec<LoadedModelStatus>,
}

#[derive(Debug, Serialize)]
pub struct LoadedModelStatus {
    pub model: String,
    pub nodes: usize,
    pub tps: f32,
    pub loaded: bool,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeStatus {
    pub hosted_corpora: Vec<String>,
    pub total_chunks_searchable: u64,
}
