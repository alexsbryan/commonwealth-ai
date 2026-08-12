// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use axum::extract::State;
use axum::Json;
use commonwealth_core::ids::NodeId;
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
            m.is_active()
                && (m.status == commonwealth_core::mesh::NodeStatus::Online
                    || m.status == commonwealth_core::mesh::NodeStatus::Busy)
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

    // Ground-truth residency from the embedded engine (the `ollama ps`
    // analog). `None` on the orchestrator daemon — which keeps reporting
    // residency via the `llama_addr:` store keys below, so this only
    // ADDS truth for the embedded/desktop path, never removes it.
    let resident: Vec<crate::state::ResidentSlot> = match &state.inner.local_inference {
        Some(svc) => svc.resident_slots(),
        None => Vec::new(),
    };

    // Supervised compute children (P1) — the glassbox source for
    // "distributed across N children / warming / recovering". Empty unless
    // `[compute]` pools are configured.
    let compute_children: Vec<crate::state::ComputeChildStatus> = match &state.inner.local_inference
    {
        Some(svc) => svc.compute_children(),
        None => Vec::new(),
    };

    // Read once, published under two keys (`edit` and the deprecated
    // `fim` mirror) so the two can never report different arrangements.
    let edit_slot_status: Option<crate::state::EditSlotStatus> = match &state.inner.local_inference
    {
        Some(svc) => svc.edit_status(),
        None => None,
    };

    let loaded_models: Vec<LoadedModelStatus> = plan
        .model_plans
        .iter()
        .map(|p| {
            let model = format!("{}", p.model);
            // OR-fix: the orchestrator `llama_addr:` key is never written
            // on the embedded path, so fall back to real engine residency.
            // The engine reports GGUF stems, not ModelIds, so the join key
            // is the registered model NAME (`ModelInfo.name`) — comparing
            // against the ModelId's `model-<hex>` Display can never match.
            let name = state
                .inner
                .inference_store
                .get_model_info(p.model)
                .map(|m| m.name);
            LoadedModelStatus {
                loaded: state
                    .inner
                    .inference_store
                    .get_llama_address(p.model)
                    .is_some()
                    || resident
                        .iter()
                        .any(|r| r.resident && Some(&r.model_id) == name.as_ref()),
                nodes: p.assignments.len(),
                tps: p.estimated_tokens_per_sec,
                model,
            }
        })
        .collect();

    // Real hosted-corpora inventory (was hardcoded empty until
    // 2026-06-10). Same `installed_indexes()` read the gossip tick and
    // the knowledge routes already perform — metadata-level, not a
    // corpus scan. Code indexes are excluded: they serve symbol lookup,
    // not prose retrieval (see `CorpusKind::Code`); every other kind
    // (Knowledge, Catalog, future additions) counts as searchable.
    let (hosted_corpora, total_chunks_searchable) = match &state.inner.corpus_engine {
        Some(engine) => {
            let infos = engine.installed_indexes().await.unwrap_or_default();
            let mut ids: Vec<String> = Vec::new();
            let mut chunks: u64 = 0;
            for info in infos {
                if matches!(info.kind, corpus_engine::CorpusKind::Code) {
                    continue;
                }
                chunks += info.chunk_count;
                ids.push(info.corpus_id);
            }
            ids.sort();
            (ids, chunks)
        }
        None => (Vec::new(), 0),
    };

    Json(StatusResponse {
        node_id: format!(
            "{}",
            state.inner.self_node_id_swap.load_full().as_ref().clone()
        ),
        mesh: MeshStatus {
            name: mesh.name.clone(),
            members_online,
            members_total: mesh.members.values().filter(|m| m.is_active()).count(),
            pooled_vram_gb,
            pooled_storage_gb,
        },
        inference: InferenceStatus {
            loaded_models,
            resident,
            compute_children,
            edit: edit_slot_status.clone(),
            // Deprecated mirror — see the field doc. Same value, so
            // the two keys can never disagree.
            fim: edit_slot_status,
            peer_requests: {
                // Join the tally's opaque NodeIds against the mesh
                // roster so the answer is "BeefyMac is being served",
                // not "node-6c955b5f1361… is being served". A node
                // that left the roster (or was never in it) still
                // shows, with `name` omitted.
                let names: HashMap<_, _> = mesh
                    .members
                    .iter()
                    .map(|(id, m)| (*id, m.name.clone()))
                    .collect();
                let rejected = state.inner.last_rejected_x_node_id();
                state
                    .inner
                    .peer_tally_snapshot()
                    .into_iter()
                    .map(|(node, t)| PeerRequestStatus {
                        node_id: format!("{node}"),
                        name: names.get(&node).cloned(),
                        active: t.active,
                        served_total: t.served_total,
                        last_request_at: t.last_request_at,
                        // The zero bucket is the malformed-header bucket
                        // (admission.rs buckets parse failures there, fix 7):
                        // name the rejected value and the expected wire form
                        // instead of leaving an opaque zero row. Only this
                        // row carries the fields.
                        rejected_header_value: (node == NodeId::from_u128(0))
                            .then(|| rejected.as_ref().map(|r| r.raw.clone()))
                            .flatten(),
                        rejected_at_unix: (node == NodeId::from_u128(0))
                            .then(|| rejected.as_ref().map(|r| r.at_unix))
                            .flatten(),
                        expected_wire_form: (node == NodeId::from_u128(0) && rejected.is_some())
                            .then(crate::state::RejectedNodeIdHeader::expected_wire_form),
                    })
                    .collect()
            },
        },
        knowledge: KnowledgeStatus {
            hosted_corpora,
            total_chunks_searchable,
        },
        process: ProcessStatus {
            uptime_seconds: state.inner.started_at.elapsed().as_secs(),
            rss_mb: current_rss_mb(),
            peak_rss_mb: peak_rss_mb(),
        },
        rpc_worker: rpc_worker_port().map(|port| RpcWorkerStatus {
            port,
            iroh: state.rpc_iroh_accept(),
        }),
    })
}

/// Current resident set size in MiB, platform-native. Sampled per
/// request — a `getrusage`/`proc_pidinfo` call, microseconds. Pairs
/// with the daemon-side memory watch: this is the pull surface the
/// doctor's memory check reads.
fn current_rss_mb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: zeroed out-struct of the correct size; the call
        // writes at most `size` bytes into it.
        let pid = std::process::id() as libc::c_int;
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let rc = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if rc != size {
            return None;
        }
        Some(info.pti_resident_size / (1024 * 1024))
    }
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        Some(pages * page_size as u64 / (1024 * 1024))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Peak RSS in MiB via getrusage. macOS reports `ru_maxrss` in
/// *bytes*; Linux in *kilobytes* — preserve the unit split.
#[cfg(unix)]
fn peak_rss_mb() -> Option<u64> {
    // SAFETY: getrusage with a properly-zeroed `rusage` struct is safe.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    if rc != 0 {
        return None;
    }
    let raw = ru.ru_maxrss as u64;
    #[cfg(target_os = "macos")]
    {
        Some(raw / (1024 * 1024))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(raw / 1024)
    }
}

#[cfg(not(unix))]
fn peak_rss_mb() -> Option<u64> {
    None
}

/// The TCP port this node's in-process RPC inference worker is serving, parsed
/// from `SOVEREIGN_RPC_SERVE` (e.g. `0.0.0.0:50052` → 50052) **and only when a
/// worker is actually accepting on it**. `None` when this node is not configured
/// as a worker, or is configured but the worker isn't live — e.g. ggml tore its
/// accept loop down on a transient `accept()` error and the supervisor hasn't
/// re-bound yet. Advertised on `/status` for host auto-discovery, so this gate
/// stops us from publishing a dead port that hosts would connect to and skip
/// every discovery cycle (the second half of the worker-resilience fix; the
/// first half is the supervisor restart in `sovereign-inference`).
fn rpc_worker_port() -> Option<u16> {
    let bind = std::env::var("SOVEREIGN_RPC_SERVE").ok()?;
    let port: u16 = bind.trim().rsplit(':').next()?.parse().ok()?;
    rpc_worker_listening(&bind).then_some(port)
}

/// True when a TCP connection to the configured RPC bind address succeeds — i.e.
/// a worker is genuinely accepting there, not merely configured. This is exactly
/// the promise `/status` makes ("a peer can reach this RPC port"), so we verify
/// it directly rather than trusting the env var. `0.0.0.0` / `::` / empty hosts
/// are probed on loopback. Short localhost connect; pure over the bind string so
/// it is unit-testable without touching the process environment.
fn rpc_worker_listening(bind: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let Some((host, port_str)) = bind.trim().rsplit_once(':') else {
        return false;
    };
    let Ok(port) = port_str.trim().parse::<u16>() else {
        return false;
    };
    let host = match host.trim() {
        "" | "0.0.0.0" | "::" | "[::]" | "*" => "127.0.0.1",
        h => h,
    };
    match (host, port).to_socket_addrs() {
        Ok(addrs) => addrs
            .into_iter()
            .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()),
        Err(_) => false,
    }
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub node_id: String,
    pub mesh: MeshStatus,
    pub inference: InferenceStatus,
    pub knowledge: KnowledgeStatus,
    pub process: ProcessStatus,
    /// Present when this node serves an in-process RPC inference worker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_worker: Option<RpcWorkerStatus>,
}

/// Process vitals for the pager: `uptime_seconds` resets are the
/// witness of a real restart; `rss_mb` is what the doctor's memory
/// check compares against the soft limit.
#[derive(Debug, Serialize)]
pub struct ProcessStatus {
    pub uptime_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_mb: Option<u64>,
}

/// Advertised RPC inference-worker endpoint for mesh auto-discovery.
#[derive(Debug, Serialize)]
pub struct RpcWorkerStatus {
    pub port: u16,
    /// True when this worker's iroh acceptor also routes the RPC ALPN —
    /// a cross-network host may reach the rpc-server through a mesh
    /// tunnel instead of the raw `ip:port`. Omitted when false, so the
    /// JSON is byte-identical for non-iroh workers (additive wire).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub iroh: bool,
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
    /// Ground-truth in-memory residency of each engine slot — the
    /// `ollama ps` analog. Empty on the orchestrator daemon (no
    /// embedded engine); populated on the desktop/embedded daemon.
    #[serde(default)]
    pub resident: Vec<crate::state::ResidentSlot>,
    /// Supervised compute children (P1). Empty unless `[compute]` pools are
    /// configured — then one entry per replica, with its live lifecycle.
    #[serde(default)]
    pub compute_children: Vec<crate::state::ComputeChildStatus>,
    /// Code-editing arrangement — which model serves next-edit and/or
    /// FIM, and which lanes are actually available
    /// (`sovereign/docs/NEXT_EDIT.md`, `INLINE_COMPLETION.md` §6).
    /// `None` only when there is no editing model at all; the VSCode
    /// extension's status bar reads this to distinguish "daemon up, no
    /// editing model" from "daemon down".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<crate::state::EditSlotStatus>,
    /// **Deprecated mirror of [`Self::edit`]**, byte-identical to it.
    ///
    /// Kept because `inference.fim` is read by JSON path — not by a
    /// typed client — in at least three shipped places (the VSCode
    /// extension's `probeStatus`, `svrn setup`'s verification pointer,
    /// and `scripts/fim-smoke.sh`). Dropping the key outright would
    /// make an already-installed extension report "no FIM model
    /// configured" against a perfectly healthy daemon, which reads as
    /// a broken install rather than a renamed field.
    ///
    /// Remove once the ledger row for the rename graduates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fim: Option<crate::state::EditSlotStatus>,
    /// Per-peer request tally (order `seat-resource-commons` UC-R1) —
    /// the "is my GPU serving a peer right now?" answer. Empty when no
    /// peer request has ever been admitted since this daemon started.
    /// An entry with `active: 0` means "served before, idle now" —
    /// that is the zero reading, distinct from "never served". Read
    /// `active` for the headline, `served_total` as the cumulative
    /// attribution witness, `last_request_at` for staleness.
    #[serde(default)]
    pub peer_requests: Vec<PeerRequestStatus>,
}

/// One peer's tally row on `/status` (UC-R1). `name` is joined from
/// the mesh roster so the reading is a name, not an opaque hash;
/// absent when the node is not a roster member.
///
/// Wire forms (order commons-fluency fix 7 — one canonical form per
/// surface, documented here): `node_id` is the DISPLAY form
/// (`node-` + first 16 hex chars of the id — what `NodeId`'s `Display`
/// prints). The `X-Node-Id` HEADER surface is the different, full
/// form: `NodeId::to_hex()`, exactly 32 lowercase hex chars
/// (`crate::headers::parse_x_node_id` accepts nothing else). A
/// truncated hex string from a status row must never be echoed back
/// as a header — resolve through the roster or `to_hex`.
#[derive(Debug, Serialize)]
pub struct PeerRequestStatus {
    pub node_id: String,
    /// Mesh roster name (e.g. `BeefyMac`). Omitted when the node is
    /// not (or no longer) a roster member.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Requests whose response body is streaming RIGHT NOW.
    pub active: u64,
    /// Requests admitted since daemon start (cumulative witness).
    pub served_total: u64,
    /// Unix seconds of the most recent admission.
    pub last_request_at: i64,
    /// Zero-bucket row only (fix 7): the raw `X-Node-Id` header value
    /// that failed [`crate::headers::parse_x_node_id`]. `None` on
    /// every well-formed row and on the zero row when no malformed
    /// header has ever arrived.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_header_value: Option<String>,
    /// Zero-bucket row only (fix 7): unix seconds when the malformed
    /// value above was last seen, so the row reads as a live signal,
    /// not a fossil.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_at_unix: Option<i64>,
    /// Zero-bucket row only (fix 7): the canonical wire form the
    /// header must match — the inverse of `parse_x_node_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_wire_form: Option<&'static str>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn rpc_worker_listening_reflects_actual_listener() {
        // A live listener on an ephemeral port reads as listening: connect_timeout
        // completes the kernel handshake even before the app calls accept().
        let live = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = live.local_addr().unwrap().port();
        assert!(
            rpc_worker_listening(&format!("0.0.0.0:{port}")),
            "0.0.0.0 form must probe loopback and see the listener"
        );
        assert!(
            rpc_worker_listening(&format!("127.0.0.1:{port}")),
            "explicit-host form must connect too"
        );

        // Once freed, the same port reads as NOT listening — this is the bug we
        // fixed: SOVEREIGN_RPC_SERVE set, but no worker accepting => don't advertise.
        drop(live);
        assert!(
            !rpc_worker_listening(&format!("0.0.0.0:{port}")),
            "a configured-but-dead port must not be advertised"
        );
    }

    #[test]
    fn rpc_worker_listening_rejects_malformed_bind() {
        assert!(!rpc_worker_listening("not-a-bind"));
        assert!(!rpc_worker_listening("0.0.0.0:not-a-port"));
        assert!(!rpc_worker_listening(""));
    }
}

#[cfg(test)]
mod process_status_tests {
    use super::*;

    #[test]
    fn process_status_serializes_and_samples() {
        let p = ProcessStatus {
            uptime_seconds: 42,
            rss_mb: current_rss_mb(),
            peak_rss_mb: peak_rss_mb(),
        };
        // We're a live process: both samples must be present + nonzero
        // on unix targets.
        #[cfg(unix)]
        {
            assert!(p.rss_mb.unwrap() > 0);
            assert!(p.peak_rss_mb.unwrap() > 0);
        }
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["uptime_seconds"], 42);
    }

    #[test]
    fn zero_bucket_row_names_rejected_header_and_expected_form() {
        // Fix 7: the malformed-header bucket (node id 0) names the rejected
        // raw value + expected wire form; well-formed rows are unchanged —
        // the three fields are omitted from the JSON entirely, byte-identical
        // to pre-fix-7 hosts.
        let rejected = Some(crate::state::RejectedNodeIdHeader {
            raw: "not-a-node-id!".into(),
            at_unix: 1786549000,
        });
        let zero_row = PeerRequestStatus {
            node_id: "node-0000000000000000".into(),
            name: None,
            active: 1,
            served_total: 2,
            last_request_at: 1786549000,
            rejected_header_value: rejected.as_ref().map(|r| r.raw.clone()),
            rejected_at_unix: rejected.as_ref().map(|r| r.at_unix),
            expected_wire_form: rejected
                .is_some()
                .then(crate::state::RejectedNodeIdHeader::expected_wire_form),
        };
        let json = serde_json::to_value(&zero_row).unwrap();
        assert_eq!(json["rejected_header_value"], "not-a-node-id!");
        assert_eq!(json["rejected_at_unix"], 1786549000);
        assert!(json["expected_wire_form"]
            .as_str()
            .unwrap()
            .contains("32 lowercase hex chars"));

        let clean_row = PeerRequestStatus {
            node_id: "node-6c955b5f1361aaaa".into(),
            name: Some("BeefyMac".into()),
            active: 0,
            served_total: 5,
            last_request_at: 1786549000,
            rejected_header_value: None,
            rejected_at_unix: None,
            expected_wire_form: None,
        };
        let clean = serde_json::to_value(&clean_row).unwrap();
        assert_eq!(
            clean,
            serde_json::json!({
                "node_id": "node-6c955b5f1361aaaa",
                "name": "BeefyMac",
                "active": 0,
                "served_total": 5,
                "last_request_at": 1786549000,
            })
        );
    }

    #[test]
    fn rpc_worker_iroh_flag_is_additive_on_the_wire() {
        // false → key omitted entirely: byte-identical JSON to pre-task-6
        // workers, so old hosts parse it unchanged.
        let off = serde_json::to_value(RpcWorkerStatus {
            port: 50052,
            iroh: false,
        })
        .unwrap();
        assert_eq!(off, serde_json::json!({ "port": 50052 }));
        // true → advertised; hosts read it with `.get("iroh")` off the
        // opaque JSON, absent-means-false.
        let on = serde_json::to_value(RpcWorkerStatus {
            port: 50052,
            iroh: true,
        })
        .unwrap();
        assert_eq!(on, serde_json::json!({ "port": 50052, "iroh": true }));
    }
}
