// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's mesh-KV serving surface — `/v1/mesh/kv/*` (five-programs
//! fp-33).
//!
//! Before this module, the `svrn` workbench opened the mesh's replicated KV
//! directly — `MeshReplicatedKv::open` on a repo-local `mesh.db` — which is
//! the ownership line §4 rule 1 and §12 decision 2 draw wrong: a second
//! process never opens the mesh's store, it DIALS the process that owns it.
//! A record written into a repo-local island was invisible to the daemon,
//! to gossip, and to every peer; the store this module serves is the ONE
//! instance (`AppState.inner.fabric.mesh_store`) the daemon's own work-atlas
//! writes and gossip publishes from, so a claim the workbench dials in now
//! lands where every reader already looks.
//!
//! The wire vocabulary is `sovereign_contracts::peer`'s own:
//! [`sovereign_contracts::peer::ReplicatedKvEntry`] carries its serde form,
//! and the request shapes are the `Kv*` structs beside it — one schema, both
//! ends link contracts. The dialing client is
//! `sovereign-cli-dev/src/mesh_kv_client.rs`.
//!
//! Mounted on the Operator surface only (the same bind the workbench's other
//! daemon calls use); a peer or guest listener 404s these paths rather than
//! gating them. Transport-level absence is the caller's to report: a daemon
//! that is down simply does not answer, and the client turns that into a
//! named `ReplicatedKvError` — never a quiet empty store (principle 6).

use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};

use sovereign_contracts::peer::{KvLookup, KvScanQuery, KvSetBody, ReplicatedKvEntry};

use crate::http_response::internal_error;
use crate::state::AppState;

/// The mesh-KV routes, over the one shared store. A store failure is a 500
/// naming the operation; every success is 200 with the operation's own
/// answer (`null` for an absent key IS the answer — "absent" is a fact about
/// the store, not an error).
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/mesh/kv/entry",
            get(kv_get).post(kv_set).delete(kv_delete),
        )
        .route("/v1/mesh/kv/entries", get(kv_scan))
}

/// GET /v1/mesh/kv/entry?app_id=&key= — one record, or `null`.
async fn kv_get(
    State(state): State<AppState>,
    Query(q): Query<KvLookup>,
) -> Result<Json<Option<ReplicatedKvEntry>>, Response> {
    match state.inner.fabric.mesh_store.get(&q.app_id, &q.key) {
        Ok(entry) => Ok(Json(entry.map(to_entry))),
        Err(e) => Err(internal_error(format!("mesh kv get: {e}"))),
    }
}

/// POST /v1/mesh/kv/entry — write one record; answers whether the stored
/// value CHANGED (the port's [`sovereign_contracts::peer::ReplicatedKv::set`]
/// contract, unchanged across the wire).
async fn kv_set(
    State(state): State<AppState>,
    Json(body): Json<KvSetBody>,
) -> Result<Json<bool>, Response> {
    let KvSetBody {
        app_id,
        key,
        value,
        origin,
    } = body;
    match state
        .inner
        .fabric
        .mesh_store
        .set(&app_id, &key, value, origin)
    {
        Ok(changed) => Ok(Json(changed)),
        Err(e) => Err(internal_error(format!("mesh kv set: {e}"))),
    }
}

/// DELETE /v1/mesh/kv/entry?app_id=&key= — answers whether anything was
/// there to remove.
async fn kv_delete(
    State(state): State<AppState>,
    Query(q): Query<KvLookup>,
) -> Result<Json<bool>, Response> {
    match state.inner.fabric.mesh_store.delete(&q.app_id, &q.key) {
        Ok(deleted) => Ok(Json(deleted)),
        Err(e) => Err(internal_error(format!("mesh kv delete: {e}"))),
    }
}

/// GET /v1/mesh/kv/entries?app_id=&prefix= — every record whose key starts
/// with `prefix`; an empty prefix enumerates the namespace.
async fn kv_scan(
    State(state): State<AppState>,
    Query(q): Query<KvScanQuery>,
) -> Result<Json<Vec<ReplicatedKvEntry>>, Response> {
    match state.inner.fabric.mesh_store.scan(&q.app_id, &q.prefix) {
        Ok(rows) => Ok(Json(rows.into_iter().map(to_entry).collect())),
        Err(e) => Err(internal_error(format!("mesh kv scan: {e}"))),
    }
}

/// The store's row, as the wire type. Field-for-field the same mapping
/// `sovereign_mesh::peer_adapter` applies between `MeshStore` and the port —
/// the types agree by construction.
fn to_entry(e: commonwealth_state::StoreEntry) -> ReplicatedKvEntry {
    ReplicatedKvEntry {
        app_id: e.app_id,
        key: e.key,
        value: e.value,
        timestamp: e.timestamp,
        origin: e.origin,
    }
}
