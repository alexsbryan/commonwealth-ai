// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/v1/engine/self`: what this process's provider says about itself, read
//! at the moment of the request from the reload cell, so a reload's new
//! provider and embed family are what the next read sees. The svrn daemon's
//! loopback terminal arm answers its own `model_id_for`, `resident_slots`,
//! `edit_slot_info` and the rest from this (pb-svrn-dials-serve).

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::Json;
use host_kit::shell::RouteBundle;
use sovereign_contracts::engine_state::{ServedSelf, SERVED_SELF_PATH};
use sovereign_contracts::{InferenceProvider, Speed};
use tracing::debug;

use crate::reload::ReloadableProvider;

/// The self-report route, over the cell every other route answers from.
pub fn bundle(cell: Arc<ReloadableProvider>) -> RouteBundle {
    RouteBundle::new("serve_self")
        .route(SERVED_SELF_PATH, get(served_self))
        .with_state(cell)
}

async fn served_self(State(cell): State<Arc<ReloadableProvider>>) -> Json<ServedSelf> {
    let this = ServedSelf {
        primary_model: cell.model_id_for(Speed::Slow),
        medium_model: cell.model_id_for(Speed::Medium),
        fast_model: cell.model_id_for(Speed::Fast),
        embed_model: cell.embed_model_id(),
        embed_family: cell.embed_family(),
        code_model: cell.code_model_id(),
        resident_slots: cell.resident_slots(),
        edit_slot: cell.edit_slot_info(),
        context_size: cell.effective_context_size(),
        compute_children: cell.compute_children(),
    };
    debug!(target: "serve", primary = %this.primary_model, family = ?this.embed_family, slots = this.resident_slots.len(), edit = this.edit_slot.is_some(), "served self: this process's provider");
    Json(this)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::model_family::ModelFamily;

    #[tokio::test]
    async fn serve_describes_its_own_provider_on_the_self_route() {
        let cell = Arc::new(ReloadableProvider::new(
            Arc::new(sovereign_compute::mock::MockProvider {
                tokens: 1,
                delay: std::time::Duration::ZERO,
            }),
            ModelFamily::Qwen3Embedding,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(host_kit::shell::serve(
            [listener],
            vec![bundle(cell)],
            std::future::pending(),
        ));
        let this: ServedSelf = reqwest::get(format!("{base}{SERVED_SELF_PATH}"))
            .await
            .expect("answered")
            .json()
            .await
            .expect("a ServedSelf");
        assert_eq!(this.primary_model, sovereign_compute::mock::MOCK_MODEL);
        assert_eq!(this.resident_slots.len(), 1);
        assert_eq!(this.resident_slots[0].role, "primary");
        assert_eq!(this.embed_family, ModelFamily::Qwen3Embedding);
    }
}
