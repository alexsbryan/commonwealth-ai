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
pub(crate) fn live_device_list_if_pruning_needed() -> Option<Vec<crate::llama::sys::ggml_backend_dev_t>> {
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
    pub(crate) overrides: Vec<(std::ffi::CString, crate::llama::sys::ggml_backend_buffer_type_t)>,
    pub(crate) plan: Vec<NodeShard>,
    pub(crate) assignments: Vec<RpcWarmAssignment>,
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
fn plan_distribution(model_path: &Path) -> Option<DistributionPlan> {
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

    let n_layer = match gguf_block_count(model_path) {
        Ok(Some(n)) if n > 0 => n,
        _ => return None, // can't plan deterministically without the layer count
    };

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
    let assignments: Vec<RpcWarmAssignment> = eligible_rpc
        .iter()
        .enumerate()
        .map(|(i, (_, ep))| RpcWarmAssignment {
            endpoint: ep.clone(),
            device_index: i,
        })
        .collect();
    // RPC-first device list (eligible only), then local GPU — the order
    // `plan_shards`/`with_devices` index, and the order `assignments` renumbered.
    let mut devs: Vec<crate::llama::sys::ggml_backend_dev_t> =
        eligible_rpc.into_iter().map(|(d, _)| d).collect();
    devs.extend(local);

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
            // quantized to coarse buckets.
            let weights: Vec<f32> = devs
                .iter()
                .map(|&d| {
                    let (mut free, mut total): (usize, usize) = (0, 0);
                    unsafe { crate::llama::sys::ggml_backend_dev_memory(d, &mut free, &mut total) };
                    quantize_vram((if free > 0 { free } else { total }) as u64) as f32
                })
                .collect();
            let computed = plan_shards(n_layer, &weights);
            tracing::info!(model = %key.0, devices = devs.len(), "plan_distribution: computed new shard plan");
            cache.insert(key.clone(), computed.clone());
            computed
        }
    };

    let overrides: Vec<(std::ffi::CString, crate::llama::sys::ggml_backend_buffer_type_t)> =
        override_patterns(&plan)
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
        assignments,
    })
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
    OwnedOverrides { auto_warm: bool },
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

/// Resolve how to place `model_path` across the local GPU and any mesh RPC
/// workers. NOT pure: when it intends to distribute it registers the workers (so
/// the plan can enumerate their devices), and for the auto-warm path it DRIVES
/// the orchestrator to seed every worker's shard before returning the override
/// plan. Glassbox: every branch logs the decision and its reason.
pub(crate) fn resolve_placement(model_path: &Path, model_bytes: u64, distributable: bool) -> LoadPlacement {
    let decision = classify_placement(
        distributable,
        rpc_workers_present(),
        model_bytes,
        safe_rpc_stream_bytes(),
        rpc_assume_warmed(),
        RPC_WARM_ORCHESTRATOR.get().is_some(),
    );

    let auto_warm = match decision {
        PlacementDecision::LocalOnly => return LoadPlacement::LocalOnly,
        PlacementDecision::StreamSplit => {
            // Publish the workers so the stream path can enumerate + split them.
            register_rpc_workers();
            return LoadPlacement::StreamSplit;
        }
        PlacementDecision::OwnedOverrides { auto_warm } => auto_warm,
    };

    // Owned-override path. Publish the workers, then plan the shards ONCE (shared
    // by warm + load, so they can't diverge — the plan-agreement invariant).
    register_rpc_workers();
    let Some(dist) = plan_distribution(model_path) else {
        tracing::warn!(
            model_mb = model_bytes / (1024 * 1024),
            "wanted to distribute a large primary but couldn't plan the shards (no RPC \
             device, GGUF block count unreadable, or unmappable worker) — loading local-only"
        );
        return LoadPlacement::LocalOnly;
    };

    if !auto_warm {
        tracing::info!(
            workers = dist.assignments.len(),
            "SOVEREIGN_RPC_ASSUME_WARMED set — trusting worker shards are warm (skipping auto-warm)"
        );
        return LoadPlacement::OwnedOverrides(dist);
    }

    // Auto-warm: ask the injected orchestrator to seed every worker's shard. It
    // blocks until they're warm (or gives up). Any failure → local-only (never
    // wedge); a later reload retries once the worker(s) are reachable.
    let Some(orchestrator) = RPC_WARM_ORCHESTRATOR.get() else {
        // classify_placement only returns auto_warm when an orchestrator is
        // present, so this is unreachable in practice — but never wedge.
        return LoadPlacement::LocalOnly;
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
            LoadPlacement::OwnedOverrides(dist)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "auto-warm failed — loading the primary local-only (never wedge); a later \
                 reload will retry once the worker(s) are reachable"
            );
            LoadPlacement::LocalOnly
        }
    }
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
                tracing::warn!(bind, "RPC bind contains an interior NUL — worker not started");
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
/// `~/.sovereign/rpc-cache`. Override with `SOVEREIGN_RPC_CACHE_DIR`; set that to
/// `off` / `0` / empty to disable caching. Returns `None` (caching off) when the
/// directory can't be created or no home dir is known.
fn rpc_cache_dir() -> Option<std::path::PathBuf> {
    let dir = match std::env::var("SOVEREIGN_RPC_CACHE_DIR") {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() || v.eq_ignore_ascii_case("off") || v == "0" {
                return None;
            }
            std::path::PathBuf::from(v)
        }
        Err(_) => dirs::home_dir()?.join(".sovereign").join("rpc-cache"),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "could not create RPC cache dir — caching disabled");
        return None;
    }
    Some(dir)
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
        assert!(!rpc_distribution_safe_decision(30_000 * mb, 512 * mb, false));
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
        assert_eq!(forced_choice_candidates(&CompletionRequest::default()), None);
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
            assert!(b <= Duration::from_secs(5), "backoff must be capped at 5s (n={n})");
            last = b;
        }
        // Saturates at the cap rather than growing unboundedly.
        assert_eq!(rpc_worker_restart_backoff(6), Duration::from_secs(5));
        assert_eq!(rpc_worker_restart_backoff(100), Duration::from_secs(5));
    }

    /// Move raw ggml device pointers + the bind CString into the worker thread.
    struct SendArgs(std::ffi::CString, Vec<crate::llama::sys::ggml_backend_dev_t>);
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

