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

use async_trait::async_trait;

use crate::state::{AppState, RpcShardWarmer};
use sovereign_inference::embedded::{cache_file_name, warm_cache_for_device, Fnv1a};

/// All sibling file names of a split GGUF (`<stem>-<idx>-of-<count>.gguf`),
/// including `name` itself, in shard order; `[name]` for a non-split name.
///
/// Delegates to `sovereign-inference`'s `split_shard_names` — host and worker
/// MUST agree on what "the whole model" means, and a second copy of the parser
/// is a standing invitation to disagree. This wrapper only adds the
/// non-split fallback the fetch path wants.
fn split_sibling_names(name: &str) -> Vec<String> {
    sovereign_inference::embedded::split_shard_names(name).unwrap_or_else(|| vec![name.to_string()])
}

/// The cache dir the in-process RPC worker actually reads, so the bytes we
/// warm land where its RPC server looks for `SET_TENSOR_HASH` hits. `Err` when
/// caching is disabled: warming into a stray dir would let the load stream
/// anyway and wedge, so we refuse — the host then loads local-only (never
/// wedge).
///
/// This used to re-derive the rule, with a note explaining that
/// `sovereign-inference`'s `default_cache_dir` "doesn't model the disabled
/// case". It does now, so the mirror is a delegation and the two cannot drift
/// apart again (ARCH §10.6). What stays here is the REFUSAL — the message an
/// operator reads — because only this caller has a host to fall back for.
fn worker_cache_dir() -> Result<PathBuf, String> {
    sovereign_inference::embedded::default_cache_dir().ok_or_else(|| {
        "RPC tensor cache is disabled (SOVEREIGN_RPC_CACHE_DIR=off) — \
         cannot auto-warm; the host will load local-only"
            .to_string()
    })
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
    // This node's mesh-proof header pair, or `None` on a mesh with no
    // credential. The sources are a PEER's `/internal/v1/models/file/*`, so on
    // a plain-IP hop it is the only thing that tells the host's internal port
    // a member is range-fetching rather than a stranger. One pair for the whole
    // warm: a range warm is a burst, not a loop that outlives a proof window.
    mesh_proof: Option<(&str, &str)>,
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
            let mut request = http
                .get(&source_urls[i])
                .header(reqwest::header::RANGE, range.as_str());
            if let Some((name, value)) = mesh_proof {
                request = request.header(name, value);
            }
            match request.send().await {
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
            .unwrap_or_else(|| sovereign_contracts::rebrand::svrnmesh_root().join("models"));
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
        mesh_proof: Option<(&str, &str)>,
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
                    self.fetch_one(model_id, peer_bases, mesh_proof).await?
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
            self.fetch_one(sibling, peer_bases, mesh_proof).await?;
        }
        Ok(primary)
    }

    /// Fetch one named model file from the first reachable host base.
    async fn fetch_one(
        &self,
        name: &str,
        peer_bases: &[String],
        mesh_proof: Option<(&str, &str)>,
    ) -> Result<PathBuf, String> {
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
                mesh_proof,
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
    let Some(id) = host_node_id.and_then(|h| kernel_types::NodeId::from_hex(h)) else {
        return Vec::new();
    };
    let member = {
        state
            .inner
            .fabric
            .mesh
            .read()
            .await
            .members
            .get(&id)
            .cloned()
    };
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
        // Resolved beside the bases: the fetches below leave this crate (the
        // range warm) and this workspace crate boundary (`model_fetch`, which
        // cannot name `AppState`), so the read is made here, once.
        let warm_proof = state.mesh_proof_stamp().await.map(|s| {
            let (name, value) = s.pair();
            (name.to_string(), value.to_string())
        });

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
                        .map(|b| oicp_types::model_transfer::model_file_url(b, &req.model_id)),
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
                                .map(|b| oicp_types::model_transfer::model_file_url(b, &name)),
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
                    warm_proof.as_ref().map(|(n, v)| (n.as_str(), v.as_str())),
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
                    .resolve_whole_gguf(
                        &req.model_id,
                        local_model_path,
                        &bases,
                        warm_proof.as_ref().map(|(n, v)| (n.as_str(), v.as_str())),
                    )
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

// The host-side orchestrator and the wire types are the loader's
// (`sovereign_compute::distributed_warm`, pb-serve-distributes); re-exported
// at their historical paths while this worker side still names them.
pub use sovereign_compute::distributed_warm::{
    install_rpc_warm_orchestrator, RpcWarmShardRequest, RpcWarmShardResponse, RpcWarmSource,
    TensorRange,
};

// Moved to a sibling file: inline, these put this file past its arch-gate
// slack (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "tests/rpc_warm_http.rs"]
mod tests;
