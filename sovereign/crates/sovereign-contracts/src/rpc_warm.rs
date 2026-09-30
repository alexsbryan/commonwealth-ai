// SPDX-License-Identifier: AGPL-3.0-or-later
//! The worker side of distributed-inference auto-warm, as a port
//! (pb-serve-distributes): `POST /internal/rpc-warm` hands the warmer the
//! host's request, this node's local copy of the model if it holds one, and
//! the reach the route resolved from the mesh it holds. The warmer is the
//! loader's (`sovereign_compute::distributed_warm`); the route and the mesh
//! are whoever serves it (svrn's daemon until the flip).

use std::path::PathBuf;

use async_trait::async_trait;

/// How this worker reaches the host that asked: the host's model-file bases
/// through this node's own transport (tried before the request's raw bases,
/// which stay as the LAN fallback), and this node's mesh proof for the host's
/// internal port. Both empty on a mesh that could not resolve them — the
/// warmer then uses the request's raw bases alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WarmReach {
    /// The host's fetch bases through this node's transport, preferred first.
    pub host_bases: Vec<String>,
    /// This node's mesh-proof header `(name, value)`, `None` on a mesh with
    /// no credential.
    pub proof: Option<(String, String)>,
}

/// Seeds this worker's RPC tensor cache with its shard of a model. `request`
/// and the answer are the opaque wire bodies (`RpcWarmShardRequest`,
/// `RpcWarmShardResponse`, the loader's); `local_model_path` is this node's
/// own copy when the route found one on its servable list.
#[async_trait]
pub trait RpcShardWarmer: Send + Sync {
    /// Warm this node's shard, answering the response body or naming why not.
    async fn warm_shard(
        &self,
        request: serde_json::Value,
        local_model_path: Option<PathBuf>,
        reach: WarmReach,
    ) -> Result<serde_json::Value, String>;
}

/// Find this node's local copy of `model_id` in the servable allowlist: first an exact
/// entry (a configured slot), then the same filename in any servable model's
/// DIRECTORY — so a worker that holds the GGUF on disk (even when it isn't a
/// configured slot) warms from it instead of re-fetching. Returns `None` when the
/// node genuinely doesn't have it (the warmer then fetches). Path-safety: a
/// `model_id` containing a path separator is rejected — it's matched as a bare
/// file name only, never a traversal.
pub fn resolve_local_model(allow: &[PathBuf], model_id: &str) -> Option<PathBuf> {
    if model_id.is_empty() || model_id.contains('/') || model_id.contains('\\') {
        return None;
    }
    // 1. Exact slot match.
    if let Some(p) = allow
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(model_id))
    {
        return Some(p.clone());
    }
    // 2. Same filename in any servable model's directory (on disk, not a slot).
    let mut seen = std::collections::HashSet::new();
    for dir in allow.iter().filter_map(|p| p.parent()) {
        if seen.insert(dir.to_path_buf()) {
            let candidate = dir.join(model_id);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
