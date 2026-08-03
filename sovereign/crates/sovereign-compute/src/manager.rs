// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ComputeChildManager`] — spawns and supervises the configured compute
//! children (one child per `[[compute.slot]]`), translating each
//! supervisor's lifecycle into the routable [`ChildRuntimeState`].
//!
//! [`ComputeRoutedProvider`] — the daemon's `InferenceProvider` facade: it
//! routes a request to a child when `model_id` names one (or, for
//! embeddings, when a capturing embed child is serving), and otherwise
//! delegates to the in-process engine. Default OFF: with no `[compute]`
//! slots it is never constructed and the daemon behaves exactly as before.
//!
//! There is no N-replica pool: a live embed run showed process replicas lose
//! to in-process batching for a fits-on-one-box model. The boundary is kept
//! for crash isolation + the can't-fit-one-box (distributed) case, where a
//! slot is exactly one child.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use serde::Serialize;
use sovereign_contracts::setup_config::{ComputeSection, ComputeSlotConfig};
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, ComputeChildStatus, InferenceProvider,
    ProviderCapabilities, ResidentSlot, Result, Speed, StreamFrame,
};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::child::{ChildLifecycle, ChildProvider, ChildRuntimeState};
use crate::client::ComputeChildClient;
use crate::distribution::DistributionHandoff;
use crate::supervisor::{
    HealthTarget, SpawnGate, SpawnVerdict, Supervisor, SupervisorConfig, SupervisorState,
};

/// Per-child supervision handles retained by the manager.
struct ManagedChild {
    name: String,
    role: String,
    model_id: String,
    supervisor: Arc<Supervisor>,
    state_rx: watch::Receiver<ChildRuntimeState>,
    _run: JoinHandle<()>,
    _collector: JoinHandle<()>,
}

/// A flat, serialisable status snapshot for one child (rendered by `/status`).
#[derive(Debug, Clone, Serialize)]
pub struct ChildStatusSnapshot {
    /// The child / addressable slot name.
    pub name: String,
    /// `"generate"` | `"embed"`.
    pub role: String,
    /// The addressable model id (== name).
    pub model_id: String,
    /// Current lifecycle phase.
    pub lifecycle: ChildLifecycle,
    /// OS process id, when serving/warming.
    pub pid: Option<u32>,
    /// Current ephemeral port, when serving/warming.
    pub port: Option<u16>,
    /// Restart count.
    pub restarts: u32,
    /// Reason for the most recent transition.
    pub last_transition_reason: String,
    /// Reason for the most recent exit/crash.
    pub last_exit: Option<String>,
}

/// What the daemon must tell the manager to host a distributed primary in a
/// child. Assembled from the daemon's models config — `[compute]` knows only
/// that the mode is on, not which GGUF is the primary.
#[derive(Debug, Clone)]
pub struct DistributedPrimarySpec {
    /// Addressable slot name (the shared-model id).
    pub name: String,
    /// The primary GGUF (first shard, for a split model).
    pub model: PathBuf,
    /// Context size for the child's slot; `None` = the child's default.
    pub context_size: Option<u32>,
    /// GPU layers for the child's slot; `None` = auto.
    pub n_gpu_layers: Option<u32>,
    /// Additional model ids that name this primary (e.g. the GGUF stem), so an
    /// explicitly-addressed request still routes to the child.
    pub model_ids: Vec<String>,
    /// Where the daemon writes the [`DistributionHandoff`] the child reads.
    /// A plain file on purpose: `cat` it and you know exactly which workers
    /// the running child was told to load across, and with which shard cut.
    pub handoff_path: PathBuf,
}

/// Owns the compute children + the model_id → child routes over them.
pub struct ComputeChildManager {
    children: Vec<ManagedChild>,
    routes: HashMap<String, Arc<ChildProvider>>,
    embed_child: Option<Arc<ChildProvider>>,
    /// The distributed-primary slot, when that mode is on. Created unspawned:
    /// the daemon spawns it once it has warmed a worker set, and respawns it
    /// whenever that set changes.
    distributed: Option<(Arc<DynamicChildSlot>, DistributedPrimarySpec)>,
}

impl ComputeChildManager {
    /// Spawn every warm slot declared in `section` and build the routes.
    /// `binary` is the executable to re-exec as `--compute-child`
    /// (`current_exe()` in production).
    pub fn start(section: &ComputeSection, binary: PathBuf, crash_log_dir: PathBuf) -> Arc<Self> {
        Self::start_with_distributed(section, binary, crash_log_dir, None)
    }

    /// As [`Self::start`], plus (optionally) the distributed-primary slot.
    pub fn start_with_distributed(
        section: &ComputeSection,
        binary: PathBuf,
        crash_log_dir: PathBuf,
        distributed_spec: Option<DistributedPrimarySpec>,
    ) -> Arc<Self> {
        let mut children = Vec::new();
        let mut routes = HashMap::new();
        let mut embed_child = None;

        for slot_cfg in &section.slot {
            if !slot_cfg.warm {
                continue;
            }
            let spec = ChildSpec::for_slot(slot_cfg);
            let (tx, _) = watch::channel(ChildRuntimeState::starting());
            let managed = spawn_managed(&spec, &binary, &crash_log_dir, LifecycleSink::single(tx));
            let provider = Arc::new(ChildProvider::new(
                spec.name.clone(),
                managed.state_rx.clone(),
            ));
            info!(
                target: "compute_child",
                slot = %slot_cfg.name,
                role = %slot_cfg.role,
                "compute child spawned"
            );
            if slot_cfg.role == "embed" && slot_cfg.capture_embed {
                embed_child = Some(Arc::clone(&provider));
            }
            routes.insert(slot_cfg.name.clone(), provider);
            children.push(managed);
        }

        let distributed = distributed_spec.map(|spec| {
            info!(
                target: "compute_child",
                slot = %spec.name,
                model = %spec.model.display(),
                "distributed-primary slot registered (unspawned — waits for a warmed worker set)"
            );
            (
                DynamicChildSlot::new(spec.clone(), binary.clone(), crash_log_dir.clone()),
                spec,
            )
        });

        Arc::new(Self {
            children,
            routes,
            embed_child,
            distributed,
        })
    }

    /// Routes keyed by addressable model id.
    pub fn routes(&self) -> &HashMap<String, Arc<ChildProvider>> {
        &self.routes
    }

    /// The embed child that captures all `/v1/embeddings`, if configured.
    pub fn embed_child(&self) -> Option<&Arc<ChildProvider>> {
        self.embed_child.as_ref()
    }

    /// The distributed-primary slot, for the daemon's worker-set-change loop.
    pub fn distributed_slot(&self) -> Option<Arc<DynamicChildSlot>> {
        self.distributed.as_ref().map(|(slot, _)| Arc::clone(slot))
    }

    /// The distributed primary's spawn spec (model path, ctx, gpu layers).
    pub fn distributed_spec(&self) -> Option<&DistributedPrimarySpec> {
        self.distributed.as_ref().map(|(_, spec)| spec)
    }

    /// Routing entry for the distributed primary, consumed by the facade.
    fn distributed_primary_route(&self) -> Option<DistributedPrimaryRoute> {
        self.distributed.as_ref().map(|(slot, spec)| {
            let mut model_ids = vec![spec.name.clone()];
            model_ids.extend(spec.model_ids.iter().cloned());
            model_ids.dedup();
            DistributedPrimaryRoute {
                slot: Arc::clone(slot),
                provider: slot.provider(),
                model_ids,
            }
        })
    }

    /// A status snapshot for every managed child, including the
    /// distributed-primary slot when configured.
    pub fn statuses(&self) -> Vec<ChildStatusSnapshot> {
        let mut out: Vec<ChildStatusSnapshot> = self
            .children
            .iter()
            .map(|c| {
                let st = c.state_rx.borrow();
                ChildStatusSnapshot {
                    name: c.name.clone(),
                    role: c.role.clone(),
                    model_id: c.model_id.clone(),
                    lifecycle: st.lifecycle,
                    pid: st.pid,
                    port: st.port,
                    restarts: st.restarts,
                    last_transition_reason: st.last_transition_reason.clone(),
                    last_exit: st.last_exit.clone(),
                }
            })
            .collect();
        if let Some((slot, _)) = &self.distributed {
            out.push(slot.status());
        }
        out
    }

    /// Gracefully stop every child (SIGTERM → grace → SIGKILL).
    pub fn shutdown(&self) {
        for c in &self.children {
            info!(target: "compute_child", child = %c.name, "terminating compute child");
            c.supervisor.terminate();
        }
        if let Some((slot, _)) = &self.distributed {
            slot.shutdown();
        }
    }

    /// Spawn a single model-free **mock** child under one generate-role slot —
    /// for the crash-isolation e2e (mock isn't a config role, so this is the
    /// only way to reach it through the manager). The mock streams
    /// `mock_tokens` tokens with `mock_token_delay_ms` between them so a test
    /// can `kill` mid-stream.
    pub fn start_mock_slot(
        name: &str,
        binary: PathBuf,
        crash_log_dir: PathBuf,
        mock_tokens: usize,
        mock_token_delay_ms: u64,
    ) -> Arc<Self> {
        let spec = ChildSpec::mock_slot(name, mock_tokens, mock_token_delay_ms);
        let (tx, _) = watch::channel(ChildRuntimeState::starting());
        let managed = spawn_managed(&spec, &binary, &crash_log_dir, LifecycleSink::single(tx));
        let provider = Arc::new(ChildProvider::new(
            spec.name.clone(),
            managed.state_rx.clone(),
        ));
        let mut routes = HashMap::new();
        routes.insert(name.to_string(), provider);
        Arc::new(Self {
            children: vec![managed],
            routes,
            embed_child: None,
            distributed: None,
        })
    }
}

/// How long a child may take to bind, hand back its port, and load its model
/// before the supervisor calls it stuck. Sized for a local GGUF load.
const DEFAULT_LOAD_DEADLINE: Duration = Duration::from_secs(180);

/// The same budget for a DISTRIBUTED primary. A ~90 GB model assembling across
/// mesh workers is a different order of magnitude: the daemon's own warm of one
/// worker took 3.5 minutes in the 2026-07-27 capture, and the child's `-ot` load
/// walks every shard afterwards. Too tight a deadline here doesn't protect
/// anything — it just restarts a child that was making progress.
const DISTRIBUTED_LOAD_DEADLINE: Duration = Duration::from_secs(1800);

/// The full spawn args for one child.
struct ChildSpec {
    name: String,
    role: String,
    model_id: String,
    args: Vec<String>,
    /// Extra environment for the child process (on top of the inherited
    /// daemon environment). The distributed primary uses this to assert
    /// `SOVEREIGN_RPC_ASSUME_WARMED`.
    env: Vec<(String, String)>,
    /// Handshake + model-load budget; see the two constants above.
    load_deadline: Duration,
    /// Precondition the supervisor consults before every spawn of this child,
    /// including its own crash restarts. `None` for static slots and mocks —
    /// only the distributed primary has an external reason a respawn could be
    /// provably futile.
    spawn_gate: Option<SpawnGate>,
}

impl ChildSpec {
    fn for_slot(slot: &ComputeSlotConfig) -> Self {
        let mut args = vec![
            "--compute-child".to_string(),
            "--role".to_string(),
            slot.role.clone(),
            "--name".to_string(),
            slot.name.clone(),
            "--bind".to_string(),
            "127.0.0.1:0".to_string(),
            "--model".to_string(),
            slot.model.display().to_string(),
        ];
        if let Some(ctx) = slot.context_size {
            args.push("--ctx".to_string());
            args.push(ctx.to_string());
        }
        if let Some(gpu) = slot.n_gpu_layers {
            args.push("--gpu-layers".to_string());
            args.push(gpu.to_string());
        }
        Self {
            name: slot.name.clone(),
            role: slot.role.clone(),
            model_id: slot.name.clone(),
            args,
            env: Vec::new(),
            load_deadline: DEFAULT_LOAD_DEADLINE,
            spawn_gate: None,
        }
    }

    /// The child that hosts the mesh's DISTRIBUTED primary.
    ///
    /// Differs from a static slot in three ways, each load-bearing:
    /// - `--distribution <handoff>` names the warmed worker set + the shard
    ///   plan they were warmed against (see [`crate::distribution`]).
    /// - `SOVEREIGN_RPC_ASSUME_WARMED=1` — no warm orchestrator exists inside a
    ///   child (it needs the daemon's mesh directory), so without this
    ///   assertion `classify_placement` refuses to distribute a large model and
    ///   silently falls back to a local load.
    /// - a much longer load deadline.
    fn distributed_primary(
        name: &str,
        model: &Path,
        context_size: Option<u32>,
        n_gpu_layers: Option<u32>,
        handoff_path: &Path,
    ) -> Self {
        let mut args = vec![
            "--compute-child".to_string(),
            "--role".to_string(),
            "generate".to_string(),
            "--name".to_string(),
            name.to_string(),
            "--bind".to_string(),
            "127.0.0.1:0".to_string(),
            "--model".to_string(),
            model.display().to_string(),
            "--distribution".to_string(),
            handoff_path.display().to_string(),
        ];
        if let Some(ctx) = context_size {
            args.push("--ctx".to_string());
            args.push(ctx.to_string());
        }
        if let Some(gpu) = n_gpu_layers {
            args.push("--gpu-layers".to_string());
            args.push(gpu.to_string());
        }
        Self {
            name: name.to_string(),
            role: "generate".to_string(),
            model_id: name.to_string(),
            args,
            env: vec![("SOVEREIGN_RPC_ASSUME_WARMED".to_string(), "1".to_string())],
            load_deadline: DISTRIBUTED_LOAD_DEADLINE,
            spawn_gate: None,
        }
    }

    /// A model-free mock child (crash-isolation e2e).
    fn mock_slot(name: &str, mock_tokens: usize, mock_token_delay_ms: u64) -> Self {
        let args = vec![
            "--compute-child".to_string(),
            "--role".to_string(),
            "mock".to_string(),
            "--name".to_string(),
            name.to_string(),
            "--bind".to_string(),
            "127.0.0.1:0".to_string(),
            "--mock-tokens".to_string(),
            mock_tokens.to_string(),
            "--mock-token-delay-ms".to_string(),
            mock_token_delay_ms.to_string(),
        ];
        Self {
            name: name.to_string(),
            role: "mock".to_string(),
            model_id: name.to_string(),
            args,
            env: Vec::new(),
            load_deadline: DEFAULT_LOAD_DEADLINE,
            // Ungated by construction: a mock child dials nothing, so there is
            // no external precondition that could make its spawn futile.
            spawn_gate: None,
        }
    }
}

/// Where one supervisor generation publishes its lifecycle transitions.
///
/// A statically-configured child has exactly one generation, so this is just a
/// `watch::Sender`. A [`DynamicChildSlot`] respawns with new argv across the
/// daemon's life and must keep the SAME channel (the routing facade holds a
/// receiver), so the sender is shared and each generation carries an epoch: a
/// superseded generation's late transition — an aborting ggml child can take
/// seconds to die — must not clobber the live child's state.
#[derive(Clone)]
struct LifecycleSink {
    tx: Arc<watch::Sender<ChildRuntimeState>>,
    generation: u64,
    live_generation: Arc<AtomicU64>,
}

impl LifecycleSink {
    /// A sink for a child that is never respawned with different args.
    fn single(tx: watch::Sender<ChildRuntimeState>) -> Self {
        Self {
            tx: Arc::new(tx),
            generation: 0,
            live_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// `true` iff this generation is still the live one.
    fn is_live(&self) -> bool {
        self.live_generation.load(Ordering::SeqCst) == self.generation
    }

    /// Publish, unless this generation has been superseded or the channel is
    /// closed. `false` means "stop collecting".
    fn send(&self, state: ChildRuntimeState) -> bool {
        self.is_live() && self.tx.send(state).is_ok()
    }

    fn current(&self) -> ChildRuntimeState {
        self.tx.borrow().clone()
    }
}

/// Build the supervisor for one child, wire its lifecycle collector, and
/// spawn both tasks.
fn spawn_managed(
    spec: &ChildSpec,
    binary: &Path,
    crash_log_dir: &Path,
    sink: LifecycleSink,
) -> ManagedChild {
    let config = SupervisorConfig {
        binary_path: binary.to_path_buf(),
        args: spec.args.clone(),
        working_dir: None,
        env: spec.env.clone(),
        health: HealthTarget::StdoutHandshake {
            health_path: crate::wire::ROUTE_HEALTH.to_string(),
            handshake_deadline: spec.load_deadline,
        },
        crash_log_dir: crash_log_dir.to_path_buf(),
        heartbeat_interval: Duration::from_secs(2),
        heartbeat_timeout: Duration::from_secs(5),
        heartbeat_failure_threshold: 3,
        ready_deadline: spec.load_deadline,
        backoff_schedule: vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(10),
            Duration::from_secs(60),
        ],
        crash_loop_window: Duration::from_secs(600),
        crash_loop_max: 5,
        stderr_ring_lines: 200,
    };

    let supervisor = Arc::new(match spec.spawn_gate.clone() {
        Some(gate) => Supervisor::new(config).with_spawn_gate(gate),
        None => Supervisor::new(config),
    });
    let state_rx = sink.tx.subscribe();
    let collector = tokio::spawn(collect_lifecycle(
        spec.name.clone(),
        Arc::clone(&supervisor),
        sink,
    ));
    let run = {
        let s = Arc::clone(&supervisor);
        tokio::spawn(async move { s.run().await })
    };

    ManagedChild {
        name: spec.name.clone(),
        role: spec.role.clone(),
        model_id: spec.model_id.clone(),
        supervisor,
        state_rx,
        _run: run,
        _collector: collector,
    }
}

/// Translate a child's `SupervisorState` broadcast into the routable
/// [`ChildRuntimeState`], rebuilding the client when it becomes serving and
/// tracing every lifecycle transition (glassbox, target `compute_child`).
async fn collect_lifecycle(name: String, supervisor: Arc<Supervisor>, sink: LifecycleSink) {
    let mut sub = supervisor.subscribe();
    let mut port: Option<u16> = None;
    let mut pid: Option<u32> = None;
    let mut restarts: u32 = 0;

    loop {
        let state = match sub.recv().await {
            Ok(s) => s,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };

        let (lifecycle, reason, serving, exit) = match state {
            SupervisorState::Starting => {
                port = None;
                pid = None;
                (
                    ChildLifecycle::Starting,
                    "starting".to_string(),
                    false,
                    None,
                )
            }
            SupervisorState::Warming {
                pid: child_pid,
                port: p,
            } => {
                port = Some(p);
                pid = Some(child_pid);
                (ChildLifecycle::Warming, "warming".to_string(), false, None)
            }
            SupervisorState::Healthy { pid: child_pid, .. } => {
                pid = Some(child_pid);
                (ChildLifecycle::Serving, "serving".to_string(), true, None)
            }
            SupervisorState::Unhealthy {
                consecutive_failures,
                ..
            } => (
                ChildLifecycle::Degraded,
                format!("{consecutive_failures} failed health probes"),
                false,
                None,
            ),
            SupervisorState::Restarting { reason, .. } => {
                restarts += 1;
                port = None;
                pid = None;
                (
                    ChildLifecycle::Restarting,
                    reason.clone(),
                    false,
                    Some(reason),
                )
            }
            SupervisorState::Failed { reason, .. } => {
                port = None;
                pid = None;
                (ChildLifecycle::Failed, reason.clone(), false, Some(reason))
            }
            // A held gate is NOT a restart: deliberately no `restarts += 1`
            // (contrast the `Restarting` arm above). We are between children
            // for an external reason, which is `Restarting` as far as routing
            // is concerned — the facade fail-fasts either way — but counting it
            // would misreport a healthy wait as instability. No new
            // `ChildLifecycle` variant, so `/status` and the desktop are
            // unchanged; the reason string carries the glassbox.
            SupervisorState::GateHeld { reason } => {
                port = None;
                pid = None;
                (
                    ChildLifecycle::Restarting,
                    reason.clone(),
                    false,
                    Some(reason),
                )
            }
        };

        let client = if serving {
            match port.and_then(|p| ComputeChildClient::from_port(p).ok()) {
                Some(c) => Some(c),
                None => {
                    warn!(target: "compute_child", child = %name, "serving but no port to build client");
                    None
                }
            }
        } else {
            None
        };

        // A superseded generation stops here: after a respawn the old
        // supervisor's SIGTERM/exit transitions are history, not the live
        // slot's state, and publishing them would show the fresh child as
        // dead.
        if !sink.is_live() {
            info!(
                target: "compute_child",
                child = %name,
                generation = sink.generation,
                "lifecycle collector retired (superseded by a respawn)"
            );
            break;
        }

        let prev = sink.current().lifecycle;
        if prev != lifecycle {
            info!(
                target: "compute_child",
                child = %name,
                from = prev.as_str(),
                to = lifecycle.as_str(),
                reason = %reason,
                "lifecycle transition"
            );
        }

        if !sink.send(ChildRuntimeState {
            lifecycle,
            pid,
            port,
            client,
            restarts,
            last_transition_reason: reason,
            last_exit: exit,
        }) {
            break;
        }
    }
}

/// A compute child whose spawn arguments change over the daemon's life.
///
/// The distributed primary is the case this exists for: its worker set changes
/// as mesh anchors join and leave, and the response to a change is **kill the
/// child and spawn a fresh one against the new set** — never an in-place
/// reload. That is not a stylistic choice. A graceful reload has to free the
/// old sharded model's buffers on workers that may already be gone, and ggml's
/// RPC client has no error path for a dead endpoint: it aborts the process. On
/// 2026-07-27 that is exactly how the daemon died — the shrink-fast-prune
/// reload, the very mechanism meant to protect the host from a departed worker,
/// hit `ggml-rpc.cpp:386` and SIGABRT'd (note c4ef6fa0). Inside a child that
/// abort is contained; respawning avoids it entirely.
///
/// The `Arc<ChildProvider>` handed to the routing facade is built once and
/// survives every respawn — the watch channel is owned here, not by a
/// generation. While a respawn is in flight the provider fail-fasts with
/// `ComputeUnavailable`, which is the intended posture: callers cascade to a
/// mesh peer or get a clean 503, never a daemon abort.
pub struct DynamicChildSlot {
    name: String,
    role: String,
    model_id: String,
    /// Model, context size, GPU layers, handoff path — everything about this
    /// slot that does NOT change over the daemon's life. Only the worker set
    /// changes, which is why respawn takes just a handoff.
    spec: DistributedPrimarySpec,
    binary: PathBuf,
    crash_log_dir: PathBuf,
    /// Publish side, shared by every generation's collector.
    tx: Arc<watch::Sender<ChildRuntimeState>>,
    /// The generation whose transitions are authoritative. Bumped on every
    /// respawn/retire so a dying generation can't clobber the live state.
    live_generation: Arc<AtomicU64>,
    provider: Arc<ChildProvider>,
    /// The generation currently running; `None` once retired.
    current: Mutex<Option<ManagedChild>>,
    /// When the live generation was spawned; `None` once retired. Read by the
    /// daemon's discovery loop so it cannot tear down a child that has only just
    /// started — a distributed child is still walking its shards seconds in.
    spawned_at: Mutex<Option<std::time::Instant>>,
    /// Precondition consulted before every spawn of this slot's child,
    /// INCLUDING the supervisor's own crash restarts. Installed by the daemon's
    /// discovery loop, which is the only component that knows which RPC workers
    /// are currently eligible. Takes the endpoints the LIVE handoff pinned, so
    /// the verdict is about the worker set this generation will actually dial.
    gate: Mutex<Option<PinnedSpawnGate>>,
    /// The handoff the LIVE generation was spawned against; `None` before the
    /// first respawn and once retired.
    ///
    /// Kept so the parent can state the child's placement on `/status`. The
    /// child performs the load, so the split lives in the child's own
    /// process-global cell and the parent cannot read it — which is why
    /// `/status` used to report the mode with an empty split. This is the
    /// parent's own copy of the cut it warmed and handed over, so reporting from
    /// it needs no IPC and cannot disagree with what the child was told.
    live_handoff: Mutex<Option<DistributionHandoff>>,
}

/// What the slot knows about the spawn the supervisor is about to make, handed
/// to the gate so the policy can judge it.
///
/// Everything here comes from the handoff THIS generation was warmed against —
/// i.e. what the child will actually dial and actually load when it re-reads
/// that file at startup, not what the discovery loop last planned.
#[derive(Debug, Clone, Copy)]
pub struct SpawnContext<'a> {
    /// Endpoints the live handoff names. The child will dial exactly these.
    pub pinned: &'a [String],
    /// Transformer blocks that will stay on THIS host. With `total_blocks`,
    /// this is the fraction of the weights the spawn will ask the local device
    /// for — the term a memory precondition needs.
    pub local_blocks: u32,
    /// Total blocks the plan apportions. `0` when there is no block plan.
    pub total_blocks: u32,
}

/// A [`SpawnGate`] that has not yet been bound to a generation: the daemon
/// supplies the policy, the slot supplies the facts of whichever handoff is
/// live when a spawn is attempted.
///
/// The context carries the CUT as well as the endpoints because "may we spawn"
/// has two independent answers. The worker question (are the pinned peers still
/// there?) was the original one. The memory question — would this spawn starve
/// the host? — was added after 2026-08-02, when a respawn 7 s after a crash took
/// the machine to 1.8 GB free and Mesa aborted the desktop compositor on an
/// ENOMEM submit. Only the slot knows the cut; only the daemon knows the policy.
pub type PinnedSpawnGate = Arc<dyn Fn(&SpawnContext<'_>) -> SpawnVerdict + Send + Sync>;

impl DynamicChildSlot {
    /// Create the slot WITHOUT spawning anything. The provider is live
    /// immediately and fail-fasts until the first [`Self::respawn_distributed`],
    /// so the daemon can install routing before the cluster has formed.
    pub fn new(spec: DistributedPrimarySpec, binary: PathBuf, crash_log_dir: PathBuf) -> Arc<Self> {
        let (tx, rx) = watch::channel(ChildRuntimeState::starting());
        let provider = Arc::new(ChildProvider::new(spec.name.clone(), rx));
        Arc::new(Self {
            name: spec.name.clone(),
            role: "generate".to_string(),
            model_id: spec.name.clone(),
            spec,
            binary,
            crash_log_dir,
            tx: Arc::new(tx),
            live_generation: Arc::new(AtomicU64::new(0)),
            provider,
            current: Mutex::new(None),
            spawned_at: Mutex::new(None),
            gate: Mutex::new(None),
            live_handoff: Mutex::new(None),
        })
    }

    /// Install the spawn precondition for this slot's children.
    ///
    /// Separate from construction because the knowledge is: the slot is built
    /// during provider assembly, while the eligible-worker snapshot the gate
    /// reads only exists once the discovery loop starts.
    /// How long the live generation has been running, or `None` when the slot
    /// holds no child.
    pub fn spawned_at(&self) -> Option<std::time::Instant> {
        *self.spawned_at.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_spawn_gate(&self, gate: PinnedSpawnGate) {
        *self.gate.lock().unwrap_or_else(|e| e.into_inner()) = Some(gate);
    }

    /// The primary GGUF this slot's child loads — the daemon warms against
    /// exactly this path.
    pub fn model_path(&self) -> &Path {
        &self.spec.model
    }

    /// Where the live generation's weights actually are: which blocks went to
    /// which remote worker and how many stayed local. `None` before the first
    /// respawn and once retired — in both cases there is no child holding a
    /// split, and a stated placement would be a claim about nothing.
    pub fn placement(&self) -> Option<sovereign_core::traits::SlotPlacement> {
        self.live_handoff
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| h.placement())
    }

    /// Context size the child's slot loads with; `None` = the child's own
    /// default ([`crate::child_main::DEFAULT_CTX`]). The daemon's warm path
    /// reads this so its memory projection sizes KV for the context the child
    /// will actually build.
    pub fn context_size(&self) -> Option<u32> {
        self.spec.context_size
    }

    /// The endpoints the LIVE generation's handoff names — what the child is
    /// actually dialing right now. Empty before the first respawn and once
    /// retired. Read by the daemon's discovery loop as engagement evidence:
    /// a child warming/serving across an endpoint vouches for that worker
    /// while its single-connection RPC server cannot answer probes.
    pub fn pinned_endpoints(&self) -> Vec<String> {
        self.live_handoff
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| h.endpoints.clone())
            .unwrap_or_default()
    }

    /// The stable routing handle. Valid across respawns.
    pub fn provider(&self) -> Arc<ChildProvider> {
        Arc::clone(&self.provider)
    }

    /// Subscribe to this slot's lifecycle transitions.
    ///
    /// The channel is owned by the SLOT, not by a generation, so the receiver
    /// survives every respawn and retire — the same guarantee [`Self::provider`]
    /// gives the routing facade.
    ///
    /// Exists because what this node ADVERTISES has to track what it can
    /// actually serve. The mesh self-manifest is a snapshot taken at daemon
    /// construction, when this slot has deliberately not spawned yet
    /// ([`Self::new`]), so the heavyweight model is absent from it and every
    /// request that names the shared model 503s from a healthy cluster (live
    /// 2026-07-28, note c5678d34). A subscriber can now rebuild on the
    /// transition instead of guessing at an interval — and, just as important,
    /// UN-advertise on retire, so peers never route into a slot that is parked.
    ///
    /// The receiver starts marked-seen: `changed()` awaits the NEXT transition.
    /// A subscriber that must not miss the current state should read it once
    /// (`borrow_and_update`) before its first await.
    pub fn subscribe(&self) -> watch::Receiver<ChildRuntimeState> {
        self.tx.subscribe()
    }

    /// The slot's addressable model id (== its name).
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Replace the running child with one loaded across `handoff`'s worker set.
    /// Terminates the previous generation first (SIGTERM → grace → SIGKILL,
    /// which also covers a child stuck in ggml's abort handler — that handler
    /// shells out to gdb and can take seconds to die).
    ///
    /// The handoff is written to disk before the spawn, so the child reads the
    /// same bytes an operator can inspect.
    ///
    /// Must be called from within a Tokio runtime (it spawns the supervisor
    /// tasks). Non-blocking: the new child loads asynchronously and the slot
    /// reports `warming` until it serves.
    pub fn respawn_distributed(
        &self,
        handoff: &DistributionHandoff,
    ) -> std::result::Result<(), String> {
        handoff.write(&self.spec.handoff_path).map_err(|e| {
            format!(
                "cannot write distribution handoff {}: {e}",
                self.spec.handoff_path.display()
            )
        })?;
        // Remember the cut before spawning, so `/status` can state this
        // generation's placement from the moment it exists. Recorded from the
        // handoff we just persisted rather than from the child, which owns the
        // load but is in another process.
        let placement = handoff.placement();
        *self
            .live_handoff
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handoff.clone());
        info!(
            target: "compute_child",
            child = %self.name,
            model = %self.spec.model.display(),
            workers = handoff.endpoints.len(),
            endpoints = ?handoff.endpoints,
            total_blocks = placement.total_blocks,
            local_blocks = placement.local_blocks,
            handoff = %self.spec.handoff_path.display(),
            "distributed primary: respawning the child across the warmed worker set"
        );
        let mut spec = ChildSpec::distributed_primary(
            &self.name,
            &self.spec.model,
            self.spec.context_size,
            self.spec.n_gpu_layers,
            &self.spec.handoff_path,
        );
        // Bind the gate to THIS generation's warmed endpoints — deliberately
        // the handoff's set (what the child will dial), not the discovery
        // loop's `attempted` set, which can legitimately be a superset when a
        // worker went ineligible between planning and warming.
        spec.spawn_gate = self
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map(|g| {
                let pinned = handoff.endpoints.clone();
                let (local_blocks, total_blocks) =
                    (placement.local_blocks, placement.total_blocks);
                Arc::new(move || {
                    g(&SpawnContext {
                        pinned: &pinned,
                        local_blocks,
                        total_blocks,
                    })
                }) as SpawnGate
            });
        self.swap_generation(spec);
        Ok(())
    }

    /// Terminate the running generation (if any) and start `spec` as the new
    /// one, on the SAME watch channel.
    ///
    /// The generation counter is what makes this safe. A child being replaced
    /// can take seconds to actually exit — a ggml abort handler shells out to
    /// gdb before dying — and its final `Restarting`/`Failed` transitions would
    /// otherwise land after the fresh child's and show a healthy slot as dead.
    fn swap_generation(&self, spec: ChildSpec) {
        let generation = self.live_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let sink = LifecycleSink {
            tx: Arc::clone(&self.tx),
            generation,
            live_generation: Arc::clone(&self.live_generation),
        };

        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = current.take() {
            info!(
                target: "compute_child",
                child = %self.name,
                generation,
                "terminating the previous child before respawn"
            );
            previous.supervisor.terminate();
        }
        // The fresh generation starts from a clean state — otherwise the
        // facade would keep routing to the terminated child's client until
        // the new supervisor's first transition lands.
        let _ = self.tx.send(ChildRuntimeState::starting());
        *current = Some(spawn_managed(
            &spec,
            &self.binary,
            &self.crash_log_dir,
            sink,
        ));
        *self.spawned_at.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(std::time::Instant::now());
    }

    /// Respawn this slot with a model-free **mock** child.
    ///
    /// The respawn machinery is the risky new part of the distributed-primary
    /// path — a superseded generation must not clobber the live one, and the
    /// routing handle must survive the swap — and none of that needs a GGUF to
    /// exercise. This is the seam the crash-isolation e2e drives, mirroring
    /// [`ComputeChildManager::start_mock_slot`].
    pub fn respawn_mock(&self, mock_tokens: usize, mock_token_delay_ms: u64) {
        let spec = ChildSpec::mock_slot(&self.name, mock_tokens, mock_token_delay_ms);
        self.swap_generation(spec);
    }

    /// Stop the child and park the slot in a terminal not-serving state.
    ///
    /// This is the "cluster can't hold the model" posture — quorum lost, no
    /// eligible workers, warm failed. Mirrors `LoadPlacement::InsufficientCluster`:
    /// stay unavailable and wait for the next worker-set change, rather than
    /// fall back to a local load that would starve the host.
    pub fn retire(&self, reason: &str) {
        // Bump first: the terminated generation's exit transitions are now
        // stale and must not overwrite the reason we publish below.
        self.live_generation.fetch_add(1, Ordering::SeqCst);
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = current.take() {
            previous.supervisor.terminate();
        }
        *self.spawned_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // A retired slot has no placement. Keeping the last one would let
        // `/status` describe a split that no process is holding.
        *self
            .live_handoff
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        info!(
            target: "compute_child",
            child = %self.name,
            reason,
            "distributed-primary child retired — slot stays unavailable until the cluster re-forms"
        );
        // Read the restart count BEFORE sending: holding a `borrow()` guard
        // across `send()` would deadlock the watch channel's lock.
        let restarts = self.tx.borrow().restarts;
        let _ = self.tx.send(ChildRuntimeState {
            lifecycle: ChildLifecycle::Failed,
            pid: None,
            port: None,
            client: None,
            restarts,
            last_transition_reason: reason.to_string(),
            last_exit: Some(reason.to_string()),
        });
    }

    /// `true` iff a child is currently spawned (whatever its lifecycle).
    pub fn is_spawned(&self) -> bool {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Status snapshot for `/status.inference.compute_children`.
    pub fn status(&self) -> ChildStatusSnapshot {
        let st = self.tx.borrow();
        ChildStatusSnapshot {
            name: self.name.clone(),
            role: self.role.clone(),
            model_id: self.model_id.clone(),
            lifecycle: st.lifecycle,
            pid: st.pid,
            port: st.port,
            restarts: st.restarts,
            last_transition_reason: st.last_transition_reason.clone(),
            last_exit: st.last_exit.clone(),
        }
    }

    /// Terminate for good (daemon shutdown).
    pub fn shutdown(&self) {
        self.retire("daemon shutting down");
    }
}

/// The daemon-side facade: single-child routing over an in-process fallback.
pub struct ComputeRoutedProvider {
    inner: Arc<dyn InferenceProvider>,
    routes: HashMap<String, Arc<ChildProvider>>,
    embed_child: Option<Arc<ChildProvider>>,
    /// The manager backing these children (for `/status`). `None` in tests
    /// that construct routes directly without spawning processes.
    manager: Option<Arc<ComputeChildManager>>,
    /// The child hosting the mesh's distributed primary, when that mode is on.
    distributed_primary: Option<DistributedPrimaryRoute>,
}

/// Routing for the distributed primary: which requests belong to it, and the
/// slot that owns the child serving them.
struct DistributedPrimaryRoute {
    slot: Arc<DynamicChildSlot>,
    provider: Arc<ChildProvider>,
    /// Model ids that name this primary explicitly (the slot name and the
    /// GGUF's own id). A request naming one of these is primary traffic no
    /// matter what speed it asked for.
    model_ids: Vec<String>,
}

impl DistributedPrimaryRoute {
    /// Is this request primary-class work?
    ///
    /// Two ways in, mirroring the engine's own `select_slot`: it names the
    /// primary outright, or it names nothing and asks for a slow/medium
    /// (i.e. substantive) answer. An unnamed `Speed::Fast` request is NOT
    /// captured — the daemon still owns the fast, embed, and code slots
    /// in-process, and sending a title-generation call across a mesh-sharded
    /// 122B would be absurd.
    fn claims(&self, request: &CompletionRequest) -> bool {
        match request.model_id.as_deref() {
            Some(id) => self.model_ids.iter().any(|m| m == id),
            None => matches!(request.preferred_speed, Speed::Slow | Speed::Medium),
        }
    }
}

impl ComputeRoutedProvider {
    /// Wrap `inner` with routing to `manager`'s children.
    pub fn new(inner: Arc<dyn InferenceProvider>, manager: Arc<ComputeChildManager>) -> Self {
        Self {
            routes: manager.routes().clone(),
            embed_child: manager.embed_child().cloned(),
            distributed_primary: manager.distributed_primary_route(),
            manager: Some(manager),
            inner,
        }
    }

    /// Construct directly from routes (no manager) — for tests.
    pub fn with_routes(
        inner: Arc<dyn InferenceProvider>,
        routes: HashMap<String, Arc<ChildProvider>>,
        embed_child: Option<Arc<ChildProvider>>,
    ) -> Self {
        Self {
            inner,
            routes,
            embed_child,
            manager: None,
            distributed_primary: None,
        }
    }

    /// The child a request addresses, if any: an explicitly-named static slot
    /// first, then the distributed primary's claim.
    ///
    /// A claimed request is NEVER handed back to `inner` when the child is
    /// down. That is deliberate: `inner` would try to load the distributed
    /// model in-process, which is both halves of the 2026-07-27 incident at
    /// once — a host-starving local load, and ggml aborts inside the daemon.
    /// The child's fail-fast `ComputeUnavailable` is the correct answer;
    /// callers cascade to a mesh peer or get a clean 503.
    fn child_for(&self, request: &CompletionRequest) -> Option<&Arc<ChildProvider>> {
        if let Some(child) = request.model_id.as_deref().and_then(|m| self.routes.get(m)) {
            return Some(child);
        }
        self.distributed_primary
            .as_ref()
            .filter(|d| d.claims(request))
            .map(|d| &d.provider)
    }

    /// The manager (for `/status`).
    pub fn manager(&self) -> Option<&Arc<ComputeChildManager>> {
        self.manager.as_ref()
    }

    /// The distributed-primary slot behind this facade, if the mode is on —
    /// the handle the daemon's worker-discovery loop respawns and retires.
    pub fn distributed_slot(&self) -> Option<Arc<DynamicChildSlot>> {
        self.distributed_primary
            .as_ref()
            .map(|d| Arc::clone(&d.slot))
    }
}

#[async_trait]
impl InferenceProvider for ComputeRoutedProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        match self.child_for(request) {
            Some(child) => child.complete(request).await,
            None => self.inner.complete(request).await,
        }
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        match self.child_for(request) {
            Some(child) => child.complete_stream(request).await,
            None => self.inner.complete_stream(request).await,
        }
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        match self.child_for(request) {
            Some(child) => child.complete_stream_with_finish(request).await,
            None => self.inner.complete_stream_with_finish(request).await,
        }
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // A capturing embed child takes ALL embeddings while serving; else
        // fall back to the in-process embed slot.
        if let Some(child) = &self.embed_child {
            if child.is_serving() {
                return child.embed(text).await;
            }
        }
        self.inner.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if let Some(child) = &self.embed_child {
            if child.is_serving() {
                return child.embed_batch(texts).await;
            }
        }
        self.inner.embed_batch(texts).await
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        if let Some(child) = &self.embed_child {
            if child.is_serving() {
                return child.embed_query(query).await;
            }
        }
        self.inner.embed_query(query).await
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.inner.rerank_batch(query, docs).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn model_id_for(&self, speed: Speed) -> String {
        // With the primary withheld from the in-process engine, `inner` would
        // answer the Slow tier with the small FAST model — so this node would
        // advertise a 0.8B as its heavyweight (`oicp_synthesis`) while a child
        // serves the 122B. Answer with the child's id while it is serving; fall
        // back to `inner` when it is not, so a request still lands somewhere
        // that can answer.
        if matches!(speed, Speed::Slow | Speed::Medium) {
            if let Some(d) = &self.distributed_primary {
                if d.provider.is_serving() {
                    return d.slot.model_id().to_string();
                }
            }
        }
        self.inner.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        self.inner.embed_model_id()
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.inner.effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.inner.n_ctx_train_for_primary()
    }

    fn count_tokens(&self, text: &str) -> u32 {
        self.inner.count_tokens(text)
    }

    fn code_model_id(&self) -> Option<String> {
        self.inner.code_model_id()
    }

    fn fim_slot_info(&self) -> Option<sovereign_core::types::FimSlotInfo> {
        // FIM is served by the in-process engine, never fanned out
        // to compute children — forward the inner arrangement.
        self.inner.fim_slot_info()
    }

    async fn warmup_primary(&self) -> Result<()> {
        self.inner.warmup_primary().await
    }

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.inner.load_extra_slot(slot_name, path, context_size)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.inner.unload_extra_slot(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        // Advertise generate child ids so the mesh self-manifest lists them
        // as locally available (locate_named_model routes Local → this facade).
        let mut v = self.inner.extras_inventory();
        for (id, _child) in &self.routes {
            // embed children are captured via /v1/embeddings, not model_id.
            if self
                .embed_child
                .as_ref()
                .is_none_or(|e| e.name() != id.as_str())
            {
                v.push((id.clone(), id.clone()));
            }
        }
        // The distributed primary is advertised only while its child is
        // actually serving. Unlike a static slot — a local GGUF that reloads in
        // seconds — this one is unavailable whenever the cluster is re-forming,
        // and advertising it then would pull peer traffic into a guaranteed 503.
        if let Some(d) = &self.distributed_primary {
            if d.provider.is_serving() {
                for id in &d.model_ids {
                    v.push((id.clone(), id.clone()));
                }
            }
        }
        v
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        let mut slots = self.inner.resident_slots();
        // The primary is in another process, so `inner` cannot see it and
        // `/status` would show a node with no primary at all. Say where it
        // actually is — mode `child-distributed` — rather than leave the
        // operator to infer it from the compute_children array.
        //
        // The split comes from the handoff this slot spawned its child against
        // (`DynamicChildSlot::placement`). Until 2026-07-29 it was hardcoded to
        // `total_blocks: 0, local_blocks: 0, workers: []` on the reasoning that
        // the parent could not see into the child. True of the child's own
        // placement cell, but the parent PLANNED and WARMED this cut and still
        // holds it — so the blank was avoidable, and it was not harmless:
        // `svrn mesh bench` hashes `placement_digest` from this report, so every
        // distributed measurement was keyed and rendered as a one-node local
        // run, unfindable by the `mesh plan` lookup it exists to answer.
        if let Some(d) = &self.distributed_primary {
            let serving = d.provider.is_serving();
            slots.push(ResidentSlot {
                role: "primary".to_string(),
                model_id: d.slot.model_id().to_string(),
                resident: serving,
                size_bytes: None,
                transitioning: !serving && d.slot.is_spawned(),
                placement: d.slot.placement(),
            });
        }
        slots
    }

    fn compute_children(&self) -> Vec<ComputeChildStatus> {
        match &self.manager {
            Some(m) => m
                .statuses()
                .into_iter()
                .map(|s| ComputeChildStatus {
                    name: s.name,
                    role: s.role,
                    model_id: s.model_id,
                    lifecycle: s.lifecycle.as_str().to_string(),
                    port: s.port,
                    restarts: s.restarts,
                    last_transition_reason: s.last_transition_reason,
                    last_exit: s.last_exit,
                })
                .collect(),
            None => Vec::new(),
        }
    }
}

/// Build the compute layer: start the manager and wrap `inner` in the
/// routing facade. Returns `None` when the section is disabled or declares
/// no slots (the daemon then uses `inner` unchanged).
pub fn build_compute_layer(
    section: &ComputeSection,
    inner: Arc<dyn InferenceProvider>,
    binary: PathBuf,
    crash_log_dir: PathBuf,
) -> Option<(Arc<ComputeRoutedProvider>, Arc<ComputeChildManager>)> {
    build_compute_layer_with_distributed(section, inner, binary, crash_log_dir, None)
}

/// As [`build_compute_layer`], plus the distributed-primary slot. The layer is
/// built when EITHER static slots or a distributed primary is configured — the
/// distributed mode needs no `[[compute.slot]]` entries of its own.
pub fn build_compute_layer_with_distributed(
    section: &ComputeSection,
    inner: Arc<dyn InferenceProvider>,
    binary: PathBuf,
    crash_log_dir: PathBuf,
    distributed_spec: Option<DistributedPrimarySpec>,
) -> Option<(Arc<ComputeRoutedProvider>, Arc<ComputeChildManager>)> {
    if !section.enabled || (section.slot.is_empty() && distributed_spec.is_none()) {
        return None;
    }
    let wants_distributed = distributed_spec.is_some();
    let manager =
        ComputeChildManager::start_with_distributed(section, binary, crash_log_dir, distributed_spec);
    if manager.routes().is_empty() && !wants_distributed {
        return None;
    }
    let facade = Arc::new(ComputeRoutedProvider::new(inner, Arc::clone(&manager)));
    Some((facade, manager))
}

#[cfg(test)]
mod distributed_slot_tests {
    use super::*;
    use sovereign_inference::embedded::NodeShard;

    fn spec(dir: &Path) -> DistributedPrimarySpec {
        DistributedPrimarySpec {
            name: "shared-122b".to_string(),
            model: dir.join("Qwen3.5-122B-00001-of-00003.gguf"),
            context_size: Some(32768),
            n_gpu_layers: None,
            model_ids: vec!["Qwen3.5-122B-00001-of-00003".to_string()],
            handoff_path: dir.join("distribution.json"),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dyn-slot-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn route(spec: &DistributedPrimarySpec, provider: Arc<ChildProvider>) -> DistributedPrimaryRoute {
        let slot = DynamicChildSlot::new(spec.clone(), PathBuf::from("/nonexistent"), scratch("route"));
        let mut model_ids = vec![spec.name.clone()];
        model_ids.extend(spec.model_ids.iter().cloned());
        DistributedPrimaryRoute {
            slot,
            provider,
            model_ids,
        }
    }

    fn request(model_id: Option<&str>, speed: Speed) -> CompletionRequest {
        let mut r = CompletionRequest::default();
        r.model_id = model_id.map(|s| s.to_string());
        r.preferred_speed = speed;
        r
    }

    /// The capture rule, stated as a test because getting it wrong is
    /// expensive in both directions: too narrow and primary traffic loads the
    /// model in the daemon (the abort we are containing); too wide and every
    /// title-generation call goes to a mesh-sharded 122B.
    #[test]
    fn the_distributed_primary_claims_primary_traffic_only() {
        let dir = scratch("claims");
        let s = spec(&dir);
        let (_tx, rx) = watch::channel(ChildRuntimeState::starting());
        let r = route(&s, Arc::new(ChildProvider::new(s.name.clone(), rx)));

        // Named outright — by slot name or by the GGUF's id — at any speed.
        assert!(r.claims(&request(Some("shared-122b"), Speed::Fast)));
        assert!(r.claims(&request(Some("Qwen3.5-122B-00001-of-00003"), Speed::Slow)));

        // Unnamed substantive work is the primary's by default.
        assert!(r.claims(&request(None, Speed::Slow)));
        assert!(r.claims(&request(None, Speed::Medium)));

        // Unnamed FAST work is not: the daemon still owns the fast slot
        // in-process, and it must keep answering while the cluster re-forms.
        assert!(!r.claims(&request(None, Speed::Fast)));

        // Someone else's model id is not ours.
        assert!(!r.claims(&request(Some("qwen3.5-0.8b"), Speed::Slow)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A superseded generation must not publish. Without this guard the
    /// terminated child's `Failed` transition — which can arrive seconds later,
    /// because ggml's abort handler shells out to gdb before dying — would
    /// overwrite the fresh child's state and the facade would fail-fast against
    /// a perfectly healthy process.
    #[test]
    fn a_superseded_generation_cannot_clobber_the_live_one() {
        let (tx, rx) = watch::channel(ChildRuntimeState::starting());
        let tx = Arc::new(tx);
        let live = Arc::new(AtomicU64::new(1));

        let old = LifecycleSink {
            tx: Arc::clone(&tx),
            generation: 0,
            live_generation: Arc::clone(&live),
        };
        let new = LifecycleSink {
            tx: Arc::clone(&tx),
            generation: 1,
            live_generation: Arc::clone(&live),
        };

        let mut state = ChildRuntimeState::starting();
        state.lifecycle = ChildLifecycle::Serving;
        state.last_transition_reason = "serving".to_string();
        assert!(new.send(state));

        let mut dying = ChildRuntimeState::starting();
        dying.lifecycle = ChildLifecycle::Failed;
        dying.last_transition_reason = "previous generation exited".to_string();
        // Refused, and reported as "stop collecting".
        assert!(!old.send(dying));
        assert!(!old.is_live());

        assert_eq!(rx.borrow().lifecycle, ChildLifecycle::Serving);
    }

    /// Retiring parks the slot unavailable — the "cluster can't hold it"
    /// posture — without ever handing the request back to the in-process
    /// engine. No child is spawned here, which is itself the point: the slot
    /// is routable (and fail-fast) from the moment it exists.
    #[test]
    fn retire_parks_the_slot_unavailable_with_a_stated_reason() {
        let dir = scratch("retire");
        let slot = DynamicChildSlot::new(
            spec(&dir),
            PathBuf::from("/nonexistent-binary"),
            dir.clone(),
        );

        assert!(!slot.is_spawned());
        assert!(!slot.provider().is_serving());

        slot.retire("no eligible RPC workers");

        let status = slot.status();
        assert_eq!(status.lifecycle, ChildLifecycle::Failed);
        assert_eq!(status.last_transition_reason, "no eligible RPC workers");
        assert_eq!(status.name, "shared-122b");
        assert!(!slot.provider().is_serving());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A subscriber taken before anything spawns must still receive the slot's
    /// terminal transition.
    ///
    /// This is the seam the mesh self-manifest refresher hangs off. Its job is
    /// symmetric — advertise the model when the child serves, and STOP
    /// advertising it when the slot is parked — so a subscription that only
    /// survived while a child happened to be alive would leave peers routing
    /// into a retired slot. The channel is owned by the slot, not by a
    /// generation, and this pins that.
    #[tokio::test]
    async fn subscribe_survives_retire_and_delivers_the_terminal_transition() {
        let dir = scratch("subscribe");
        let slot = DynamicChildSlot::new(
            spec(&dir),
            PathBuf::from("/nonexistent-binary"),
            dir.clone(),
        );

        // Subscribed before any generation exists.
        let mut rx = slot.subscribe();
        assert_eq!(rx.borrow_and_update().lifecycle, ChildLifecycle::Starting);

        slot.retire("no eligible RPC workers");

        rx.changed().await.expect("slot alive, transition delivered");
        let st = rx.borrow_and_update();
        assert_eq!(st.lifecycle, ChildLifecycle::Failed);
        assert_eq!(st.last_transition_reason, "no eligible RPC workers");
        assert!(
            st.client.is_none(),
            "a retired slot must not hand out a client"
        );
        drop(st);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The handoff must be on disk before the child is spawned — the child
    /// reads it at startup, and an operator reads it to answer "which workers
    /// is this child actually loaded across?".
    #[tokio::test]
    async fn respawn_writes_the_handoff_before_spawning() {
        let dir = scratch("handoff");
        let s = spec(&dir);
        let slot = DynamicChildSlot::new(s.clone(), PathBuf::from("/nonexistent-binary"), dir.clone());

        let handoff = DistributionHandoff {
            endpoints: vec!["127.0.0.1:41001".to_string()],
            plan: vec![NodeShard {
                device_index: 0,
                blocks: Some((0, 11)),
                holds_output: false,
                fraction: 0.25,
            }],
        };
        slot.respawn_distributed(&handoff).expect("respawn");

        let written = DistributionHandoff::read(&s.handoff_path).expect("handoff on disk");
        assert_eq!(written, handoff);
        assert!(slot.is_spawned());

        // The binary does not exist, so the supervisor will never reach
        // serving — the slot stays fail-fast rather than pretending.
        assert!(!slot.provider().is_serving());
        slot.retire("test over");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `/status` must state the child's real split, and only while a child holds
    /// one. This is the wiring `resident_slots` reads; it used to be hardcoded to
    /// zeros, which made a two-node run indistinguishable from a local one in
    /// `svrn mesh bench`'s `placement_digest`. Reverting to a constant here
    /// fails this test.
    #[tokio::test]
    async fn placement_is_stated_only_while_a_child_holds_one() {
        let dir = scratch("placement");
        let slot = DynamicChildSlot::new(
            spec(&dir),
            PathBuf::from("/nonexistent-binary"),
            dir.clone(),
        );

        // Before the first respawn there is no child and therefore no split.
        assert!(
            slot.placement().is_none(),
            "an unspawned slot must not claim a placement"
        );

        // The live 122B cut: 12 blocks on the peer, 36 + the output head here.
        slot.respawn_distributed(&DistributionHandoff {
            endpoints: vec!["192.168.1.2:50052".to_string()],
            plan: vec![
                NodeShard {
                    device_index: 0,
                    blocks: Some((0, 11)),
                    holds_output: false,
                    fraction: 0.2631579,
                },
                NodeShard {
                    device_index: 1,
                    blocks: Some((12, 47)),
                    holds_output: true,
                    fraction: 0.7368421,
                },
            ],
        })
        .expect("respawn");

        let p = slot.placement().expect("a spawned slot states its split");
        assert_eq!(p.mode, "child-distributed");
        assert_eq!(p.total_blocks, 48);
        assert_eq!(p.local_blocks, 36);
        assert_eq!(p.workers.len(), 1);
        assert_eq!(p.workers[0].endpoint, "192.168.1.2:50052");
        assert_eq!(p.workers[0].blocks, 12);

        // Retiring parks the slot; describing a split no process is holding
        // would be a claim about nothing.
        slot.retire("test over");
        assert!(
            slot.placement().is_none(),
            "a retired slot must not keep reporting its last split"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
