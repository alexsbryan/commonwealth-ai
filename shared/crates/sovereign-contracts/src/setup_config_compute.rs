// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[compute]` and `[[compute.slot]]` — the supervised compute-child process
//! boundary.
//!
//! Moved out of `setup_config.rs` (pinned at its oversized ceiling) when
//! es-f1-rpc-peer-child gave `[compute]` its `rpc_peer` key. Re-exported
//! there, so every reader still spells these `setup_config::ComputeSection`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
/// `[compute]` — the supervised compute-child process boundary
/// (DISTRIBUTED_PILOT_READINESS.md P1). Off by default: an absent or
/// `[compute]` — the supervised compute-child process boundary
/// (DISTRIBUTED_PILOT_READINESS.md P1). Off by default: an absent or
/// `enabled = false` section spawns no children and changes nothing. When
/// enabled, the daemon spawns each `[[compute.slot]]` as ONE child process
/// (`current_exe() --compute-child …`) and the provider facade routes
/// matching requests to it. There is no N-replica pool — a live embed run
/// showed process replicas lose to in-process batching for a fits-on-one-box
/// model; the boundary is kept for crash isolation + the distributed case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComputeSection {
    /// Master switch. `false` (default) → no children, zero behaviour change.
    #[serde(default)]
    pub enabled: bool,
    /// The compute slots to run — each is exactly one child process.
    #[serde(default)]
    pub slot: Vec<ComputeSlotConfig>,
    /// Run the mesh's DISTRIBUTED primary in a compute child instead of
    /// in-process. `false` (default) keeps the legacy in-daemon path exactly as
    /// it was.
    ///
    /// This is the containment for ggml's uncatchable RPC aborts. Distributing
    /// a primary across mesh workers puts ggml's RPC client in the daemon, and
    /// that client has no error path: a worker that dies mid-decode
    /// (`ggml-rpc.cpp:491`) — or one already gone when the prune reload frees
    /// its buffers (`:386`, which killed the daemon live on 2026-07-27) —
    /// SIGABRTs the whole process, taking gossip, `/status`, and the client API
    /// with it. In a child, that abort kills the child; the daemon observes the
    /// exit and respawns against the surviving workers.
    ///
    /// Requires `enabled = true`. Needs no `[[compute.slot]]` entries: the
    /// primary's model comes from `[models]`, and its worker set from mesh
    /// discovery at runtime.
    #[serde(default)]
    pub distributed_primary: bool,
    /// `ip:port` at which this node lends its GPU to another node's
    /// llama-server: the shipped `ggml-rpc-server -c`, run as a supervised
    /// child (`sovereign_compute::rpc_peer`). A static endpoint the head names
    /// in its own config; no discovery. It must be this host's LAN or tunnel
    /// address: rpc-server authenticates nothing, so a loopback or unspecified
    /// bind is refused at startup. Independent of `enabled`; unset lends
    /// nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_peer: Option<String>,
    /// `[compute.work_offer]` as a config written before pb-work-donor holds
    /// it: the donor and its section moved to cw-rails (`rails.toml`
    /// `[work_offer]`), and `svrn mesh up` hands this table over before it
    /// brings cw-rails up (sovereign-cli-mesh `rail_migration`). Carried
    /// verbatim, never read and never written back once handed over, so a
    /// save before the handover does not drop it. LAST field: a table, and
    /// TOML emits a table's scalars before its sub-tables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_offer: Option<toml::Table>,
}

/// One `[[compute.slot]]` — a single model in a single supervised child.
/// Requests whose `model_id` equals `name` route to the child; an `embed`
/// slot with `capture_embed = true` additionally captures all `/v1/embeddings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeSlotConfig {
    /// Addressable id: a request `model_id` equal to this routes to the
    /// child. Also used as the child `--name`.
    pub name: String,
    /// `"generate"` (EmbeddedLlamaCpp), `"embed"` (EmbedOnlyProvider), or a
    /// served kind's child role (`"rerank"`; `sovereign_inference::served_kind`).
    pub role: String,
    /// GGUF path the child loads.
    pub model: PathBuf,
    /// Context size (generate role). Default 4096 (child-side).
    #[serde(default)]
    pub context_size: Option<u32>,
    /// GPU offload layers. `None` (default) = auto; `Some(0)` = CPU-only.
    #[serde(default)]
    pub n_gpu_layers: Option<u32>,
    /// Spawn this child at daemon boot. Default `true`.
    #[serde(default = "default_true")]
    pub warm: bool,
    /// Embed slots only: when `true`, ALL `/v1/embeddings` traffic routes to
    /// this child (embeddings carry no model id to route on). Default false.
    #[serde(default)]
    pub capture_embed: bool,
}

fn default_true() -> bool {
    true
}
