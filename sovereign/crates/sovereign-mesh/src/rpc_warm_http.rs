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

use commonwealth_api::state::{AppState, RpcShardWarmer};
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

/// All sibling file names of a split GGUF (`<stem>-<idx>-of-<count>.gguf`),
/// including `name` itself, in shard order; `[name]` for a non-split name.
/// Pure name arithmetic — mirrors `sovereign-inference`'s shard enumeration
/// so host and worker agree on what "the whole model" means.
fn split_sibling_names(name: &str) -> Vec<String> {
    let parsed = name.rfind("-of-").and_then(|of| {
        let count: u32 = name.get(of + 4..)?.strip_suffix(".gguf")?.parse().ok()?;
        let before = name.get(..of)?;
        let dash = before.rfind('-')?;
        let idx = before.get(dash + 1..)?;
        idx.parse::<u32>().ok()?;
        Some((before.get(..dash)?.to_string(), count, idx.len()))
    });
    match parsed {
        Some((stem, count, width)) if count > 1 => (1..=count)
            .map(|i| format!("{stem}-{i:0width$}-of-{count:0width$}.gguf"))
            .collect(),
        _ => vec![name.to_string()],
    }
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
    file_urls: &[Vec<String>],
) -> Result<WarmRangeStats, String> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    if source_urls.is_empty() && file_urls.iter().all(|f| f.is_empty()) {
        return Err("no source URL for byte-range warm".to_string());
    }
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("create cache dir: {e}"))?;
    let mut stats = WarmRangeStats::default();
    // Sticky preferred source: once one serves a range, keep using it.
    // Index is positional within whichever URL list a tensor's file uses —
    // the base ordering is identical across files, so stickiness carries.
    let mut url_idx = 0usize;

    for t in tensors {
        // Split-aware: a tensor's offsets are relative to ITS shard file.
        // `file_urls[file_idx]` is that file's candidate list; empty/absent
        // (single-file model, pre-split host) falls back to `source_urls`.
        let source_urls: &[String] = match file_urls.get(t.file_idx as usize) {
            Some(urls) if !urls.is_empty() => urls,
            _ => source_urls,
        };
        if source_urls.is_empty() {
            return Err(format!(
                "no source URL for file_idx {} of a byte-range warm",
                t.file_idx
            ));
        }
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
        // Split GGUFs: the warm reader walks every `-NNNNN-of-NNNNN` sibling,
        // so ALL shard files must be local, not just the named one.
        let needed = split_sibling_names(model_id);
        let primary = match local_model_path {
            Some(p) => p,
            None => {
                let already = self.fetch_dir.join(model_id);
                if already.is_file() {
                    already
                } else {
                    self.fetch_one(model_id, peer_bases).await?
                }
            }
        };
        let dir = primary.parent().map(Path::to_path_buf).unwrap_or_default();
        for sibling in &needed {
            if sibling == model_id {
                continue;
            }
            if dir.join(sibling).is_file() || self.fetch_dir.join(sibling).is_file() {
                continue;
            }
            // Missing sibling shard — fetch it beside the others. The warm
            // reader falls back to single-file (→ empty-warm guard on the
            // host) if any sibling is absent, so failing here is loud anyway.
            self.fetch_one(sibling, peer_bases).await?;
        }
        Ok(primary)
    }

    /// Fetch one named model file from the first reachable host base.
    async fn fetch_one(&self, name: &str, peer_bases: &[String]) -> Result<PathBuf, String> {
        if peer_bases.is_empty() {
            return Err(format!(
                "node does not hold '{name}' and the warm request carried no host base to fetch from"
            ));
        }
        let mut last_err = "no host base reachable".to_string();
        for base in peer_bases {
            match crate::model_fetch::fetch_named_model_from_peer(
                &self.http,
                base,
                name,
                &self.fetch_dir,
                |_, _| {},
            )
            .await
            {
                Ok(p) => {
                    tracing::info!(
                        model_id = name,
                        base,
                        "rpc-warm: fetched GGUF file for warming"
                    );
                    return Ok(p);
                }
                Err(e) => {
                    last_err = format!("{base}: {e}");
                    tracing::warn!(base, error = %last_err, "rpc-warm: GGUF fetch failed, trying next host base");
                }
            }
        }
        Err(format!("could not fetch '{name}': {last_err}"))
    }
}

impl Default for MeshRpcShardWarmer {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the HOST's fetch bases through THIS node's own transport, given the
/// `host_node_id` hex the warm request carried. On an iroh-routed mesh the
/// raw-IP bases in the request may be unroutable from here (host on a
/// different network) — but the mesh transport already reaches the member as a
/// loopback bridge. Empty when the id is absent/unparseable (legacy host) or
/// the host isn't in our membership; the caller then uses raw bases alone.
async fn host_transport_bases(state: &AppState, host_node_id: Option<&str>) -> Vec<String> {
    let Some(id) = host_node_id.and_then(|h| commonwealth_core::ids::NodeId::from_hex(h)) else {
        return Vec::new();
    };
    let member = { state.inner.mesh.read().await.members.get(&id).cloned() };
    let Some(member) = member else {
        tracing::debug!(
            host = %id,
            "rpc-warm: host_node_id not in local membership; using raw bases only"
        );
        return Vec::new();
    };
    state
        .peer_transport()
        .endpoints(
            &commonwealth_transport::peer_contact(&member),
            commonwealth_transport::TrafficClass::ModelTransfer,
        )
        .await
        .into_iter()
        .map(|e| e.base_url)
        .collect()
}

/// Preferred-first merge without duplicates, order-preserving — transport
/// bases go ahead of the request's raw-IP bases, which stay as LAN fallback.
fn merge_bases(preferred: impl IntoIterator<Item = String>, rest: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in preferred.into_iter().chain(rest.iter().cloned()) {
        if !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

#[async_trait]
impl RpcShardWarmer for MeshRpcShardWarmer {
    async fn warm_shard(
        &self,
        request: serde_json::Value,
        local_model_path: Option<PathBuf>,
        state: AppState,
    ) -> Result<serde_json::Value, String> {
        let req: RpcWarmShardRequest =
            serde_json::from_value(request).map_err(|e| format!("malformed rpc-warm body: {e}"))?;
        let cache_dir = worker_cache_dir()?;
        std::fs::create_dir_all(&cache_dir).map_err(|e| format!("create cache dir: {e}"))?;

        let transport_bases = host_transport_bases(&state, req.host_node_id.as_deref()).await;

        tracing::info!(
            model_id = %req.model_id,
            device_index = req.device_index,
            mode = match &req.source { RpcWarmSource::WholeGguf { .. } => "whole_gguf", RpcWarmSource::ByteRanges { .. } => "byte_ranges" },
            transport_bases = transport_bases.len(),
            "rpc-warm: seeding this node's shard"
        );

        let resp = match &req.source {
            RpcWarmSource::ByteRanges {
                source_urls,
                tensors,
                file_urls,
            } => {
                let urls = merge_bases(
                    transport_bases
                        .iter()
                        .map(|b| format!("{b}/internal/v1/models/file/{}", req.model_id)),
                    source_urls,
                );
                // Per-file lists get the same transport-first merge. The file
                // NAME rides inside the URLs the host built — recover it from
                // the last path segment so the transport candidates target the
                // same shard file.
                let merged_file_urls: Vec<Vec<String>> = file_urls
                    .iter()
                    .map(|urls_for_file| {
                        let name = urls_for_file
                            .first()
                            .and_then(|u| u.rsplit('/').next())
                            .unwrap_or_default()
                            .to_string();
                        if name.is_empty() {
                            return urls_for_file.clone();
                        }
                        merge_bases(
                            transport_bases
                                .iter()
                                .map(|b| format!("{b}/internal/v1/models/file/{name}")),
                            urls_for_file,
                        )
                    })
                    .collect();
                let stats = warm_cache_from_ranges(
                    &self.http,
                    &urls,
                    tensors,
                    &cache_dir,
                    &merged_file_urls,
                )
                .await?;
                RpcWarmShardResponse {
                    model_id: req.model_id.clone(),
                    device_index: req.device_index,
                    tensors_written: stats.written,
                    tensors_already_present: stats.already_present,
                    bytes_written: stats.bytes_written,
                }
            }
            RpcWarmSource::WholeGguf { peer_bases } => {
                let bases = merge_bases(transport_bases.iter().cloned(), peer_bases);
                let gguf = self
                    .resolve_whole_gguf(&req.model_id, local_model_path, &bases)
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

    // Host identity for the request: a worker that knows WHO we are resolves
    // its fetch bases back to us through ITS transport (iroh bridge), instead
    // of trusting the raw-IP bases alone.
    let host_node_id = daemon.self_node_id().await.map(|id| id.to_hex());

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
        let worker_node = daemon.rpc_endpoint_node(&assignment.endpoint);
        let mut candidates: Vec<(String, String, Option<commonwealth_transport::PeerEndpoint>)> =
            Vec::new();
        if let Some(node) = worker_node {
            for ep in daemon.model_transfer_endpoints(node).await {
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
                            .map(|b| format!("{b}/internal/v1/models/file/{f}"))
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
            let mut last_err = format!("{label}: no warm-POST candidate");
            for (url, via, ep) in &candidates {
                match http.post(url).json(&body).send().await {
                    Ok(r) if r.status().is_success() => {
                        let stats = r.json::<RpcWarmShardResponse>().await.unwrap_or_default();
                        tracing::info!(
                            worker = %label,
                            via = %via,
                            written = stats.tensors_written,
                            already = stats.tensors_already_present,
                            "rpc-warm: worker shard warm"
                        );
                        if let (Some(node), Some(ep)) = (worker_node, ep.as_ref()) {
                            daemon.note_model_transfer_success(node, ep).await;
                        }
                        return Ok(());
                    }
                    Ok(r) => {
                        let status = r.status();
                        let detail = r.text().await.unwrap_or_default();
                        last_err = format!("{label} via {via}: warm returned {status}: {detail}");
                        // The worker's error body is the ONLY place the actual
                        // failure reason surfaces (its own log may be unreachable
                        // remotely) — losing it here cost a live 122B acceptance
                        // run a blind retry loop (2026-07-27).
                        tracing::warn!(worker = %label, via = %via, status = %status, detail = %truncate_for_log(&detail), "rpc-warm: candidate answered with an error; trying next");
                    }
                    Err(e) => {
                        last_err =
                            format!("{label} via {via}: warm request failed: {}", error_chain(&e));
                        tracing::warn!(worker = %label, via = %via, "rpc-warm: candidate unreachable; trying next");
                    }
                }
            }
            Err(last_err)
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
                file_idx: 0,
            }
        };
        let tensors = vec![mk(0, 1000), mk(1000, 2000)];

        let cache = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let stats = warm_cache_from_ranges(&http, &[url.clone()], &tensors, cache.path(), &[])
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
        let again = warm_cache_from_ranges(&http, &[url], &tensors, cache.path(), &[])
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
            file_idx: 0,
        }];
        let cache = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let err = warm_cache_from_ranges(&http, &[url], &bad, cache.path(), &[])
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
                    file_idx: 0,
                }],
                file_urls: vec![],
            },
            host_node_id: Some("0123456789abcdef0123456789abcdef".into()),
        };
        let v = serde_json::to_value(&req).unwrap();
        // The opaque-JSON seam in commonwealth-api reads `model_id` for path
        // resolution — it must be a top-level string.
        assert_eq!(v.get("model_id").and_then(|x| x.as_str()), Some("m.gguf"));
        let back: RpcWarmShardRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.device_index, 1);
        assert_eq!(
            back.host_node_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        matches!(back.source, RpcWarmSource::ByteRanges { .. });
    }

    #[test]
    fn old_wire_body_without_host_node_id_still_parses() {
        // A pre-identity host omits the field entirely — a rolling-upgrade
        // worker must not reject the body.
        let v = serde_json::json!({
            "model_id": "m.gguf",
            "device_index": 0,
            "plan": [],
            "source": { "mode": "whole_gguf", "peer_bases": ["http://10.0.0.1:9742"] }
        });
        let req: RpcWarmShardRequest = serde_json::from_value(v).unwrap();
        assert_eq!(req.host_node_id, None);
    }

    #[test]
    fn split_sibling_names_generates_the_full_set() {
        assert_eq!(
            split_sibling_names("m-00001-of-00003.gguf"),
            vec![
                "m-00001-of-00003.gguf",
                "m-00002-of-00003.gguf",
                "m-00003-of-00003.gguf"
            ]
        );
        // Non-split and degenerate names pass through untouched.
        assert_eq!(split_sibling_names("model.gguf"), vec!["model.gguf"]);
        assert_eq!(
            split_sibling_names("m-00001-of-00001.gguf"),
            vec!["m-00001-of-00001.gguf"]
        );
    }

    #[tokio::test]
    async fn warm_from_ranges_routes_tensors_to_their_own_file() {
        // Two "shard files" with distinct contents on separate servers; a
        // tensor from each. Per-file routing must fetch each range from ITS
        // file — the split-GGUF fix (fetching both from file 0 would
        // hash-mismatch tensor 1).
        let data0 = vec![11u8; 2048];
        let mut data1 = vec![0u8; 2048];
        for (i, b) in data1.iter_mut().enumerate() {
            *b = (i % 97) as u8;
        }
        let (url0, s0) = spawn_range_server(data0.clone()).await;
        let (url1, s1) = spawn_range_server(data1.clone()).await;

        let mk = |data: &[u8], off: usize, len: usize, file_idx: u32| {
            let mut h = Fnv1a::new();
            h.update(&data[off..off + len]);
            TensorRange {
                gguf_offset: off as u64,
                nbytes: len as u64,
                hash: h.finish(),
                file_idx,
            }
        };
        let tensors = vec![mk(&data0, 0, 1000, 0), mk(&data1, 500, 1200, 1)];
        let file_urls = vec![vec![url0.clone()], vec![url1.clone()]];

        let cache = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let stats = warm_cache_from_ranges(&http, &[], &tensors, cache.path(), &file_urls)
            .await
            .expect("split warm");
        assert_eq!(stats.written, 2);
        // Each cache entry holds the bytes of ITS OWN file's range.
        for (t, data) in [(&tensors[0], &data0), (&tensors[1], &data1)] {
            let got = std::fs::read(cache.path().join(cache_file_name(t.hash))).unwrap();
            assert_eq!(
                &got[..],
                &data[t.gguf_offset as usize..(t.gguf_offset + t.nbytes) as usize]
            );
        }
        s0.abort();
        s1.abort();
    }

    #[test]
    fn raw_fallback_refused_for_loopback_worker() {
        // A loopback worker_ip means a bridge-local endpoint (task 6): the
        // hand-built raw URL would target OURSELF, and a self-warm reports
        // success while the real worker stays cold → upload deadlock.
        assert!(!raw_warm_fallback_allowed("127.0.0.1"));
        assert!(!raw_warm_fallback_allowed("::1"));
        // Real remote IPs and unparseable hostnames keep the legacy fallback.
        assert!(raw_warm_fallback_allowed("192.168.1.2"));
        assert!(raw_warm_fallback_allowed("100.104.36.28"));
        assert!(raw_warm_fallback_allowed("beefymac.local"));
    }

    #[test]
    fn merge_bases_prefers_transport_and_dedups() {
        let raw = vec![
            "http://192.168.1.19:9742".to_string(),
            "http://127.0.0.1:60001".to_string(),
        ];
        // The transport bridge duplicates one raw entry — it must lead the
        // merged order, appearing exactly once, with the rest behind it.
        let merged = merge_bases(["http://127.0.0.1:60001".to_string()], &raw);
        assert_eq!(
            merged,
            vec![
                "http://127.0.0.1:60001".to_string(),
                "http://192.168.1.19:9742".to_string(),
            ]
        );
    }
}
