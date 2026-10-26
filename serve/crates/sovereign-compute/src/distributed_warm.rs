// SPDX-License-Identifier: AGPL-3.0-or-later
//! The RPC shard warm: the `/internal/rpc-warm` wire types and the worker
//! side ([`worker`], behind the contracts port `RpcShardWarmer`).
//!
//! The host side, the orchestrator that fanned a distributing primary's plan
//! out to every worker, answered the embedded engine's warm hook and was
//! deleted with it (ENGINE_SWAP_PLAN G2).

use serde::{Deserialize, Serialize};
use sovereign_inference::rpc_warm_cache::NodeShard;

#[cfg(test)]
mod tests;

mod worker;
pub use worker::{warm_cache_from_ranges, MeshRpcShardWarmer, WarmRangeStats};

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
