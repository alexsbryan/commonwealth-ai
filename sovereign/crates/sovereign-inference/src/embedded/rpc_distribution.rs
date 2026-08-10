// SPDX-License-Identifier: AGPL-3.0-or-later
//! RPC distribution + multi-node tensor sharding — the pure planning
//! half of the embedded engine's multi-GPU story: primary sibling
//! pool, RPC worker provider/orchestrator registration (global
//! OnceLock callbacks), device-list pruning, distribution planning,
//! load-placement classification, and the in-process RPC worker
//! server. Zero unsafe beyond FFI device enumeration; no slot state.
//! Extracted verbatim from `model_slot.rs` in the 2026-06-10
//! decomposition (the consuming decode/placement call sites stay in
//! `ModelSlot`); re-exported flat through `embedded/mod.rs` so every
//! `crate::embedded::<Item>` path is unchanged.

#![allow(unused_imports)]
use super::*;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use crate::llama::cpp::context::params::{LlamaContextParams, LlamaContextType};
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaChatMessage, LlamaModel};
use crate::llama::cpp::mtp::MtpSession;
use crate::llama::cpp::sampling::LlamaSampler;
use crate::llama::cpp::token::LlamaToken;
use crate::llama::{LlamaContextExt, LlamaModelExt};

use sovereign_core::error::Error;
use sovereign_core::model_family::{
    EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

/// Operator-declared primary-slot sibling count.
///
/// Set `SOVEREIGN_PRIMARY_SIBLINGS=N` with N≥2 to eager-load N
/// independent primary `LlamaContext`s sharing one `Arc<LlamaModel>`
/// (weights are not duplicated; each sibling pays only its own KV
/// cache). Non-streaming chat-completion dispatch then round-robins
/// across siblings so two callers can actually generate in parallel
/// instead of serialising on the lazy slot's single `Mutex<Context>`.
///
/// Returns `None` for absent / unparseable / `0` / `1` — in any of
/// those cases the daemon keeps its single-context lazy behaviour
/// and the change is a no-op.
///
/// Scope (intentional, experimental):
///   - Sibling pool covers `SlotTarget::Primary` non-streaming
///     completion only. Streaming, embed, rerank, and the lazy
///     warm-load helper continue to use the single lazy slot.
///   - Incompatible with `code_path`: the Code specialist relies on
///     hot-swapping the lazy slot's one resident model. Mixing the
///     two would require hot-swapping every sibling on a hint
///     transition. Startup fails with a clear message when both are
///     configured.
pub(crate) fn parse_primary_siblings(raw: Option<&str>) -> Option<std::num::NonZeroU32> {
    let n: u32 = raw?.trim().parse().ok()?;
    if n <= 1 {
        return None;
    }
    std::num::NonZeroU32::new(n)
}

pub(crate) fn primary_siblings_env() -> Option<std::num::NonZeroU32> {
    parse_primary_siblings(std::env::var("SOVEREIGN_PRIMARY_SIBLINGS").ok().as_deref())
}

/// Eager-loaded pool of primary contexts. All siblings share one
/// `Arc<LlamaModel>` (no weight duplication); each owns its own
/// `LlamaContext` + `Mutex<SlotContext>` + `inflight` permit, so
/// `pool.len()` callers can be in `generate_sync` concurrently.
///
/// Memory cost is therefore additive in KV cache only — for a
/// 35B-A3B Q4 at `n_ctx=32768`, that's a few GB per sibling on top
/// of the one-time ~22 GB weight load. On Strix Halo Vulkan
/// (124 GiB GTT) 2–4 siblings is comfortable; tighter hardware
/// should keep N low and watch resident-size in `daemon.out`.
///
/// Dispatch picks siblings round-robin. A more sophisticated
/// "least-loaded" policy could read each sibling's
/// `inflight.available_permits()`, but for the SEP-ingest workload
/// (uniform per-call cost) round-robin is already optimal and the
/// atomic counter is wait-free.
pub(crate) struct PrimarySiblingPool {
    pub(crate) slots: Vec<Arc<ModelSlot>>,
    pub(crate) next: std::sync::atomic::AtomicUsize,
}

impl PrimarySiblingPool {
    pub(crate) fn pick(&self) -> (usize, Arc<ModelSlot>) {
        let i = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.slots.len();
        (i, Arc::clone(&self.slots[i]))
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }
}

/// Parse `SOVEREIGN_RPC_TENSOR_SPLIT` (comma-separated per-device fractions, in
/// llama.cpp device order: RPC workers first, then local GPUs) into a split
/// vector. Returns `None` when unset/empty so llama.cpp keeps its default
/// memory-proportional split.
pub(crate) fn rpc_tensor_split() -> Option<Vec<f32>> {
    let raw = std::env::var("SOVEREIGN_RPC_TENSOR_SPLIT").ok()?;
    let split: Vec<f32> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<f32>().ok())
        .collect();
    (!split.is_empty()).then_some(split)
}

/// Injected source of mesh RPC worker endpoints (`host:port`), e.g. the daemon's
/// auto-discovery via peer `/status`. Consulted on every model load in addition
/// to `SOVEREIGN_RPC_WORKERS`, so a host needs no manual worker list. Set once at
/// daemon startup — this keeps the engine decoupled from the mesh crate.
type RpcWorkerProvider = Box<dyn Fn() -> Vec<String> + Send + Sync>;
static RPC_WORKER_PROVIDER: std::sync::OnceLock<RpcWorkerProvider> = std::sync::OnceLock::new();

/// Inject an auto-discovery source for mesh RPC workers. The daemon wires this to
/// a peer-`/status` scan so a host node needs no manual `SOVEREIGN_RPC_WORKERS`.
pub fn set_rpc_worker_provider(provider: impl Fn() -> Vec<String> + Send + Sync + 'static) {
    if RPC_WORKER_PROVIDER.set(Box::new(provider)).is_err() {
        tracing::warn!("RPC worker provider already set — ignoring duplicate");
    }
}

/// The mesh RPC worker endpoints (`host:port`) for this load: the union of
/// `SOVEREIGN_RPC_WORKERS` (manual, comma-separated) and the injected
/// auto-discovery provider. The single source every distribution decision reads
/// — registration, dead-worker pruning, the has-workers gate — so they can't
/// disagree about which workers exist.
fn gather_rpc_endpoints() -> Vec<String> {
    let mut endpoints: Vec<String> = Vec::new();
    if let Ok(raw) = std::env::var("SOVEREIGN_RPC_WORKERS") {
        endpoints.extend(
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    if let Some(provider) = RPC_WORKER_PROVIDER.get() {
        endpoints.extend(provider());
    }
    endpoints
}

/// Are any mesh RPC workers available to distribute to? Gate read before paying
/// to compute a plan or warm a shard — no workers means an ordinary local load.
fn rpc_workers_present() -> bool {
    !gather_rpc_endpoints().is_empty()
}

/// One worker's warm assignment under a distributed primary load: its RPC
/// endpoint (`host:port`) and the device index it occupies in the RPC-first
/// plan. The orchestrator turns each into a peer call — "warm device `i` of
/// model M with this plan."
#[derive(Debug, Clone)]
pub struct RpcWarmAssignment {
    pub endpoint: String,
    pub device_index: usize,
}

/// Everything the injected orchestrator needs to seed every worker's shard
/// before a distributed override-load: the host's model file, the ONE plan
/// (shipped whole, so warm-time placement and the load-time `-ot` overrides
/// derive from the identical assignment and cannot diverge), and the per-worker
/// device assignments.
#[derive(Debug, Clone)]
pub struct RpcWarmPlan {
    pub model_path: PathBuf,
    pub plan: Vec<NodeShard>,
    pub assignments: Vec<RpcWarmAssignment>,
}

/// Injected auto-warm orchestrator. Given the plan, ensure EVERY worker has
/// warmed its shard (fetched + cached its slice of the GGUF) so the host's
/// subsequent `-ot` load is all `SET_TENSOR_HASH` cache hits — no bulk weight
/// send, so no upload deadlock. Blocking: it's called from the load's blocking
/// context and must not return until the workers are warm (or it gives up).
/// Returns `Ok(())` iff every worker is warm; an `Err` means "do NOT distribute
/// — fall back to a local-only load" (never wedge). The daemon wires this to a
/// peer-`/internal/rpc-warm` fan-out; sovereign-inference stays decoupled from
/// the mesh/HTTP crate, exactly as with [`set_rpc_worker_provider`].
type RpcWarmOrchestrator =
    Box<dyn Fn(&RpcWarmPlan) -> std::result::Result<(), String> + Send + Sync>;
static RPC_WARM_ORCHESTRATOR: std::sync::OnceLock<RpcWarmOrchestrator> = std::sync::OnceLock::new();

/// Inject the auto-warm orchestrator (once, at daemon startup). This is what
/// retires the manual `SOVEREIGN_RPC_ASSUME_WARMED` for the common case: a large
/// primary now seeds each worker's shard automatically, then loads across them.
pub fn set_rpc_warm_orchestrator(
    orchestrator: impl Fn(&RpcWarmPlan) -> std::result::Result<(), String> + Send + Sync + 'static,
) {
    if RPC_WARM_ORCHESTRATOR.set(Box::new(orchestrator)).is_err() {
        tracing::warn!("RPC warm orchestrator already set — ignoring duplicate");
    }
}

/// Endpoints already published into ggml's device registry this process.
/// `add_server` dedupes by endpoint, but `ggml_backend_register` must run at most
/// once per reg or the device double-appears — so we track and skip.
static REGISTERED_RPC: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Register mesh RPC workers into ggml's global backend-device registry so the
/// next `LlamaModel::load_from_file` spreads the model's layers across the local
/// GPU **and** those workers (llama.cpp pipeline-parallel RPC) — the embedded-host
/// equivalent of `llama-server --rpc <addrs>`.
///
/// Endpoints are the union of `SOVEREIGN_RPC_WORKERS` (manual, comma-separated
/// `host:port`) and the injected auto-discovery provider ([`set_rpc_worker_provider`],
/// fed by the daemon's peer-`/status` scan) — so the manual list is optional.
/// Each endpoint registers at most once per process; peers discovered later
/// register on the next load. Split is by advertised VRAM (default), overridable
/// with `SOVEREIGN_RPC_TENSOR_SPLIT`. No sources → no-op → unchanged local load.
/// Glassbox: every endpoint logs success or a skip-with-reason.
fn register_rpc_workers() {
    let endpoints = gather_rpc_endpoints();
    if endpoints.is_empty() {
        return;
    }

    let mut registered = REGISTERED_RPC.lock().unwrap_or_else(|e| e.into_inner());
    for endpoint in endpoints {
        if registered.iter().any(|e| e == &endpoint) {
            continue;
        }
        let c_endpoint = match std::ffi::CString::new(endpoint.clone()) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(endpoint, "RPC endpoint contains an interior NUL — skipping");
                continue;
            }
        };
        // SAFETY: `ggml_backend_rpc_add_server` connects to the worker and returns
        // a backend reg (NULL on unreachable / zero devices), deduped by endpoint;
        // `ggml_backend_register` publishes its devices into ggml's global registry
        // where the loader's default device selection picks them up as GPU devices
        // named "RPC" and splits layers across local GPU + workers.
        let reg = unsafe { crate::llama::sys::ggml_backend_rpc_add_server(c_endpoint.as_ptr()) };
        if reg.is_null() {
            tracing::warn!(
                endpoint,
                "rpc-server unreachable or advertised 0 devices — skipping this worker"
            );
            continue;
        }
        unsafe { crate::llama::sys::ggml_backend_register(reg) };
        registered.push(endpoint.clone());
        // Glassbox: the worker's advertised device memory (drives the default split).
        let (mut free, mut total): (usize, usize) = (0, 0);
        unsafe {
            crate::llama::sys::ggml_backend_rpc_get_device_memory(
                c_endpoint.as_ptr(),
                0,
                &mut free,
                &mut total,
            );
        }
        tracing::info!(
            endpoint,
            free_gb = free as f64 / 1e9,
            total_gb = total as f64 / 1e9,
            "registered mesh RPC device for distributed inference"
        );
    }
}

/// When a previously-registered RPC worker is no longer live (its endpoint left
/// the env+provider set — the peer died or dropped off the mesh), the model must
/// not load across its now-dead device. ggml has no unregister, so we build an
/// explicit device list of only the LIVE devices (local GPU + live RPC) and pass
/// it via `with_devices`, pruning the dead one. Returns `None` when no pruning is
/// needed (no dead RPC device) so the caller keeps the proven NULL-`devices`
/// auto-enumeration path. Device order matches llama.cpp's NULL path — RPC first,
/// then local GPU — so `SOVEREIGN_RPC_TENSOR_SPLIT` semantics are unchanged.
pub(crate) fn live_device_list_if_pruning_needed(
) -> Option<Vec<crate::llama::sys::ggml_backend_dev_t>> {
    // Live endpoints = env ∪ provider — the same source register_rpc_workers uses.
    let live: std::collections::HashSet<String> = gather_rpc_endpoints().into_iter().collect();

    let n = unsafe { crate::llama::sys::ggml_backend_dev_count() };
    let mut rpc_live: Vec<crate::llama::sys::ggml_backend_dev_t> = Vec::new();
    let mut local_gpu: Vec<crate::llama::sys::ggml_backend_dev_t> = Vec::new();
    let mut dead = 0usize;
    for i in 0..n {
        let dev = unsafe { crate::llama::sys::ggml_backend_dev_get(i) };
        let dtype = unsafe { crate::llama::sys::ggml_backend_dev_type(dev) };
        // Keep GPU (1) and IGPU (2) — a Strix Halo host GPU reports as IGPU.
        // CPU / accel / meta are handled separately by llama.cpp.
        if dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_CPU
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_ACCEL
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_META
        {
            continue;
        }
        let reg = unsafe { crate::llama::sys::ggml_backend_dev_backend_reg(dev) };
        let reg_name =
            unsafe { std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_reg_name(reg)) }
                .to_string_lossy();
        if reg_name == "RPC" {
            // The RPC device's description is the endpoint it was registered with.
            let desc = unsafe {
                std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_dev_description(dev))
            }
            .to_string_lossy()
            .into_owned();
            if live.contains(&desc) {
                rpc_live.push(dev);
            } else {
                dead += 1;
            }
        } else {
            local_gpu.push(dev);
        }
    }

    if dead == 0 {
        return None; // nothing dead → keep the proven NULL-devices path
    }
    tracing::warn!(
        dead,
        live_rpc = rpc_live.len(),
        local_gpu = local_gpu.len(),
        "pruning dead RPC worker(s) from the model device set"
    );
    // RPC devices first, then local GPU — matches llama.cpp's NULL-path order.
    let mut list = rpc_live;
    list.extend(local_gpu);
    Some(list)
}

/// Conservative safety floor for distributed loads. Streaming a model's weight
/// share to a remote RPC worker deadlocks the host in `send()` above ~800MB
/// (observed root cause). Until each worker's shard is warmed — so the host skips
/// the bulk send via `SET_TENSOR_HASH` hits — we only stream-distribute models
/// small enough that even a 100% share can't approach the deadlock. Override the
/// threshold with `SOVEREIGN_RPC_SAFE_STREAM_MB`.
const SAFE_RPC_STREAM_BYTES: u64 = 512 * 1024 * 1024;

fn safe_rpc_stream_bytes() -> u64 {
    std::env::var("SOVEREIGN_RPC_SAFE_STREAM_MB")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(SAFE_RPC_STREAM_BYTES)
}

/// The operator/orchestrator asserts the discovered workers' shards are already
/// warm (so a distributed load is all cache-hits, no bulk send). Set by the
/// auto-warm orchestration once shards are seeded; a manual escape hatch until then.
fn rpc_assume_warmed() -> bool {
    std::env::var("SOVEREIGN_RPC_ASSUME_WARMED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Pure decision: is it safe to stream-distribute a `model_bytes` model to remote
/// workers without risking the `send()` deadlock? Safe iff small enough that any
/// share stays under the floor, or the caller asserts the workers are warm.
fn rpc_distribution_safe_decision(model_bytes: u64, safe_bytes: u64, assume_warmed: bool) -> bool {
    assume_warmed || model_bytes <= safe_bytes
}

/// Total physical device memory (GB) of the LOCAL GPU/IGPU backends, summed via
/// `ggml_backend_dev_memory` — the authoritative, backend-agnostic VRAM figure.
/// This is the number the mesh advertises for `svrn mesh plan --from-mesh`:
/// sysfs under-reports unified-memory AMD APUs (it sees only the tiny dedicated
/// VRAM carveout, e.g. 0.5 GB on Strix Halo, while ggml reports the real ~128 GB
/// usable pool). `None` when there is no local GPU/IGPU device (CPU-only node)
/// or ggml isn't initialized yet. Remote RPC devices are excluded. Cheap FFI.
pub fn local_gpu_total_vram_gb() -> Option<u32> {
    let n = unsafe { crate::llama::sys::ggml_backend_dev_count() };
    let mut total_bytes: u64 = 0;
    let mut found_local_gpu = false;
    for i in 0..n {
        let dev = unsafe { crate::llama::sys::ggml_backend_dev_get(i) };
        let dtype = unsafe { crate::llama::sys::ggml_backend_dev_type(dev) };
        if dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_CPU
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_ACCEL
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_META
        {
            continue;
        }
        let reg = unsafe { crate::llama::sys::ggml_backend_dev_backend_reg(dev) };
        let reg_name =
            unsafe { std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_reg_name(reg)) }
                .to_string_lossy();
        if reg_name == "RPC" {
            continue; // a remote worker's device, not this node's
        }
        let (mut free, mut total): (usize, usize) = (0, 0);
        unsafe { crate::llama::sys::ggml_backend_dev_memory(dev, &mut free, &mut total) };
        if total > 0 {
            total_bytes += total as u64;
            found_local_gpu = true;
        }
    }
    if found_local_gpu {
        Some((total_bytes / (1024 * 1024 * 1024)) as u32)
    } else {
        None
    }
}

/// The local (non-RPC) GPU devices, in registry order. Used to force a load onto
/// the local GPU only — robust even when an RPC worker was registered by a prior
/// (smaller) load and still sits in ggml's global device registry.
pub(crate) fn local_gpu_device_list() -> Vec<crate::llama::sys::ggml_backend_dev_t> {
    let n = unsafe { crate::llama::sys::ggml_backend_dev_count() };
    let mut local = Vec::new();
    for i in 0..n {
        let dev = unsafe { crate::llama::sys::ggml_backend_dev_get(i) };
        let dtype = unsafe { crate::llama::sys::ggml_backend_dev_type(dev) };
        if dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_CPU
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_ACCEL
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_META
        {
            continue;
        }
        let reg = unsafe { crate::llama::sys::ggml_backend_dev_backend_reg(dev) };
        let reg_name =
            unsafe { std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_reg_name(reg)) }
                .to_string_lossy();
        if reg_name != "RPC" {
            local.push(dev);
        }
    }
    local
}

/// A fully-resolved distributed placement for one model load: the explicit
/// RPC-first device list, the per-block `-ot` overrides that ENFORCE it, the
/// underlying shard `plan` (shipped to workers so warm-time placement equals
/// load-time placement), and the per-worker warm assignments (endpoint ↔ device
/// index). Computed ONCE and consumed for both warming and loading, so the two
/// cannot disagree — the plan-agreement invariant that keeps every distributed
/// weight a `SET_TENSOR_HASH` cache hit (no bulk send, no deadlock).
pub(crate) struct DistributionPlan {
    pub(crate) devs: Vec<crate::llama::sys::ggml_backend_dev_t>,
    pub(crate) overrides: Vec<(
        std::ffi::CString,
        crate::llama::sys::ggml_backend_buffer_type_t,
    )>,
    pub(crate) plan: Vec<NodeShard>,
    /// The `tensor_split` that pins llama.cpp's per-layer device map
    /// (`model.dev_layer`) to the same cut as the `-ot` overrides. MUST be
    /// applied together with `overrides`: dev_layer is computed from
    /// tensor_split over `n_layer + 1` units and never consults overrides, and
    /// a disagreement straddles a layer across devices — on a hybrid (Gated
    /// DeltaNet) model that disables the fused GDN kernel and the unfused
    /// GGML_OP_SET path aborts the host on first distributed decode
    /// (docs/DISTRIBUTED_GDN_CRASH_STATUS.md). See [`dev_layer_tensor_split`].
    pub(crate) tensor_split: Vec<f32>,
    pub(crate) assignments: Vec<RpcWarmAssignment>,
    /// Eligible RPC worker peers (anchors lending memory) this plan
    /// distributes onto — the quorum-count input for the shared-model gate.
    pub(crate) eligible_workers: usize,
    /// Per-device memory (bytes), in **plan order** — RPC workers first, the
    /// local GPU last, the same order `plan` indexes.
    ///
    /// Kept per-device rather than pre-summed because the sum answers the wrong
    /// question. Pooled memory says the cluster could hold the model *somewhere*;
    /// it says nothing about whether the device each block was actually assigned
    /// to can hold its share. A plan can pass the pooled gate comfortably and
    /// still hand one worker more than it has — which is what
    /// [`shard_fits`] now catches. The sum is still available (`.iter().sum()`)
    /// and is still what the quorum gate checks.
    pub(crate) device_vram_bytes: Vec<u64>,
    /// The model's byte mass, when the tensor table could be read.
    ///
    /// Computed unconditionally, **before** the plan-cache branch, so a cached
    /// plan is judged against the same mass a freshly-computed one is. `None`
    /// when the header parse failed — the apportionment then falls back to a
    /// count split and the per-device fit check honestly reports that it cannot
    /// judge, rather than clearing every device against zeros.
    pub(crate) mass: Option<ModelMass>,
    /// llama.cpp's projected non-weight terms (KV + compute per device) at the
    /// load's context size — see [`projected_overheads`]. `None` when the
    /// projection failed; the fit gate then judges on weights × headroom
    /// alone, exactly as it did before 2026-08-02.
    pub(crate) overheads: Option<PlanOverheads>,
}

/// Process-wide cache of the shard plan, keyed by `(model_id, sorted RPC
/// endpoints)`. The plan was recomputed from LIVE free VRAM on every reload, but a
/// worker's free VRAM swings by its loaded shard size between reloads, so the
/// split drifted — invalidating workers' warm caches (`already=20` vs `36`).
/// Keying by the model + the stable worker set means a reload across the same
/// workers reuses the exact assignment; a real topology change is a new key →
/// recompute.
fn plan_cache() -> &'static std::sync::Mutex<HashMap<(String, Vec<String>), Vec<NodeShard>>> {
    static C: std::sync::OnceLock<
        std::sync::Mutex<HashMap<(String, Vec<String>), Vec<NodeShard>>>,
    > = std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Compute the distributed placement for `model_path`. Enumerates the RPC-first
/// device set, weights the per-block split by each device's advertised VRAM, runs
/// our `plan_shards` policy, and derives BOTH the `(regex, buffer_type)` overrides
/// llama.cpp honors verbatim AND the per-worker warm assignments from the same
/// `plan`. Placement is OURS (no prediction of llama.cpp's split), and warm + load
/// read the identical plan, so a worker's cached shard is exactly what the host
/// pins onto it.
///
/// The endpoint for each RPC device comes from `REGISTERED_RPC` in registration
/// order — which is the order ggml appends RPC devices to its registry — so RPC
/// device index `i` (the plan's device order) is `REGISTERED_RPC[i]`. Only RPC
/// devices get a warm assignment; the local GPU (`devs[n_rpc..]`) loads its shard
/// straight from the host's own GGUF and needs no cache.
///
/// `None` when there is no RPC worker to distribute to, the GGUF's block count
/// can't be read, or an RPC device can't be mapped to an endpoint — in every case
/// the caller falls back to a local-only load (never wedge).
/// One device a distributed load would place blocks on, with its memory as ggml
/// reports it right now.
///
/// The two figures answer two different questions and must not be collapsed:
/// `total_bytes` is what the silicon could hold if nothing else were resident (a
/// durable hardware fact), `free_bytes` is what is available at this instant (a
/// reading that moves as other work loads and unloads). The fit gate consumes
/// `free`-else-`total` as a single `capacity_bytes`, which is precisely why a
/// refusal used to be undiagnosable — see [`placed_devices`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceMemory {
    /// The RPC worker endpoint this device lives behind; `None` for a local
    /// (host) device.
    pub endpoint: Option<String>,
    pub free_bytes: u64,
    pub total_bytes: u64,
    /// What the OWNING node keeps for itself and will not lend — see
    /// [`host_reserve_bytes`]. Carried on the reading rather than applied by the
    /// caller so that every consumer of this device (apportioner, per-device fit
    /// gate, pooled gate, `mesh plan` preview) subtracts the same amount; a
    /// reserve applied at some call sites and not others is the "apportion from
    /// one number, check against another" bug this struct exists to prevent.
    ///
    /// Populated for THIS node's own devices. Remote devices carry `0` until a
    /// peer's declared reserve travels over the mesh status channel — the host
    /// must not invent a reserve on a peer's behalf.
    pub reserve_bytes: u64,
}

impl DeviceMemory {
    /// The single number the fit gate judges against: live free when ggml
    /// reports one, else the device total, less the owning node's reserve.
    ///
    /// A backend that declines to report free memory yields 0, and 0 would fail
    /// every device — so total is the only safe fallback. Keep this the ONLY
    /// place the free-else-total rule is written: a planner that previews a load
    /// and a loader that performs it must collapse the two figures identically,
    /// or the preview describes a cut the loader would not make.
    pub fn capacity_bytes(&self) -> u64 {
        let raw = if self.free_bytes > 0 {
            self.free_bytes
        } else {
            self.total_bytes
        };
        raw.saturating_sub(self.reserve_bytes)
    }

    /// Memory held by work other than the load being planned.
    pub fn held_by_others_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

/// The devices a distributed load would place across, in PLAN ORDER (eligible
/// RPC workers first, local GPU last), each with its live memory.
///
/// This is the loader's own oracle, factored out so a *preview* can quote it
/// rather than approximate it. Approximating it is what made `svrn mesh plan`
/// disagree with the load it previews: the plan read the gossiped device TOTAL
/// and apportioned 14/34, while the loader read live FREE and ran 12/36 — two
/// different cuts, so the measurement `mesh bench` had just recorded could never
/// be found under the plan's key. Same function, same numbers, same cut.
///
/// BOTH figures are carried per device, not just the one the gate uses. The gate
/// reports a single `capacity_mb` with the source string "ggml_backend_dev_memory
/// (free, else total)", which cannot distinguish "this device is small" from
/// "this device is momentarily busy" — and those demand opposite responses. A
/// refusal on 2026-07-29 (worker assigned 12 blocks / 21430 MiB,
/// capacity_mb=20000) was unresolvable from the logs for exactly that reason:
/// 20000 could have been a 20 GB device, or a 51 GB device with 31 GB
/// transiently held by the outgoing generation. It was the latter.
///
/// `None` when there is nothing to distribute to: no RPC device is registered,
/// none is currently eligible, or a device cannot be mapped to an endpoint.
fn placed_devices() -> Option<Vec<(crate::llama::sys::ggml_backend_dev_t, DeviceMemory)>> {
    // Ordered device set: RPC workers first, then local GPU — the order
    // `plan_shards` indexes and `with_devices` expects.
    let count = unsafe { crate::llama::sys::ggml_backend_dev_count() };
    let mut rpc = Vec::new();
    let mut local = Vec::new();
    for i in 0..count {
        let dev = unsafe { crate::llama::sys::ggml_backend_dev_get(i) };
        let dtype = unsafe { crate::llama::sys::ggml_backend_dev_type(dev) };
        if dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_CPU
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_ACCEL
            || dtype == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_META
        {
            continue;
        }
        let reg = unsafe { crate::llama::sys::ggml_backend_dev_backend_reg(dev) };
        let reg_name =
            unsafe { std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_reg_name(reg)) }
                .to_string_lossy();
        if reg_name == "RPC" {
            rpc.push(dev);
        } else {
            local.push(dev);
        }
    }
    let n_rpc = rpc.len();
    if n_rpc == 0 {
        return None; // nothing to distribute to — not a distributed load
    }

    // Map each RPC device to its endpoint, then KEEP ONLY ELIGIBLE workers. ggml
    // appends RPC devices in registration order (the order pushed to
    // REGISTERED_RPC), so the k-th enumerated RPC device is REGISTERED_RPC[k]. A
    // worker the eligibility gate quarantined is dropped from the provider, but its
    // ggml device lingers (ggml has no unregister) — so we filter to the live
    // (env ∪ provider = eligible) endpoints and RENUMBER, exactly as the stream
    // path's `live_device_list_if_pruning_needed` does, so OwnedOverrides never
    // pins a shard onto a quarantined worker. (One device per worker: if a worker
    // advertised several, n_rpc would exceed the endpoint count → decline.)
    let registered = REGISTERED_RPC.lock().unwrap_or_else(|e| e.into_inner());
    if registered.len() < n_rpc {
        tracing::warn!(
            rpc_devices = n_rpc,
            registered = registered.len(),
            "cannot map every RPC device to an endpoint (multi-device worker?) — not distributing"
        );
        return None;
    }
    let live: std::collections::HashSet<String> = gather_rpc_endpoints().into_iter().collect();
    let eligible_rpc: Vec<(crate::llama::sys::ggml_backend_dev_t, String)> = rpc
        .into_iter()
        .zip(registered.iter().cloned())
        .filter(|(_, ep)| live.contains(ep))
        .collect();
    drop(registered);
    if eligible_rpc.is_empty() {
        return None; // every RPC device is ineligible (quarantined/left) → local-only
    }

    // RPC-first device list (eligible only), then local GPU — the order
    // `plan_shards`/`with_devices` index, and the order the warm assignments
    // renumber against.
    let placed: Vec<(crate::llama::sys::ggml_backend_dev_t, Option<String>)> = eligible_rpc
        .into_iter()
        .map(|(d, ep)| (d, Some(ep)))
        .chain(local.into_iter().map(|d| (d, None)))
        .collect();

    let read: Vec<(crate::llama::sys::ggml_backend_dev_t, DeviceMemory)> = placed
        .into_iter()
        .map(|(d, endpoint)| {
            let (mut free, mut total): (usize, usize) = (0, 0);
            unsafe { crate::llama::sys::ggml_backend_dev_memory(d, &mut free, &mut total) };
            // Only THIS node's devices carry this node's reserve. A peer's
            // reserve is the peer's to declare; inventing one here would apportion
            // against a budget the peer never agreed to.
            let reserve_bytes = match endpoint {
                None => host_reserve_bytes_detected(total as u64),
                Some(_) => 0,
            };
            (
                d,
                DeviceMemory {
                    endpoint,
                    free_bytes: free as u64,
                    total_bytes: total as u64,
                    reserve_bytes,
                },
            )
        })
        .collect();

    // Publish the reading for out-of-band readers (`/v1/mesh/status`, and through
    // it `svrn mesh plan`). Cached rather than re-sampled on demand because this
    // read can round-trip to a busy worker — see `last_device_memory`.
    *device_memory_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(DeviceMemorySnapshot {
        observed_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        devices: read.iter().map(|(_, m)| m.clone()).collect(),
    });

    Some(read)
}

/// The raw `SOVEREIGN_RPC_BLOCK_SPLIT` value when the operator has pinned the
/// per-device block split, else `None`.
///
/// Published so a PREVIEW can tell the truth. When this is set the loader ignores
/// VRAM apportionment entirely (see the `explicit` branch in `plan_distribution`),
/// so a `svrn mesh plan` that derives a cut from capacities describes a split that
/// will never load. On this host that was a silent 14/34-vs-12/36 disagreement:
/// the plan looked right, the load did something else, and the measurement filed
/// under the real cut could not be found from the plan's key.
///
/// Deliberately raw rather than parsed: parsing needs the model's block count and
/// the device count, which only the caller knows. Both sides then run the SAME
/// [`parse_block_split`](crate::embedded::parse_block_split) over the same string,
/// so they cannot disagree about whether a pin is valid — including agreeing to
/// ignore a malformed one.
pub fn pinned_block_split_raw() -> Option<String> {
    std::env::var("SOVEREIGN_RPC_BLOCK_SPLIT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The per-device memory the loader last observed, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceMemorySnapshot {
    /// Unix seconds at which [`placed_devices`] took this reading.
    ///
    /// Carried because the reading is an OBSERVATION, not a live value, and a
    /// consumer that cannot see its age would present a stale number as current.
    pub observed_unix: u64,
    /// Plan order: eligible RPC workers first, local GPU last.
    pub devices: Vec<DeviceMemory>,
}

fn device_memory_cache() -> &'static std::sync::Mutex<Option<DeviceMemorySnapshot>> {
    static C: std::sync::OnceLock<std::sync::Mutex<Option<DeviceMemorySnapshot>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(None))
}

/// The per-device memory the loader read the last time it planned a distributed
/// load. `None` before any distributed load has been planned in this process.
///
/// **This deliberately does NOT sample.** `ggml_backend_dev_memory` on an RPC
/// device is a synchronous round-trip to the worker, and a worker busy serving a
/// resident model can take arbitrarily long to answer it — a 122B decoding on
/// BeefyMac made `/v1/mesh/status` hang past 70s on 2026-07-30, which in turn
/// broke `svrn mesh bench` (it reads that endpoint to identify the mesh). A status
/// endpoint must not block on a remote, busy resource, so the read happens where
/// it is already being paid for — on the load path — and every reader gets the
/// cached observation with its timestamp.
///
/// The cached value is also the MORE correct thing to publish: "the memory the
/// loader saw when it chose this cut" is exactly what explains the cut, whereas a
/// fresh sample describes a moment the load never saw.
pub fn last_device_memory() -> Option<DeviceMemorySnapshot> {
    device_memory_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// llama.cpp's own three-term memory projection (weights / KV / compute) for
/// this model at the load's context size — the real numbers behind
/// [`PlanOverheads`], replacing the hand-rolled proxies that either charged
/// nothing (`shard_fits` had no context or compute term: the 2026-08-02
/// 0.07 GiB-margin loads reached `serving` and died on first inference) or
/// charged wrong (`weights/8` over-estimated an MLA model's KV ~5× and
/// refused loads that fit).
///
/// `None` = "could not project", and callers MUST treat it as "judge without
/// these terms" (the pre-projection behaviour), never as a refusal — a failed
/// estimate must not brick a load.
///
/// Cost: measured ~278 ms warm / <1 s cold on a 155 GB 5-shard GGUF
/// (`tests/device_memory_probe.rs`) — the model is loaded `no_alloc` and freed
/// before returning. Successes are cached per `(path, n_ctx, n_ubatch)`
/// because the projection is deterministic in those inputs; failures are NOT
/// cached, so a transient error (backend not ready yet) is retried on the
/// next plan rather than pinning "no projection" for the daemon's life.
pub fn projected_overheads(model_path: &Path, n_ctx: u32) -> Option<PlanOverheads> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<(PathBuf, u32, u32), PlanOverheads>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);

    // The SAME micro-batch the slot will load with — the compute buffer
    // scales with it, so projecting at a different ubatch would size the
    // margin for a load that never happens.
    let n_ubatch = super::prompt_helpers::chat_slot_n_ubatch();
    let key = (model_path.to_path_buf(), n_ctx, n_ubatch);
    if let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Some(*hit);
    }

    let t0 = std::time::Instant::now();
    let mparams = LlamaModelParams::default().with_n_gpu_layers(999);
    let cparams = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx.max(1)))
        .with_n_ubatch(n_ubatch);
    let report = match crate::llama::cpp::fit::get_device_memory_data(
        model_path,
        &mparams,
        &cparams,
        crate::llama::sys::GGML_LOG_LEVEL_ERROR,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                model = %model_path.display(),
                "projected_overheads: device-memory projection failed — fit gates \
                 will judge without KV/compute terms this plan (not cached; retried \
                 on the next plan)"
            );
            return None;
        }
    };

    // Device order is ggml's registry order: accelerators first, the host CPU
    // buffer last (observed as `Vulkan0, Vulkan_Host` in the probe). The KV
    // total is distribution-independent (a layer's cache lives wherever the
    // layer does), so the SUM is chargeable pro-rata by blocks; compute
    // buffers exist per device, so each plan device is charged the largest
    // accelerator buffer, and the host additionally its CPU-side scheduler
    // buffer.
    let n = report.entries.len();
    let context_total: u64 = report.entries.iter().map(|e| e.context as u64).sum();
    let compute_accel = report
        .entries
        .iter()
        .take(n.saturating_sub(1))
        .map(|e| e.compute as u64)
        .max()
        .unwrap_or(0);
    let compute_host = report.entries.last().map(|e| e.compute as u64).unwrap_or(0);
    let overheads = PlanOverheads {
        context_total_bytes: context_total,
        compute_accel_bytes: compute_accel,
        compute_host_bytes: compute_host,
    };
    const MIB: u64 = 1024 * 1024;
    tracing::info!(
        target: "placement",
        model = %model_path.display(),
        n_ctx,
        n_ubatch,
        elapsed_ms = t0.elapsed().as_millis() as u64,
        context_total_mb = context_total / MIB,
        compute_accel_mb = compute_accel / MIB,
        compute_host_mb = compute_host / MIB,
        devices = n,
        "projected_overheads: llama.cpp three-term projection (KV + compute per device)"
    );
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, overheads);
    Some(overheads)
}

fn plan_distribution(model_path: &Path, n_ctx: u32) -> Option<DistributionPlan> {
    // The device set and its live memory — the same oracle `live_device_memory`
    // publishes to the planner, so a preview and this load cannot disagree about
    // how much room each device has.
    let placed = placed_devices()?;

    let n_layer = match gguf_block_count(model_path) {
        Ok(Some(n)) if n > 0 => n,
        _ => return None, // can't plan deterministically without the layer count
    };

    let assignments: Vec<RpcWarmAssignment> = placed
        .iter()
        .enumerate()
        .filter_map(|(i, (_, m))| {
            m.endpoint.clone().map(|endpoint| RpcWarmAssignment {
                endpoint,
                device_index: i,
            })
        })
        .collect();
    let devs: Vec<crate::llama::sys::ggml_backend_dev_t> =
        placed.iter().map(|(d, _)| *d).collect();
    let device_memory: Vec<DeviceMemory> = placed.into_iter().map(|(_, m)| m).collect();

    for (i, m) in device_memory.iter().enumerate() {
        const MIB: u64 = 1024 * 1024;
        tracing::info!(
            target: "placement",
            device = i,
            endpoint = ?m.endpoint,
            free_mb = m.free_bytes / MIB,
            total_mb = m.total_bytes / MIB,
            held_by_others_mb = m.held_by_others_bytes() / MIB,
            "device memory as ggml reports it (plan order: RPC workers first, host last)"
        );
    }
    // Kept per-device: the sum answers "could the cluster hold this at all",
    // which is not the same question as "can the device this block landed on
    // hold it".
    let device_vram_bytes: Vec<u64> = device_memory
        .iter()
        .map(DeviceMemory::capacity_bytes)
        .collect();
    let eligible_workers = assignments.len();

    // llama.cpp's projection of what the load needs BEYOND the weights. Feeds
    // two decisions below, and both fall back to the old weights-only
    // behaviour when it is `None`: the apportioner (which must not fill a
    // device to a capacity the KV + compute buffers will then fight for) and
    // the per-device fit gate (which used to pass 0.07 GiB margins that died
    // on first inference).
    let overheads = projected_overheads(model_path, n_ctx);

    // The model's byte mass — a GGUF header-table parse, no weight load. Read
    // BEFORE the plan-cache branch so a cache hit and a cache miss are judged
    // against the same numbers; a mass that only existed on the miss path would
    // mean the fit gate silently stopped running after the first load.
    let mass: Option<ModelMass> = match tensor_sizes(model_path) {
        Ok(sizes) => Some(model_mass_from_sizes(&sizes, n_layer)),
        Err(e) => {
            tracing::warn!(
                error = %e,
                model = %model_path.display(),
                "plan_distribution: tensor_sizes failed — falling back to the count-based \
                 split, and the per-device fit check will report that it cannot judge"
            );
            None
        }
    };

    // ONE stable shard plan per (model, worker set). Reuse the cached plan across
    // reloads with the same workers so each worker's warm cache stays valid; only
    // recompute when the model or the eligible worker set actually changes. VRAM is
    // quantized as a belt so a re-plan after a topology change doesn't churn on
    // sub-bucket jitter.
    let model_id = model_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut rpc_endpoints: Vec<String> = assignments.iter().map(|a| a.endpoint.clone()).collect();
    rpc_endpoints.sort();
    let key = (model_id, rpc_endpoints);
    let plan: Vec<NodeShard> = {
        let mut cache = plan_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = cache.get(&key) {
            tracing::debug!(model = %key.0, "plan_distribution: reusing cached shard plan (stable worker set)");
            cached.clone()
        } else {
            // Weight by advertised free VRAM (host RAM for a CPU-backed worker),
            // quantized to coarse buckets. Derived from the SAME `device_memory`
            // snapshot the fit gate judges against — re-reading here would let
            // the split be apportioned from one reading and checked against
            // another, so a device could be handed a share it is then refused
            // for, with neither number visible as wrong.
            //
            // Apportion against EFFECTIVE capacity: what remains after each
            // device's projected compute scratch and (pro-rata) KV share.
            // Without this, the split fills devices to their raw capacity and
            // the corrected fit gate below then refuses the very plan we just
            // computed — the fix would have turned "dies at first inference"
            // into "refuses to load". The KV share is apportioned by capacity
            // fraction as a first-order stand-in for the block fraction it
            // becomes; on a 1–2 GiB context total the circularity is not worth
            // an iteration loop.
            let total_capacity: u64 = device_vram_bytes.iter().sum();
            let effective = |i: usize, &b: &u64| -> u64 {
                let Some(o) = overheads.as_ref() else { return b };
                let ctx_share = if total_capacity == 0 {
                    0
                } else {
                    (o.context_total_bytes as u128 * b as u128 / total_capacity as u128) as u64
                };
                let host_extra = if i + 1 == device_vram_bytes.len() {
                    o.compute_host_bytes
                } else {
                    0
                };
                b.saturating_sub(o.compute_accel_bytes + host_extra + ctx_share)
            };
            let weights: Vec<f32> = device_vram_bytes
                .iter()
                .enumerate()
                .map(|(i, b)| quantize_vram(effective(i, b)) as f32)
                .collect();
            // Byte-mass-aware split: overlay the model's REAL per-block byte mass
            // so a non-uniform (MoE / hybrid) model apportions by BYTES, not block
            // count — a count split can hand a small node a heavy contiguous run
            // and OOM it. `mass` was read above; on a header-read failure it is
            // `None` and we fall back to the count split rather than wedge.
            //
            // Byte-mass apportionment is safe on hybrid (recurrent) models too,
            // PROVIDED the load also pins `tensor_split` to the plan (see
            // `DistributionPlan::tensor_split`). An earlier hybrid→count-based
            // forcing here was H3 of the 2026-07-27 crash hunt and was
            // FALSIFIED: count-based produced the identical 8/24 cut and the
            // identical abort, because llama.cpp's per-layer device map follows
            // tensor_split (default: advertised free VRAM over n_layer+1 units),
            // not our overrides — WHICH apportionment rule we use never decided
            // anything.
            let block_bytes: &[u64] = mass.as_ref().map_or(&[], |m| m.block_bytes.as_slice());
            let head_bytes = mass.as_ref().map_or(0, |m| m.head_bytes);
            if mass.as_ref().is_some_and(|m| m.recurrent) {
                tracing::debug!(
                    model = %key.0,
                    "plan_distribution: hybrid/recurrent model (ssm_* blocks) — \
                     byte-mass split, dev_layer pinned via tensor_split"
                );
            }
            let byte_aware = !block_bytes.is_empty();
            // Diagnostic override: `SOVEREIGN_RPC_BLOCK_SPLIT=11,21` pins the
            // per-device block counts (device order: RPC workers first, then
            // local), bypassing the VRAM-derived apportionment. The computed
            // split's cut points follow advertised VRAM, so without this there
            // is no way to AIM the device boundary at a chosen layer — which is
            // what separates "the distributed-decode fault follows a boundary
            // that lands on a Gated DeltaNet layer" from "any RPC boundary
            // faults". A malformed or non-tiling value is REFUSED (warned, then
            // ignored) rather than repaired: a run that silently used a
            // different split than the operator asked for would make the
            // experiment's result meaningless.
            let explicit = pinned_block_split_raw().and_then(|raw| {
                match parse_block_split(&raw, n_layer, weights.len()) {
                    Some(counts) => plan_shards_explicit(n_layer, &weights, &counts),
                    None => {
                        tracing::warn!(
                            raw = %raw,
                            n_layer,
                            devices = weights.len(),
                            "SOVEREIGN_RPC_BLOCK_SPLIT ignored — must be one count per device, \
                             summing to n_layer; falling back to the computed split"
                        );
                        None
                    }
                }
            });
            let computed = match explicit {
                Some(plan) => {
                    tracing::warn!(
                        blocks = ?plan.iter().map(|s| s.blocks).collect::<Vec<_>>(),
                        "SOVEREIGN_RPC_BLOCK_SPLIT active — placement is PINNED by env, \
                         NOT derived from VRAM (diagnostic mode)"
                    );
                    plan
                }
                None => plan_shards_weighted(n_layer, &weights, block_bytes, head_bytes),
            };

            // Glassbox: log the resulting per-device byte balance so an operator
            // can see WHY each node got its range — and spot a residual overflow a
            // contiguous split can't avoid on a very skewed model. Same
            // `shard_fits` the gate below judges on, at headroom 1.0, so the log
            // and the refusal can never describe different numbers.
            if let Some(m) = mass.as_ref() {
                let total_mass = m.block_bytes.iter().sum::<u64>() + m.head_bytes;
                if let Some(fits) =
                    shard_fits(&computed, &device_vram_bytes, m, 1.0, overheads.as_ref())
                {
                    for f in &fits {
                        tracing::info!(
                            device = f.device_index,
                            blocks = ?computed.get(f.device_index).and_then(|s| s.blocks),
                            held_gb = f.held_bytes as f64 / 1.073_741_824e9,
                            overhead_gb = f.overhead_bytes as f64 / 1.073_741_824e9,
                            capacity_gb = f.capacity_bytes as f64 / 1.073_741_824e9,
                            share_pct = if total_mass > 0 { 100.0 * f.held_bytes as f64 / total_mass as f64 } else { 0.0 },
                            vram_weight = weights[f.device_index],
                            "plan_distribution: byte-mass shard"
                        );
                    }
                }
            }
            tracing::info!(model = %key.0, devices = devs.len(), byte_aware, "plan_distribution: computed new shard plan");
            cache.insert(key.clone(), computed.clone());
            computed
        }
    };

    // Pin llama.cpp's per-layer device map (`model.dev_layer`) to the SAME cut
    // the overrides enforce — dev_layer is computed from tensor_split over
    // n_layer+1 units and never sees the overrides, and one straddled layer is
    // the distributed-GDN host abort (DISTRIBUTED_GDN_CRASH_STATUS.md §4).
    let tensor_split = dev_layer_tensor_split(&plan, n_layer);
    tracing::info!(
        model = %key.0,
        split = ?tensor_split,
        blocks = ?plan.iter().map(|s| s.blocks).collect::<Vec<_>>(),
        "plan_distribution: dev_layer tensor_split pinned to the shard plan"
    );

    let overrides: Vec<(
        std::ffi::CString,
        crate::llama::sys::ggml_backend_buffer_type_t,
    )> = override_patterns(&plan)
        .into_iter()
        .filter_map(|(pattern, di)| {
            let c = std::ffi::CString::new(pattern).ok()?;
            let buft = unsafe { crate::llama::sys::ggml_backend_dev_buffer_type(devs[di]) };
            Some((c, buft))
        })
        .collect();

    Some(DistributionPlan {
        devs,
        overrides,
        plan,
        tensor_split,
        assignments,
        eligible_workers,
        device_vram_bytes,
        mass,
        overheads,
    })
}

/// Operator escape hatch for the per-device fit gate:
/// `SOVEREIGN_SKIP_PER_DEVICE_FIT=1`. Mirrors `SOVEREIGN_SKIP_LOCAL_FIT_CHECK`.
///
/// It exists for one specific hazard. On a **reload**, a worker may still be
/// holding its previous shard when the host re-plans, so `free` under-reports
/// and the gate can manufacture an overflow that never happens — holding the
/// model unavailable over a measurement artifact. The three mitigations are this
/// flag (named in the refusal itself), logging both raw numbers and the capacity
/// source on every refusal, and parking rather than retrying (an overflow is not
/// time-fixable, so hammering it changes nothing).
fn per_device_fit_skip() -> bool {
    std::env::var("SOVEREIGN_SKIP_PER_DEVICE_FIT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// The worker whose share does not fit, if any — the per-device half of the
/// shared-model gate.
///
/// Returns the FIRST overflowing device in plan order. One is enough to refuse,
/// and naming one device an operator can act on beats a list they have to read.
/// `None` means either every device fits or the fit could not be judged; the
/// caller proceeds in both cases, because refusing a load on the strength of an
/// unread tensor table would brick loading on a header-parse bug.
fn first_worker_overflow(dist: &DistributionPlan) -> Option<WorkerOverflow> {
    const MIB: u64 = 1024 * 1024;
    if per_device_fit_skip() {
        tracing::warn!(
            "SOVEREIGN_SKIP_PER_DEVICE_FIT=1 — per-device fit gate disabled; a worker \
             whose share exceeds its memory will fail at load instead"
        );
        return None;
    }
    let mass = dist.mass.as_ref()?;
    let headroom = rpc_headroom_factor();
    let Some(fits) = shard_fits(
        &dist.plan,
        &dist.device_vram_bytes,
        mass,
        headroom,
        dist.overheads.as_ref(),
    ) else {
        // Explicitly NOT a pass — say so rather than let silence read as one.
        tracing::warn!(
            devices = dist.device_vram_bytes.len(),
            shards = dist.plan.len(),
            mass_known = mass.is_known(),
            "per-device fit gate: cannot judge (the plan and the model's byte mass do not \
             describe each other) — proceeding without the per-device check"
        );
        return None;
    };
    let endpoint_of: HashMap<usize, String> = dist
        .assignments
        .iter()
        .map(|a| (a.device_index, a.endpoint.clone()))
        .collect();
    let bad = fits.iter().find(|f| !f.fits())?;
    Some(WorkerOverflow {
        // `None` is this node's own GPU. A distributed plan can overflow the
        // host's share just as easily as a worker's, and refusing only for
        // remote devices would leave the same failure unguarded at home.
        endpoint: endpoint_of.get(&bad.device_index).cloned(),
        device_index: bad.device_index,
        blocks: dist
            .plan
            .iter()
            .find(|s| s.device_index == bad.device_index)
            .and_then(|s| s.blocks)
            .map(|(a, b)| b - a + 1)
            .unwrap_or(0),
        held_mb: bad.held_bytes / MIB,
        need_mb: bad.need_bytes / MIB,
        capacity_mb: bad.capacity_bytes / MIB,
    })
}

/// One device's share exceeding its memory — the payload shared by the three
/// mirrored refusal enums, so the numbers reaching an operator are identical
/// whichever path refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOverflow {
    /// RPC endpoint of the worker, or `None` for this node's own GPU.
    pub endpoint: Option<String>,
    /// Index in the plan's device order (RPC workers first, host last).
    pub device_index: usize,
    /// Transformer blocks this device was assigned.
    pub blocks: u32,
    /// Resident weight bytes it would hold, in MiB.
    pub held_mb: u64,
    /// `held × headroom` — what must fit, in MiB.
    pub need_mb: u64,
    /// What it has, in MiB.
    pub capacity_mb: u64,
}

impl WorkerOverflow {
    /// How this device is named to an operator.
    pub fn device_label(&self) -> String {
        match &self.endpoint {
            Some(ep) => format!("worker {ep}"),
            None => "this node's local GPU".to_string(),
        }
    }

    /// The refusal, in full, with the two things an operator can actually do
    /// about it.
    ///
    /// It says **lower** the headroom, not raise it. `need = held × headroom`,
    /// so raising the factor makes the refusal worse — advice that reads as
    /// helpful and moves the operator further from a working load. `mesh plan`
    /// already says lower; this message existing and saying the opposite would
    /// reintroduce, in a new place, exactly the preview↔load drift the shared
    /// decider was built to close.
    pub fn refusal(&self) -> String {
        format!(
            "per-device fit gate: {} was assigned {} block(s) — {} MiB of weights, \
             {} MiB with headroom + projected KV/compute — but advertises only {} MiB. \
             Not loading; a shard \
             that does not fit fails at load, and a local fallback of a model this \
             size is the 2026-07-27 session-kill. \
             Fixes: give that device more memory, LOWER `[shared_model] headroom` \
             (need = held × headroom, so raising it makes this worse), or add a \
             worker so the split gets finer. SOVEREIGN_SKIP_PER_DEVICE_FIT=1 \
             overrides. Capacity is the device's advertised free memory, falling \
             back to its total; on a reload a worker still holding its previous \
             shard under-reports free memory, which the override exists for.",
            self.device_label(),
            self.blocks,
            self.held_mb,
            self.need_mb,
            self.capacity_mb,
        )
    }
}

/// The device strategy for one model load — resolved up front, then applied.
/// Making the decision a value (not a tangle of booleans threaded through the
/// load) keeps every branch named and the never-wedge default explicit.
pub(crate) enum LoadPlacement {
    /// Local GPU only — never wedges. The default for a non-distributable slot,
    /// no workers present, a large model with no warm path, or any plan/warm
    /// failure.
    LocalOnly,
    /// Small enough that streaming each worker its share can't hit the send()
    /// deadlock — the proven path (prune dead workers + optional split).
    StreamSplit,
    /// Owned per-block placement: the workers hold their shards warm, so the host
    /// loads with `-ot` overrides and sends only tensor hashes (cache hits).
    OwnedOverrides(DistributionPlan),
    /// Wanted to distribute a large primary, but the cluster can't hold it yet —
    /// too few eligible anchors or insufficient pooled memory. The host does NOT
    /// load (a too-big primary loaded locally would OOM); it stays unavailable and
    /// a later reload (on the next worker-set change) retries. The shared model
    /// reports "forming" until quorum + memory are met.
    InsufficientCluster { eligible: usize, quorum: u32 },
    /// Placement fell back to local, but the model would not fit beside the
    /// host (local-fit gate). The host does NOT load — the 2026-07-27
    /// incident is a 91 GB fallback load starving the desktop compositor on
    /// unified memory. Same recovery as `InsufficientCluster`: stay
    /// unavailable; a later reload (next worker-set change) retries.
    LocalUnfit { need_mb: u64, usable_mb: u64 },
    /// The cluster has enough memory in aggregate, but one device's assigned
    /// share exceeds what that device has. Distinct from `InsufficientCluster`
    /// on purpose: pooling more memory does not fix this, and telling an
    /// operator "the cluster is forming" when the cluster is fully formed sends
    /// them looking for a peer that is already there.
    ///
    /// Recovery differs too. `InsufficientCluster` resolves itself as anchors
    /// join, so it retries. An overflow is not time-fixable — the same plan
    /// against the same devices overflows again — so the slot is **parked**
    /// until the worker set actually changes.
    WorkerUnfit(WorkerOverflow),
}

/// The placement decision as a PURE function of its inputs — split out so the
/// whole gate is unit-testable without touching ggml or the network.
/// `resolve_placement` gathers the inputs, calls this, then performs the effects.
#[derive(Debug, PartialEq, Eq)]
enum PlacementDecision {
    LocalOnly,
    StreamSplit,
    /// Distribute via owned `-ot` overrides. `auto_warm` = seed the workers'
    /// caches first (false means the operator asserted they're already warm).
    OwnedOverrides {
        auto_warm: bool,
    },
}

/// Minimum eligible anchor workers before the host distributes a shared model —
/// the quorum gate. From `SOVEREIGN_RPC_QUORUM_ANCHORS` (set by the daemon from
/// `[shared_model] quorum_anchors`); default 1.
fn rpc_quorum_anchors() -> u32 {
    std::env::var("SOVEREIGN_RPC_QUORUM_ANCHORS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(1)
}

/// Memory headroom FACTOR the host gates on: `pooled >= model_size × factor`.
/// From `SOVEREIGN_RPC_HEADROOM` (set from `[shared_model] headroom`); default
/// 1.2, clamped to >= 1.0. `svrn mesh plan` defaults its `--headroom` to this
/// same value, so a previewed plan uses the headroom the load executes with.
pub(crate) fn rpc_headroom_factor() -> f64 {
    std::env::var("SOVEREIGN_RPC_HEADROOM")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|&f| f >= 1.0)
        .unwrap_or(1.2)
}

/// Optional explicit floor (bytes) on pooled cluster memory, from
/// `SOVEREIGN_RPC_MIN_POOLED_GB` (set from `[shared_model] min_pooled_gb`). `0`
/// when unset — the computed `model_size × headroom` floor then governs alone.
fn rpc_min_pooled_bytes() -> u64 {
    std::env::var("SOVEREIGN_RPC_MIN_POOLED_GB")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|gb| (gb * 1024.0 * 1024.0 * 1024.0) as u64)
        .unwrap_or(0)
}

// ── Local-fit gate ──────────────────────────────────────────────────────
//
// 2026-07-27 incident: a fault-tolerance test killed every RPC worker, the
// daemon restarted, placement fell back to LocalOnly, and the shared 122B
// primary lazily loaded ~91 GB beside the desktop. On unified memory the
// compositor's GPU allocations starved (`amdgpu: CS rejected (-12)`),
// gnome-shell aborted, and systemd tore down the entire user session.
//
// "Never wedge" must not mean "load anything locally". A model this large
// only ever resolves LocalOnly as a *fallback* (no workers / plan failure /
// warm failure), and the resilient fallback is the one InsufficientCluster
// already models: do NOT load, stay unavailable, retry on the next
// worker-set change. The gate below turns a LocalOnly resolution into
// LocalUnfit when the model would not fit beside the host.

/// Only models at or above this size are gated (default 32 GiB, via
/// `SOVEREIGN_LOCAL_FIT_MIN_GB`). Below it, existing behaviour is
/// untouched — tight-but-working small-box configs must not regress.
fn local_fit_min_bytes() -> u64 {
    std::env::var("SOVEREIGN_LOCAL_FIT_MIN_GB")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|&gb| gb > 0.0)
        .map(|gb| (gb * 1024.0 * 1024.0 * 1024.0) as u64)
        .unwrap_or(32 * 1024 * 1024 * 1024)
}

/// Operator escape hatch: `SOVEREIGN_SKIP_LOCAL_FIT_CHECK=1` disables the
/// gate entirely (mirrors `SOVEREIGN_SKIP_VRAM_CHECK` for the preflight).
fn local_fit_skip() -> bool {
    std::env::var("SOVEREIGN_LOCAL_FIT_CHECK_SKIP")
        .or_else(|_| std::env::var("SOVEREIGN_SKIP_LOCAL_FIT_CHECK"))
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Is this node also being used interactively for graphics?
///
/// The distinction a reserve actually turns on. A headless node can lend nearly
/// everything it has; a node driving a display must keep enough for a compositor
/// to GROW into, because a graphics driver that cannot satisfy a submit commonly
/// aborts its client rather than stalling it — which turns "the model did not
/// fit" into "the operator lost their session".
///
/// Deliberately not a proportion of RAM: whether a machine has a screen is not a
/// function of how much memory it has.
fn node_is_graphical() -> bool {
    // A session type the desktop stack set for us is the strongest signal.
    if let Ok(t) = std::env::var("XDG_SESSION_TYPE") {
        let t = t.trim().to_ascii_lowercase();
        if t == "wayland" || t == "x11" {
            return true;
        }
        if t == "tty" {
            return false;
        }
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
        return true;
    }
    // macOS always runs a window server.
    if cfg!(target_os = "macos") {
        return true;
    }
    // Fallback for a daemon whose environment was not inherited from a session
    // (a container, a systemd unit): ask the kernel whether any display is
    // physically attached. Absent sysfs — not Linux, or a sandbox — assume
    // graphical, because over-reserving costs capacity while under-reserving
    // costs the session.
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return true;
    };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("status"))
            .map(|s| s.trim() == "connected")
            .unwrap_or(false)
    })
}

/// Memory this node keeps for itself and never lends: the OS, the daemon's own
/// non-model memory, and — when a display is attached — room for the compositor
/// to grow into.
///
/// ONE reserve policy for this node, consulted wherever a placement decision is
/// made, so a host cannot get different headroom depending on which door the
/// load came through. Absolute rather than proportional, because what the rest
/// of the machine needs does not scale with how much memory the machine has: a
/// large headless server should lend nearly all of itself, and a small desktop
/// should not lend the compositor's working set.
///
/// Clamped to half of total so a small host is never gated into uselessness.
/// Declare an explicit value with `SOVEREIGN_LOCAL_FIT_RESERVE_GB` (`0`
/// disables the reserve entirely).
pub fn host_reserve_bytes(total_bytes: u64, graphical: bool) -> u64 {
    const GIB: u64 = 1024 * 1024 * 1024;
    if let Some(declared) = std::env::var("SOVEREIGN_LOCAL_FIT_RESERVE_GB")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|&gb| gb >= 0.0)
        .map(|gb| (gb * GIB as f64) as u64)
    {
        return declared.min(total_bytes);
    }
    let want = if graphical { 8 * GIB } else { 2 * GIB };
    if total_bytes == 0 {
        return want;
    }
    want.min(total_bytes / 2)
}

/// [`host_reserve_bytes`] against this node's detected posture.
pub fn host_reserve_bytes_detected(total_bytes: u64) -> u64 {
    host_reserve_bytes(total_bytes, node_is_graphical())
}

/// This node's system memory as `(available, total)`; `(0, 0)` when the sensor
/// fails.
///
/// One reader, so a reserve derived from `total` and a fit judged against
/// `available` are always the same sample. Callers must treat `(0, 0)` as
/// "cannot judge" and fail open — never as "no memory".
pub fn system_memory_bytes() -> (u64, u64) {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    (sys.available_memory(), sys.total_memory())
}

/// Shortfall report when a local load would not fit. MiB so the numbers
/// drop straight into logs and error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalFitShortfall {
    pub(crate) need_mb: u64,
    pub(crate) usable_mb: u64,
}

/// PURE fit verdict: `None` = fits.
///
/// `overhead_bytes` is llama.cpp's own projection of the non-weight footprint
/// (KV + compute, [`projected_overheads`]) — the real number, replacing the
/// `weights/8 + 1 GiB` proxy that over-charged an MLA model's KV ~5× and
/// padded ~11 GB over a measured MoE footprint (2026-07-31 solo-122B
/// measurement). The proxy remains ONLY as the `None` fallback, so a failed
/// projection degrades to the old conservative behaviour instead of judging
/// against nothing. (`capacity.rs`'s boot preflight still uses the proxy
/// shape wholesale: it runs before the llama backend may be initialized and
/// double-init is fatal there, so the real projection cannot reach it.)
pub(crate) fn local_fit_verdict(
    model_bytes: u64,
    overhead_bytes: Option<u64>,
    available_bytes: u64,
    reserve_bytes: u64,
) -> Option<LocalFitShortfall> {
    const MIB: u64 = 1024 * 1024;
    let need = model_bytes + overhead_bytes.unwrap_or(model_bytes / 8 + 1024 * MIB);
    let usable = available_bytes.saturating_sub(reserve_bytes);
    (need > usable).then_some(LocalFitShortfall {
        need_mb: need / MIB,
        usable_mb: usable / MIB,
    })
}

/// Resolve a would-be LocalOnly placement through the fit gate. Small
/// models (or a skipped/failed sensor) pass straight through; a gated
/// model that doesn't fit resolves to `LocalUnfit` — same no-load,
/// stay-unavailable, retry-on-worker-change behaviour as
/// `InsufficientCluster`. Glassbox: every refusal logs the numbers.
fn gate_local(model_path: &Path, model_bytes: u64, cpu_only: bool, n_ctx: u32) -> LoadPlacement {
    if model_bytes < local_fit_min_bytes() || local_fit_skip() {
        return LoadPlacement::LocalOnly;
    }
    // CPU-only loads mmap the weights: file-backed, reclaimable page
    // cache the kernel can evict under pressure. A headless box
    // deliberately running a big model on CPU is a legitimate config
    // and must not be refused. The incident class is GPU loads, where
    // weights are PINNED in GTT/unified memory and the desktop starves.
    if cpu_only {
        tracing::info!(
            model_mb = model_bytes / (1024 * 1024),
            "local-fit gate: CPU-only load (mmap, reclaimable) — not gated"
        );
        return LoadPlacement::LocalOnly;
    }
    let (available, total) = system_memory_bytes();
    if available == 0 || total == 0 {
        tracing::warn!(
            model_mb = model_bytes / (1024 * 1024),
            "local-fit gate: memory sensor returned zero — cannot judge fit; \
             allowing the local load (never brick loading on a failed sensor)"
        );
        return LoadPlacement::LocalOnly;
    }
    // The SAME node reserve the distributed door uses — see `host_reserve_bytes`.
    let reserve = host_reserve_bytes_detected(total);
    // Everything lands locally on this path, so the whole projected overhead
    // (KV total + accelerator scratch + host scheduler buffer) is the term.
    let overhead = projected_overheads(model_path, n_ctx)
        .map(|o| o.context_total_bytes + o.compute_accel_bytes + o.compute_host_bytes);
    match local_fit_verdict(model_bytes, overhead, available, reserve) {
        None => LoadPlacement::LocalOnly,
        Some(shortfall) => {
            tracing::warn!(
                model_mb = model_bytes / (1024 * 1024),
                need_mb = shortfall.need_mb,
                usable_mb = shortfall.usable_mb,
                available_mb = available / (1024 * 1024),
                reserve_mb = reserve / (1024 * 1024),
                "local-fit gate: refusing local fallback load — model would starve \
                 the host (2026-07-27 session-kill class); staying unavailable until \
                 the cluster re-forms. SOVEREIGN_SKIP_LOCAL_FIT_CHECK=1 overrides."
            );
            LoadPlacement::LocalUnfit {
                need_mb: shortfall.need_mb,
                usable_mb: shortfall.usable_mb,
            }
        }
    }
}

/// The never-wedge decision tree. Distribute ONLY a distributable (primary) slot,
/// ONLY when workers exist. A small model streams safely; a large model must NOT
/// stream (the send() deadlock) — it distributes only against warm caches, which
/// we either auto-warm (orchestrator present) or trust (`assume_warmed`).
/// Everything else is LocalOnly — the load never wedges.
fn classify_placement(
    distributable: bool,
    has_workers: bool,
    model_bytes: u64,
    safe_bytes: u64,
    assume_warmed: bool,
    has_orchestrator: bool,
) -> PlacementDecision {
    // §4.0 gate: only the primary distributes, and only when workers exist. This
    // keeps non-primary slots (fast / embed / code) off the RPC path — the
    // multi-slot crash — and means an idle mesh never touches a worker.
    if !distributable || !has_workers {
        return PlacementDecision::LocalOnly;
    }
    // `(.., assume_warmed=false)` is exactly `model_bytes <= safe_bytes`: small
    // enough that no shard reaches the upload deadlock, so streaming is safe.
    if rpc_distribution_safe_decision(model_bytes, safe_bytes, false) {
        return PlacementDecision::StreamSplit;
    }
    // Large model — owned placement against warm caches is the only safe path.
    if assume_warmed {
        return PlacementDecision::OwnedOverrides { auto_warm: false };
    }
    if has_orchestrator {
        return PlacementDecision::OwnedOverrides { auto_warm: true };
    }
    // Large primary, workers present, but no way to get the shards warm — never
    // wedge. The daemon wires the orchestrator; a bare example binary does not.
    PlacementDecision::LocalOnly
}

/// The most-recent PRIMARY placement decision — the glassbox surface behind
/// `/status`. Set on every distributable (primary) load by `resolve_placement`,
/// so an operator can query placement outright instead of inferring it from
/// `free` deltas or decode-latency signatures.
static LAST_PRIMARY_PLACEMENT: std::sync::Mutex<Option<sovereign_core::traits::SlotPlacement>> =
    std::sync::Mutex::new(None);

/// Read the last primary placement (for `/status`). `None` before the first
/// primary load.
pub(crate) fn last_primary_placement() -> Option<sovereign_core::traits::SlotPlacement> {
    LAST_PRIMARY_PLACEMENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Summarize a load decision into the queryable placement report — mode plus,
/// for a distributed load, the per-device block split and which worker holds
/// what. The block count comes straight from the plan the load ENFORCES via
/// `-ot`, so it is ground truth, not an estimate.
fn summarize_placement(placement: &LoadPlacement) -> sovereign_core::traits::SlotPlacement {
    use sovereign_core::traits::{SlotPlacement, WorkerPlacement};
    let bare = |mode: &str| SlotPlacement {
        mode: mode.to_string(),
        total_blocks: 0,
        local_blocks: 0,
        workers: Vec::new(),
    };
    match placement {
        LoadPlacement::LocalOnly => bare("local"),
        LoadPlacement::StreamSplit => bare("stream-split"),
        LoadPlacement::InsufficientCluster { .. } => bare("forming"),
        LoadPlacement::LocalUnfit { .. } => bare("unfit-local"),
        LoadPlacement::WorkerUnfit(_) => bare("unfit-worker"),
        LoadPlacement::OwnedOverrides(dist) => {
            let ep: std::collections::HashMap<usize, String> = dist
                .assignments
                .iter()
                .map(|a| (a.device_index, a.endpoint.clone()))
                .collect();
            let (mut total, mut local) = (0u32, 0u32);
            let mut workers = Vec::new();
            for shard in &dist.plan {
                let n = shard
                    .blocks
                    .map(|(f, l)| l.saturating_sub(f) + 1)
                    .unwrap_or(0);
                total += n;
                match ep.get(&shard.device_index) {
                    Some(e) => workers.push(WorkerPlacement {
                        endpoint: e.clone(),
                        blocks: n,
                        holds_output: shard.holds_output,
                    }),
                    None => local += n,
                }
            }
            SlotPlacement {
                mode: "distributed".to_string(),
                total_blocks: total,
                local_blocks: local,
                workers,
            }
        }
    }
}

/// Resolve placement and, for the PRIMARY (distributable) slot, publish the
/// decision to the glassbox surface: a `target: "placement"` INFO log (always
/// worth surfacing) plus the `/status`-queryable global. Distributed-vs-local
/// and the split are STATED here — never left to inference.
pub(crate) fn resolve_placement(
    model_path: &Path,
    model_bytes: u64,
    distributable: bool,
    cpu_only: bool,
    n_ctx: u32,
) -> LoadPlacement {
    let placement =
        resolve_placement_inner(model_path, model_bytes, distributable, cpu_only, n_ctx);
    if distributable {
        let summary = summarize_placement(&placement);
        tracing::info!(
            target: "placement",
            mode = %summary.mode,
            total_blocks = summary.total_blocks,
            local_blocks = summary.local_blocks,
            workers = ?summary.workers,
            "primary slot placement decided"
        );
        *LAST_PRIMARY_PLACEMENT
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(summary);
    }
    placement
}

/// Resolve how to place `model_path` across the local GPU and any mesh RPC
/// workers. NOT pure: when it intends to distribute it registers the workers (so
/// the plan can enumerate their devices), and for the auto-warm path it DRIVES
/// the orchestrator to seed every worker's shard before returning the override
/// plan. Glassbox: every branch logs the decision and its reason.
fn resolve_placement_inner(
    model_path: &Path,
    model_bytes: u64,
    distributable: bool,
    cpu_only: bool,
    n_ctx: u32,
) -> LoadPlacement {
    let decision = classify_placement(
        distributable,
        rpc_workers_present(),
        model_bytes,
        safe_rpc_stream_bytes(),
        rpc_assume_warmed(),
        RPC_WARM_ORCHESTRATOR.get().is_some(),
    );

    let auto_warm = match decision {
        PlacementDecision::LocalOnly => {
            return gate_local(model_path, model_bytes, cpu_only, n_ctx)
        }
        PlacementDecision::StreamSplit => {
            // Publish the workers so the stream path can enumerate + split them.
            register_rpc_workers();
            return LoadPlacement::StreamSplit;
        }
        PlacementDecision::OwnedOverrides { auto_warm } => auto_warm,
    };

    match plan_and_warm(model_path, model_bytes, auto_warm, n_ctx) {
        PlannedDistribution::Ready(dist) => LoadPlacement::OwnedOverrides(dist),
        PlannedDistribution::Insufficient { eligible, quorum } => {
            LoadPlacement::InsufficientCluster { eligible, quorum }
        }
        // NOT routed to `gate_local`. This is the load-bearing arm: the cluster
        // has the memory, so a fallback here would load an 80–90 GB model onto
        // the host — the 2026-07-27 session-kill, arrived at by a path that
        // looks like resilience. Stay unavailable instead.
        PlannedDistribution::WorkerOverflow(o) => LoadPlacement::WorkerUnfit(o),
        // Unplannable and warm-failure are both "never wedge" — load local,
        // fit-gated. A later reload retries once the workers are reachable.
        PlannedDistribution::Unplannable | PlannedDistribution::WarmFailed(_) => {
            gate_local(model_path, model_bytes, cpu_only, n_ctx)
        }
    }
}

/// The result of planning (and optionally warming) a distributed placement —
/// the shared tail of both the in-process load path ([`resolve_placement_inner`])
/// and the out-of-process one ([`warm_distributed_primary`], which warms on
/// behalf of a compute child). ONE code path, so the two cannot drift.
enum PlannedDistribution {
    /// Planned, quorum-approved, and (when asked) warm. Ready to load with `-ot`.
    Ready(DistributionPlan),
    /// Quorum or pooled memory unmet — the cluster is still forming.
    Insufficient { eligible: usize, quorum: u32 },
    /// Pooled memory is sufficient but one device's share is not. See
    /// [`LoadPlacement::WorkerUnfit`].
    WorkerOverflow(WorkerOverflow),
    /// No plan: no RPC device, unreadable block count, or an unmappable worker.
    Unplannable,
    /// The orchestrator ran and gave up.
    WarmFailed(String),
}

/// Publish the workers, plan the shards ONCE, gate on quorum + pooled memory,
/// and — when `auto_warm` — drive the injected orchestrator until every worker
/// holds its shard.
///
/// The plan is computed once and consumed for BOTH warming and loading, so the
/// two cannot disagree — the plan-agreement invariant that keeps every
/// distributed weight a `SET_TENSOR_HASH` cache hit (no bulk send, no deadlock).
///
/// Blocking: the orchestrator bridges to async internally. Call it from a
/// blocking context (a load thread or `spawn_blocking`), never from a runtime
/// worker.
fn plan_and_warm(
    model_path: &Path,
    model_bytes: u64,
    auto_warm: bool,
    n_ctx: u32,
) -> PlannedDistribution {
    register_rpc_workers();
    let Some(dist) = plan_distribution(model_path, n_ctx) else {
        tracing::warn!(
            model_mb = model_bytes / (1024 * 1024),
            "wanted to distribute a large primary but couldn't plan the shards (no RPC \
             device, GGUF block count unreadable, or unmappable worker) — loading local-only"
        );
        return PlannedDistribution::Unplannable;
    };

    // Quorum + pooled-memory gate (shared-model host): never attempt a load the
    // cluster can't hold. A too-big primary loaded locally would OOM; instead stay
    // unavailable and let the next worker-set-change reload retry as anchors join.
    let quorum = rpc_quorum_anchors();
    let needed = ((model_bytes as f64 * rpc_headroom_factor()) as u64).max(rpc_min_pooled_bytes());
    let pooled_vram_bytes: u64 = dist.device_vram_bytes.iter().sum();
    if (dist.eligible_workers as u32) < quorum || pooled_vram_bytes < needed {
        tracing::warn!(
            eligible_anchors = dist.eligible_workers,
            quorum,
            pooled_gb = pooled_vram_bytes / (1024 * 1024 * 1024),
            need_gb = needed / (1024 * 1024 * 1024),
            "shared-model cluster forming — quorum or pooled memory not met; not loading \
             (retries on the next worker-set change)"
        );
        return PlannedDistribution::Insufficient {
            eligible: dist.eligible_workers,
            quorum,
        };
    }

    // Per-device fit. The gate above asked whether the cluster could hold the
    // model *somewhere*; this asks whether the device each block was actually
    // assigned to can hold its share. A plan can pass the first and fail the
    // second — pooled memory is a sum, and a sum cannot see a skew.
    //
    // Refused BEFORE warming: warming a shard onto a worker that cannot hold it
    // spends minutes of GGUF transfer to arrive at the same refusal.
    if let Some(overflow) = first_worker_overflow(&dist) {
        tracing::warn!(
            target: "placement",
            device = overflow.device_index,
            endpoint = ?overflow.endpoint,
            blocks = overflow.blocks,
            held_mb = overflow.held_mb,
            need_mb = overflow.need_mb,
            capacity_mb = overflow.capacity_mb,
            headroom = rpc_headroom_factor(),
            capacity_source = "ggml_backend_dev_memory (free, else total)",
            "{}",
            overflow.refusal()
        );
        return PlannedDistribution::WorkerOverflow(overflow);
    }

    if !auto_warm {
        tracing::info!(
            workers = dist.assignments.len(),
            "SOVEREIGN_RPC_ASSUME_WARMED set — trusting worker shards are warm (skipping auto-warm)"
        );
        return PlannedDistribution::Ready(dist);
    }

    // Auto-warm: ask the injected orchestrator to seed every worker's shard. It
    // blocks until they're warm (or gives up). Any failure → the caller falls
    // back (never wedge); a later reload retries once the workers are reachable.
    let Some(orchestrator) = RPC_WARM_ORCHESTRATOR.get() else {
        // classify_placement only returns auto_warm when an orchestrator is
        // present, so this is unreachable from the load path — but never wedge.
        return PlannedDistribution::WarmFailed("no warm orchestrator installed".to_string());
    };
    let req = RpcWarmPlan {
        model_path: model_path.to_path_buf(),
        plan: dist.plan.clone(),
        assignments: dist.assignments.clone(),
    };
    tracing::info!(
        model_mb = model_bytes / (1024 * 1024),
        workers = req.assignments.len(),
        "auto-warming worker shards before distributed load"
    );
    match orchestrator(&req) {
        Ok(()) => {
            tracing::info!(
                "auto-warm complete — all worker shards seeded; loading with -ot overrides"
            );
            PlannedDistribution::Ready(dist)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "auto-warm failed — falling back to local (never wedge, fit-gated); a \
                 later reload will retry once the worker(s) are reachable"
            );
            PlannedDistribution::WarmFailed(e)
        }
    }
}

// ─── Warming on behalf of a SEPARATE loader process (the compute child) ──────

/// What the host learned when it warmed a distributed primary's worker shards
/// on behalf of a **different process** — the `sovereign-compute` child that
/// will hold the model and absorb ggml's uncatchable RPC abort.
///
/// Warming cannot move into the child: the orchestrator needs the mesh member
/// directory, the iroh transport bases, and the daemon's resolved ports. So the
/// daemon warms and hands the child the RESULT.
#[derive(Debug, Clone, PartialEq)]
pub enum DistributedWarmOutcome {
    /// Every worker holds its shard. `endpoints` is the worker set the child
    /// loads across; `plan` is the shard plan they were warmed AGAINST and must
    /// be pinned in the child via [`pin_shard_plan`] — see that function for why
    /// re-planning in the child would silently invalidate every warm cache.
    Warm {
        endpoints: Vec<String>,
        plan: Vec<NodeShard>,
    },
    /// Quorum or pooled memory unmet — the cluster is still forming. Stay
    /// unavailable and retry on the next worker-set change.
    InsufficientCluster { eligible: usize, quorum: u32 },
    /// Pooled memory is sufficient but one device's assigned share exceeds what
    /// that device has. Park the slot rather than retry — see
    /// [`LoadPlacement::WorkerUnfit`].
    WorkerUnfit(WorkerOverflow),
    /// No plan could be computed: no RPC device, unreadable GGUF block count,
    /// or an RPC device that maps to no endpoint.
    Unplannable,
    /// The orchestrator ran and gave up (worker unreachable, fetch failed, …).
    WarmFailed { error: String },
}

/// Plan + warm `model_path`'s shards across the eligible mesh workers so a
/// SEPARATE process can load it distributed. The daemon calls this before
/// spawning (or respawning) the compute child that owns the distributed primary.
///
/// Blocking — the injected orchestrator bridges to async with `block_on` and a
/// full warm can take minutes of GGUF transfer. Call from `spawn_blocking`,
/// never from a runtime worker thread.
///
/// `n_ctx` is the context size the CHILD will load with (its `--ctx`, or the
/// child's own default when the spec leaves it unset) — the overhead
/// projection sizes KV from it, so a wrong value here would size the margin
/// for a load that never happens.
pub fn warm_distributed_primary(model_path: &Path, n_ctx: u32) -> DistributedWarmOutcome {
    let model_bytes = super::model_slot::total_model_bytes(model_path);
    match plan_and_warm(model_path, model_bytes, /* auto_warm */ true, n_ctx) {
        PlannedDistribution::Ready(dist) => DistributedWarmOutcome::Warm {
            endpoints: dist
                .assignments
                .iter()
                .map(|a| a.endpoint.clone())
                .collect(),
            plan: dist.plan.clone(),
        },
        PlannedDistribution::Insufficient { eligible, quorum } => {
            DistributedWarmOutcome::InsufficientCluster { eligible, quorum }
        }
        PlannedDistribution::WorkerOverflow(o) => DistributedWarmOutcome::WorkerUnfit(o),
        PlannedDistribution::Unplannable => DistributedWarmOutcome::Unplannable,
        PlannedDistribution::WarmFailed(error) => DistributedWarmOutcome::WarmFailed { error },
    }
}

/// The plan-cache key: the model's file name plus its worker set, sorted. Shared
/// by [`plan_distribution`] and [`pin_shard_plan`] so a pinned plan and a
/// computed one land on the same entry.
fn plan_cache_key(model_path: &Path, endpoints: &[String]) -> (String, Vec<String>) {
    let model_id = model_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut eps = endpoints.to_vec();
    eps.sort();
    (model_id, eps)
}

/// Seed this process's plan cache with a shard plan computed ELSEWHERE — the
/// cross-process half of the plan-agreement invariant.
///
/// The in-process cache already keeps warm-time and load-time placement
/// identical across reloads, because a worker's free VRAM swings by its own
/// cached shard between reloads and a re-plan would drift the split (the
/// `already=20` vs `36` cache invalidation). That cache is process-local, so a
/// compute child loading a primary the DAEMON warmed would re-plan against
/// post-warm VRAM, cut the blocks differently, miss every cache, and fall back
/// to bulk weight send — the send() deadlock the warm path exists to avoid.
///
/// Pinning the daemon's plan closes that hole: the child's `plan_distribution`
/// takes the cache hit and derives its `-ot` overrides and `tensor_split` from
/// the identical shards the workers were warmed against.
pub fn pin_shard_plan(model_path: &Path, endpoints: &[String], plan: Vec<NodeShard>) {
    let key = plan_cache_key(model_path, endpoints);
    tracing::info!(
        model = %key.0,
        workers = key.1.len(),
        blocks = ?plan.iter().map(|s| s.blocks).collect::<Vec<_>>(),
        "pinned an externally-computed shard plan (plan-agreement across the process boundary)"
    );
    plan_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, plan);
}

/// One-shot guard so the in-process RPC worker starts at most once.
static RPC_SERVE_STARTED: std::sync::Once = std::sync::Once::new();

/// If `SOVEREIGN_RPC_SERVE` is set (e.g. `0.0.0.0:50052`), start an in-process
/// llama.cpp RPC server on a background thread exposing this node's local GPU
/// device(s) to mesh peers — the **distributable** counterpart of running a
/// standalone `rpc-server` binary, with **no separate build**: the daemon
/// already links ggml with `GGML_RPC=ON`. An operator turns their node into a
/// worker by running the daemon they already run, plus one env var.
///
/// Must be called after `LlamaBackend::init()` (so ggml's device registry is
/// populated). Serves every local non-CPU device, skipping any remote "RPC"
/// device, mirroring stock `rpc-server`'s default selection.
/// `ggml_backend_rpc_start_server` blocks, so it owns a dedicated thread for
/// the life of the process.
pub(crate) fn serve_rpc_worker_if_configured() {
    RPC_SERVE_STARTED.call_once(|| {
        let Ok(bind) = std::env::var("SOVEREIGN_RPC_SERVE") else {
            return;
        };
        let bind = bind.trim().to_string();
        if bind.is_empty() {
            return;
        }

        // Collect local GPU devices (skip CPU and any already-registered remote
        // RPC devices, so a host+worker hybrid never re-serves a peer).
        let mut devices: Vec<crate::llama::sys::ggml_backend_dev_t> = Vec::new();
        let n = unsafe { crate::llama::sys::ggml_backend_dev_count() };
        for i in 0..n {
            let dev = unsafe { crate::llama::sys::ggml_backend_dev_get(i) };
            if unsafe { crate::llama::sys::ggml_backend_dev_type(dev) }
                == crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_CPU
            {
                continue;
            }
            let reg = unsafe { crate::llama::sys::ggml_backend_dev_backend_reg(dev) };
            let reg_name =
                unsafe { std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_reg_name(reg)) }
                    .to_string_lossy();
            if reg_name == "RPC" {
                continue;
            }
            devices.push(dev);
        }
        if devices.is_empty() {
            tracing::warn!(
                bind,
                "SOVEREIGN_RPC_SERVE set but no local GPU device found — RPC worker not started"
            );
            return;
        }
        let c_bind = match std::ffi::CString::new(bind.clone()) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(
                    bind,
                    "RPC bind contains an interior NUL — worker not started"
                );
                return;
            }
        };
        let n_devices = devices.len();
        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get() / 2)
            .unwrap_or(4)
            .max(1);

        // Tensor cache: with a cache dir, a reload of the same model loads each
        // weight tensor from local disk instead of re-receiving it over the
        // network (llama.cpp hashes weight tensors >10MB and skips the transfer
        // on a cache hit). So the cold first load pays the wire cost once; every
        // reload after is hash round-trips only.
        let cache = rpc_cache_dir();
        let c_cache = cache
            .as_ref()
            .and_then(|p| std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok());
        tracing::info!(
            bind,
            n_devices,
            n_threads,
            cache = cache.as_ref().map(|p| p.display().to_string()),
            "starting in-process RPC worker (serving local GPU to mesh peers)"
        );

        // Device pointers are valid for the process lifetime (owned by ggml's
        // registry) but `*mut` is not Send; wrap to move into the server thread.
        struct Served {
            bind: std::ffi::CString,
            cache: Option<std::ffi::CString>,
            devices: Vec<crate::llama::sys::ggml_backend_dev_t>,
        }
        // SAFETY: the wrapped pointers reference process-static ggml devices.
        unsafe impl Send for Served {}
        let payload = Served {
            bind: c_bind,
            cache: c_cache,
            devices,
        };

        let spawned = std::thread::Builder::new()
            .name("rpc-worker".into())
            .spawn(move || {
                // Re-bind the whole `payload` so the closure captures the Send
                // wrapper as a unit — Rust 2021 disjoint capture would otherwise
                // grab the non-Send `Vec<*mut>` field directly.
                let mut payload = payload;
                let cache_ptr = payload
                    .cache
                    .as_ref()
                    .map_or(std::ptr::null(), |c| c.as_ptr());
                let bind_str = payload.bind.to_string_lossy().into_owned();
                // Supervisor loop. `ggml_backend_rpc_start_server` runs ggml's
                // accept loop and blocks — but ggml `return`s from it on a SINGLE
                // failed `accept()` (ggml-rpc.cpp: a transient ECONNABORTED from a
                // peer that connected then reset tears the whole worker down — it
                // `return`s rather than `continue`s). A one-shot call therefore
                // left the worker permanently dead while `/status` still
                // advertised the port, so every host kept connecting and skipping
                // it. We supervise instead: when the server loop returns, re-create
                // it (re-binding the freed port) with exponential backoff — a
                // transient error recovers in ~100ms, while a persistent fault
                // (e.g. the port is already held) can't hot-loop. Runs for the life
                // of the process; the only exit is process teardown.
                let mut consecutive_fast_exits: u32 = 0;
                loop {
                    let started = Instant::now();
                    tracing::info!(
                        bind = %bind_str,
                        n_devices,
                        "in-process RPC worker: entering ggml server accept loop"
                    );
                    // SAFETY: device pointers outlive the process; start_server
                    // blocks here until its accept loop tears down, then returns.
                    unsafe {
                        crate::llama::sys::ggml_backend_rpc_start_server(
                            payload.bind.as_ptr(),
                            cache_ptr,
                            n_threads,
                            n_devices,
                            payload.devices.as_mut_ptr(),
                        );
                    }
                    // A loop that ran a meaningful while was healthy; reset the flap
                    // counter so the next teardown recovers quickly. A loop that
                    // exits fast is flapping → back off harder each time.
                    let ran_for = started.elapsed();
                    consecutive_fast_exits = if ran_for >= std::time::Duration::from_secs(30) {
                        0
                    } else {
                        consecutive_fast_exits.saturating_add(1)
                    };
                    let backoff = rpc_worker_restart_backoff(consecutive_fast_exits);
                    tracing::warn!(
                        bind = %bind_str,
                        ran_secs = ran_for.as_secs_f64(),
                        consecutive_fast_exits,
                        backoff_ms = backoff.as_millis() as u64,
                        "in-process RPC worker server loop exited — restarting \
                         (ggml tears its accept loop down on a transient accept() error)"
                    );
                    std::thread::sleep(backoff);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "failed to spawn RPC worker thread");
        }
    });
}

/// Restart backoff for the in-process RPC worker supervisor. The first restart
/// after a healthy run is near-instant (a transient `accept()` error recovers as
/// soon as the freed port is re-bound); each successive *fast* exit — a persistent
/// fault such as the port already being held — backs off exponentially to a 5s
/// cap so the supervisor can never hot-loop. Pure fn so the schedule is
/// unit-testable without standing up a real worker.
fn rpc_worker_restart_backoff(consecutive_fast_exits: u32) -> std::time::Duration {
    let ms = match consecutive_fast_exits {
        0 => 100,
        // 200, 400, 800, 1600, 3200, then capped at 5000.
        n => (100u64 << n.min(6)).min(5_000),
    };
    std::time::Duration::from_millis(ms)
}

/// Resolve the RPC worker's tensor cache directory — the on-disk store that lets
/// a model reload skip re-receiving weights over the network. Default:
/// `~/.svrnmesh/rpc-cache`. Override with `SOVEREIGN_RPC_CACHE_DIR`; set that to
/// `off` / `0` / empty to disable caching. Returns `None` (caching off) when the
/// directory can't be created.
fn rpc_cache_dir() -> Option<std::path::PathBuf> {
    let dir = match std::env::var("SOVEREIGN_RPC_CACHE_DIR") {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() || v.eq_ignore_ascii_case("off") || v == "0" {
                return None;
            }
            std::path::PathBuf::from(v)
        }
        Err(_) => sovereign_core::rebrand::svrnmesh_root().join("rpc-cache"),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "could not create RPC cache dir — caching disabled");
        return None;
    }
    Some(dir)
}

#[cfg(test)]
mod distributed_handoff_tests {
    use super::*;

    fn shard(device_index: usize, first: u32, last: u32) -> NodeShard {
        NodeShard {
            device_index,
            blocks: Some((first, last)),
            holds_output: false,
            fraction: 0.5,
        }
    }

    /// A plan pinned from ANOTHER process must land on the same cache entry
    /// `plan_distribution` looks up — that is the whole mechanism keeping a
    /// compute child's `-ot` cut identical to the one the daemon warmed the
    /// workers against. Endpoint ORDER must not matter: discovery hands the
    /// worker set back in whatever order peers answered.
    #[test]
    fn pinned_plan_is_keyed_the_way_plan_distribution_looks_it_up() {
        let model = Path::new("/models/Qwen3.5-122B-A10B-00001-of-00003.gguf");
        let plan = vec![shard(0, 0, 11), shard(1, 12, 47)];
        let pinned_order = vec!["10.0.0.9:50052".to_string(), "10.0.0.2:50052".to_string()];

        pin_shard_plan(model, &pinned_order, plan.clone());

        // Same worker set, opposite order → same key, so the child takes the
        // cache hit instead of re-planning against post-warm VRAM.
        let lookup_order = vec!["10.0.0.2:50052".to_string(), "10.0.0.9:50052".to_string()];
        let key = plan_cache_key(model, &lookup_order);
        let cached = plan_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        assert_eq!(cached, Some(plan));

        // A DIFFERENT worker set is a different key — a real topology change
        // must re-plan rather than reuse a cut that no longer fits.
        let other = plan_cache_key(model, &["10.0.0.2:50052".to_string()]);
        assert!(plan_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&other)
            .is_none());
    }

    /// The child's environment — workers present, warm asserted, NO
    /// orchestrator (one can't exist in a child; it needs the daemon's mesh
    /// directory) — must reach the override path WITHOUT trying to warm.
    /// If this ever regressed to `LocalOnly`, a child would quietly load a
    /// 90 GB model on one box: the 2026-07-27 host-starvation incident.
    #[test]
    fn a_compute_childs_environment_reaches_owned_overrides_without_warming() {
        let mb = 1024 * 1024;
        assert_eq!(
            classify_placement(
                /* distributable */ true,
                /* has_workers */ true,
                /* model_bytes */ 87_000 * mb,
                /* safe_bytes */ 512 * mb,
                /* assume_warmed */ true,
                /* has_orchestrator */ false,
            ),
            PlacementDecision::OwnedOverrides { auto_warm: false }
        );
    }
}

#[cfg(test)]
mod rpc_prune_tests {
    use super::*;

    #[test]
    fn rpc_distribution_safe_decision_is_a_never_wedge_floor() {
        let mb = 1024 * 1024;
        // Small model: any share is under the deadlock point → safe to stream.
        assert!(rpc_distribution_safe_decision(100 * mb, 512 * mb, false));
        // At the threshold: safe (<=).
        assert!(rpc_distribution_safe_decision(512 * mb, 512 * mb, false));
        // Large + cold cache: NOT safe → caller loads local-only, never wedges.
        assert!(!rpc_distribution_safe_decision(
            30_000 * mb,
            512 * mb,
            false
        ));
        // Large + asserted-warm: safe (host skips the bulk send via cache hits).
        assert!(rpc_distribution_safe_decision(30_000 * mb, 512 * mb, true));
    }

    #[test]
    fn forced_choice_sentinel_detection() {
        // The sentinel is detected and the candidate labels read off `enum`.
        let mut req = CompletionRequest::default();
        req.structured_output = Some(serde_json::json!({
            "type": "string", "enum": ["A", "B", "C"], "x_forced_choice": true
        }));
        assert_eq!(
            forced_choice_candidates(&req),
            Some(vec!["A".to_string(), "B".to_string(), "C".to_string()])
        );

        // An ordinary structured-output schema (no marker) is NOT a forced
        // choice — every existing path stays unaffected.
        let mut plain = CompletionRequest::default();
        plain.structured_output = Some(serde_json::json!({
            "type": "object", "properties": {"x": {"type": "string"}}
        }));
        assert_eq!(forced_choice_candidates(&plain), None);

        // No structured_output at all → None.
        assert_eq!(
            forced_choice_candidates(&CompletionRequest::default()),
            None
        );
    }

    #[test]
    fn classify_placement_is_the_never_wedge_gate() {
        use PlacementDecision::*;
        let mb = 1024 * 1024;
        let safe = 512 * mb;
        let small = 100 * mb;
        let large = 30_000 * mb;

        // §4.0: a non-distributable slot NEVER distributes — regardless of size,
        // workers, warm-state, or orchestrator. This is the multi-slot-crash guard.
        assert_eq!(
            classify_placement(false, true, large, safe, true, true),
            LocalOnly
        );
        assert_eq!(
            classify_placement(false, true, small, safe, false, true),
            LocalOnly
        );

        // No workers → local, even for the primary.
        assert_eq!(
            classify_placement(true, false, large, safe, true, true),
            LocalOnly
        );

        // Small primary + workers → safe to stream.
        assert_eq!(
            classify_placement(true, true, small, safe, false, false),
            StreamSplit
        );

        // Large primary, asserted warm → owned overrides, NO auto-warm needed.
        assert_eq!(
            classify_placement(true, true, large, safe, true, false),
            OwnedOverrides { auto_warm: false }
        );
        // assume_warmed wins even when an orchestrator exists (operator override).
        assert_eq!(
            classify_placement(true, true, large, safe, true, true),
            OwnedOverrides { auto_warm: false }
        );

        // Large primary, cold, orchestrator present → auto-warm then overrides.
        assert_eq!(
            classify_placement(true, true, large, safe, false, true),
            OwnedOverrides { auto_warm: true }
        );

        // Large primary, cold, NO orchestrator and NOT asserted warm → the
        // never-wedge default: local-only (a later reload retries).
        assert_eq!(
            classify_placement(true, true, large, safe, false, false),
            LocalOnly
        );
    }

    #[test]
    fn local_fit_verdict_replays_the_2026_07_27_incident() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // The incident, in the gate's own units: an ~84 GiB 122B GGUF, a
        // 128 GiB box with ~110 GiB available, default reserve 16 GiB
        // (total/8). Estimated need = 84 + 84/8 + 1 = ~95.5 GiB against
        // 94 GiB usable → REFUSED. This exact load reached ~91 GB RSS and
        // killed the desktop session; the verdict must never let it through.
        let shortfall = local_fit_verdict(84 * GIB, None, 110 * GIB, 16 * GIB)
            .expect("the incident load must be refused");
        assert!(shortfall.need_mb > shortfall.usable_mb);

        // The same model on a box with genuine headroom (e.g. a 256 GiB
        // host with 200 GiB available) fits — the gate protects the host,
        // it does not ban big models.
        assert_eq!(local_fit_verdict(84 * GIB, None, 200 * GIB, 32 * GIB), None);

        // The canonical 35B primary (~20 GiB) beside a desktop on a 64 GiB
        // box: need ≈ 23.5 GiB vs 40−8 = 32 GiB usable → fits. Existing
        // working configs must not regress.
        assert_eq!(local_fit_verdict(20 * GIB, None, 40 * GIB, 8 * GIB), None);

        // Reserve is honoured: same numbers with the reserve eating the
        // margin flips the verdict.
        assert!(local_fit_verdict(20 * GIB, None, 25 * GIB, 8 * GIB).is_some());

        // Saturating: reserve larger than available must not underflow.
        assert!(local_fit_verdict(1 * GIB, None, 2 * GIB, 10 * GIB).is_some());
    }

    /// The projection REPLACES the proxy: an MLA-class model whose real
    /// KV + compute is ~3.5 GiB must not be charged the ~11.5 GiB the
    /// `weights/8 + 1 GiB` proxy invents for it. Measured live 2026-07-31:
    /// the proxy padded ~11 GB over a 122B MoE's real footprint and the
    /// solo load had to be forced past the gate by hand.
    #[test]
    fn a_real_overhead_projection_replaces_the_kv_proxy() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // 84 GiB weights, 94 GiB usable. The proxy needs 95.5 GiB → refused.
        assert!(local_fit_verdict(84 * GIB, None, 110 * GIB, 16 * GIB).is_some());
        // The projection says the real overhead is 4 GiB → 88 GiB fits.
        assert_eq!(
            local_fit_verdict(84 * GIB, Some(4 * GIB), 110 * GIB, 16 * GIB),
            None,
            "a load that fits by the real numbers must not be refused by a proxy"
        );
        // And a projection LARGER than the proxy still refuses — the real
        // number wins in both directions.
        assert!(local_fit_verdict(84 * GIB, Some(20 * GIB), 110 * GIB, 16 * GIB).is_some());
    }

    #[test]
    fn local_unfit_placement_reports_unfit_local_mode() {
        // Glassbox: /status must say WHY the shared model is unavailable —
        // "unfit-local" is distinct from "forming" (cluster short) so an
        // operator can tell "waiting for workers" from "refused to starve
        // the host".
        let summary = summarize_placement(&LoadPlacement::LocalUnfit {
            need_mb: 97_792,
            usable_mb: 96_256,
        });
        assert_eq!(summary.mode, "unfit-local");
        let forming = summarize_placement(&LoadPlacement::InsufficientCluster {
            eligible: 0,
            quorum: 1,
        });
        assert_eq!(forming.mode, "forming");
    }

    #[test]
    fn rpc_worker_restart_backoff_is_bounded_and_monotonic() {
        use std::time::Duration;
        // First restart after a healthy run recovers fast.
        assert_eq!(rpc_worker_restart_backoff(0), Duration::from_millis(100));
        // Repeated fast exits back off monotonically and never exceed the 5s cap —
        // this is the guarantee that the supervisor can never hot-loop.
        let mut last = Duration::ZERO;
        for n in 0..=20 {
            let b = rpc_worker_restart_backoff(n);
            assert!(b >= last, "backoff must be non-decreasing (n={n})");
            assert!(
                b <= Duration::from_secs(5),
                "backoff must be capped at 5s (n={n})"
            );
            last = b;
        }
        // Saturates at the cap rather than growing unboundedly.
        assert_eq!(rpc_worker_restart_backoff(6), Duration::from_secs(5));
        assert_eq!(rpc_worker_restart_backoff(100), Duration::from_secs(5));
    }

    /// Move raw ggml device pointers + the bind CString into the worker thread.
    struct SendArgs(
        std::ffi::CString,
        Vec<crate::llama::sys::ggml_backend_dev_t>,
    );
    // SAFETY: the wrapped device pointer is a process-static ggml CPU device.
    unsafe impl Send for SendArgs {}

    /// Spawn an in-process RPC worker serving the local **CPU** device on
    /// `endpoint` (CPU, not GPU, so same-process loopback can't alias a GPU).
    /// Returns false if the CPU device or thread can't be obtained.
    fn start_cpu_rpc_worker(endpoint: &str) -> bool {
        let cpu = unsafe {
            crate::llama::sys::ggml_backend_dev_by_type(
                crate::llama::sys::GGML_BACKEND_DEVICE_TYPE_CPU,
            )
        };
        if cpu.is_null() {
            return false;
        }
        let args = SendArgs(std::ffi::CString::new(endpoint).unwrap(), vec![cpu]);
        std::thread::Builder::new()
            .name("test-rpc-worker".into())
            .spawn(move || {
                // Force whole-struct capture (Rust 2021 disjoint capture would
                // otherwise grab the non-Send `Vec<*mut>` field directly).
                let args = args;
                let SendArgs(ep, mut devs) = args;
                // Blocks for the life of the (test) process.
                unsafe {
                    crate::llama::sys::ggml_backend_rpc_start_server(
                        ep.as_ptr(),
                        std::ptr::null(),
                        2,
                        devs.len(),
                        devs.as_mut_ptr(),
                    );
                }
            })
            .is_ok()
    }

    fn wait_listening(addr: &str, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect(addr).is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    /// The set-shrink core: an RPC device's **description is its endpoint**, and
    /// `live_device_list_if_pruning_needed` prunes a worker that has left the live
    /// set (returning an explicit device list without it) while leaving the
    /// proven NULL-devices path (`None`) in place when the worker is present.
    ///
    /// Uses a real in-process CPU RPC worker — no model, no GPU. Skips (passes)
    /// gracefully if the environment can't bring an RPC worker up.
    #[test]
    fn pruning_excludes_a_dead_rpc_worker() {
        let endpoint = "127.0.0.1:51099";
        if !start_cpu_rpc_worker(endpoint) || !wait_listening(endpoint, 5) {
            eprintln!("skipped: could not start an in-process RPC worker here");
            return;
        }

        // Register the worker as an RPC device in ggml's global registry.
        let c = std::ffi::CString::new(endpoint).unwrap();
        let reg = unsafe { crate::llama::sys::ggml_backend_rpc_add_server(c.as_ptr()) };
        if reg.is_null() {
            eprintln!("skipped: add_server returned null (RPC unavailable here)");
            return;
        }
        unsafe { crate::llama::sys::ggml_backend_register(reg) };

        // Find the RPC device and assert its description == the endpoint — the
        // load-bearing fact set-shrink uses to identify a specific worker.
        let mut rpc_dev: Option<crate::llama::sys::ggml_backend_dev_t> = None;
        let n = unsafe { crate::llama::sys::ggml_backend_dev_count() };
        for i in 0..n {
            let dev = unsafe { crate::llama::sys::ggml_backend_dev_get(i) };
            let dreg = unsafe { crate::llama::sys::ggml_backend_dev_backend_reg(dev) };
            let reg_name =
                unsafe { std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_reg_name(dreg)) }
                    .to_string_lossy();
            if reg_name == "RPC" {
                let desc = unsafe {
                    std::ffi::CStr::from_ptr(crate::llama::sys::ggml_backend_dev_description(dev))
                }
                .to_string_lossy()
                .into_owned();
                if desc == endpoint {
                    rpc_dev = Some(dev);
                }
            }
        }
        let rpc_dev =
            rpc_dev.expect("RPC device with the endpoint as its description was not registered");

        // Worker LIVE → provider returns the endpoint → nothing to prune.
        let live = std::sync::Arc::new(std::sync::RwLock::new(vec![endpoint.to_string()]));
        set_rpc_worker_provider({
            let l = std::sync::Arc::clone(&live);
            move || l.read().unwrap().clone()
        });
        assert!(
            live_device_list_if_pruning_needed().is_none(),
            "a worker present in the live set must not trigger pruning",
        );

        // Worker DEAD → provider drops it → an explicit list excluding it.
        *live.write().unwrap() = Vec::new();
        let pruned = live_device_list_if_pruning_needed()
            .expect("a dead worker must produce an explicit pruned device list");
        assert!(
            !pruned.contains(&rpc_dev),
            "the pruned device list must exclude the dead RPC worker",
        );
    }
}

#[cfg(test)]
mod node_reserve_tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// The reserve is about what the machine is FOR, not how big it is. A large
    /// headless node lends nearly all of itself; a small desktop keeps room for
    /// its compositor. Proportional-to-total would get both backwards.
    #[test]
    fn the_reserve_tracks_posture_not_size() {
        if std::env::var_os("SOVEREIGN_LOCAL_FIT_RESERVE_GB").is_some() {
            return; // an explicit declaration outranks detection — tested below
        }
        // A big server: 2 GiB of 512 is a rounding error, as it should be.
        assert_eq!(host_reserve_bytes(512 * GIB, false), 2 * GIB);
        // The same server with a display attached keeps a compositor's worth.
        assert_eq!(host_reserve_bytes(512 * GIB, true), 8 * GIB);
        // A modest desktop keeps the SAME absolute amount as the big one — the
        // compositor's needs do not shrink because the box is smaller.
        assert_eq!(host_reserve_bytes(32 * GIB, true), 8 * GIB);
    }

    /// A host is never gated into uselessness by its own reserve, however small
    /// it is. Half of total is the floor on lendability.
    #[test]
    fn a_small_host_is_never_reserved_into_uselessness() {
        if std::env::var_os("SOVEREIGN_LOCAL_FIT_RESERVE_GB").is_some() {
            return;
        }
        assert_eq!(host_reserve_bytes(8 * GIB, true), 4 * GIB);
        assert_eq!(host_reserve_bytes(2 * GIB, true), GIB);
        assert_eq!(host_reserve_bytes(2 * GIB, false), GIB);
        for total in [1u64, 2, 4, 8, 16, 64, 256, 1024].map(|g| g * GIB) {
            for graphical in [true, false] {
                assert!(
                    host_reserve_bytes(total, graphical) * 2 <= total,
                    "reserve took more than half of a {total}-byte host"
                );
            }
        }
    }

    /// An explicit declaration outranks detection in both directions, including
    /// `0` — an operator who says "lend everything" gets that.
    #[test]
    fn a_declared_reserve_outranks_detection() {
        let _guard = EnvGuard::set("SOVEREIGN_LOCAL_FIT_RESERVE_GB", "0");
        assert_eq!(host_reserve_bytes(512 * GIB, true), 0);
        let _guard = EnvGuard::set("SOVEREIGN_LOCAL_FIT_RESERVE_GB", "40");
        assert_eq!(host_reserve_bytes(512 * GIB, false), 40 * GIB);
        // A declaration larger than the machine is clamped, not honoured.
        assert_eq!(host_reserve_bytes(16 * GIB, false), 16 * GIB);
    }

    /// `capacity_bytes` is the single collapse point, so the reserve must come
    /// off there — and off the TOTAL fallback too, not just the free reading.
    #[test]
    fn capacity_subtracts_the_reserve_on_both_branches() {
        let live = DeviceMemory {
            endpoint: None,
            free_bytes: 100 * GIB,
            total_bytes: 128 * GIB,
            reserve_bytes: 8 * GIB,
        };
        assert_eq!(live.capacity_bytes(), 92 * GIB);
        // free == 0 means "backend declined to report", so total is the basis.
        let no_free = DeviceMemory {
            free_bytes: 0,
            ..live.clone()
        };
        assert_eq!(no_free.capacity_bytes(), 120 * GIB);
        // A reserve larger than the reading floors at zero rather than wrapping.
        let tiny = DeviceMemory {
            free_bytes: GIB,
            reserve_bytes: 8 * GIB,
            ..live.clone()
        };
        assert_eq!(tiny.capacity_bytes(), 0);
    }

    /// A peer's device carries no reserve of ours: its budget is its own to
    /// declare, and inventing one here would apportion against a number the peer
    /// never agreed to.
    #[test]
    fn a_remote_device_carries_no_local_reserve() {
        let remote = DeviceMemory {
            endpoint: Some("10.0.0.2:50052".into()),
            free_bytes: 50 * GIB,
            total_bytes: 60 * GIB,
            reserve_bytes: 0,
        };
        assert_eq!(remote.capacity_bytes(), 50 * GIB);
    }

    /// Serialise env mutation within this module so the declaration tests cannot
    /// race the detection tests.
    struct EnvGuard(&'static str, Option<String>);
    impl EnvGuard {
        fn set(k: &'static str, v: &str) -> Self {
            let prev = std::env::var(k).ok();
            unsafe { std::env::set_var(k, v) };
            Self(k, prev)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.1 {
                Some(v) => unsafe { std::env::set_var(self.0, v) },
                None => unsafe { std::env::remove_var(self.0) },
            }
        }
    }
}
