// SPDX-License-Identifier: AGPL-3.0-or-later
//! Distributed-inference auto-warm orchestration — the HTTP layer.
//!
//! When a host decides to distribute a large primary across the mesh, it must
//! NOT stream each worker its weight share at load time (the host-side `send()`
//! deadlock above ~800 MB). Instead every worker pre-seeds its RPC tensor cache
//! with its shard, so the host's `-ot` load is all `SET_TENSOR_HASH` cache hits
//! and sends zero bulk weight bytes. This module is both ends of that handshake:
//!
//! - **Worker side** ([`MeshRpcShardWarmer`], the `POST /internal/rpc-warm`
//!   backend): given the host's plan + this node's `device_index`, warm exactly
//!   this node's shard — from the whole GGUF the node already holds / fetches
//!   (`#5a`), or by range-fetching only its tensors (`#5b`, [`warm_cache_from_ranges`]).
//! - **Host side** ([`install_rpc_warm_orchestrator`]): the seam
//!   `sovereign-inference` calls during a distributing load. It fans the warm
//!   request out to every worker and blocks until all report warm — then the load
//!   proceeds with overrides. This replaces the manual `SOVEREIGN_RPC_ASSUME_WARMED`.
//!
//! The host computes the plan ONCE (`sovereign-inference::plan_distribution`) and
//! ships it whole, so warm-time placement and load-time placement derive from the
//! identical assignment and cannot diverge — the plan-agreement invariant.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use commonwealth_api::state::RpcShardWarmer;
use sovereign_inference::embedded::{
    build_manifest, cache_file_name, tensor_device, warm_cache_for_device, Fnv1a, NodeShard,
    RpcWarmPlan,
};

use crate::daemon::EmbeddedDaemon;

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

/// The cache dir the in-process RPC worker actually reads — must mirror
/// `model_slot::rpc_cache_dir` exactly, so the bytes we warm land where the
/// worker's RPC server looks for `SET_TENSOR_HASH` hits. `Err` when caching is
/// disabled (`off` / `0` / empty): warming into a stray dir would let the load
/// stream anyway and wedge, so we refuse — the host then loads local-only (never
/// wedge). `sovereign-inference`'s `default_cache_dir` doesn't model the disabled
/// case, which is why this lives here.
fn worker_cache_dir() -> Result<PathBuf, String> {
    match std::env::var("SOVEREIGN_RPC_CACHE_DIR") {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() || v.eq_ignore_ascii_case("off") || v == "0" {
                return Err(
                    "RPC tensor cache is disabled (SOVEREIGN_RPC_CACHE_DIR=off) — \
                            cannot auto-warm; the host will load local-only"
                        .to_string(),
                );
            }
            Ok(PathBuf::from(v))
        }
        Err(_) => std::env::var("HOME")
            .map(|h| Path::new(&h).join(".sovereign").join("rpc-cache"))
            .map_err(|_| "no HOME for the default RPC cache dir".to_string()),
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
    ByteRanges {
        source_urls: Vec<String>,
        tensors: Vec<TensorRange>,
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

// ─── `#5b` worker primitive: warm a shard by HTTP byte-range ─────────────────

/// Counts from a byte-range warm run.
#[derive(Debug, Default, Clone)]
pub struct WarmRangeStats {
    pub written: usize,
    pub already_present: usize,
    pub bytes_written: u64,
}

/// Fetch exactly this shard's tensors by HTTP `Range` from one of `source_urls`
/// (the host's `serve_model_file`, which honors `Range`), verify each against its
/// expected FNV-1a hash, and write it as a cache file named by that hash —
/// byte-identical to what the local-GGUF warmer writes, so the host's later
/// `SET_TENSOR_HASH` is a hit. Streams each tensor (no whole-tensor buffer).
/// Idempotent (a present, right-sized file is left). This is the only warm path
/// that keeps a worker at `O(model/N)` on disk — the `500 GB × N-node` endgame.
///
/// `source_urls` are tried in order, sticking with the first that serves a range,
/// so a multi-homed host degrades gracefully.
pub async fn warm_cache_from_ranges(
    http: &reqwest::Client,
    source_urls: &[String],
    tensors: &[TensorRange],
    cache_dir: &Path,
) -> Result<WarmRangeStats, String> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    if source_urls.is_empty() {
        return Err("no source URL for byte-range warm".to_string());
    }
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("create cache dir: {e}"))?;
    let mut stats = WarmRangeStats::default();
    // Sticky preferred source: once one serves a range, keep using it.
    let mut url_idx = 0usize;

    for t in tensors {
        let name = cache_file_name(t.hash);
        let cache_file = cache_dir.join(&name);
        // Idempotent: skip a present, correctly-sized entry.
        if let Ok(meta) = std::fs::metadata(&cache_file) {
            if meta.len() == t.nbytes {
                stats.already_present += 1;
                continue;
            }
        }

        let end = t.gguf_offset + t.nbytes - 1;
        let range = format!("bytes={}-{}", t.gguf_offset, end);

        // Try sources starting at the sticky index, wrapping once.
        let mut resp = None;
        let mut last_err = String::new();
        for step in 0..source_urls.len() {
            let i = (url_idx + step) % source_urls.len();
            match http
                .get(&source_urls[i])
                .header(reqwest::header::RANGE, range.as_str())
                .send()
                .await
            {
                Ok(r)
                    if r.status() == reqwest::StatusCode::PARTIAL_CONTENT
                        || r.status().is_success() =>
                {
                    url_idx = i;
                    resp = Some(r);
                    break;
                }
                Ok(r) => last_err = format!("{}: status {}", source_urls[i], r.status()),
                Err(e) => last_err = format!("{}: {e}", source_urls[i]),
            }
        }
        let resp =
            resp.ok_or_else(|| format!("range GET {name} failed on all sources: {last_err}"))?;

        // Stream → hash → temp file; verify both length and hash; atomic rename so
        // a torn write never looks like a valid cache entry.
        let tmp = cache_dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut hasher = Fnv1a::new();
        let mut written: u64 = 0;
        {
            let mut out = tokio::fs::File::create(&tmp)
                .await
                .map_err(|e| format!("create {}: {e}", tmp.display()))?;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("range body {name}: {e}"))?;
                hasher.update(&chunk);
                out.write_all(&chunk)
                    .await
                    .map_err(|e| format!("write {name}: {e}"))?;
                written += chunk.len() as u64;
            }
            out.flush()
                .await
                .map_err(|e| format!("flush {name}: {e}"))?;
        }

        if written != t.nbytes {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "range {name}: got {written} bytes, expected {} — host served the wrong range",
                t.nbytes
            ));
        }
        if hasher.finish() != t.hash {
            // The host's bytes don't hash to the cache key the host itself will
            // request — distributing now would miss + stream. Refuse.
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "range {name}: content hash mismatch — fetched bytes are not the expected tensor"
            ));
        }
        std::fs::rename(&tmp, &cache_file).map_err(|e| format!("rename {name}: {e}"))?;
        stats.written += 1;
        stats.bytes_written += written;
        tracing::debug!(tensor = %name, bytes = written, "rpc-warm: wrote cache entry from byte range");
    }
    Ok(stats)
}

// ─── Worker side: the `RpcShardWarmer` impl ──────────────────────────────────

/// Worker-side warmer wired into `AppState` by the daemon. Holds an HTTP client
/// and a directory to fetch a whole GGUF into when the node doesn't already hold
/// the model (the `#5a` fallback). The `#5b` path needs neither.
pub struct MeshRpcShardWarmer {
    http: reqwest::Client,
    /// Where a whole-GGUF fetch lands when the node lacks the model.
    fetch_dir: PathBuf,
}

impl MeshRpcShardWarmer {
    pub fn new() -> Self {
        let fetch_dir = std::env::var("SOVEREIGN_RPC_MODELS_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| Path::new(&h).join(".sovereign").join("models"))
            })
            .unwrap_or_else(|| PathBuf::from(".sovereign/models"));
        // Short CONNECT timeout so an UNREACHABLE host base (e.g. a LAN IP the
        // host advertised that we can't route — WiFi client isolation) fails in
        // seconds and we fall through to the next base, instead of hanging on the
        // OS's multi-minute SYN-retry budget. The actual download has no timeout.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http, fetch_dir }
    }

    /// Resolve the GGUF to warm from for the whole-GGUF path: the local copy the
    /// route already found, else a previously-fetched copy, else fetch it from a
    /// host base. `#5b` (ByteRanges) never calls this.
    async fn resolve_whole_gguf(
        &self,
        model_id: &str,
        local_model_path: Option<PathBuf>,
        peer_bases: &[String],
    ) -> Result<PathBuf, String> {
        if let Some(p) = local_model_path {
            return Ok(p);
        }
        let already = self.fetch_dir.join(model_id);
        if already.is_file() {
            return Ok(already);
        }
        if peer_bases.is_empty() {
            return Err(format!(
                "node does not hold '{model_id}' and the warm request carried no host base to fetch from"
            ));
        }
        let mut last_err = "no host base reachable".to_string();
        for base in peer_bases {
            match crate::model_fetch::fetch_named_model_from_peer(
                &self.http,
                base,
                model_id,
                &self.fetch_dir,
                |_, _| {},
            )
            .await
            {
                Ok(p) => {
                    tracing::info!(model_id, base, "rpc-warm: fetched whole GGUF for warming");
                    return Ok(p);
                }
                Err(e) => {
                    last_err = format!("{base}: {e}");
                    tracing::warn!(base, error = %last_err, "rpc-warm: whole-GGUF fetch failed, trying next host base");
                }
            }
        }
        Err(format!("could not fetch '{model_id}': {last_err}"))
    }
}

impl Default for MeshRpcShardWarmer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RpcShardWarmer for MeshRpcShardWarmer {
    async fn warm_shard(
        &self,
        request: serde_json::Value,
        local_model_path: Option<PathBuf>,
    ) -> Result<serde_json::Value, String> {
        let req: RpcWarmShardRequest =
            serde_json::from_value(request).map_err(|e| format!("malformed rpc-warm body: {e}"))?;
        let cache_dir = worker_cache_dir()?;
        std::fs::create_dir_all(&cache_dir).map_err(|e| format!("create cache dir: {e}"))?;

        tracing::info!(
            model_id = %req.model_id,
            device_index = req.device_index,
            mode = match &req.source { RpcWarmSource::WholeGguf { .. } => "whole_gguf", RpcWarmSource::ByteRanges { .. } => "byte_ranges" },
            "rpc-warm: seeding this node's shard"
        );

        let resp = match &req.source {
            RpcWarmSource::ByteRanges {
                source_urls,
                tensors,
            } => {
                let stats =
                    warm_cache_from_ranges(&self.http, source_urls, tensors, &cache_dir).await?;
                RpcWarmShardResponse {
                    model_id: req.model_id.clone(),
                    device_index: req.device_index,
                    tensors_written: stats.written,
                    tensors_already_present: stats.already_present,
                    bytes_written: stats.bytes_written,
                }
            }
            RpcWarmSource::WholeGguf { peer_bases } => {
                let gguf = self
                    .resolve_whole_gguf(&req.model_id, local_model_path, peer_bases)
                    .await?;
                // `warm_cache_for_device` is synchronous file I/O — run it off the
                // reactor. It warms exactly this device's shard (its blocks + any
                // output head it owns), reading the same plan the host loads from.
                let plan = req.plan.clone();
                let device_index = req.device_index;
                let cache = cache_dir.clone();
                let stats = tokio::task::spawn_blocking(move || {
                    warm_cache_for_device(&gguf, &cache, &plan, device_index)
                })
                .await
                .map_err(|e| format!("warm task panicked: {e}"))?
                .map_err(|e| format!("warm: {e}"))?;
                RpcWarmShardResponse {
                    model_id: req.model_id.clone(),
                    device_index: req.device_index,
                    tensors_written: stats.written,
                    tensors_already_present: stats.already_present,
                    bytes_written: stats.bytes_written,
                }
            }
        };
        tracing::info!(
            model_id = %resp.model_id,
            device_index = resp.device_index,
            written = resp.tensors_written,
            already = resp.tensors_already_present,
            mb = resp.bytes_written / (1024 * 1024),
            "rpc-warm: shard warm complete"
        );
        serde_json::to_value(resp).map_err(|e| format!("serialize rpc-warm response: {e}"))
    }
}

// ─── Host side: the auto-warm orchestrator ───────────────────────────────────

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
/// once at daemon startup (host role). During a distributing primary load,
/// `sovereign-inference` calls this with the plan; we fan the warm out to every
/// worker and block until all are warm — then the load proceeds with overrides.
///
/// Must be called from within the Tokio runtime (it captures the current
/// `Handle` to bridge the synchronous seam to async HTTP).
pub fn install_rpc_warm_orchestrator(daemon: Arc<EmbeddedDaemon>) {
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
        let daemon = Arc::clone(&daemon);
        let http = http.clone();
        handle.block_on(async move { orchestrate_warm(&daemon, &http, &owned).await })
    });
    tracing::info!("rpc-warm: auto-warm orchestrator installed (host role)");
}

/// Fan the warm request out to every worker in `plan.assignments` and wait for
/// all to report warm. Any failure → `Err` (the caller falls back to a local-only
/// load — never wedge). Glassbox: logs per-worker outcome.
async fn orchestrate_warm(
    daemon: &EmbeddedDaemon,
    http: &reqwest::Client,
    plan: &RpcWarmPlan,
) -> Result<(), String> {
    let model_id = plan
        .model_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "model path has no file name".to_string())?
        .to_string();

    let (_client_port, internal_port) = daemon.resolved_ports().await;
    // The host's own reachable bases on the internal port — where workers fetch
    // the GGUF (or its ranges) back from.
    let host_bases: Vec<String> = crate::mesh_discovery::reachable_addresses(internal_port)
        .into_iter()
        .map(|a| format!("http://{a}"))
        .collect();
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
        let warm_url = format!("http://{worker_ip}:{internal_port}/internal/rpc-warm");

        // Hand THIS worker the bases it's most likely to reach first — its own
        // network before a shared-but-unroutable LAN (see order_host_bases). The
        // worker still tries them all, but the reachable one is first so it
        // doesn't burn its connect budget on a dead LAN IP.
        let ordered_bases = order_host_bases(&host_bases, worker_ip.parse().ok());

        let source = match &manifest {
            Some(m) => {
                let tensors: Vec<TensorRange> = m
                    .iter()
                    .filter(|e| {
                        e.cacheable
                            && tensor_device(&e.name, e.layer, &plan.plan)
                                == Some(assignment.device_index)
                    })
                    .map(|e| TensorRange {
                        gguf_offset: e.gguf_offset,
                        nbytes: e.nbytes,
                        hash: e.hash,
                    })
                    .collect();
                let source_urls = ordered_bases
                    .iter()
                    .map(|b| format!("{b}/internal/v1/models/file/{model_id}"))
                    .collect();
                RpcWarmSource::ByteRanges {
                    source_urls,
                    tensors,
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
        };
        let http = http.clone();
        let endpoint = assignment.endpoint.clone();
        tasks.push(async move {
            let label = format!("{endpoint} (device {})", body.device_index);
            match http.post(&warm_url).json(&body).send().await {
                Ok(r) if r.status().is_success() => {
                    let stats = r.json::<RpcWarmShardResponse>().await.unwrap_or_default();
                    tracing::info!(
                        worker = %label,
                        written = stats.tensors_written,
                        already = stats.tensors_already_present,
                        "rpc-warm: worker shard warm"
                    );
                    Ok(())
                }
                Ok(r) => {
                    let status = r.status();
                    let detail = r.text().await.unwrap_or_default();
                    Err(format!("{label}: warm returned {status}: {detail}"))
                }
                Err(e) => Err(format!("{label}: warm request failed: {}", error_chain(&e))),
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    /// Serve a fixed byte buffer with `Range` support, mirroring the production
    /// `serve_model_file` 206 path — enough to prove the warmer's round-trip
    /// without dragging in commonwealth-api (a circular dep for this crate).
    async fn spawn_range_server(data: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let data = Arc::new(data);
        let handler = move |headers: axum::http::HeaderMap| {
            let data = Arc::clone(&data);
            async move {
                let size = data.len() as u64;
                if let Some((s, e)) = headers
                    .get(axum::http::header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|h| {
                        let spec = h.trim().strip_prefix("bytes=")?;
                        let (a, b) = spec.split_once('-')?;
                        Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
                    })
                {
                    let slice = data[s as usize..=(e as usize)].to_vec();
                    return (
                        axum::http::StatusCode::PARTIAL_CONTENT,
                        [(
                            axum::http::header::CONTENT_RANGE,
                            format!("bytes {s}-{e}/{size}"),
                        )],
                        slice,
                    )
                        .into_response();
                }
                (axum::http::StatusCode::OK, (*data).clone()).into_response()
            }
        };
        use axum::response::IntoResponse;
        let app = Router::new().route("/internal/v1/models/file/m.gguf", get(handler));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            format!("http://{addr}/internal/v1/models/file/m.gguf"),
            server,
        )
    }

    #[tokio::test]
    async fn warm_from_ranges_writes_hash_named_cache_files() {
        // A fake "GGUF": two distinct tensor regions in one buffer.
        let mut data = vec![0u8; 4096];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let (url, server) = spawn_range_server(data.clone()).await;

        // Two tensors at known offsets; compute their true FNV hashes.
        let mk = |off: usize, len: usize| {
            let mut h = Fnv1a::new();
            h.update(&data[off..off + len]);
            TensorRange {
                gguf_offset: off as u64,
                nbytes: len as u64,
                hash: h.finish(),
            }
        };
        let tensors = vec![mk(0, 1000), mk(1000, 2000)];

        let cache = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let stats = warm_cache_from_ranges(&http, &[url.clone()], &tensors, cache.path())
            .await
            .expect("warm");
        assert_eq!(stats.written, 2);
        assert_eq!(stats.bytes_written, 3000);

        // Each cache file is named by its hash and holds the exact bytes.
        for t in &tensors {
            let f = cache.path().join(cache_file_name(t.hash));
            let got = std::fs::read(&f).unwrap();
            assert_eq!(got.len() as u64, t.nbytes);
            assert_eq!(
                &got[..],
                &data[t.gguf_offset as usize..(t.gguf_offset + t.nbytes) as usize]
            );
        }

        // Idempotent: a second run writes nothing new.
        let again = warm_cache_from_ranges(&http, &[url], &tensors, cache.path())
            .await
            .unwrap();
        assert_eq!(again.written, 0);
        assert_eq!(again.already_present, 2);

        server.abort();
    }

    #[tokio::test]
    async fn warm_from_ranges_rejects_a_hash_mismatch() {
        let data = vec![7u8; 2048];
        let (url, server) = spawn_range_server(data).await;
        // A tensor claiming a hash the bytes don't produce → must refuse (else the
        // host's SET_TENSOR_HASH would miss and stream → deadlock).
        let bad = vec![TensorRange {
            gguf_offset: 0,
            nbytes: 1000,
            hash: 0xdead_beef_dead_beef,
        }];
        let cache = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let err = warm_cache_from_ranges(&http, &[url], &bad, cache.path())
            .await
            .unwrap_err();
        assert!(err.contains("hash mismatch"), "got: {err}");
        // No cache file left behind.
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
        server.abort();
    }

    #[test]
    fn request_round_trips_through_json() {
        let req = RpcWarmShardRequest {
            model_id: "m.gguf".into(),
            device_index: 1,
            plan: Vec::new(),
            source: RpcWarmSource::ByteRanges {
                source_urls: vec!["http://h/internal/v1/models/file/m.gguf".into()],
                tensors: vec![TensorRange {
                    gguf_offset: 10,
                    nbytes: 20,
                    hash: 30,
                }],
            },
        };
        let v = serde_json::to_value(&req).unwrap();
        // The opaque-JSON seam in commonwealth-api reads `model_id` for path
        // resolution — it must be a top-level string.
        assert_eq!(v.get("model_id").and_then(|x| x.as_str()), Some("m.gguf"));
        let back: RpcWarmShardRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.device_index, 1);
        matches!(back.source, RpcWarmSource::ByteRanges { .. });
    }
}
