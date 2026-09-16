// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use tokio::sync::RwLock;

use async_trait::async_trait;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_state::store_adapter::InferenceStateStore;
use commonwealth_state::{ActivityEmitter, ContributionEmitter, MeshStore, PeerPreferenceStore};
use corpus_engine::CorpusEngine;
use oicp_types::model_aliases::ModelAliasTable;
use serving_policy::fair_sched::{reciprocity_weight, SchedCore, TryGrant};
use sovereign_core::identity::IdentityReader;
use sovereign_grants::{EphemeralGrantStore, GuestGrantStore, WorkQueueManager};
use sovereign_meshapp_registry::proxy::AppPortMap;
use sovereign_meshapp_registry::registry::AppRegistry;
use sovereign_serving_host::admission::Principal;

// Moved to the leaves by domains `REVIEW-build-local-inference`: the wire types
// are protocol vocabulary (`oicp-types`) and the OpenAI-shaped port is a
// behavioural contract (`sovereign-contracts`). Re-exported here so the route
// shells and tests that name `sovereign_api::state::*` keep compiling; the two
// host-bound modules (`inference_adapter`, `fim_adapter`) are repointed to the
// leaves directly, which is what lets them leave `sovereign-mesh`.
pub use oicp_types::{EditSlotStatus, FimCompletionRequest, FimStreamStart, LocalInferenceError};
pub use sovereign_core::traits::LocalInferenceService;

pub mod answering;
pub mod fabric;
pub mod ingest;
pub mod node;
pub mod serving;
pub mod workbench;

// Fabric's construction seed and its readers, re-exported so the daemon and the
// test harnesses name them at `sovereign_api::state::*` (the same surface the
// constructors take).
pub use fabric::{ClockReader, DialInfoReader, DialSigner, FabricSeed, PeerTransportReader};

/// One inference slot's *actual* in-memory residency, as reported by
/// the embedded engine — the daemon-facing mirror of
/// `sovereign_core::traits::ResidentSlot`. Kept as its own type here
/// because `commonwealth-api` depends on `sovereign-core` but not
/// `sovereign-contracts`; the [`LocalInferenceService`] adapter maps
/// across the seam. This is the ground truth behind `/status`'s
/// `loaded` flag (the `ollama ps` analog).
/// Where a slot's weights physically live — the `/status` mirror of
/// `sovereign_core::traits::SlotPlacement`. The glassbox answer to "is this
/// model distributed across the mesh, and how is it split?", so an operator
/// never has to infer distribution from `free` deltas.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlotPlacement {
    /// `local` | `distributed` | `stream-split` | `forming`.
    pub mode: String,
    /// Total transformer blocks the plan apportions (`0` when local).
    pub total_blocks: u32,
    /// Blocks resident on THIS node's local GPU.
    pub local_blocks: u32,
    /// Per remote RPC worker: endpoint + the block count pinned onto it.
    pub workers: Vec<WorkerPlacement>,
}

/// One remote worker's share of a distributed slot (`/status` mirror).
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerPlacement {
    pub endpoint: String,
    pub blocks: u32,
    pub holds_output: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResidentSlot {
    /// Role stem: `fast` | `primary` | `embed` | `code` | `rerank` |
    /// `extra:<name>` | `primary_pool`.
    pub role: String,
    /// The gguf file stem currently occupying the slot.
    pub model_id: String,
    /// `true` when the weights are resident in memory this instant.
    pub resident: bool,
    /// Resident byte footprint when the engine knows it, else `None`.
    pub size_bytes: Option<u64>,
    /// `true` when the slot is mid load/unload (residency momentarily
    /// indeterminate). Never forces a load to resolve it.
    pub transitioning: bool,
    /// Physical placement (distributed vs local + the split). `None` for
    /// non-distributable slots. Stated, never inferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<SlotPlacement>,
}

/// One supervised compute-child's status (`/status` mirror of
/// `sovereign_core::traits::ComputeChildStatus`). commonwealth-api cannot
/// depend on sovereign-contracts, so this is copied field-for-field — the
/// same convention as [`ResidentSlot`] / [`SlotPlacement`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComputeChildStatus {
    /// Replica name (`<pool>-<i>`).
    pub name: String,
    /// `"generate"` | `"embed"`.
    pub role: String,
    /// The addressable pool id this replica belongs to.
    pub model_id: String,
    /// `starting` | `warming` | `serving` | `degraded` | `restarting` | `failed`.
    pub lifecycle: String,
    /// Current ephemeral port, when serving/warming.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Restart count.
    pub restarts: u32,
    /// Reason for the most recent lifecycle transition.
    pub last_transition_reason: String,
    /// Reason for the most recent exit/crash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<String>,
}

// The `/status` projections of the engine's residency report. The engine's
// types live in `oicp-types` (the leaf every crate reaches); these mirrors
// stay here because their serialised shape differs — they OMIT `placement` /
// `port` / `last_exit` when `None` where the originals emit `null`. Converging
// them is a `/status` wire change with a golden, deferred to the
// HUMAN-behaviour-rungs (domains `REVIEW-build-local-inference`). Until then
// this is the ONE projection (ARCH §10.6): the trait returns the leaf type and
// these two `From` impls are the only translation.
impl From<oicp_types::ResidentSlot> for ResidentSlot {
    fn from(s: oicp_types::ResidentSlot) -> Self {
        Self {
            role: s.role,
            model_id: s.model_id,
            resident: s.resident,
            size_bytes: s.size_bytes,
            transitioning: s.transitioning,
            placement: s.placement.map(|p| SlotPlacement {
                mode: p.mode,
                total_blocks: p.total_blocks,
                local_blocks: p.local_blocks,
                workers: p
                    .workers
                    .into_iter()
                    .map(|w| WorkerPlacement {
                        endpoint: w.endpoint,
                        blocks: w.blocks,
                        holds_output: w.holds_output,
                    })
                    .collect(),
            }),
        }
    }
}

impl From<oicp_types::ComputeChildStatus> for ComputeChildStatus {
    fn from(c: oicp_types::ComputeChildStatus) -> Self {
        Self {
            name: c.name,
            role: c.role,
            model_id: c.model_id,
            lifecycle: c.lifecycle,
            port: c.port,
            restarts: c.restarts,
            last_transition_reason: c.last_transition_reason,
            last_exit: c.last_exit,
        }
    }
}

/// Worker side of the distributed-inference auto-warm orchestration. When a host
/// distributes a large primary across the mesh, it asks each worker (this node)
/// to seed its RPC tensor cache with ITS shard of the model — so the host's
/// subsequent `-ot` load is all `SET_TENSOR_HASH` cache hits and never streams a
/// large weight share (the upload deadlock). The impl (sovereign-mesh) holds an
/// HTTP client so it can fetch the GGUF — or, for the byte-range path, only its
/// shard's tensors — and the warm primitives from sovereign-inference. Injected
/// by the daemon; `None` on a node with no local inference.
///
/// Defined as an OPAQUE-JSON seam (`request`/return are the wire bodies, an
/// `RpcWarmShardRequest`/`RpcWarmShardResponse` defined in sovereign-mesh) so
/// commonwealth-api needn't depend on sovereign-inference's plan types — the same
/// decoupling [`LocalInferenceService`] gives the chat path. The route handler
/// resolves `model_id` → `local_model_path` against the servable allowlist (which
/// lives here) and passes it in, so the warmer can warm a model the node already
/// holds without re-fetching. Route: `POST /internal/rpc-warm`.
#[async_trait]
pub trait RpcShardWarmer: Send + Sync {
    /// `state` is this worker node's own `AppState`: the warmer resolves the
    /// HOST's fetch bases through this node's `PeerTransport` (the request may
    /// carry a `host_node_id`), so a cross-network host is reached over the
    /// mesh transport (iroh bridge) instead of a raw IP it may not route to.
    async fn warm_shard(
        &self,
        request: serde_json::Value,
        local_model_path: Option<std::path::PathBuf>,
        state: AppState,
    ) -> Result<serde_json::Value, String>;
}

/// Callback the route handlers fire whenever they mutate `Mesh` —
/// `/internal/join` (accepting a new member), `/internal/gossip`
/// (merging a peer's view). `sovereign-mesh::EmbeddedDaemon` installs
/// a hook that persists `mesh.json` synchronously so a restart within
/// the gossip interval never forgets a mutation. Tests leave this
/// `None` and rely on their assertions without touching disk.
pub type MeshMutationHook = std::sync::Arc<dyn Fn(&Mesh, NodeId) + Send + Sync>;

/// Shared application state for all API handlers.
/// One peer's request tally on this daemon (order `seat-resource-commons`
/// UC-R1) — the "who is my GPU serving right now?" answer `/status`
/// publishes.
///
/// `active` counts requests whose response BODY is still streaming (the
/// truthful in-flight window — scheduler slots release at headers time,
/// so they cannot answer "serving right now" for streaming responses).
/// `served_total` is cumulative since daemon start: the contamination-
/// attribution witness (e.g. "BeefyMac's daemon served N requests during
/// my soak window"). `last_request_at` is the unix-seconds admission
/// time of the most recent request, so a reader can tell "actively
/// serving" from "served before, idle since".
///
/// Keyed by `NodeId` parsed from the `X-Node-Id` header — the ONLY peer
/// attribution the daemon has (iroh tunnels raw-forward without
/// identity). Only ADMITTED requests are tallied; rejections are not
/// "serving".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerTally {
    /// Requests whose response body is currently streaming.
    pub active: u64,
    /// Requests admitted since daemon start (cumulative, monotonic).
    pub served_total: u64,
    /// Unix seconds of the most recent admission.
    pub last_request_at: i64,
}

/// A peer request whose `X-Node-Id` header was present but not the
/// canonical wire form (32 lowercase hex chars — [`NodeId::to_hex`]).
/// The request is still gated and tallied under the zero node; this
/// record lets `/status` name the rejected value so a misconfigured
/// peer's traffic is diagnosable, not opaque (fix 7).
#[derive(Debug, Clone)]
pub struct RejectedNodeIdHeader {
    /// The raw header value as received. Capped for display safety —
    /// a hostile or buggy peer can send an arbitrary-length header.
    pub raw: String,
    /// Unix seconds when the malformed value was last seen.
    pub at_unix: i64,
}

impl RejectedNodeIdHeader {
    /// The canonical wire form the header must match — the inverse of
    /// `crate::headers::parse_x_node_id` (which accepts exactly this).
    pub fn expected_wire_form() -> &'static str {
        "exactly 32 lowercase hex chars — NodeId::to_hex(), e.g. \
         0123456789abcdef0123456789abcdef"
    }
}

/// The notes-rail convergence stamps (order commons-fluency fix 9).
/// Written by the daemon's outbound notes publish sink (a note accepted
/// onto the mesh) and its inbound ingest poller (a peer batch applied);
/// read by `/status` as the publish-path liveness signal. A `None`
/// stamp means that path has never succeeded since boot — absence is
/// reported, never defaulted (ARCH §18.3). One shared instance is
/// carried into [`AppStateInner`] at construction ([`FabricSeed::convergence`])
/// so the daemon-side writers and the `/status` reader cannot disagree.
#[derive(Debug, Default)]
pub struct ConvergenceRecord {
    stamps: std::sync::Mutex<ConvergenceStamps>,
}

/// The two stamps behind [`ConvergenceRecord`].
#[derive(Debug, Default, Clone)]
struct ConvergenceStamps {
    /// Unix seconds when the outbound publish sink last accepted a
    /// note onto the mesh (set() Ok).
    last_outbound_publish_at: Option<i64>,
    /// Unix seconds when the inbound ingest poller last applied a
    /// peer batch (ingest_remote_notes Ok with events).
    last_inbound_ingest_at: Option<i64>,
}

impl ConvergenceRecord {
    /// A fresh record: both paths never-succeeded since boot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stamp the outbound publish path as alive. Called by the notes
    /// propagation sink's success arm (daemon bootstrap).
    pub fn record_outbound_publish_success(&self, at_unix: i64) {
        self.stamps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_outbound_publish_at = Some(at_unix);
    }

    /// Stamp the inbound ingest path as alive. Called when the daemon's
    /// ingest poller applies a peer batch.
    pub fn record_inbound_ingest_success(&self, at_unix: i64) {
        self.stamps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_inbound_ingest_at = Some(at_unix);
    }

    /// Read both stamps for `/status`.
    pub fn snapshot(&self) -> (Option<i64>, Option<i64>) {
        let s = self.stamps.lock().unwrap_or_else(|e| e.into_inner());
        (s.last_outbound_publish_at, s.last_inbound_ingest_at)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

/// Peer-inflight ceiling a freshly-constructed [`AppState`] starts with:
/// unbounded. This is the *pre-configuration* value only — the daemon ALWAYS
/// applies a finite ceiling at boot from `DaemonSection.max_peer_inflight`
/// (default 1, see `sovereign-mesh::daemon::start_daemon`), so a headless
/// contributor is never actually unbounded in production. Tests that build an
/// `AppState` directly inherit this and don't admit peer traffic, so the lack
/// of a bound is harmless there.
pub const DEFAULT_PEER_INFLIGHT_CEILING: usize = usize::MAX;

/// Reciprocity gain for the peer-admission per-node cap. A top contributor's
/// effective cap reaches the full ceiling; a pure consumer's stays at the
/// base. `0` would disable reciprocity (uniform cap = ceiling). Matches the
/// chat server's `[server] reciprocity_k` default.
pub const PEER_RECIPROCITY_K: f64 = 0.5;

/// Base per-node concurrency cap when rationing — a pure consumer (no
/// contribution) may hold this many peer slots at once.
const PEER_BASE_CAP: u32 = 1;

/// Derive a peer node's effective concurrency cap from the global ceiling and
/// its reciprocity weight. Not rationing (`ceiling` unbounded) → no per-node
/// limit, so the pool is shared freely (preserves the pre-existing default).
/// Rationing → a pure consumer holds [`PEER_BASE_CAP`]; a top contributor
/// (`weight → 1 + k`) may hold up to the whole ceiling.
fn effective_peer_cap(ceiling: usize, weight: f64) -> u32 {
    // `usize::MAX` is the "not rationing" sentinel (no comparison can
    // exceed it — `==` is the whole check).
    if ceiling == usize::MAX {
        return u32::MAX;
    }
    let ceiling = ceiling.min(u32::MAX as usize) as u32;
    if ceiling <= PEER_BASE_CAP || PEER_RECIPROCITY_K <= 0.0 {
        return ceiling.max(PEER_BASE_CAP);
    }
    // weight ∈ [1.0, 1.0 + k] → bonus ∈ [0, ceiling − base].
    let frac = ((weight - 1.0) / PEER_RECIPROCITY_K).clamp(0.0, 1.0);
    let bonus = (frac * f64::from(ceiling - PEER_BASE_CAP)).round() as u32;
    (PEER_BASE_CAP + bonus).clamp(PEER_BASE_CAP, ceiling)
}

pub struct AppStateInner {
    /// Fabric's part: the node id with its public key, the dial-info provider
    /// and dial signer, the roster, the ring rail and its write nudge, the
    /// replicated KV, transport, clock, gossip's liveness maps, the mutation
    /// persistence hook, the convergence recorder, the fan-out gauge, the
    /// mesh-app registry and port map, the contribution emitter and the
    /// RPC-over-iroh flag. Held as a part so route shells read it directly
    /// (DC §4.2).
    pub fabric: fabric::FabricPart,
    /// Serving's part: the model, pipeline and slot aliases, the servable
    /// model files, the local inference handle and RPC shard warmer, the
    /// inference store, the peer and client admission schedulers with their
    /// caps, switch, tallies, rejected-header record and reciprocity weights,
    /// the contribution pause and yield-peers switch, the availability
    /// composite, the in-flight gauge and the venue preferences. Held as a
    /// part so route shells read it directly (DC §4.2).
    pub serving: serving::ServingPart,
    /// The node's part: the client token, guest grants, start instant, the
    /// corpus-engine handle, the foreground-yield signal (last active, window,
    /// in-flight), the storage budget and usage, and the activity emitter.
    /// Held as a part so route shells read it directly (DC §4.2).
    pub node: node::NodePart,
    /// Answering's part: the ATOS middleware registry, session store and repo
    /// root. Held as a part so route shells read it directly (DC §4.2).
    pub answering: answering::AnsweringPart,
    /// Collaborative ingest's part: the active-ingest set, progress, the work
    /// queue and grants, pull loops and verify reports, the quiesce and
    /// throttle dials, and the newsworthy tick handle. Held as a part so route
    /// shells read it directly (DC §4.2).
    pub ingest: ingest::IngestPart,
    /// Workbench's part: the next-edit model lane's one-in-flight budget.
    /// Held as a part so route shells read it directly (DC §4.2).
    pub workbench: workbench::WorkbenchPart,
}

impl AppStateInner {
    /// **The** foreground-yield predicate: seconds left in the current yield
    /// window, or `None` when nothing is being yielded to.
    ///
    /// One decider, one name (ARCH §10.6). This arithmetic used to exist three
    /// times — `AppState::should_yield_to_foreground`,
    /// `AppState::seconds_until_foreground_idle`, and
    /// `yield_hook::AppStateYieldHook::should_yield` — and the copies had
    /// already drifted: on a backwards clock jump (`elapsed < 0`) the first
    /// said "not yielding" while the second said "a full window remains", so
    /// the `/internal/daemon/foreground_state` route could report the exact
    /// opposite of what ingest was doing. Reconciled here in favour of the
    /// conservative reading — a timestamp in the future means a foreground
    /// request landed *very* recently, so yield — and the deferral bound in
    /// `corpus-engine-yield` caps the cost of being wrong.
    ///
    /// `window == 0` disables the feature; the `0` last-active sentinel means
    /// no foreground request has ever landed, and a fresh boot must not pause.
    ///
    /// The clock-reading wrapper; [`Self::foreground_yield_remaining_secs_at`]
    /// is the decider, so the admission decision can take `now` as an argument
    /// rather than read the clock (`SERVING_BOUNDARY.md` (c)).
    pub(crate) fn foreground_yield_remaining_secs(&self) -> Option<u64> {
        self.foreground_yield_remaining_secs_at(sovereign_time::unix_now())
    }

    /// [`Self::foreground_yield_remaining_secs`] against a passed clock.
    pub(crate) fn foreground_yield_remaining_secs_at(&self, now: i64) -> Option<u64> {
        let window = self
            .node
            .yield_window_secs
            .load(std::sync::atomic::Ordering::Relaxed);
        if window == 0 {
            return None;
        }
        if self
            .node
            .foreground_inflight
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
        {
            return Some(window);
        }
        let last = self
            .node
            .foreground_last_active_ts
            .load(std::sync::atomic::Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        let elapsed = now.saturating_sub(last);
        if elapsed < 0 {
            return Some(window);
        }
        let elapsed = elapsed as u64;
        if elapsed >= window {
            None
        } else {
            Some(window - elapsed)
        }
    }

    /// Open a peer's tally row: `active += 1`, `served_total += 1`,
    /// stamp `last_request_at`. Called by the admission middleware the
    /// moment a peer request is ADMITTED — before the handler runs, so
    /// the row exists for the whole serving window.
    ///
    /// Lives on `AppStateInner` (not `AppState`) so the admission
    /// middleware's `TallyGuard`, which holds `Arc<AppStateInner>`,
    /// can open/close rows without reaching through a second Arc.
    pub fn tally_peer_request_begin(&self, node: NodeId) {
        let now = sovereign_time::unix_now();
        let mut tally = self
            .serving
            .peer_tally
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let row = tally.entry(node).or_default();
        row.active += 1;
        row.served_total += 1;
        row.last_request_at = now;
        tracing::debug!(node = %node, active = row.active, "peer_tally: request began");
    }

    /// Close a peer's tally row: `active` decrements (saturating — a
    /// poison-recovered or raced decrement must never go negative).
    /// Called when the response BODY ends (see `admission::GuardedBody`),
    /// so `active` tracks the true streaming window, not headers time.
    pub fn tally_peer_request_end(&self, node: NodeId) {
        let mut tally = self
            .serving
            .peer_tally
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(row) = tally.get_mut(&node) {
            row.active = row.active.saturating_sub(1);
            tracing::debug!(node = %node, active = row.active, "peer_tally: request ended");
        }
    }

    /// Snapshot the per-peer tally, sorted by `NodeId` for a
    /// deterministic `/status` payload. Entries are never pruned
    /// during a daemon lifetime: `active: 0` after service is exactly
    /// the "idle now, served before" reading UC-R1's negative control
    /// needs to distinguish from "never served".
    pub fn peer_tally_snapshot(&self) -> Vec<(NodeId, PeerTally)> {
        let tally = self
            .serving
            .peer_tally
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<(NodeId, PeerTally)> = tally.iter().map(|(k, v)| (*k, *v)).collect();
        out.sort_by_key(|(k, _)| *k);
        out
    }

    /// Record a present-but-malformed `X-Node-Id` header value so
    /// `/status` can name it on the zero-bucket tally row (fix 7).
    /// Call once per rejected parse, on the admission path. The raw
    /// value is capped to keep a hostile header from bloating memory
    /// or the status payload.
    pub fn record_rejected_x_node_id(&self, raw: &str) {
        let mut slot = self
            .serving
            .peer_tally_rejected
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *slot = Some(RejectedNodeIdHeader {
            raw: raw.chars().take(64).collect(),
            at_unix: sovereign_time::unix_now(),
        });
    }

    /// The most recent malformed-header record, for `/status`'s
    /// zero-bucket row. `None` when every peer header parsed.
    pub fn last_rejected_x_node_id(&self) -> Option<RejectedNodeIdHeader> {
        self.serving
            .peer_tally_rejected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The convergence recorder the daemon's sink/poller writers stamp and
    /// `/status` reads — ONE instance, set at construction (fix 9 — one
    /// decider, one name). `None` when the boot had no rails; a status poll
    /// then reads no convergence, honestly.
    pub fn convergence_recorder(&self) -> Option<std::sync::Arc<ConvergenceRecord>> {
        self.fabric.convergence.clone()
    }
}

impl AppState {
    /// Resolve a slot-name alias (`primary`, `fast`, `code`, `embed`,
    /// or any of those prefixed `commonwealth/`) to the concrete model
    /// id (GGUF stem) currently bound to that slot. Returns `None`
    /// when the input is not a registered slot alias — callers fall
    /// through to the next resolution layer.
    ///
    /// Lookup is exact-match on both the bare alias and the
    /// `commonwealth/`-namespaced form. We pre-register both forms at
    /// install time so the lookup is a single map probe regardless of
    /// which form the client sent.
    pub fn resolve_slot_alias(&self, model_name: &str) -> Option<String> {
        let map = self.inner.serving.slot_aliases.load();
        map.get(model_name).cloned()
    }
    /// Replace the slot alias table atomically. Daemon startup calls
    /// this once after `SetupConfig` is loaded; the admin reload path
    /// calls it again whenever `[models]` changes on disk so clients
    /// using `commonwealth/primary` follow the swap without restart.
    pub fn install_slot_aliases(&self, aliases: std::collections::HashMap<String, String>) {
        self.inner.serving.slot_aliases.store(Arc::new(aliases));
    }

    /// Replace the servable-model-files allowlist atomically. Daemon
    /// startup calls this once after the slot table is built; the
    /// admin reload path calls it again on `[models]` change. Each
    /// path should be absolute (no `..`/symlink trickery) — the
    /// serve handler matches only on `file_name()` and reads the
    /// canonical path, but feeding it relative inputs would still
    /// be a footgun for whoever calls it next.
    pub fn install_servable_model_files(&self, files: Vec<std::path::PathBuf>) {
        self.inner
            .serving
            .servable_model_files
            .store(Arc::new(files));
    }

    /// This node's identity pubkey, if the node has one.
    pub fn self_node_pubkey(&self) -> Option<commonwealth_core::ids::NodePubkey> {
        self.inner.fabric.self_node_pubkey
    }

    /// This node's current iroh dial info, if iroh access is on. Pulled live
    /// from the endpoint each call through the reader.
    pub fn self_iroh_dialinfo(&self) -> Option<commonwealth_core::mesh::IrohDialInfo> {
        self.inner.fabric.dial_info.current()
    }

    /// Fabric's dial-info reader — the owner's write handle. The endpoint owner
    /// (the daemon, and the reachability watchdog on an endpoint rebuild)
    /// publishes through it while the part reads. A reader created first, not a
    /// slot filled later (DC §4.2 "Construction is staged, and parts are
    /// total").
    pub fn dial_info_reader(&self) -> fabric::DialInfoReader {
        self.inner.fabric.dial_info.clone()
    }

    /// Sign this node's dial info (hex), or `None` if the node has no identity
    /// key (iroh disabled / pre-identity build).
    pub fn sign_dial_info(
        &self,
        version: u64,
        relay_url: Option<&str>,
        direct_addrs: &[std::net::SocketAddr],
    ) -> Option<String> {
        let signer = self.inner.fabric.self_dial_signer.clone()?;
        Some(signer(
            version,
            relay_url.map(|s| s.to_string()),
            direct_addrs.to_vec(),
        ))
    }

    /// The ring rail's storage, or `None` if the daemon has none.
    pub fn ring_rail(&self) -> Option<Arc<commonwealth_rail::RingRail>> {
        self.inner.fabric.ring_rail.clone()
    }

    /// The wake-up that makes a local store write travel now.
    ///
    /// Raised by the KV pump after it signs a write onto a journal, and by the
    /// work atlas after it writes a claim a peer is waiting on; awaited by the
    /// ring-sync loop beside its interval. One accessor, one `Notify` (ARCH
    /// §7.5).
    pub fn ring_write_nudge(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.inner.fabric.ring_write_nudge)
    }

    /// Install the client-API bearer token. The embedded daemon calls
    /// this at startup with the token from
    /// `commonwealth_transport::identity::load_or_create_client_token`
    /// ONLY when it binds a non-loopback address; loopback-only
    /// deployments leave it `None` (no secret generated, all local
    /// traffic admitted by [`crate::client_auth`]).
    pub fn install_client_token(&self, token: Option<Arc<str>>) {
        *self
            .inner
            .node
            .client_token
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = token;
    }

    /// Snapshot of the configured client-API bearer token (cheap RwLock
    /// read + Arc clone). `None` ⇒ no token configured. Read per request
    /// by the [`crate::client_auth`] layer.
    pub fn client_token(&self) -> Option<Arc<str>> {
        self.inner
            .node
            .client_token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Record whether the iroh acceptor routes the RPC ALPN to a local
    /// ggml rpc-server (see [`fabric::FabricPart::rpc_iroh_accept`]). Called by
    /// the daemon's iroh install; re-runnable (watchdog endpoint swaps).
    pub fn set_rpc_iroh_accept(&self, on: bool) {
        self.inner
            .fabric
            .rpc_iroh_accept
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether `/status` may honestly advertise `rpc_worker.iroh: true`.
    pub fn rpc_iroh_accept(&self) -> bool {
        self.inner
            .fabric
            .rpc_iroh_accept
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Snapshot of the active [`PeerTransport`]. Cheap (one atomic load
    /// + Arc clone); call per dial, don't cache across awaits — the
    /// watchdog may publish a new one.
    pub fn peer_transport(&self) -> Arc<dyn commonwealth_transport::PeerTransport> {
        self.inner.fabric.peer_transport.current()
    }

    /// Fabric's peer-transport reader — the owner's write handle. The bootstrap
    /// seeds it at construction and the iroh watchdog publishes through it while
    /// the part reads (DC §4.2 "Construction is staged, and parts are total").
    pub fn peer_transport_reader(&self) -> fabric::PeerTransportReader {
        self.inner.fabric.peer_transport.clone()
    }

    /// Snapshot of the active [`commonwealth_core::Clock`]. Cheap (one atomic
    /// load + Arc clone); call per timestamp, don't cache across awaits.
    pub fn clock(&self) -> Arc<dyn commonwealth_core::Clock> {
        self.inner.fabric.clock.current()
    }

    /// Fabric's clock reader — the owner's write handle. The harness publishes a
    /// per-node `TestClock` through it to drive skew; production leaves the
    /// `SystemClock` the seed supplied.
    pub fn clock_reader(&self) -> fabric::ClockReader {
        self.inner.fabric.clock.clone()
    }

    /// Record that we just observed `peer`'s liveness — its gossiped record
    /// advanced (added or LWW-updated, possibly via transitive gossip) or we
    /// reached it directly — at local time `now_secs`. The stamp is always
    /// OUR clock, so a peer's skewed `last_seen` can't drive offline-decay.
    pub fn observe_peer_contact(&self, peer: NodeId, now_secs: u64) {
        self.inner
            .fabric
            .peer_last_contact
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer, now_secs);
    }

    /// Record that this round SPENT A SLOT dialing `peer` — called for every
    /// selected peer before the dial, so it stamps refusals and timeouts too.
    ///
    /// Deliberately not folded into [`Self::observe_peer_contact`]: that one is
    /// liveness evidence and a failed dial is not evidence of life. See
    /// `peer_last_attempt` for why the two clocks are separate.
    pub fn note_peer_attempt(&self, peer: NodeId, now_secs: u64) {
        self.inner
            .fabric
            .peer_last_attempt
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer, now_secs);
    }

    /// When a round last spent a slot on `peer`, initializing to `now_secs`
    /// when we have never dialed it.
    ///
    /// The lazy init matters for FAIRNESS rather than for grace: a peer we have
    /// never tried starts level with one we just tried, so it takes its turn on
    /// staleness like everyone else instead of jumping the queue for ever.
    pub fn peer_attempt_or_init(&self, peer: NodeId, now_secs: u64) -> u64 {
        *self
            .inner
            .fabric
            .peer_last_attempt
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(peer)
            .or_insert(now_secs)
    }

    /// Local-observation time for `peer`, initializing it to `now_secs` (and
    /// returning that) when we have no record yet. The lazy init gives a
    /// freshly-seen peer a full threshold grace window before it can decay, so
    /// a peer learned at startup isn't decayed before we've had a chance to
    /// gossip with it.
    pub fn peer_contact_or_init(&self, peer: NodeId, now_secs: u64) -> u64 {
        *self
            .inner
            .fabric
            .peer_last_contact
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(peer)
            .or_insert(now_secs)
    }

    /// Record which credential generation `peer` is running, learned from a
    /// gossip payload we just merged (`MergeReport::peer_pre_split`).
    ///
    /// Call this on EVERY successful merge, not only when the answer is
    /// "pre-split": a peer that upgrades mid-session must be able to clear its
    /// own flag, or the first pre-split round it ever sent would block invite
    /// rotation for the rest of the daemon's life.
    pub fn observe_peer_split_generation(&self, peer: NodeId, post_split: bool) {
        self.inner
            .fabric
            .peer_post_split
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer, post_split);
    }

    /// Whether we have positively confirmed `peer` is post-credential-split.
    ///
    /// Unknown answers `false` — see [`fabric::FabricPart::peer_post_split`]. A
    /// caller using this to decide whether a destructive action is safe gets
    /// the conservative answer until a gossip round proves otherwise.
    pub fn peer_confirmed_post_split(&self, peer: NodeId) -> bool {
        self.peer_split_generation(peer).unwrap_or(false)
    }

    /// What we actually know about `peer`'s credential generation, WITHOUT
    /// collapsing the two ways of not knowing into one.
    ///
    /// - `Some(true)`  — it proved possession, or sent a matching secret.
    /// - `Some(false)` — we merged from it and it offered neither. A genuinely
    ///                   pre-split build.
    /// - `None`        — we have not merged from it since this daemon started.
    ///
    /// [`Self::peer_confirmed_post_split`] answers the SAFETY question and is
    /// right to fold `None` into "unsafe". This answers the DIAGNOSTIC one, and
    /// folding there produced a refusal that told the operator their fleet was
    /// un-migrated when the truth was "this daemon has been up for four
    /// seconds". Same map, two questions, one decider each (ARCH §10.6).
    pub fn peer_split_generation(&self, peer: NodeId) -> Option<bool> {
        self.inner
            .fabric
            .peer_post_split
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&peer)
            .copied()
    }

    pub fn new(self_node_id: NodeId, mesh: Mesh) -> Self {
        // Test-support constructor (callers in tests/ + the test-harness);
        // in-memory MeshStore creation is infallible — fail-fast is correct.
        #[allow(clippy::expect_used)]
        let mesh_store = Arc::new(MeshStore::in_memory().expect("in-memory MeshStore failed"));
        Self::new_with_platform(self_node_id, mesh, mesh_store, Arc::new(AppRegistry::new()))
    }

    /// Create state with explicit platform components (used by the daemon).
    pub fn new_with_platform(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
    ) -> Self {
        Self::new_with_platform_and_engine(self_node_id, mesh, mesh_store, app_registry, None)
    }

    /// Create state with an optional `CorpusEngine` attached. The
    /// engine is what the knowledge routes (`/v1/knowledge/search`
    /// and `/internal/knowledge/search`) query to turn a request
    /// into scored chunks. When `None` (default), the knowledge
    /// routes behave as if this node hosts no corpora — the path
    /// that used to yield the `is_stub: "true"` placeholder. The
    /// `sovereign-mesh::EmbeddedDaemon` passes `Some(engine)` so
    /// the in-process daemon has something real to search.
    ///
    /// No in-flight gauge: a caller with a provider names one through
    /// [`Self::new_with_platform_and_engine_and_gauge`], because the gauge
    /// must be the same handle the provider increments (`quality/DAEMON_CORE.md`
    /// §4.2 "Where an install slot breaks a cycle"). Tests and storage-only
    /// nodes take the absent form.
    pub fn new_with_platform_and_engine(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
        corpus_engine: Option<Arc<CorpusEngine>>,
    ) -> Self {
        Self::new_with_platform_and_engine_and_gauge(
            self_node_id,
            mesh,
            mesh_store,
            app_registry,
            corpus_engine,
            None,
        )
    }

    /// [`Self::new_with_platform_and_engine`] with the node's in-flight gauge.
    ///
    /// The gauge is created before the provider and passed to both — the
    /// provider's RAII guards write it, this part holds it, and gossip reads it
    /// through [`Self::current_local_in_flight`]. A node that has no provider
    /// passes `None`; the absence is what gossip publishes as "no signal",
    /// never a zeroed default (ARCH 6).
    pub fn new_with_platform_and_engine_and_gauge(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
        corpus_engine: Option<Arc<CorpusEngine>>,
        in_flight_gauge: Option<sovereign_core::in_flight::LocalInFlightGauge>,
    ) -> Self {
        Self::new_with_platform_and_engine_and_gauge_and_fabric(
            self_node_id,
            mesh,
            mesh_store,
            app_registry,
            corpus_engine,
            in_flight_gauge,
            fabric::FabricSeed::default(),
        )
    }

    /// [`Self::new_with_platform_and_engine_and_gauge`] with Fabric's part.
    ///
    /// DC §4.2 "Construction is staged, and parts are total": Fabric's values
    /// exist before the part is built, so the daemon gathers them into a
    /// [`fabric::FabricSeed`] and passes it here rather than installing them
    /// afterwards. Tests take [`fabric::FabricSeed::default`] through the
    /// shorter constructors.
    pub fn new_with_platform_and_engine_and_gauge_and_fabric(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
        corpus_engine: Option<Arc<CorpusEngine>>,
        in_flight_gauge: Option<sovereign_core::in_flight::LocalInFlightGauge>,
        fabric_seed: fabric::FabricSeed,
    ) -> Self {
        let inference_store = InferenceStateStore::new(Arc::clone(&mesh_store), self_node_id);
        let contribution_emitter = ContributionEmitter::new((*mesh_store).clone(), self_node_id);
        let activity_emitter = ActivityEmitter::new((*mesh_store).clone(), self_node_id);
        let peer_preferences = PeerPreferenceStore::new((*mesh_store).clone(), self_node_id);
        // ATOS middleware registry with the M4 core four implementations
        // registered under their TOML ids. The wiring is intentionally
        // additive — operators deploying a stock Commonwealth daemon
        // get the full stack without extra config; tests that want a
        // bare daemon can build a minimal registry themselves.
        let mut middleware_registry = crate::middleware::MiddlewareRegistry::new();
        #[cfg(feature = "atos")]
        middleware_registry.register(Arc::new(crate::middleware::ApprovalGate::new()));
        // 2026-05-22: ContextInjector + ToolInjector descriptor lists
        // were previously pulled from `sovereign_tools::manifest`, a
        // global static that forced commonwealth-api to drag the
        // tree-sitter grammar crates through every downstream binary.
        // They're now injected at construction time. AppState
        // constructs them with `Vec::new()` because the registry of
        // available tools lives in the daemon host (sovereign-cli-atos,
        // sovereign-desktop, sovereign-server) — those wire the real
        // descriptors via the `with_tool_descriptors` shim below the
        // platform constructors.
        #[cfg(feature = "atos")]
        middleware_registry.register(Arc::new(crate::middleware::ContextInjector::empty()));
        middleware_registry.register(Arc::new(crate::middleware::ToolInjector::empty()));
        #[cfg(feature = "atos")]
        middleware_registry.register(Arc::new(crate::middleware::ArtifactSurface::new()));
        #[cfg(feature = "atos")]
        middleware_registry.register(Arc::new(crate::middleware::SessionBriefing::new()));
        // Phase 7.2: per-turn DecisionExtractor mines assistant
        // responses for decision-shaped phrases on `post_process`,
        // then on the next turn either persists as
        // `source='extracted'` or drops on a user correction
        // phrase. Lives at the END of the chain so it observes
        // the response after every other middleware has had its
        // say. Stateless beyond `MiddlewareSession.pending_decision`,
        // which is already plumbed through routes_inference's
        // session round-trip.
        middleware_registry.register(Arc::new(crate::middleware::DecisionExtractor::new()));
        // `read_only_enforcer` is the red-team alias's gate. For M4
        // it shares the ApprovalGate implementation under a distinct
        // id — M5 splits them if the behavior actually diverges.
        #[cfg(feature = "atos")]
        {
            let read_only = Arc::new(crate::middleware::ApprovalGate::new());
            middleware_registry.register(read_only);
        }

        // Session store is wired up when the daemon has a MeshStore
        // in hand. The handler falls back to legacy routing when
        // this is None. ATOS-only.
        #[cfg(feature = "atos")]
        let session_store = Some(sovereign_atos::session::SessionStore::new(
            (*mesh_store).clone(),
            self_node_id,
        ));
        let fabric = fabric::FabricPart {
            identity: IdentityReader::new(self_node_id),
            mesh: RwLock::new(mesh),
            self_node_pubkey: fabric_seed.self_node_pubkey,
            dial_info: fabric_seed.dial_info,
            self_dial_signer: fabric_seed.self_dial_signer,
            ring_rail: fabric_seed.ring_rail,
            ring_write_nudge: Arc::new(tokio::sync::Notify::new()),
            peer_transport: fabric_seed.peer_transport,
            clock: fabric_seed.clock,
            peer_last_contact: std::sync::RwLock::new(std::collections::HashMap::new()),
            peer_last_attempt: std::sync::RwLock::new(std::collections::HashMap::new()),
            peer_post_split: std::sync::RwLock::new(std::collections::HashMap::new()),
            rpc_iroh_accept: std::sync::atomic::AtomicBool::new(false),
            mesh_store,
            app_registry,
            app_port_map: AppPortMap::new(),
            fanout_inflight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            on_mesh_mutation: fabric_seed.mesh_mutation_hook,
            convergence: fabric_seed.convergence,
            contribution_emitter,
        };
        Self {
            inner: Arc::new(AppStateInner {
                fabric,
                serving: serving::ServingPart {
                    inference_store,
                    model_aliases: ModelAliasTable::default_table(),
                    pipeline_aliases:
                        serving_policy::pipeline_aliases::PipelineAliasTable::default_table(),
                    slot_aliases: ArcSwap::from_pointee(std::collections::HashMap::new()),
                    servable_model_files: ArcSwap::from_pointee(Vec::new()),
                    local_inference_availability: RwLock::new(1.0_f32),
                    activity_inference_availability: RwLock::new(1.0_f32),
                    local_inference: None,
                    rpc_shard_warmer: None,
                    // usize::MAX = unlimited. The desktop overwrites this
                    // at boot with the user's persisted setting (default
                    // matched to their consent-dialog choice in W4);
                    // headless / CLI daemons leave it unlimited so they
                    // don't surprise their operators.
                    // Peer-admission fair scheduler: global ceiling = the boot
                    // value above; queue depth is unused on this shed-only gate
                    // (`try_grant` never queues). Reciprocity weights start empty
                    // (every node neutral) until the daemon's refresh loop runs.
                    peer_sched: Mutex::new(SchedCore::new(DEFAULT_PEER_INFLIGHT_CEILING, 1)),
                    // Client-admission fair scheduler. `usize::MAX` slots on
                    // purpose: no depth ceiling here (see the field docs) — the
                    // per-principal equal share is the only rule, and the
                    // inference slot queue stays the one shed decider.
                    client_sched: Mutex::new(SchedCore::new(usize::MAX, 1)),
                    client_fair_concurrency: std::sync::atomic::AtomicU32::new(
                        crate::admission::client_fair_concurrency_from_env(),
                    ),
                    client_fairness_enabled: std::sync::atomic::AtomicBool::new(
                        crate::admission::client_fairness_enabled_from_env(),
                    ),
                    peer_tally: std::sync::RwLock::new(HashMap::new()),
                    peer_tally_rejected: std::sync::Mutex::new(None),
                    reciprocity_weights: ArcSwap::from_pointee(HashMap::new()),
                    // 0 = not paused. Wall-clock unix-seconds expiry when
                    // a user-initiated pause is active.
                    contribution_paused_until: std::sync::atomic::AtomicI64::new(0),
                    // Default on: foreground-yield gates peer requests
                    // too, not just ingest. The "press send mid-chat and
                    // the GPU is pinned by a peer's enrich job" failure
                    // mode is exactly what this prevents.
                    yield_peers_to_foreground: std::sync::atomic::AtomicBool::new(true),
                    peer_preferences,
                    local_in_flight_gauge: in_flight_gauge,
                },
                node: node::NodePart {
                    client_token: std::sync::RwLock::new(None),
                    corpus_engine,
                    started_at: std::time::Instant::now(),
                    guest_grants: Arc::new(GuestGrantStore::new()),
                    // 0 sentinel = no foreground activity observed yet.
                    // The yield hook treats 0 as "never active", regardless
                    // of the window — so a fresh boot doesn't accidentally
                    // pause ingest before the first chat request.
                    foreground_last_active_ts: std::sync::atomic::AtomicI64::new(0),
                    // 0 = disabled. The daemon constructor overrides this
                    // from config (`daemon.yield_to_foreground_secs`,
                    // default 60) before AppState is shared.
                    yield_window_secs: std::sync::atomic::AtomicU64::new(0),
                    foreground_inflight: std::sync::atomic::AtomicUsize::new(0),
                    // 0 = unlimited (no clamp). The desktop overwrites
                    // this at boot with either the persisted user choice
                    // or a computed default; CLI/standalone daemons leave
                    // it at 0 so headless servers don't surprise their
                    // operators with a budget they didn't set.
                    storage_budget_bytes: std::sync::atomic::AtomicU64::new(0),
                    storage_used_bytes: std::sync::atomic::AtomicU64::new(0),
                    activity_emitter,
                },
                answering: answering::AnsweringPart {
                    middleware_registry: Arc::new(middleware_registry),
                    #[cfg(feature = "atos")]
                    session_store,
                    repo_root: std::env::current_dir().ok(),
                },
                ingest: ingest::IngestPart {
                    active_ingests: RwLock::new(HashSet::new()),
                    corpus_progress: RwLock::new(HashMap::new()),
                    newsworthy_force_tick: RwLock::new(None),
                    work_queue: Arc::new(WorkQueueManager::new()),
                    grant_store: Arc::new(EphemeralGrantStore::new()),
                    active_pull_loops: RwLock::new(HashSet::new()),
                    verify_reports: RwLock::new(HashMap::new()),
                    // false = full peer collaboration. Daemon startup
                    // overrides this from `SOVEREIGN_DISABLE_AUTO_COLLAB`
                    // when set, preserving the env-var escape hatch.
                    mesh_quiesced: std::sync::atomic::AtomicBool::new(false),
                    // 1000 ‰ = full speed; ingest pipeline pays one atomic
                    // load per batch and otherwise behaves identically to
                    // the pre-throttle build.
                    ingest_throttle_milli: std::sync::atomic::AtomicU32::new(1000),
                },
                workbench: workbench::WorkbenchPart {
                    next_edit_model_slot: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                },
            }),
        }
    }

    /// Borrow the node's in-flight publisher Arc. The hot-reload path
    /// calls this to pass the same Arc into
    /// `InferenceRouter`'s builder, so old router guards and new router
    /// guards decrement one atomic. `None` on a node that has no gauge
    /// (test harnesses, storage-only nodes), which is the same absence
    /// [`Self::current_local_in_flight`] reports.
    pub fn in_flight_publisher(&self) -> Option<Arc<std::sync::atomic::AtomicU32>> {
        self.inner
            .serving
            .local_in_flight_gauge
            .as_ref()
            .map(sovereign_core::in_flight::LocalInFlightGauge::arc)
    }

    /// Read the current local in-flight count if this node holds a gauge.
    /// `None` on nodes that never constructed a provider (storage-only, test
    /// harnesses without `InferenceRouter`). Gossip serialises this directly
    /// into `NodeCapabilities.current_in_flight`.
    pub fn current_local_in_flight(&self) -> Option<u32> {
        self.inner
            .serving
            .local_in_flight_gauge
            .as_ref()
            .map(sovereign_core::in_flight::LocalInFlightGauge::current)
    }

    /// Spawn the coordinator's pull-based work-queue reaper. Must be called
    /// once per daemon process after `new_with_platform_and_engine` so
    /// leases whose heartbeats lapse get re-queued. Tests that don't use
    /// the queue can skip this — the queue is dormant until a handoff is
    /// registered. Returns the JoinHandle so the caller can abort at
    /// shutdown, though the process normally exits before the handle
    /// would matter.
    pub fn start_work_queue_reaper(&self) -> tokio::task::JoinHandle<()> {
        Arc::clone(&self.inner.ingest.work_queue).spawn_reaper()
    }

    /// Spawn the guest-grant sweep. Call once per daemon process beside
    /// [`Self::start_work_queue_reaper`].
    ///
    /// Skipping this does not open a hole — `GuestGrantStore::live` evaluates
    /// expiry on every read, so a lapsed grant already fails closed. What it
    /// costs is unbounded growth of the grant map over a long-lived daemon.
    pub fn start_guest_grant_reaper(&self) -> tokio::task::JoinHandle<()> {
        Arc::clone(&self.inner.node.guest_grants).spawn_reaper()
    }

    /// This node's NodeId, by value. Cheap (atomic load + Arc deref).
    /// A convenience over [`Self::identity_reader`]; `join_mesh` swaps
    /// the id when adopting a founder-assigned one, and this always
    /// reads the current value rather than a copied placeholder.
    pub fn self_node_id(&self) -> NodeId {
        self.inner.fabric.identity.current()
    }

    /// Fabric's identity, published as a watch (`DC §4.2` "Identity is a
    /// reader, not a value"). A consumer that must observe a `join_mesh`
    /// adoption holds this handle rather than reading the id once; the daemon
    /// publishes through it.
    pub fn identity_reader(&self) -> IdentityReader {
        self.inner.fabric.identity.clone()
    }

    /// Install the in-process inference service. Same Arc-get_mut
    /// Whether the `with_*` installers below can still take effect.
    ///
    /// They mutate through `Arc::get_mut`, which refuses when ANY other
    /// `Arc` or `Weak` to the inner state exists — and on refusal they log
    /// and carry on, so the daemon boots with no local inference and every
    /// chat turn 503s. The caller asks this ONCE before the block and refuses
    /// to boot on `Err`, turning a silent outage into a sentence (§18.3).
    pub fn installers_can_run(&self) -> Result<(), String> {
        let strong = Arc::strong_count(&self.inner);
        let weak = Arc::weak_count(&self.inner);
        if strong == 1 && weak == 0 {
            Ok(())
        } else {
            Err(format!(
                "AppState is already shared (strong={strong}, weak={weak}) before its \
                 with_* installers ran; they would silently no-op and the daemon would \
                 boot without local inference. Move whatever cloned or downgraded \
                 `app_state.inner` below the Arc::get_mut-sensitive block in \
                 EmbeddedDaemon::start_daemon"
            ))
        }
    }

    /// contract as `with_rpc_shard_warmer` — call before cloning
    /// AppState into the HTTP servers.
    pub fn with_local_inference(
        mut self,
        service: std::sync::Arc<dyn LocalInferenceService>,
    ) -> Self {
        match Arc::get_mut(&mut self.inner) {
            Some(inner) => {
                inner.serving.local_inference = Some(service);
            }
            None => {
                tracing::error!(
                    strong_count = Arc::strong_count(&self.inner),
                    "with_local_inference called on shared AppState — \
                     local inference NOT installed and /v1/chat/completions \
                     will 503 every request with model_not_ready. \
                     Likely cause: another caller cloned AppState.inner \
                     (e.g. AppStateYieldHook::new) before this point. \
                     Move the with_* installer above any inner.clone() in \
                     EmbeddedDaemon::start_daemon."
                );
            }
        }
        self
    }

    /// Install the worker-side RPC shard warmer ([`RpcShardWarmer`]) — the
    /// `POST /internal/rpc-warm` backend. Same contract as `with_local_inference`:
    /// call before cloning AppState into the HTTP servers (uses `Arc::get_mut`).
    pub fn with_rpc_shard_warmer(mut self, warmer: std::sync::Arc<dyn RpcShardWarmer>) -> Self {
        match Arc::get_mut(&mut self.inner) {
            Some(inner) => {
                inner.serving.rpc_shard_warmer = Some(warmer);
            }
            None => {
                tracing::error!(
                    strong_count = Arc::strong_count(&self.inner),
                    "with_rpc_shard_warmer called on shared AppState — auto-warm \
                     orchestration disabled; a distributed primary load will fall \
                     back to local-only. Move the installer above any inner.clone()."
                );
            }
        }
        self
    }

    /// Register a model as available on the mesh.
    pub fn register_model(&self, model: commonwealth_core::model::ModelInfo) {
        self.inner.serving.inference_store.set_model_info(&model);
    }

    /// Set the address of a llama-server for a model (after orchestrator spawns it).
    pub fn set_llama_server_address(
        &self,
        model_id: commonwealth_core::ids::ModelId,
        address: String,
    ) {
        self.inner
            .serving
            .inference_store
            .set_llama_address(model_id, &address);
    }

    /// Get the llama-server address for a model.
    pub fn get_llama_server_address(
        &self,
        model_id: commonwealth_core::ids::ModelId,
    ) -> Option<String> {
        self.inner
            .serving
            .inference_store
            .get_llama_address(model_id)
    }

    /// Get the default model (first in the inference plan).
    pub fn default_model_id(&self) -> Option<commonwealth_core::ids::ModelId> {
        self.inner
            .serving
            .inference_store
            .get_plan()
            .and_then(|p| p.model_plans.first().map(|mp| mp.model))
    }

    /// Update the ACTIVITY input to this node's inference availability.
    /// Called by sovereign-server's ActivityReporter after a level
    /// transition; gossip picks up the recomputed published value on its
    /// next 10-second round.
    ///
    /// Records the reported level and then defers to
    /// [`Self::recompute_local_availability`], the single writer of the
    /// published field. It does NOT write the published value directly: a
    /// live yield window is also a ceiling, and an "idle" report must not be
    /// able to advertise 1.0 while this node is refusing peer requests.
    pub async fn update_local_availability(&self, availability: f32) {
        *self
            .inner
            .serving
            .activity_inference_availability
            .write()
            .await = availability;
        let published = self.recompute_local_availability().await;
        tracing::debug!(
            activity_availability = availability,
            published,
            "inference_availability: activity input updated by sovereign-server"
        );
    }

    /// The yield half of the availability composite: what this node can
    /// serve a PEER right now, given the yield-to-local-user policy.
    ///
    /// `0.0` while [`Self::admit_peer_request`] would refuse with
    /// `AdmissionReason::YieldedToLocal`, `1.0` otherwise (no constraint
    /// from this input). Derived from exactly the same two reads that
    /// `admit_peer_request` makes — `yield_peers_to_foreground()` and
    /// `seconds_until_foreground_idle()` — so the number this node
    /// ADVERTISES and the decision it ENFORCES cannot disagree. There is no
    /// separate remembered "am I yielding" flag to fall out of sync, and the
    /// window's expiry needs no timer: the predicate is a pure function of
    /// the last-active timestamp and the window width.
    pub fn yield_availability_floor(&self) -> f32 {
        if self.yield_peers_to_foreground() && self.seconds_until_foreground_idle().is_some() {
            0.0
        } else {
            1.0
        }
    }

    /// THE writer of `local_inference_availability`. Recomputes the
    /// published value from both inputs and returns it.
    ///
    /// Called by [`Self::update_local_availability`] when the activity input
    /// moves, and by the mesh gossip round immediately before it reads the
    /// field to build this node's capabilities — the yield input is
    /// time-derived, so it has no transition event of its own to hook and is
    /// instead re-derived at the moment of publication. Logs at `info` only
    /// when the published value actually CHANGES (the transition), at
    /// `debug` on every other call, so the 10-second heartbeat does not
    /// drown the signal.
    pub async fn recompute_local_availability(&self) -> f32 {
        let activity = *self
            .inner
            .serving
            .activity_inference_availability
            .read()
            .await;
        let yield_floor = self.yield_availability_floor();
        let published = activity.min(yield_floor);
        let mut slot = self
            .inner
            .serving
            .local_inference_availability
            .write()
            .await;
        let previous = *slot;
        *slot = published;
        drop(slot);
        if (previous - published).abs() > f32::EPSILON {
            tracing::info!(
                previous,
                published,
                activity,
                yield_floor,
                yielding = yield_floor == 0.0,
                "inference_availability TRANSITION — this is what gossip now advertises"
            );
        } else {
            tracing::debug!(
                published,
                activity,
                yield_floor,
                "inference_availability recomputed (unchanged)"
            );
        }
        published
    }

    /// Read the published inference availability without recomputing.
    /// The introspection routes and tests use this; gossip recomputes first.
    pub async fn local_availability_published(&self) -> f32 {
        *self.inner.serving.local_inference_availability.read().await
    }

    /// Record that a foreground inference request just landed. Called
    /// from the `chat_completions` handler before slot dispatch so any
    /// background ingest workers polling `should_yield_to_foreground`
    /// will see a fresh timestamp on their next checkpoint. Cheap
    /// (atomic store, Relaxed ordering — readers don't need
    /// happens-before, just monotonic-enough-for-comparison).
    pub fn bump_foreground_active(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.inner
            .node
            .foreground_last_active_ts
            .store(now, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read whether ingest workers should currently pause for
    /// foreground inference. `true` iff the yield window is positive
    /// AND a foreground request landed within `window` seconds. The
    /// `0` last-active sentinel always returns `false` (a fresh boot
    /// shouldn't pause before the first user request).
    pub fn should_yield_to_foreground(&self) -> bool {
        self.inner.foreground_yield_remaining_secs().is_some()
    }

    /// A turn started (see `foreground_inflight`).
    pub fn foreground_begin(&self) {
        self.bump_foreground_active();
        self.inner
            .node
            .foreground_inflight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// A turn ended; the yield window counts from here.
    pub fn foreground_end(&self) {
        let _ = self.inner.node.foreground_inflight.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |n| Some(n.saturating_sub(1)),
        );
        self.bump_foreground_active();
    }

    pub fn foreground_inflight(&self) -> usize {
        self.inner
            .node
            .foreground_inflight
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Seconds remaining in the current yield window, when one is
    /// active. Returns `None` when not currently yielding (window=0,
    /// never-active sentinel, or window already expired). Useful for
    /// progress messages and the `/internal/daemon/foreground_state`
    /// introspection route.
    pub fn seconds_until_foreground_idle(&self) -> Option<u64> {
        self.inner.foreground_yield_remaining_secs()
    }

    /// [`Self::seconds_until_foreground_idle`] against a passed clock, so the
    /// admission decision reads no clock of its own.
    pub fn seconds_until_foreground_idle_at(&self, now: i64) -> Option<u64> {
        self.inner.foreground_yield_remaining_secs_at(now)
    }

    /// Replace the yield window at runtime. The daemon constructor
    /// calls this once with the configured value; the desktop's
    /// Settings tab calls it on user toggle. Setting `0` disables
    /// the feature entirely.
    pub fn set_yield_window_secs(&self, secs: u64) {
        self.inner
            .node
            .yield_window_secs
            .store(secs, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the configured yield window (seconds). `0` means disabled.
    pub fn yield_window_secs(&self) -> u64 {
        self.inner
            .node
            .yield_window_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Read the last-foreground-active unix timestamp. `0` when the
    /// daemon has not yet served a chat request. Surfaced via
    /// `/internal/daemon/foreground_state` so operators can confirm
    /// the feature is actually wired during contention triage.
    pub fn foreground_last_active_ts(&self) -> i64 {
        self.inner
            .node
            .foreground_last_active_ts
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Read the mesh-quiesce flag. `true` means the auto-collaborate
    /// loop is suppressed: this node will not pull peer-assigned work
    /// and will not dispatch to peers on this tick.
    pub fn mesh_quiesced(&self) -> bool {
        self.inner
            .ingest
            .mesh_quiesced
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Flip the mesh-quiesce flag at runtime. Set to `true` when the
    /// operator wants to stop participating in shared ingests
    /// (foreground inference contention, focused work session).
    /// Reset to `false` to rejoin the auto-collaborate loop.
    pub fn set_mesh_quiesced(&self, quiesced: bool) {
        self.inner
            .ingest
            .mesh_quiesced
            .store(quiesced, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the maximum concurrent peer-served inference requests this
    /// node will admit. `usize::MAX` disables the cap. `0` rejects all
    /// peer work. Settings UI / `/internal/contribution/ceiling`.
    pub fn set_contribution_max_peer_inflight(&self, max: usize) {
        self.lock_peer_sched().set_slots(max);
    }

    /// Read the configured peer-inflight ceiling (the global slot budget).
    pub fn contribution_max_peer_inflight(&self) -> usize {
        self.lock_peer_sched().slots()
    }

    /// Read the current in-flight peer request count.
    pub fn peer_inflight_count(&self) -> usize {
        self.lock_peer_sched().in_flight()
    }

    /// Read the current count of **outbound** peer fan-out requests in flight
    /// (the `fanout_inflight` gauge). Drives the `BoundedFanOut` glassbox check.
    pub fn fanout_inflight_count(&self) -> usize {
        self.inner
            .fabric
            .fanout_inflight
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lock the client fairness scheduler, recovering from poison rather than
    /// cascading a panic into every future admission (same rule as
    /// [`Self::lock_peer_sched`]).
    pub(crate) fn lock_client_sched(&self) -> std::sync::MutexGuard<'_, SchedCore<Principal>> {
        self.inner
            .serving
            .client_sched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// In-flight client turns currently attributed to `key`.
    pub fn client_inflight_of(&self, key: &Principal) -> u32 {
        self.lock_client_sched().inflight_of(key)
    }

    /// Total client turns in flight across every principal.
    pub fn client_inflight_count(&self) -> usize {
        self.lock_client_sched().in_flight()
    }

    /// The concurrency budget shared out by
    /// [`serving_policy::fair_sched::fair_share_cap`].
    pub fn client_fair_concurrency(&self) -> u32 {
        self.inner
            .serving
            .client_fair_concurrency
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Override the budget (tests, and any future settings surface).
    pub fn set_client_fair_concurrency(&self, budget: u32) {
        self.inner
            .serving
            .client_fair_concurrency
            .store(budget, std::sync::atomic::Ordering::Relaxed);
    }

    /// Is the client fairness gate enforcing (as opposed to observing)?
    pub fn client_fairness_enabled(&self) -> bool {
        self.inner
            .serving
            .client_fairness_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Flip the gate between enforcing and observe-only.
    pub fn set_client_fairness_enabled(&self, enabled: bool) {
        self.inner
            .serving
            .client_fairness_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Lock the peer scheduler, recovering from poison rather than cascading a
    /// panic into every future admission.
    fn lock_peer_sched(&self) -> std::sync::MutexGuard<'_, SchedCore<NodeId>> {
        self.inner
            .serving
            .peer_sched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Reciprocity weight for a peer node (`1.0` = neutral / unknown). A
    /// lock-free `ArcSwap` read — safe on the admission hot path.
    fn peer_reciprocity_weight(&self, node: &NodeId) -> f64 {
        self.inner
            .serving
            .reciprocity_weights
            .load()
            .get(node)
            .copied()
            .unwrap_or(1.0)
    }

    /// Recompute the cached per-node reciprocity weights from the contribution
    /// ledger. Called off the hot path (a daemon loop, ~30 s cadence); never
    /// per request. `k` is the reciprocity gain (`0` disables it). On error
    /// the previous weights are kept — a transient ledger hiccup must not flap
    /// everyone to neutral mid-contention.
    pub async fn refresh_reciprocity_weights(&self, k: f64) {
        let caps: HashMap<NodeId, commonwealth_core::capabilities::NodeCapabilities> = {
            let view = self.inner.fabric.mesh.read().await;
            view.members
                .iter()
                .map(|(id, m)| (*id, m.capabilities.clone()))
                .collect()
        };
        let contributions = match commonwealth_state::current_contributions(
            &self.inner.fabric.mesh_store,
            &caps,
            commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
        ) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(error = %e, "reciprocity: aggregate failed; keeping last weights");
                return;
            }
        };
        let max = contributions
            .values()
            .map(|c| c.inference_served.wall_seconds)
            .fold(0.0_f64, f64::max);
        let weights: HashMap<NodeId, f64> = contributions
            .into_iter()
            .filter_map(|(id, c)| {
                let w = reciprocity_weight(c.inference_served.wall_seconds, max, k);
                (w > 1.0).then_some((id, w))
            })
            .collect();
        let n = weights.len();
        self.inner
            .serving
            .reciprocity_weights
            .store(Arc::new(weights));
        tracing::debug!(contributors = n, "reciprocity: peer weights refreshed");
    }

    /// Set a runtime contribution pause that expires at the given
    /// unix-seconds timestamp. `0` clears any active pause. Caller
    /// (the Settings UI / `/internal/contribution/pause`) is
    /// responsible for computing the expiry — the admission layer
    /// just compares against `now()` on each peer request.
    pub fn set_contribution_paused_until(&self, expiry_unix: i64) {
        self.inner
            .serving
            .contribution_paused_until
            .store(expiry_unix, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the contribution-pause expiry (unix seconds). `0` means
    /// not paused.
    pub fn contribution_paused_until(&self) -> i64 {
        self.inner
            .serving
            .contribution_paused_until
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Seconds until the active pause expires. `Some(0)` is never
    /// returned — `None` means "not currently paused."
    ///
    /// The clock-reading wrapper; [`Self::seconds_until_unpaused_at`] is the
    /// decider.
    pub fn seconds_until_unpaused(&self) -> Option<u64> {
        self.seconds_until_unpaused_at(sovereign_time::unix_now())
    }

    /// [`Self::seconds_until_unpaused`] against a passed clock.
    pub fn seconds_until_unpaused_at(&self, now: i64) -> Option<u64> {
        let expiry = self.contribution_paused_until();
        if expiry == 0 {
            return None;
        }
        let remaining = expiry.saturating_sub(now);
        if remaining <= 0 {
            None
        } else {
            Some(remaining as u64)
        }
    }

    /// When `true`, peer-served requests respect the foreground-yield
    /// window. Default `true` — this is the load-bearing setting for
    /// "the user pressed send and the GPU isn't pinned by peer work."
    pub fn yield_peers_to_foreground(&self) -> bool {
        self.inner
            .serving
            .yield_peers_to_foreground
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Toggle whether peer-served requests respect the foreground-
    /// yield window.
    pub fn set_yield_peers_to_foreground(&self, on: bool) {
        self.inner
            .serving
            .yield_peers_to_foreground
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Try to admit a peer-served request from `node`. Returns a
    /// `PeerInflightGuard` (RAII: `release`s the node's slot on drop), or an
    /// `AdmissionRejection` (mapped to 503 by the middleware) when the request
    /// shouldn't be served right now.
    ///
    /// Order matters: pause checked first (the most explicit "no"), then yield
    /// (the user is actively using their machine), then the fair scheduler — a
    /// per-node cap (anti-hog) scaled by the node's reciprocity weight, under
    /// the global ceiling. The scheduler is shed-only here (the peer load
    /// balancer routes elsewhere on refusal), so a refusal is immediate.
    /// `retry_after_secs` hints how long to wait before retrying.
    pub fn admit_peer_request(
        &self,
        node: NodeId,
    ) -> Result<crate::admission::PeerInflightGuard, crate::admission::AdmissionRejection> {
        self.admit_peer_request_at(node, sovereign_time::unix_now())
    }

    /// [`Self::admit_peer_request`] against a passed clock — the decider the
    /// published `Admission::admit` calls, so the decision reads no clock of
    /// its own (`SERVING_BOUNDARY.md` (c)).
    pub fn admit_peer_request_at(
        &self,
        node: NodeId,
        now_unix_secs: i64,
    ) -> Result<crate::admission::PeerInflightGuard, crate::admission::AdmissionRejection> {
        use crate::admission::{AdmissionReason, AdmissionRejection, PeerInflightGuard};

        if let Some(remaining) = self.seconds_until_unpaused_at(now_unix_secs) {
            return Err(AdmissionRejection::new(
                "contribution paused",
                AdmissionReason::Paused,
                remaining.max(1),
            ));
        }
        if self.yield_peers_to_foreground() {
            if let Some(remaining) = self.seconds_until_foreground_idle_at(now_unix_secs) {
                return Err(AdmissionRejection::new(
                    "local user active",
                    AdmissionReason::YieldedToLocal,
                    remaining.max(1),
                ));
            }
        }

        // Fair admission: a per-node cap (reciprocity-scaled) under the global
        // ceiling, enforced by the shared `SchedCore`. `node` is `Copy`, so we
        // reuse it for the guard after the (consuming) `try_grant`.
        let weight = self.peer_reciprocity_weight(&node);
        let mut sched = self.lock_peer_sched();
        let cap = effective_peer_cap(sched.slots(), weight);
        match sched.try_grant(node, weight, cap) {
            TryGrant::Granted => {
                drop(sched);
                Ok(PeerInflightGuard::new(
                    std::sync::Arc::clone(&self.inner),
                    node,
                ))
            }
            // Both outcomes mean "at capacity now" on this shed-only gate.
            // Jittered, not constant: a fixed hint tells every shed
            // client to return in the same instant, which re-creates the
            // spike that caused the shed. See
            // `admission::jittered_retry_after_secs`.
            TryGrant::WouldQueue { .. } | TryGrant::Shed { .. } => Err(AdmissionRejection::new(
                "peer concurrency ceiling reached",
                AdmissionReason::CeilingExceeded,
                crate::admission::jittered_retry_after_secs(2),
            )),
        }
    }

    /// Read the per-batch ingest throttle factor as a normalised
    /// `f32` in `(0.0, 1.0]`. `1.0` = full speed (no post-batch
    /// sleep). Clamped on read so callers can use the value
    /// directly as a sleep multiplier.
    pub fn ingest_throttle_factor(&self) -> f32 {
        let raw = self
            .inner
            .ingest
            .ingest_throttle_milli
            .load(std::sync::atomic::Ordering::Relaxed);
        ((raw.max(1) as f32) / 1000.0).clamp(0.001, 1.0)
    }

    /// Set the throttle factor. Caller passes a value in `(0.0, 1.0]`;
    /// `0.0` is rejected (use the pause route to fully stop) and
    /// values >`1.0` are clamped. Returns the value actually stored.
    pub fn set_ingest_throttle_factor(&self, factor: f32) -> Result<f32, String> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(
                "throttle_factor must be > 0; use /internal/corpus/pause to fully stop".into(),
            );
        }
        let clamped = factor.min(1.0);
        let milli = (clamped * 1000.0).round().clamp(1.0, 1000.0) as u32;
        self.inner
            .ingest
            .ingest_throttle_milli
            .store(milli, std::sync::atomic::Ordering::Relaxed);
        Ok(milli as f32 / 1000.0)
    }

    /// Read the configured storage budget in bytes. Returns `None`
    /// when no budget is set (the underlying atomic is `0`), in which
    /// case the gossiped `free_storage_gb` is whatever the disk
    /// reports — no budget clamp.
    pub fn storage_budget_bytes(&self) -> Option<u64> {
        let raw = self
            .inner
            .node
            .storage_budget_bytes
            .load(std::sync::atomic::Ordering::Relaxed);
        (raw > 0).then_some(raw)
    }

    /// Set the storage budget. `None` (or `Some(0)`) clears the
    /// budget — the publish path falls back to raw free disk. Values
    /// below 1 GiB are rejected: anything tighter than that and the
    /// scheduler will essentially refuse work the moment the engine
    /// metadata directory grows past the threshold.
    pub fn set_storage_budget_bytes(&self, budget: Option<u64>) -> Result<(), String> {
        const MIN_BUDGET: u64 = 1_073_741_824; // 1 GiB.
        let raw = match budget {
            None | Some(0) => 0,
            Some(n) if n < MIN_BUDGET => {
                return Err(format!(
                    "storage budget must be either unset or ≥ 1 GiB ({MIN_BUDGET} bytes); got {n}"
                ))
            }
            Some(n) => n,
        };
        self.inner
            .node
            .storage_budget_bytes
            .store(raw, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Most recent observation of disk consumed by installed corpora.
    /// Updated by the gossip-tick capabilities builder.
    pub fn storage_used_bytes(&self) -> u64 {
        self.inner
            .node
            .storage_used_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Update the cached storage usage. Called once per gossip tick
    /// from the capabilities builder; the value is what
    /// `GET /internal/storage/budget` reports back to the desktop.
    pub fn set_storage_used_bytes(&self, used: u64) {
        self.inner
            .node
            .storage_used_bytes
            .store(used, std::sync::atomic::Ordering::Relaxed);
    }

    /// Bytes the budget allows above current usage. `None` when no
    /// budget is set (no clamping). Saturates at zero when usage has
    /// already exceeded the budget — the capabilities builder turns
    /// that into a published `free_storage_gb` of 0, which makes the
    /// schedulers stop assigning new shards here.
    pub fn storage_remaining_bytes(&self) -> Option<u64> {
        let budget = self.storage_budget_bytes()?;
        let used = self.storage_used_bytes();
        Some(budget.saturating_sub(used))
    }
}

/// The node's answer to Fabric's `SelfClaims` port (`quality/DAEMON_CORE.md`
/// §4.2 "Gossip asks the node what to claim"). Fabric publishes these claims
/// and does not know who computed them; the daemon computes them from Serving's
/// availability composite, the inference store, the in-flight gauge and the
/// node's storage budget.
///
/// The port lives in `sovereign-contracts` and reaches this crate through
/// `sovereign_core::self_claims` on the `identity` precedent, so implementing
/// it costs no new edge (`ralph/DECISIONS.md` 2026-09-16).
#[async_trait]
impl sovereign_core::self_claims::SelfClaims for AppState {
    async fn claims(&self) -> sovereign_core::self_claims::LocalClaims {
        // Recompute availability from BOTH its inputs at the moment of
        // publication. The yield-to-local-user half is time-derived and has no
        // transition event to hook, so a node refusing every peer request would
        // otherwise advertise a stale `1.0` (note 3234d770). This is the
        // recompute `gossip::run_one_round` used to call one line before it
        // built the capabilities; it now rides the port so Fabric stops
        // reaching into Serving for its own advertisement.
        let availability = self.recompute_local_availability().await;
        sovereign_core::self_claims::LocalClaims {
            availability,
            in_flight: self.current_local_in_flight(),
            storage_remaining: self.storage_remaining_bytes(),
            embed_model: self.inner.serving.inference_store.get_local_embed_model(),
        }
    }

    fn record_storage_used(&self, used: u64) {
        self.set_storage_used_bytes(used);
    }
}

#[cfg(test)]
pub fn test_app_state() -> AppState {
    use commonwealth_core::ids::MeshId;
    use commonwealth_core::mesh::Mesh;
    use std::collections::HashMap;
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "Test Mesh".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    };
    AppState::new(NodeId::from_u128(1), mesh)
}

/// [`test_app_state`] with Fabric's construction seed — the shape a test that
/// needs a recorder, rail, hook or clock uses now that those are constructor
/// arguments rather than installs (DC §4.2 "Construction is staged, and parts
/// are total").
pub fn test_app_state_with_seed(seed: fabric::FabricSeed) -> AppState {
    use commonwealth_core::ids::MeshId;
    use commonwealth_core::mesh::Mesh;
    use std::collections::HashMap;
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "Test Mesh".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    };
    AppState::new_with_platform_and_engine_and_gauge_and_fabric(
        NodeId::from_u128(1),
        mesh,
        Arc::new(MeshStore::in_memory().expect("in-memory MeshStore")),
        Arc::new(AppRegistry::new()),
        None,
        None,
        seed,
    )
}

#[cfg(test)]
mod installer_guard_tests {
    use super::test_app_state;
    use std::sync::Arc;

    /// A fresh state can run its installers; a state anyone has cloned OR
    /// downgraded cannot, and says so. The `Weak` case is the one that
    /// took local inference down on 2026-09-08 — a Weak reads as harmless
    /// and `Arc::get_mut` disagrees.
    #[test]
    fn installers_refuse_a_state_that_is_already_shared_even_weakly() {
        let state = test_app_state();
        assert!(state.installers_can_run().is_ok());
        let weak = Arc::downgrade(&state.inner);
        let err = state.installers_can_run().unwrap_err();
        assert!(err.contains("weak=1"), "{err}");
        drop(weak);
        assert!(
            state.installers_can_run().is_ok(),
            "dropping the Weak restores it"
        );
        let strong = state.clone();
        let err = state.installers_can_run().unwrap_err();
        assert!(err.contains("strong=2"), "{err}");
        drop(strong);
    }
}

#[cfg(test)]
mod fair_admission_tests {
    use super::effective_peer_cap;
    use crate::state::{fabric, test_app_state, test_app_state_with_seed};

    #[test]
    fn unlimited_ceiling_means_no_per_node_cap() {
        // Not rationing → share freely (preserves the pre-existing default:
        // the only bound is the global ceiling, which is unbounded here).
        assert_eq!(effective_peer_cap(usize::MAX, 1.0), u32::MAX);
        assert_eq!(effective_peer_cap(usize::MAX, 1.5), u32::MAX);
    }

    #[test]
    fn rationing_caps_a_pure_consumer_at_base() {
        // ceiling 4, neutral weight → base cap of 1 (anti-hog: one consumer
        // can't grab all four slots).
        assert_eq!(effective_peer_cap(4, 1.0), 1);
    }

    #[test]
    fn rationing_lifts_a_top_contributor_to_the_ceiling() {
        // weight 1.0 + k (= 1.5 at k=0.5) → may hold the whole ceiling.
        assert_eq!(effective_peer_cap(4, 1.5), 4);
        // A mid contributor lands between base and ceiling.
        let mid = effective_peer_cap(4, 1.25);
        assert!((2..=3).contains(&mid), "mid contributor: {mid}");
    }

    #[test]
    fn zero_ceiling_caps_at_base_slots_do_the_rejecting() {
        // The cap clamps to ≥ base even at ceiling 0; it's the 0-slot budget
        // in `SchedCore` that actually rejects, not this cap.
        assert_eq!(effective_peer_cap(0, 1.5), 1);
    }

    #[test]
    fn rejected_header_record_round_trips_and_caps_raw() {
        // Fix 7: the record is None until a malformed header arrives, then
        // holds the raw value (capped) + a timestamp, and the newest write
        // replaces the old.
        let state = test_app_state();
        assert!(state.inner.last_rejected_x_node_id().is_none());

        state.inner.record_rejected_x_node_id("short");
        let rec = state.inner.last_rejected_x_node_id().unwrap();
        assert_eq!(rec.raw, "short");
        assert!(rec.at_unix > 0);

        state
            .inner
            .record_rejected_x_node_id("averylongmalformedheader".repeat(10).as_str());
        let rec = state.inner.last_rejected_x_node_id().unwrap();
        assert_eq!(rec.raw.chars().count(), 64, "raw value must be capped");
        assert!(rec.raw.starts_with("averylongmalformedheader"));
    }

    #[test]
    fn convergence_record_is_the_one_the_node_was_constructed_with() {
        // Fix 9: the SAME Arc the daemon stamps is the one /status reads
        // (ptr_eq). The recorder is a construction argument now, so there is
        // no install step that could discard the sink's early writes.
        let rec = std::sync::Arc::new(crate::state::ConvergenceRecord::new());
        let state = test_app_state_with_seed(fabric::FabricSeed {
            convergence: Some(std::sync::Arc::clone(&rec)),
            ..Default::default()
        });
        assert!(std::sync::Arc::ptr_eq(
            &state.inner.convergence_recorder().unwrap(),
            &rec
        ));

        // Stamps are absent until written (absence is reported, never
        // defaulted — §18.3)…
        assert_eq!(rec.snapshot(), (None, None));

        // …then round-trip once written, visible through /status's read
        // path on the SAME instance.
        rec.record_outbound_publish_success(1000);
        rec.record_inbound_ingest_success(2000);
        assert_eq!(
            state.inner.convergence_recorder().unwrap().snapshot(),
            (Some(1000), Some(2000))
        );
    }
}
