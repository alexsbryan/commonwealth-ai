// SPDX-License-Identifier: AGPL-3.0-or-later
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

    let loaded_models: Vec<LoadedModelStatus> = plan
        .model_plans
        .iter()
        .map(|p| LoadedModelStatus {
            model: format!("{}", p.model),
            nodes: p.assignments.len(),
            tps: p.estimated_tokens_per_sec,
            loaded: state
                .inner
                .inference_store
                .get_llama_address(p.model)
                .is_some(),
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
        inference: InferenceStatus { loaded_models },
        knowledge: KnowledgeStatus {
            hosted_corpora,
            total_chunks_searchable,
        },
        process: ProcessStatus {
            uptime_seconds: state.inner.started_at.elapsed().as_secs(),
            rss_mb: current_rss_mb(),
            peak_rss_mb: peak_rss_mb(),
        },
        rpc_worker: rpc_worker_port().map(|port| RpcWorkerStatus { port }),
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
}
