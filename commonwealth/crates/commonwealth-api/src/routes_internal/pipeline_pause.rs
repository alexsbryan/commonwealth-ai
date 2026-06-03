//! `POST /internal/pipeline/pause` — mesh-aware pause of running
//! `sovereign pipeline run` drivers.
//!
//! The pipeline driver is a standalone CLI process (not part of the
//! daemon) that reads/writes a local SQLite worklist DB. Each peer
//! runs its own driver against its own worklist for fan-out. The CLI
//! itself can only see local PIDs via `/proc/`, so `pipeline pause`
//! used to stop only the driver on the host where it was invoked —
//! peer drivers kept claiming work until exhaustion.
//!
//! This route lets one daemon ask another to SIGTERM its local
//! drivers. Request flow:
//!
//! 1. CLI POSTs to `127.0.0.1:9742/internal/pipeline/pause` with
//!    `{ recipe_id, force, fanout: true }`.
//! 2. The local daemon walks its own `/proc/` for matching driver
//!    PIDs and SIGTERMs them.
//! 3. With `fanout: true`, the local daemon enumerates online mesh
//!    peers from `state.inner.mesh` (the same gossip-derived view
//!    the inference load balancer uses) and forwards the same
//!    request to each — with `fanout: false` so peers don't re-fan
//!    and the message can't loop.
//! 4. Aggregated `{ local, peers: [...] }` is returned. The CLI
//!    renders a per-node summary.
//!
//! No new gossip-protocol state is added — the request is a one-shot
//! HTTP fanout that mirrors what the inference path already does.
//! Peers running an older daemon binary without this route reply 404;
//! the CLI surfaces that explicitly so the operator knows which peers
//! need a rebuild.

use std::path::PathBuf;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use commonwealth_core::mesh::NodeStatus;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

use super::ErrorBody;

/// Per-peer HTTP timeout for the fanout. Pause is best-effort — a
/// slow or hung peer must not stall the operator's terminal forever.
const PEER_PAUSE_TIMEOUT_SECS: u64 = 8;

/// Request body for `POST /internal/pipeline/pause`.
#[derive(Debug, Clone, Deserialize)]
pub struct PipelinePauseRequest {
    pub recipe_id: String,
    /// When true, escalate to SIGKILL after the local /proc walk.
    /// In-flight enrich subprocesses become orphans; intended only for
    /// the wedged-driver case.
    #[serde(default)]
    pub force: bool,
    /// When true, also forward this request to every online peer with
    /// `fanout: false`. CLI sets this; peers receiving the forwarded
    /// request leave it false to prevent loops.
    #[serde(default)]
    pub fanout: bool,
}

/// Per-node pause result. Used for both the local node and each peer.
/// `Deserialize` is needed because peer-to-peer fanout calls this same
/// route and parses the response back through this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePauseResult {
    /// `"local"` for this node; peer node-id hex otherwise.
    pub node: String,
    /// Optional human-readable name from gossip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// PIDs that received the signal. Empty when no matching driver
    /// was running on that node.
    pub pids_signaled: Vec<u32>,
    /// `true` once every signaled PID has exited cleanly (SIGTERM path
    /// only). `false` means the wait deadline elapsed; the operator
    /// can retry with `force: true`. Always `true` for `force: true`.
    pub drained: bool,
    /// Set when the call could not be made (network error, 404 on
    /// older peers, transport timeout). Mutually exclusive with the
    /// other fields being meaningful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregated response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePauseResponse {
    pub local: NodePauseResult,
    /// Empty when `fanout: false` or no peers are online.
    pub peers: Vec<NodePauseResult>,
}

pub async fn pipeline_pause(
    State(state): State<AppState>,
    Json(req): Json<PipelinePauseRequest>,
) -> Result<Json<PipelinePauseResponse>, (StatusCode, Json<ErrorBody>)> {
    if req.recipe_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: "recipe_id required".into(),
            }),
        ));
    }

    let local = pause_local_drivers(&req.recipe_id, req.force).await;

    let peers = if req.fanout {
        forward_to_peers(&state, &req).await
    } else {
        Vec::new()
    };

    tracing::info!(
        recipe_id = %req.recipe_id,
        force = req.force,
        fanout = req.fanout,
        local_pids = ?local.pids_signaled,
        peer_count = peers.len(),
        "pipeline_pause: complete"
    );

    Ok(Json(PipelinePauseResponse { local, peers }))
}

/// Walk `/proc/` looking for `sovereign pipeline run` processes whose
/// recipe path resolves to `recipe_id`, signal each, and (for SIGTERM)
/// wait briefly for them to exit. Mirrors the CLI-side
/// `pipeline_cmd::find_driver_pids` + `cmd_pause` drain loop so the
/// behaviour matches when the CLI calls this route locally.
async fn pause_local_drivers(recipe_id: &str, force: bool) -> NodePauseResult {
    let pids = find_pipeline_driver_pids(recipe_id);
    if pids.is_empty() {
        return NodePauseResult {
            node: "local".into(),
            name: None,
            pids_signaled: vec![],
            drained: true,
            error: None,
        };
    }

    let signum = if force { libc::SIGKILL } else { libc::SIGTERM };
    for pid in &pids {
        // Safety: libc::kill is a thin syscall wrapper. We read errno
        // out separately rather than unwrap so a stale PID doesn't
        // panic the daemon.
        let rc = unsafe { libc::kill(*pid as libc::pid_t, signum) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(pid, signal = signum, %err, "pipeline_pause: kill failed");
        }
    }

    if force {
        return NodePauseResult {
            node: "local".into(),
            name: None,
            pids_signaled: pids,
            drained: true,
            error: None,
        };
    }

    // SIGTERM: bounded drain wait. 30 s matches the typical enrich
    // shell-out completion window; longer than this and the driver is
    // probably wedged on a model call, in which case the operator
    // should retry with `force: true`.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let drained = loop {
        let still_alive = pids
            .iter()
            .any(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists());
        if !still_alive {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    NodePauseResult {
        node: "local".into(),
        name: None,
        pids_signaled: pids,
        drained,
        error: None,
    }
}

/// Find every `sovereign pipeline run` PID on this machine whose
/// recipe-toml argument resolves to `recipe_id`. Duplicates the logic
/// from `sovereign-cli/src/pipeline_cmd.rs` because that crate isn't
/// a dependency of `commonwealth-api`; keep the two in sync.
fn find_pipeline_driver_pids(recipe_id: &str) -> Vec<u32> {
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut pids = Vec::new();
    for entry in proc_dir.flatten() {
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let cmdline_path = format!("/proc/{pid}/cmdline");
        let Ok(raw) = std::fs::read(&cmdline_path) else {
            continue;
        };
        let argv: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect();
        let is_driver = argv.windows(3).any(|w| {
            (w[0].ends_with("sovereign") || w[0].ends_with("sovereign-cli"))
                && w[1] == "pipeline"
                && w[2] == "run"
        });
        if !is_driver {
            continue;
        }
        let recipe_arg_idx = argv.iter().position(|s| s == "run").map(|i| i + 1);
        let Some(idx) = recipe_arg_idx else {
            continue;
        };
        let Some(recipe_arg) = argv.get(idx) else {
            continue;
        };
        let candidate = PathBuf::from(recipe_arg);
        if !candidate.exists() {
            continue;
        }
        // Read the recipe TOML's [recipe].id field. We do a stringly-
        // typed inspection here rather than depend on sovereign-pipeline
        // — the field shape is stable and the alternative pulls a
        // recipe-parser crate into commonwealth-api just for this.
        let Ok(toml_text) = std::fs::read_to_string(&candidate) else {
            continue;
        };
        if recipe_toml_id(&toml_text).as_deref() == Some(recipe_id) {
            pids.push(pid);
        }
    }
    pids
}

/// Extract the `[recipe] id = "..."` field from a recipe TOML, doing
/// a minimal parse. Returns None if the file isn't a recipe or the
/// field is absent.
fn recipe_toml_id(text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(text).ok()?;
    value
        .get("recipe")?
        .get("id")?
        .as_str()
        .map(|s| s.to_string())
}

/// Concurrently POST `{fanout: false}` requests to every online peer
/// known to the local daemon's mesh state and collect the results.
async fn forward_to_peers(state: &AppState, req: &PipelinePauseRequest) -> Vec<NodePauseResult> {
    let mesh = state.inner.mesh.read().await;
    let self_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let peers: Vec<_> = mesh
        .members
        .values()
        .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
        .cloned()
        .collect();
    drop(mesh);

    if peers.is_empty() {
        return Vec::new();
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(PEER_PAUSE_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "pipeline_pause: reqwest build failed");
            return peers
                .into_iter()
                .map(|m| NodePauseResult {
                    node: hex::encode(m.node_id.as_bytes()),
                    name: Some(m.name.clone()),
                    pids_signaled: vec![],
                    drained: false,
                    error: Some(format!("local reqwest build failed: {e}")),
                })
                .collect();
        }
    };

    let forwarded_body = serde_json::json!({
        "recipe_id": req.recipe_id,
        "force": req.force,
        "fanout": false,
    });

    let mut handles = Vec::with_capacity(peers.len());
    for peer in peers {
        let client = client.clone();
        let body = forwarded_body.clone();
        let node = hex::encode(peer.node_id.as_bytes());
        let name = Some(peer.name.clone());
        let addresses = peer.addresses.clone();
        let handle =
            tokio::spawn(async move { ask_peer(&client, &body, &node, name, &addresses).await });
        handles.push(handle);
    }

    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(result) => out.push(result),
            Err(join_err) => {
                tracing::warn!(error = %join_err, "pipeline_pause: peer task panicked");
            }
        }
    }
    out
}

/// Try each advertised address for `peer` in order; the first one that
/// answers wins. Mirrors the address-cycling pattern in
/// `corpus_collaborate::queue_broadcast` so peer routing behaves the
/// same across the codebase (IPv4 preferred, IPv6 falls through, etc.).
async fn ask_peer(
    client: &reqwest::Client,
    body: &serde_json::Value,
    node: &str,
    name: Option<String>,
    addresses: &[std::net::SocketAddr],
) -> NodePauseResult {
    let mut last_error = "no addresses advertised by peer".to_string();
    for addr in addresses {
        let host = match addr.ip() {
            std::net::IpAddr::V4(_) => addr.ip().to_string(),
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let url = format!("http://{host}:9742/internal/pipeline/pause");
        match client.post(&url).json(body).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<PipelinePauseResponse>().await {
                    Ok(inner) => {
                        return NodePauseResult {
                            node: node.to_string(),
                            name,
                            pids_signaled: inner.local.pids_signaled,
                            drained: inner.local.drained,
                            error: inner.local.error,
                        };
                    }
                    Err(e) => {
                        last_error = format!("malformed response from {url}: {e}");
                        continue;
                    }
                }
            }
            Ok(resp) if resp.status() == StatusCode::NOT_FOUND => {
                last_error = format!(
                    "{url}: 404 — peer is running an older daemon without /internal/pipeline/pause; \
                     rebuild + restart it to participate in mesh-wide pause"
                );
                // 404 is terminal for this peer — no other address will
                // route differently. Don't waste more reqwest connects.
                break;
            }
            Ok(resp) => {
                last_error = format!("{url}: {}", resp.status());
                continue;
            }
            Err(e) => {
                last_error = format!("{url}: {e}");
                continue;
            }
        }
    }
    NodePauseResult {
        node: node.to_string(),
        name,
        pids_signaled: vec![],
        drained: false,
        error: Some(last_error),
    }
}
