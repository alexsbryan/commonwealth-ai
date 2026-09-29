// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host side of the RPC shard warm: the auto-warm orchestrator that fans a
//! distributing primary's plan out to every worker and blocks until warm,
//! and the `/internal/rpc-warm` wire types both ends speak.
//!
//! Moved from the svrn daemon's `rpc_warm_http` (pb-serve-distributes): it
//! reaches workers through the loader's `MeshPorts` (the roster, the
//! transport, where this node serves model files, its mesh proof), so the
//! process that loads the distributed primary runs it. The worker side (the
//! route's `RpcShardWarmer`) stays in the daemon until its own move.


use kernel_types::NodeId;
use mesh_reach::{PeerEndpoint, TrafficClass};
use serde::{Deserialize, Serialize};
use sovereign_inference::embedded::{build_manifest, tensor_device, NodeShard, RpcWarmPlan};
use sovereign_serving_host::rpc_discovery::MeshPorts;

#[cfg(test)]
mod tests;

/// Ordered dial candidates for `node`'s internal HTTP surface under the
/// `ModelTransfer` traffic class (rpc-warm pushes, GGUF/shard pulls) — the
/// transport's view: on an iroh-routed mesh the first candidate is a loopback
/// bridge that tunnels to the peer, with raw-IP fallback candidates after.
/// Empty when the mesh is not up or the node has left the membership.
async fn model_transfer_endpoints(ports: &MeshPorts, node: NodeId) -> Vec<PeerEndpoint> {
    let Some(now) = (ports.mesh)().await else {
        return Vec::new();
    };
    let Some(member) = now.roster.member(node).await else {
        return Vec::new();
    };
    now.transport
        .endpoints(&member.dial, TrafficClass::ModelTransfer)
        .await
}

/// Whether to ship each worker the whole GGUF (`#5a`) or only its tensors'
/// byte ranges (`#5b`). `SOVEREIGN_RPC_SHARD_FETCH=ranges` selects byte-range;
/// anything else (default) ships whole. Byte-range keeps each worker at
/// `O(model/N)` on disk but makes the host hash the whole GGUF to build the
/// manifest, so it's opt-in until a model can't fit one node's disk.
fn byte_range_mode() -> bool {
    std::env::var("SOVEREIGN_RPC_SHARD_FETCH")
        .map(|v| v.eq_ignore_ascii_case("ranges") || v.eq_ignore_ascii_case("byte_ranges"))
        .unwrap_or(false)
}

/// Install the host-side auto-warm orchestrator into `sovereign-inference`. Called
/// once at startup where the engine loads (host role). During a distributing
/// primary load, `sovereign-inference` calls this with the plan; we fan the warm
/// out to every worker and block until all are warm — then the load proceeds
/// with overrides.
///
/// Must be called from within the Tokio runtime (it captures the current
/// `Handle` to bridge the synchronous seam to async HTTP).
pub fn install_rpc_warm_orchestrator(ports: MeshPorts) {
    let handle = tokio::runtime::Handle::current();
    // Short CONNECT timeout so an unreachable worker fails in seconds (→ the host
    // falls back to local-only) rather than blocking the whole reload on the OS's
    // multi-minute SYN-retry budget. NO overall request timeout: a reachable
    // worker may legitimately take minutes (it fetches the GGUF before warming).
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    sovereign_inference::embedded::set_rpc_warm_orchestrator(move |plan: &RpcWarmPlan| {
        // The seam is synchronous and called from a blocking load thread; bridge
        // to async on the captured runtime handle.
        let owned = plan.clone();
        let ports = ports.clone();
        let http = http.clone();
        handle.block_on(async move { orchestrate_warm(&ports, &http, &owned).await })
    });
    tracing::info!("rpc-warm: auto-warm orchestrator installed (host role)");
}

/// Fan the warm request out to every worker in `plan.assignments` and wait for
/// all to report warm. Any failure → `Err` (the caller falls back to a local-only
/// load — never wedge). Glassbox: logs per-worker outcome.
async fn orchestrate_warm(
    ports: &MeshPorts,
    http: &reqwest::Client,
    plan: &RpcWarmPlan,
) -> Result<(), String> {
    let model_id = plan
        .model_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "model path has no file name".to_string())?
        .to_string();

    // The host's own reachable bases on the internal port — where workers fetch
    // the GGUF (or its ranges) back from.
    let origin = (ports.model_origin)().await;
    let internal_port = origin.internal_port;
    let host_bases: Vec<String> = origin.bases;
    if host_bases.is_empty() {
        return Err("host has no reachable internal-port address to serve the model from".into());
    }

    // For byte-range mode, build the manifest ONCE (hashes the whole GGUF) so each
    // worker can be handed exactly its tensors. Whole-GGUF mode skips this — the
    // workers hash their own shards in parallel.
    let manifest = if byte_range_mode() {
        let path = plan.model_path.clone();
        Some(
            tokio::task::spawn_blocking(move || build_manifest(&path))
                .await
                .map_err(|e| format!("manifest task panicked: {e}"))?
                .map_err(|e| format!("build manifest: {e}"))?,
        )
    } else {
        None
    };

    tracing::info!(
        model_id = %model_id,
        workers = plan.assignments.len(),
        mode = if manifest.is_some() { "byte_ranges" } else { "whole_gguf" },
        "rpc-warm orchestrator: seeding worker shards before distributed load"
    );

    // Host identity for the request: a worker that knows WHO we are resolves
    // its fetch bases back to us through ITS transport (iroh bridge), instead
    // of trusting the raw-IP bases alone.
    let host_node_id = (ports.mesh)().await.map(|now| now.self_id.to_hex());

    let mut tasks = Vec::with_capacity(plan.assignments.len());
    for assignment in &plan.assignments {
        // Worker IP from its RPC endpoint (`ip:rpc_port`); its internal HTTP port
        // is assumed to match ours (the same assumption discovery already makes
        // for the client port).
        let worker_ip = assignment
            .endpoint
            .rsplit_once(':')
            .map(|(host, _)| host.to_string())
            .unwrap_or_else(|| assignment.endpoint.clone());

        // Warm-POST candidates, best first. When discovery recorded which mesh
        // member owns this endpoint, ask the transport for `ModelTransfer`
        // candidates — on an iroh-routed mesh the first is a loopback bridge
        // that tunnels to the peer (raw `:9742` is NOT reachable there; this
        // was the `auto-warm failed … Connection refused` blocker). The
        // hand-built raw URL stays as the final fallback and is the only
        // candidate for env-configured workers (no directory entry).
        let worker_node = ports.discovery.endpoint_node(&assignment.endpoint);
        let mut candidates: Vec<(String, String, Option<PeerEndpoint>)> = Vec::new();
        if let Some(node) = worker_node {
            for ep in model_transfer_endpoints(ports, node).await {
                candidates.push((
                    format!("{}/internal/rpc-warm", ep.base_url),
                    ep.label.clone(),
                    Some(ep),
                ));
            }
        }
        if raw_warm_fallback_allowed(&worker_ip) {
            let raw_url = format!("http://{worker_ip}:{internal_port}/internal/rpc-warm");
            if !candidates.iter().any(|(u, _, _)| *u == raw_url) {
                candidates.push((raw_url, format!("raw:{worker_ip}:{internal_port}"), None));
            }
        }

        // Hand THIS worker the bases it's most likely to reach first — its own
        // network before a shared-but-unroutable LAN (see order_host_bases). The
        // worker still tries them all, but the reachable one is first so it
        // doesn't burn its connect budget on a dead LAN IP.
        let ordered_bases = order_host_bases(&host_bases, worker_ip.parse().ok());

        let source = match &manifest {
            Some(m) => {
                // Split-aware: each tensor's offsets are relative to its own
                // shard file; assign a stable file index (order of first
                // appearance) and ship per-file URL candidate lists.
                let mut files: Vec<String> = Vec::new();
                let mut tensors: Vec<TensorRange> = Vec::new();
                for e in m.iter().filter(|e| {
                    e.cacheable
                        && tensor_device(&e.name, e.layer, &plan.plan)
                            == Some(assignment.device_index)
                }) {
                    let file_idx = match files.iter().position(|f| *f == e.file) {
                        Some(i) => i,
                        None => {
                            files.push(e.file.clone());
                            files.len() - 1
                        }
                    } as u32;
                    tensors.push(TensorRange {
                        gguf_offset: e.gguf_offset,
                        nbytes: e.nbytes,
                        hash: e.hash,
                        file_idx,
                    });
                }
                // A placed worker with ZERO cacheable tensors is a manifest
                // gap, not a warm — e.g. a split GGUF where build_manifest
                // read only the header shard (found live 2026-07-19: the
                // "warm" reported success with written=0 already=0 and the
                // load bulk-streamed 22GB into the upload deadlock). Fail
                // the warm so the caller falls back local-only. Never wedge.
                if tensors.is_empty() {
                    return Err(format!(
                        "worker {} (device {}) has 0 cacheable tensors in the manifest \
                         but is assigned blocks — manifest gap (split GGUF?); refusing \
                         an empty warm that would bulk-stream at load time",
                        assignment.endpoint, assignment.device_index
                    ));
                }
                let file_urls: Vec<Vec<String>> = files
                    .iter()
                    .map(|f| {
                        ordered_bases
                            .iter()
                            .map(|b| oicp_types::model_transfer::model_file_url(b, f))
                            .collect()
                    })
                    .collect();
                // Legacy `source_urls` = the first file's candidates, so an
                // old worker still functions for single-file models; on a
                // split it fetches wrong-file bytes → FNV mismatch → loud
                // warm failure → local-only, never a poisoned cache.
                let source_urls = file_urls.first().cloned().unwrap_or_default();
                RpcWarmSource::ByteRanges {
                    source_urls,
                    tensors,
                    file_urls,
                }
            }
            None => RpcWarmSource::WholeGguf {
                peer_bases: ordered_bases,
            },
        };

        let body = RpcWarmShardRequest {
            model_id: model_id.clone(),
            device_index: assignment.device_index,
            plan: plan.plan.clone(),
            source,
            host_node_id: host_node_id.clone(),
        };
        let http = http.clone();
        let endpoint = assignment.endpoint.clone();
        tasks.push(async move {
            let label = format!("{endpoint} (device {})", body.device_index);
            // Try candidates in order; first success wins. Glassbox: every
            // attempt logs WHICH path (`via`) carried or failed it, so "which
            // transport actually warmed this worker?" is answerable from logs.
            //
            // EVERY attempt is kept, not just the last. Keeping only the last
            // one made the reported cause always the FINAL candidate — the
            // raw-IP fallback, whose `Connection refused` is expected and
            // uninteresting on an iroh-routed mesh. That buried the real
            // failure (the iroh candidate timing out) under a message that
            // reads like "the peer needs to expose port 9742", and cost a
            // session chasing port exposure while the iroh path was in fact
            // the working one (2026-07-29).
            let mut attempts: Vec<String> = Vec::new();
            for (url, via, ep) in &candidates {
                // Stamped: the worker is a PEER and this is its
                // `/internal/rpc-warm`. Minted per candidate — the orchestrator
                // fans out over minutes and the proof window is 30 s.
                let request = match (ports.proof)().await {
                    Some((n, v)) => http.post(url).json(&body).header(n, v),
                    None => http.post(url).json(&body),
                };
                match request.send().await {
                    Ok(r) if r.status().is_success() => {
                        let stats = r.json::<RpcWarmShardResponse>().await.unwrap_or_default();
                        tracing::info!(
                            worker = %label,
                            via = %via,
                            written = stats.tensors_written,
                            already = stats.tensors_already_present,
                            "rpc-warm: worker shard warm"
                        );
                        // Feed the eligibility gate positive liveness. Keyed on
                        // `worker_node` ALONE, and deliberately BEFORE the
                        // `if let` below: that one also requires `ep`, which is
                        // `None` for the raw-IP fallback candidate, so hanging
                        // this off the same guard would silently skip exactly
                        // the transfers that took the fallback path.
                        //
                        // A peer is least able to answer an 800ms /status probe
                        // precisely while it is absorbing gigabytes from us, so
                        // "this worker just carried a multi-GB transfer" is both
                        // stronger evidence than the probe and available exactly
                        // when the probe fails.
                        if let Some(node) = worker_node {
                            if let Some(el) = sovereign_serving_host::worker_eligibility::global() {
                                el.note_alive(node, &endpoint, std::time::Instant::now());
                            }
                        }
                        if let (Some(node), Some(ep)) = (worker_node, ep.as_ref()) {
                            if let Some(now) = (ports.mesh)().await {
                                now.transport.note_success(node, TrafficClass::ModelTransfer, ep);
                            }
                        }
                        return Ok(());
                    }
                    Ok(r) => {
                        let status = r.status();
                        let detail = r.text().await.unwrap_or_default();
                        attempts.push(format!("via {via}: returned {status}: {detail}"));
                        // The worker's error body is the ONLY place the actual
                        // failure reason surfaces (its own log may be unreachable
                        // remotely) — losing it here cost a live 122B acceptance
                        // run a blind retry loop (2026-07-27).
                        tracing::warn!(worker = %label, via = %via, status = %status, detail = %truncate_for_log(&detail), "rpc-warm: candidate answered with an error; trying next");
                    }
                    Err(e) => {
                        let chain = error_chain(&e);
                        attempts.push(format!("via {via}: {chain}"));
                        // The error itself, not just the fact of one: a bare
                        // "candidate unreachable" left the iroh candidate's
                        // actual failure (a 30s dial timeout) invisible in the
                        // logs, so the only visible cause was the raw
                        // fallback's refusal.
                        tracing::warn!(worker = %label, via = %via, error = %chain, "rpc-warm: candidate unreachable; trying next");
                    }
                }
            }
            Err(if attempts.is_empty() {
                format!("{label}: no warm-POST candidate")
            } else {
                format!("{label}: all {} candidate(s) failed: {}", attempts.len(), attempts.join("; "))
            })
        });
    }

    // All workers must warm before the load proceeds.
    let results = futures::future::join_all(tasks).await;
    let failures: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    if failures.is_empty() {
        tracing::info!(model_id = %model_id, "rpc-warm orchestrator: all worker shards warm");
        Ok(())
    } else {
        Err(format!(
            "{} of {} worker(s) failed to warm: {}",
            failures.len(),
            plan.assignments.len(),
            failures.join("; ")
        ))
    }
}
// ─── Wire types (the `/internal/rpc-warm` body) ──────────────────────────────

/// One tensor's location + identity for a byte-range fetch (`#5b`): where it
/// lives in the GGUF and the FNV-1a hash its cache file is named by. The host
/// derives these from `build_manifest` for exactly this worker's shard, so the
/// worker range-GETs only its `O(model/N)` bytes and never re-hashes the file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TensorRange {
    pub gguf_offset: u64,
    pub nbytes: u64,
    pub hash: u64,
    /// Index into `ByteRanges.file_urls` naming the shard file this range is
    /// relative to (split GGUFs ship per-file offsets). `0` — the serde
    /// default, and what pre-split hosts send — means the first/only file.
    /// An OLD worker ignores this and fetches every range from
    /// `source_urls`; wrong-file bytes then fail the FNV verification and
    /// the warm errs loudly (→ local-only) instead of poisoning the cache.
    #[serde(default)]
    pub file_idx: u32,
}

/// How the worker obtains the bytes it warms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RpcWarmSource {
    /// `#5a` — warm from the whole GGUF: use the copy the node already holds (the
    /// route resolves it via the servable allowlist), else fetch it from one of
    /// `peer_bases` (a host internal-port base like `http://10.0.0.1:9742`). The
    /// worker discovers size + sha from the host's `/internal/v1/models/list`.
    WholeGguf {
        #[serde(default)]
        peer_bases: Vec<String>,
    },
    /// `#5b` — range-fetch only this shard's tensors. `source_urls` are full
    /// `/internal/v1/models/file/{name}` URLs (one per host base); the worker
    /// `Range`-GETs each tensor and verifies its hash. Never holds the whole GGUF.
    /// For split GGUFs, `file_urls[i]` is the ordered URL candidate list for
    /// shard file `i` and each tensor's `file_idx` selects its file; when
    /// `file_urls` is empty (single-file model / pre-split host) every tensor
    /// uses `source_urls`.
    ByteRanges {
        source_urls: Vec<String>,
        tensors: Vec<TensorRange>,
        #[serde(default)]
        file_urls: Vec<Vec<String>>,
    },
}

/// `POST /internal/rpc-warm` request. The host sends each worker the whole `plan`
/// + this worker's `device_index` (so warm placement == load placement) and a
/// `source` describing how to get its shard's bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcWarmShardRequest {
    pub model_id: String,
    pub device_index: usize,
    pub plan: Vec<NodeShard>,
    pub source: RpcWarmSource,
    /// Hex `NodeId` (`NodeId::to_hex`) of the HOST — the node about to
    /// distribute. Lets the worker resolve its fetch bases back to the host
    /// through its OWN mesh transport (an iroh bridge on an encrypted mesh),
    /// with the raw-IP bases in `source` retained as LAN fallback. `None`
    /// from older hosts — wire back-compat, raw bases only.
    #[serde(default)]
    pub host_node_id: Option<String>,
}

/// `POST /internal/rpc-warm` success body — what this worker warmed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RpcWarmShardResponse {
    pub model_id: String,
    pub device_index: usize,
    pub tensors_written: usize,
    pub tensors_already_present: usize,
    pub bytes_written: u64,
}

/// Render an error plus its `source()` chain — reqwest's top-level Display is just
/// "error sending request for url (…)"; the actual cause (connection refused /
/// timed out / DNS) lives in the source chain. Glassbox: a warm failure must say
/// WHY so we don't guess.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(" ← ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

/// Bound a worker's error body for a single log line. 500 bodies are
/// `{"error": …}` one-liners; anything longer is truncated, not dropped —
/// a truncated reason still beats `status=500` alone.
fn truncate_for_log(s: &str) -> String {
    const MAX: usize = 600;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} bytes total]", &s[..end], s.len())
}

fn is_private_v4(o: [u8; 4]) -> bool {
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

/// Score how likely a worker at `worker_ip` can reach a host base (`http://IP:port`)
/// — higher is better. A mesh peer reaches us best on an address in ITS OWN
/// network: a Tailscale peer (CGNAT `100.x`) reaches our `100.x`, NOT a `192.168.x`
/// LAN we happen to share but can't route across (WiFi AP client isolation — the
/// exact failure the cross-machine test hit). `-1` for an unparseable base.
fn base_reachability_score(base: &str, worker_ip: Option<std::net::IpAddr>) -> i32 {
    let Some(worker_ip) = worker_ip else {
        return 0;
    };
    let host = base
        .strip_prefix("http://")
        .map(|s| s.split('/').next().unwrap_or(s))
        .and_then(|hp| hp.rsplit_once(':').map(|(h, _)| h))
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']');
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return -1;
    };
    match (ip, worker_ip) {
        (std::net::IpAddr::V4(a), std::net::IpAddr::V4(w)) => {
            let (ao, wo) = (a.octets(), w.octets());
            if ao[0..2] == wo[0..2] {
                3 // same /16
            } else if ao[0] == wo[0] {
                2 // same /8 (e.g. both Tailscale CGNAT 100.x)
            } else if is_private_v4(ao) == is_private_v4(wo) {
                1 // same category (both private, or both not)
            } else {
                0 // a private LAN vs the worker's non-private network → try last
            }
        }
        (std::net::IpAddr::V6(_), std::net::IpAddr::V6(_)) => 1,
        _ => 0, // address-family mismatch
    }
}

/// Order a host's fetch bases so the one the worker is most likely to reach comes
/// first — so the worker hits a routable address immediately instead of burning
/// its connect budget on an unroutable shared-LAN IP. Stable within a score tier.
fn order_host_bases(bases: &[String], worker_ip: Option<std::net::IpAddr>) -> Vec<String> {
    let mut ranked: Vec<(i32, usize, &String)> = bases
        .iter()
        .enumerate()
        .map(|(i, b)| (base_reachability_score(b, worker_ip), i, b))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, _, b)| b.clone()).collect()
}

/// Whether `worker_ip` (parsed from an RPC endpoint string) may be used to
/// hand-build a raw warm-URL fallback. A loopback `worker_ip` means the
/// endpoint is a bridge-local iroh tunnel (task 6) — the hand-built
/// `http://127.0.0.1:{internal_port}/internal/rpc-warm` would be THIS
/// host's own internal router, and the resulting self-warm reports success
/// while the real worker stays cold, resurrecting the upload deadlock the
/// warm exists to prevent. Never raw-fall-back to loopback. Unparseable
/// hosts (e.g. a hostname) keep the legacy raw fallback.
fn raw_warm_fallback_allowed(worker_ip: &str) -> bool {
    worker_ip
        .parse::<std::net::IpAddr>()
        .map(|ip| !ip.is_loopback())
        .unwrap_or(true)
}
