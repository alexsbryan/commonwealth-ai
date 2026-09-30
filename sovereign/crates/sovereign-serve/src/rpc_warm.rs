// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/rpc-warm` on a standalone serve: the worker side of the
//! distributed-primary shard warm (pb-serve-distributes-standalone). A host
//! about to distribute asks each worker to seed its RPC tensor cache with its
//! shard; this route resolves the worker's local copy of the model with the
//! one resolver (`sovereign_contracts::rpc_warm::resolve_local_model`, over
//! serve's servable files), the host's fetch bases through cw-rails, and hands
//! both to the loader's warmer (`MeshRpcShardWarmer`), as svrn's route does
//! for a stock node.
//!
//! Peers reach it through cw-rails (`cwth/http/0`, registered by
//! `rails_mesh::spawn_registrations`). serve listens on loopback, so a
//! request reaches it from this host or through cw-rails, which admits
//! members only.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use host_kit::shell::RouteBundle;
use mesh_reach::{PeerTransport, TrafficClass};
use serde_json::{json, Value};
use sovereign_contracts::membership::MembershipReader;
use sovereign_contracts::rpc_warm::{resolve_local_model, RpcShardWarmer, WarmReach};
use sovereign_serving_host::state::ServableModelFilesReader;
use tracing::{info, warn};

use crate::rails_mesh::{RailsRoster, RPC_WARM_PATH};

#[derive(Clone)]
struct Warm {
    servable: ServableModelFilesReader,
    roster: RailsRoster,
    transport: Arc<dyn PeerTransport>,
    warmer: Arc<dyn RpcShardWarmer>,
}

/// The route, over serve's servable files and cw-rails' roster and reach.
pub fn bundle(
    servable: ServableModelFilesReader,
    roster: RailsRoster,
    warmer: Arc<dyn RpcShardWarmer>,
) -> RouteBundle {
    let transport: Arc<dyn PeerTransport> =
        Arc::new(mesh_reach::rails::RailsTransport::new(roster.base()));
    RouteBundle::new("serve_rpc_warm")
        .route(RPC_WARM_PATH, post(rpc_warm))
        .with_state(Warm {
            servable,
            roster,
            transport,
            warmer,
        })
}

/// The host's model-file bases through cw-rails, for the `host_node_id` hex
/// the request carries. Empty when the id is absent or unknown to the roster:
/// the warmer then uses the request's raw bases alone, as on svrn's route.
async fn host_bases(w: &Warm, host_node_id: Option<&str>) -> Vec<String> {
    let Some(id) = host_node_id.and_then(kernel_types::NodeId::from_hex) else {
        return Vec::new();
    };
    let Some(member) = w.roster.member(id).await else {
        info!(target: "serve", host = %id.to_hex(), "rpc-warm: the host is not on cw-rails' roster; raw bases only");
        return Vec::new();
    };
    w.transport
        .endpoints(&member.dial, TrafficClass::ModelTransfer)
        .await
        .into_iter()
        .map(|e| e.base_url)
        .collect()
}

async fn rpc_warm(State(w): State<Warm>, Json(body): Json<Value>) -> Response {
    let model_id = body
        .get("model_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let local = model_id
        .as_deref()
        .and_then(|id| resolve_local_model(&w.servable.current(), id));
    let reach = WarmReach {
        host_bases: host_bases(&w, body.get("host_node_id").and_then(Value::as_str)).await,
        // serve holds no mesh credential; the host's serve reads none.
        proof: None,
    };
    info!(target: "serve", model = ?model_id, local = ?local, host_bases = reach.host_bases.len(),
          "rpc-warm: warming this worker's shard");
    match w.warmer.warm_shard(body, local, reach).await {
        Ok(stats) => {
            info!(target: "serve", model = ?model_id, "rpc-warm: shard warm");
            (StatusCode::OK, Json(stats)).into_response()
        }
        Err(e) => {
            warn!(target: "serve", model = ?model_id, error = %e, "rpc-warm: the warm failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
                .into_response()
        }
    }
}
