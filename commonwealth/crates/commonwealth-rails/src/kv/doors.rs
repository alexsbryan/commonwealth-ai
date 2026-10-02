// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `/v1/mesh/kv/*` doors over [`KvHost`]'s store, re-exported at
//! `crate::kv`. A door that changed the store journals the write before it
//! answers ([`KvHost::journal_outbox`]).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use commonwealth_core::ids::NodeId;
use commonwealth_state::StoreEntry;
use host_kit::shell::RouteBundle;
use serde::Deserialize;
use tracing::{debug, warn};

use super::KvHost;
use crate::rail::err;

// ── The doors ────────────────────────────────────────────────

/// `GET`/`DELETE /v1/mesh/kv/entry` query — `sovereign_contracts::peer::KvLookup`.
#[derive(Debug, Deserialize)]
pub struct KvLookup {
    pub app_id: String,
    pub key: String,
}

/// `GET /v1/mesh/kv/entries` query — `KvScanQuery`; an empty prefix
/// enumerates the namespace.
#[derive(Debug, Deserialize)]
pub struct KvScanQuery {
    pub app_id: String,
    #[serde(default)]
    pub prefix: String,
}

/// `POST /v1/mesh/kv/entry` body — `KvSetBody`; `value` is base64.
#[derive(Debug, Deserialize)]
pub struct KvSetBody {
    pub app_id: String,
    pub key: String,
    pub value: String,
    pub origin: NodeId,
}

/// The four doors over `host`'s store, one of [`crate::api::bundles`].
pub fn router(host: Arc<KvHost>) -> RouteBundle {
    RouteBundle::new("kv")
        .route(
            "/v1/mesh/kv/entry",
            get(kv_get).post(kv_set).delete(kv_delete),
        )
        .route("/v1/mesh/kv/entries", get(kv_scan))
        .with_state(host)
}

/// A store row as `ReplicatedKvEntry`'s serde form.
fn to_entry(e: StoreEntry) -> serde_json::Value {
    serde_json::json!({
        "app_id": e.app_id,
        "key": e.key,
        "value": B64.encode(&e.value),
        "timestamp": e.timestamp,
        "origin": e.origin,
    })
}

fn store_error(op: &str, e: commonwealth_state::Error) -> Response {
    warn!(target: "rails", op, error = %e, "kv: store refused");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("mesh kv {op}: {e}"),
    )
}

/// Put the write a door just made on its journal before the door answers
/// ([`KvHost::journal_outbox`]). On the blocking pool, for the tick's reason:
/// an append re-reads the journal, synchronous I/O an async worker would hold
/// other requests behind. A write still queued after it (this node is in no
/// roster yet) is acknowledged as today, and named at `debug`.
async fn journal_before_ack(host: &Arc<KvHost>, op: &'static str) {
    let started = std::time::Instant::now();
    let runtime = tokio::runtime::Handle::current();
    let drain = {
        let host = Arc::clone(host);
        tokio::task::spawn_blocking(move || runtime.block_on(host.journal_outbox()))
    };
    match drain.await {
        Ok(out) => debug!(target: "rails", op, appended = out.appended, deferred = out.deferred,
                          refused = out.refused, elapsed_us = started.elapsed().as_micros() as u64,
                          "kv door: journaled the outbox before answering"),
        Err(e) => warn!(target: "rails", op, error = %e,
                        "kv door: the journal drain did not finish, so this write waits for the pump"),
    }
}

/// GET /v1/mesh/kv/entry — one record, or `null`.
async fn kv_get(State(host): State<Arc<KvHost>>, Query(q): Query<KvLookup>) -> Response {
    match host.store.get(&q.app_id, &q.key) {
        Ok(entry) => Json(entry.map(to_entry)).into_response(),
        Err(e) => store_error("get", e),
    }
}

/// POST /v1/mesh/kv/entry — whether the stored value CHANGED.
async fn kv_set(State(host): State<Arc<KvHost>>, Json(body): Json<KvSetBody>) -> Response {
    let value = match B64.decode(body.value.as_bytes()) {
        Ok(v) => bytes::Bytes::from(v),
        Err(e) => {
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("value is not base64: {e}"),
            )
        }
    };
    match host.store.set(&body.app_id, &body.key, value, body.origin) {
        Ok(changed) => {
            if changed {
                journal_before_ack(&host, "set").await;
            }
            Json(changed).into_response()
        }
        Err(e) => store_error("set", e),
    }
}

/// DELETE /v1/mesh/kv/entry — whether anything was there to remove.
async fn kv_delete(State(host): State<Arc<KvHost>>, Query(q): Query<KvLookup>) -> Response {
    match host.store.delete(&q.app_id, &q.key) {
        Ok(deleted) => {
            if deleted {
                journal_before_ack(&host, "delete").await;
            }
            Json(deleted).into_response()
        }
        Err(e) => store_error("delete", e),
    }
}

/// GET /v1/mesh/kv/entries — every record whose key starts with `prefix`.
async fn kv_scan(State(host): State<Arc<KvHost>>, Query(q): Query<KvScanQuery>) -> Response {
    match host.store.scan(&q.app_id, &q.prefix) {
        Ok(rows) => Json(rows.into_iter().map(to_entry).collect::<Vec<_>>()).into_response(),
        Err(e) => store_error("scan", e),
    }
}
