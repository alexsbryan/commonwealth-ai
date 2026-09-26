// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh store this daemon hosts, its pump, and the `/v1/mesh/kv/*` doors
//! over it (five-programs fp-77, decision five-programs-40).
//!
//! **The store is a projection of THIS process's journals.** One
//! `MeshStore::in_memory()`, rebuilt at start by folding every KV-shaped
//! namespace under the rail root through `commonwealth_state::rail_kv` — the
//! journal is the durable half, so a restart loses nothing the pump had
//! appended. The pump is the send half lifted from
//! `sovereign_mesh::rail_kv_pump`: every [`PUMP_INTERVAL`] it drains the
//! store's outbox onto the namespace's journal, signed with this node's key,
//! and seals + snapshots a KV namespace its outbox fed once this node's own
//! ops above its last seal pass `rail_kv::SEAL_AFTER_OWN_OPS`.
//!
//! **One sealer per journal.** A snapshot's mark retires every row of its
//! actor it does not name, so two stores sealing one journal would retire
//! each other's rows. Since the daemon pump's KV half was deleted (fp-83) this
//! is the only store that seals a store namespace, local-only ones included.
//! It seals only the namespaces its OWN outbox fed on the same tick, and never
//! the `mesh-measurements` or `work` journals — their writers are the daemon's.
//!
//! The doors' bodies are the daemon's (`routes_mesh_kv.rs`), field for field,
//! so the one client (sovereign-turn-client's `rails_kv`) reads either. The wire structs
//! themselves are `sovereign_contracts::peer`'s, which this binary may not
//! name (the lift boundary), so the request shapes are mirrored here the way
//! [`crate::api::ClaimRequest`] mirrors the publish body.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{Ed25519Verifier, RailAct, RailError, RingJournal, RingRail, Roster};
use commonwealth_state::rail_kv::{self, SEAL_AFTER_OWN_OPS};
use commonwealth_state::{MeshStore, Outboxed, StoreEntry};
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::rail::err;

/// How often the outbox is drained — the daemon pump's interval, for the
/// same reason: it is the latency of a local write reaching the ring.
pub const PUMP_INTERVAL: Duration = Duration::from_secs(2);

/// How many outbox rows one tick takes. A bound on how long one tick holds a
/// journal's writer, not a throughput knob: what is left is taken next tick.
const OUTBOX_DRAIN_LIMIT: usize = 256;

/// The store, and what projecting and pumping it needs.
pub struct KvHost {
    pub store: MeshStore,
    rail: Arc<RingRail>,
    mesh: Arc<RwLock<Mesh>>,
    self_id: NodeId,
    self_pubkey: Option<NodePubkey>,
    /// Namespaces the ingest door took new peer ops for since the last tick;
    /// [`KvHost::project_dirty`] folds each once (fp-109).
    dirty: Mutex<BTreeSet<String>>,
}

/// What one [`KvHost::pump_once`] did — returned so a test asserts on the
/// mechanism rather than on log lines. Same verdicts as the daemon pump's.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PumpOutcome {
    pub appended: usize,
    /// Left queued: the roster was unreadable, or this node is in no roster.
    pub deferred: usize,
    /// Dropped with a `warn`: the rail will never accept them.
    pub refused: usize,
    pub sealed: usize,
    /// Live rows re-appended behind those seals (the mark not counted).
    pub snapshot_rows: usize,
}

/// Whether a namespace's journal speaks `rail_kv`. The work plane folds its
/// own journal, and a gossip-excluded namespace is one the store refuses
/// inbound (`MeshStore::apply_projection`) — skipped here rather than warned.
fn is_kv_namespace(namespace: &str) -> bool {
    namespace != commonwealth_work::WORK_NAMESPACE
        && !commonwealth_state::peer_preferences::is_gossip_excluded(namespace)
}

impl KvHost {
    pub fn new(
        rail: Arc<RingRail>,
        mesh: Arc<RwLock<Mesh>>,
        self_id: NodeId,
        self_pubkey: Option<NodePubkey>,
    ) -> Result<Self, commonwealth_state::Error> {
        Ok(Self {
            store: MeshStore::in_memory()?,
            rail,
            mesh,
            self_id,
            self_pubkey,
            dirty: Mutex::new(BTreeSet::new()),
        })
    }

    /// Queue `namespace` for the next tick's fold — called by the ingest door
    /// when it admitted new ops, so a peer's write reaches the store without
    /// a restart.
    pub fn mark_dirty(&self, namespace: &str) {
        self.dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(namespace.to_string());
    }

    /// Fold every namespace marked since the last call, once each — batched
    /// per tick, never per ingested chunk. A non-store name is skipped by
    /// [`KvHost::project_namespace`]'s own debug line. Returns how many
    /// projected.
    pub async fn project_dirty(&self) -> usize {
        let dirty = std::mem::take(&mut *self.dirty.lock().unwrap_or_else(|p| p.into_inner()));
        if dirty.is_empty() {
            return 0;
        }
        let mut projected = 0usize;
        for namespace in &dirty {
            if self.project_namespace(namespace).await.is_some() {
                projected += 1;
            }
        }
        debug!(target: "rails", dirty = dirty.len(), projected,
               "kv: folded the namespaces the ingest door fed since the last tick");
        projected
    }

    /// Rebuild the store from every journal on disk. Run once at start,
    /// before the first drain; returns how many namespaces projected, the
    /// local-only ones rehydrated from this node's own rows included.
    pub async fn project_all_on_disk(&self) -> usize {
        let started = std::time::Instant::now();
        let namespaces = match self.rail.namespaces() {
            Ok(n) => n,
            Err(e) => {
                warn!(target: "rails", error = %e, "kv: cannot enumerate namespaces, nothing projected at start");
                return 0;
            }
        };
        let mut projected = 0usize;
        for namespace in &namespaces {
            if self.project_namespace(namespace).await.is_some() {
                projected += 1;
            }
        }
        // `RingRail::namespaces` omits local-only journals by design (never
        // offered), so they are read from the unfiltered disk list and folded
        // through the own-journal door (fp-108).
        let local_only: Vec<String> = match commonwealth_rail::namespaces_in(self.rail.root()) {
            Ok(all) => all
                .into_iter()
                .filter(|ns| commonwealth_rail::is_local_only(ns))
                .collect(),
            Err(e) => {
                warn!(target: "rails", error = %e, "kv: cannot enumerate local-only journals, none rehydrated");
                Vec::new()
            }
        };
        let mut rehydrated = 0usize;
        for namespace in &local_only {
            if self.project_own_namespace(namespace).await.is_some() {
                rehydrated += 1;
            }
        }
        info!(
            target: "rails",
            namespaces = namespaces.len(),
            projected,
            local_only = local_only.len(),
            rehydrated,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "kv: rebuilt the store from the journals on disk"
        );
        projected + rehydrated
    }

    /// Fold one local-only namespace's journal back into the store, taking
    /// only this node's own signed rows (`MeshStore::apply_own_projection`).
    async fn project_own_namespace(&self, namespace: &str) -> Option<usize> {
        let (journal, roster) = match self.journal_and_roster(namespace).await {
            Ok(jr) => jr,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: a local-only journal or its roster is unreadable, nothing rehydrated");
                return None;
            }
        };
        let admission = match journal.admit(&roster, &Ed25519Verifier) {
            Ok(a) => a,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: a local-only journal would not admit, nothing rehydrated");
                return None;
            }
        };
        let projection = rail_kv::project(&admission);
        match self.store.apply_own_projection(
            namespace,
            &projection,
            &self.rail.signer().actor(),
            self.self_id,
        ) {
            Ok(applied) => {
                debug!(
                    target: "rails",
                    namespace,
                    rows = projection.rows.len(),
                    merged = applied.merged,
                    deleted = applied.deleted,
                    skipped_foreign = applied.unattributed,
                    "kv: rehydrated a local-only namespace from its own journal"
                );
                Some(applied.merged + applied.deleted)
            }
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the store refused this local-only namespace");
                None
            }
        }
    }

    /// Fold one namespace's admitted ops into the store.
    pub async fn project_namespace(&self, namespace: &str) -> Option<usize> {
        if !is_kv_namespace(namespace) {
            debug!(target: "rails", namespace, "kv: not a store namespace, skipped");
            return None;
        }
        let (journal, roster) = match self.journal_and_roster(namespace).await {
            Ok(jr) => jr,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the journal or its roster is unreadable, nothing projected");
                return None;
            }
        };
        let admission = match journal.admit(&roster, &Ed25519Verifier) {
            Ok(a) => a,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the journal would not admit, nothing projected");
                return None;
            }
        };
        let projection = rail_kv::project(&admission);
        if projection.unreadable > 0 {
            warn!(target: "rails", namespace, unreadable = projection.unreadable,
                  "kv: acts on a store namespace this build cannot read as store writes");
        }
        let node_ids = self.node_ids().await;
        match self.store.apply_projection(
            namespace,
            &projection,
            |actor| node_ids.get(actor).copied(),
            self.self_id,
        ) {
            Ok(applied) => {
                debug!(
                    target: "rails",
                    namespace,
                    rows = projection.rows.len(),
                    merged = applied.merged,
                    deleted = applied.deleted,
                    reconciled = applied.reconciled,
                    expired = applied.expired,
                    unattributed = applied.unattributed,
                    "kv: projected a namespace into the store"
                );
                Some(applied.merged + applied.deleted + applied.reconciled + applied.expired)
            }
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv: the store refused this namespace");
                None
            }
        }
    }

    /// Actor key → node id, from the same membership the roster derives from
    /// (`crate::rail::derive_roster`'s rules: self's key is passed in, a row
    /// with no key places nobody).
    async fn node_ids(&self) -> HashMap<String, NodeId> {
        let mesh = self.mesh.read().await;
        let mut out = HashMap::new();
        for record in mesh.members.values() {
            let key = if record.node_id == self.self_id {
                self.self_pubkey.or(record.node_pubkey)
            } else {
                record.node_pubkey
            };
            if let Some(key) = key {
                out.insert(key.to_string(), record.node_id);
            }
        }
        out
    }

    /// One drain of the outbox onto the journals, plus the seal check for
    /// every namespace a row was appended to. Appended rows are acked;
    /// `NotInRoster` and an unreadable roster leave rows queued; any other
    /// refusal acks the row with a `warn` naming it.
    pub async fn pump_once(&self) -> PumpOutcome {
        let mut out = PumpOutcome::default();
        let queued = match self.store.outbox_take(OUTBOX_DRAIN_LIMIT) {
            Ok(rows) => rows,
            Err(e) => {
                warn!(target: "rails", error = %e, "kv pump: the outbox could not be read");
                return out;
            }
        };
        let mut by_namespace: BTreeMap<String, Vec<Outboxed>> = BTreeMap::new();
        for row in queued {
            by_namespace
                .entry(row.app_id.clone())
                .or_default()
                .push(row);
        }

        for (namespace, rows) in by_namespace {
            let (journal, roster) = match self.journal_and_roster(&namespace).await {
                Ok(jr) => jr,
                Err(e) => {
                    warn!(target: "rails", namespace, error = %e, queued = rows.len(),
                          "kv pump: the roster is unreadable, so these writes stayed queued");
                    out.deferred += rows.len();
                    continue;
                }
            };
            let mut acked: Vec<i64> = Vec::new();
            let mut appended_here = 0usize;
            let mut not_in_roster = false;
            for row in &rows {
                if not_in_roster {
                    out.deferred += 1;
                    continue;
                }
                let op = &row.op;
                let payload = match rail_kv::to_payload(&op.key, op.value.as_deref(), op.t) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(target: "rails", namespace, key = %op.key, error = %e,
                              "kv pump: this write cannot travel on the rail and was dropped");
                        out.refused += 1;
                        acked.push(row.id);
                        continue;
                    }
                };
                match journal.append(
                    RailAct::Record { payload },
                    self.rail.signer(),
                    &roster,
                    None,
                ) {
                    Ok(appended) => {
                        debug!(target: "rails", namespace, key = %op.key, t = op.t,
                               deleted = op.value.is_none(), seq = appended.kind.seq,
                               "kv pump: appended a local write");
                        acked.push(row.id);
                        appended_here += 1;
                    }
                    Err(RailError::NotInRoster { actor, .. }) => {
                        not_in_roster = true;
                        out.deferred += 1;
                        debug!(target: "rails", namespace, actor = %actor, queued = rows.len(),
                               "kv pump: this node is in no roster for this namespace yet, so its \
                                writes stay queued");
                    }
                    Err(e) => {
                        warn!(target: "rails", namespace, key = %op.key, error = %e,
                              "kv pump: the rail refused this write, which was dropped");
                        out.refused += 1;
                        acked.push(row.id);
                    }
                }
            }
            if !acked.is_empty() {
                if let Err(e) = self.store.outbox_ack(&acked) {
                    warn!(target: "rails", error = %e, rows = acked.len(), "kv pump: the outbox ack failed");
                }
            }
            out.appended += appended_here;
            if appended_here > 0 {
                self.seal_if_due(&journal, &roster, &mut out).await;
            }
        }
        out
    }

    async fn journal_and_roster(
        &self,
        namespace: &str,
    ) -> Result<(Arc<RingJournal>, Roster), RailError> {
        let journal = self.rail.journal(namespace)?;
        let roster = self.rail.roster(&journal).await?;
        Ok((journal, roster))
    }

    /// Seal and snapshot `journal` if this node's own ops at or above its
    /// floor have passed `SEAL_AFTER_OWN_OPS`. The raw line count gates the
    /// admission (admitted-above-floor can never exceed it), so the verify
    /// cost is paid only when a seal may be due.
    async fn seal_if_due(&self, journal: &RingJournal, roster: &Roster, out: &mut PumpOutcome) {
        let namespace = journal.namespace();
        // Local-only journals are sealed too: since the daemon pump's KV half
        // went (fp-83) this store is their one writer, so no twin sealer can
        // retire its rows. `is_kv_namespace` still gates projection.
        if !is_kv_namespace(namespace) && !commonwealth_rail::is_local_only(namespace) {
            return;
        }
        let mine = self.rail.signer().actor();
        let own_lines = match journal.read() {
            Ok((ops, _)) => ops.iter().filter(|o| o.actor == mine).count(),
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv pump: the journal could not be read for the seal check");
                return;
            }
        };
        if own_lines < SEAL_AFTER_OWN_OPS {
            return;
        }
        let admission = match journal.admit(roster, &Ed25519Verifier) {
            Ok(a) => a,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv pump: the journal could not be admitted for the seal check");
                return;
            }
        };
        let floor = admission.floors.get(&mine).copied().unwrap_or(0);
        let own_above_floor = admission
            .ops
            .iter()
            .filter(|o| o.actor == mine && o.seq >= floor)
            .count();
        if own_above_floor < SEAL_AFTER_OWN_OPS {
            debug!(target: "rails", namespace, own_lines, own_above_floor, floor,
                   "kv pump: the cheap line count cleared the bar and the admitted count did not");
            return;
        }
        let sealed = match journal.seal(self.rail.signer(), roster, &Ed25519Verifier) {
            Ok(s) => s,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e, "kv pump: the seal was refused, so nothing was retired");
                return;
            }
        };
        out.sealed += 1;
        if let Ok(done) = &sealed.retired {
            info!(target: "rails", namespace, own_above_floor, removed = done.removed,
                  kept = done.kept, "kv pump: sealed a store namespace");
        }
        out.snapshot_rows += self.snapshot(journal, roster, sealed.op.kind.seq);
    }

    /// Re-append this node's live rows above the floor its seal just set,
    /// then the mark that closes the snapshot — without it a seal is a delete.
    /// Only rows this node ORIGINATED; the mark is written last, and one that
    /// could not be written claims nothing (the failure direction is "do not
    /// retire").
    fn snapshot(&self, journal: &RingJournal, roster: &Roster, floor: u64) -> usize {
        let namespace = journal.namespace();
        let rows = match self.store.scan(namespace, "") {
            Ok(r) => r,
            Err(e) => {
                warn!(target: "rails", namespace, error = %e,
                      "kv pump: the live set could not be read, so the seal retired rows nothing replaced");
                return 0;
            }
        };
        let mut appended = 0usize;
        let mut skipped = 0usize;
        for row in rows {
            if row.origin != self.self_id {
                skipped += 1;
                continue;
            }
            let written = rail_kv::to_payload(&row.key, Some(&row.value), row.timestamp)
                .map_err(|e| e.to_string())
                .and_then(|payload| {
                    journal
                        .append(
                            RailAct::Record { payload },
                            self.rail.signer(),
                            roster,
                            None,
                        )
                        .map_err(|e| e.to_string())
                });
            match written {
                Ok(_) => appended += 1,
                Err(e) => warn!(target: "rails", namespace, key = %row.key, error = %e,
                                "kv pump: a live row could not be snapshotted and is now below the floor"),
            }
        }
        let mark = rail_kv::snapshot_mark(floor)
            .map_err(|e| e.to_string())
            .and_then(|payload| {
                journal
                    .append(
                        RailAct::Record { payload },
                        self.rail.signer(),
                        roster,
                        None,
                    )
                    .map_err(|e| e.to_string())
            });
        match mark {
            Ok(_) => {
                info!(target: "rails", namespace, appended, peers_rows_skipped = skipped, floor,
                           "kv pump: snapshotted this node's live rows above the new floor, and closed it")
            }
            Err(e) => warn!(target: "rails", namespace, appended, floor, error = %e,
                            "kv pump: the snapshot could not be closed, so peers will keep \
                             whatever of ours they already hold"),
        }
        appended
    }
}

/// Drain the store forever. Spawned by [`crate::RailsDaemon::run`] after it
/// has projected the store; aborted with it.
pub async fn run_forever(host: Arc<KvHost>) {
    info!(target: "rails", interval_secs = PUMP_INTERVAL.as_secs(),
          seal_after_own_ops = SEAL_AFTER_OWN_OPS, "kv pump: started");
    loop {
        host.project_dirty().await;
        let out = host.pump_once().await;
        if out != PumpOutcome::default() {
            debug!(target: "rails", appended = out.appended, deferred = out.deferred,
                   refused = out.refused, sealed = out.sealed, snapshot_rows = out.snapshot_rows,
                   "kv pump: tick");
        }
        tokio::time::sleep(PUMP_INTERVAL).await;
    }
}

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

/// The four doors over `host`'s store. Merged into [`crate::api::router`].
pub fn router(host: Arc<KvHost>) -> Router {
    Router::new()
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
        Ok(changed) => Json(changed).into_response(),
        Err(e) => store_error("set", e),
    }
}

/// DELETE /v1/mesh/kv/entry — whether anything was there to remove.
async fn kv_delete(State(host): State<Arc<KvHost>>, Query(q): Query<KvLookup>) -> Response {
    match host.store.delete(&q.app_id, &q.key) {
        Ok(deleted) => Json(deleted).into_response(),
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

#[cfg(test)]
#[path = "kv/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "kv/projection_tests.rs"]
mod projection_tests;

#[cfg(test)]
#[path = "kv/snapshot_tests.rs"]
mod snapshot_tests;
