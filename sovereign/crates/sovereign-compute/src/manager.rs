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
use std::sync::Arc;
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
use crate::supervisor::{HealthTarget, Supervisor, SupervisorConfig, SupervisorState};

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

/// Owns the compute children + the model_id → child routes over them.
pub struct ComputeChildManager {
    children: Vec<ManagedChild>,
    routes: HashMap<String, Arc<ChildProvider>>,
    embed_child: Option<Arc<ChildProvider>>,
}

impl ComputeChildManager {
    /// Spawn every warm slot declared in `section` and build the routes.
    /// `binary` is the executable to re-exec as `--compute-child`
    /// (`current_exe()` in production).
    pub fn start(section: &ComputeSection, binary: PathBuf, crash_log_dir: PathBuf) -> Arc<Self> {
        let mut children = Vec::new();
        let mut routes = HashMap::new();
        let mut embed_child = None;

        for slot_cfg in &section.slot {
            if !slot_cfg.warm {
                continue;
            }
            let spec = ChildSpec::for_slot(slot_cfg);
            let managed = spawn_managed(&spec, &binary, &crash_log_dir);
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

        Arc::new(Self {
            children,
            routes,
            embed_child,
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

    /// A status snapshot for every managed child.
    pub fn statuses(&self) -> Vec<ChildStatusSnapshot> {
        self.children
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
            .collect()
    }

    /// Gracefully stop every child (SIGTERM → grace → SIGKILL).
    pub fn shutdown(&self) {
        for c in &self.children {
            info!(target: "compute_child", child = %c.name, "terminating compute child");
            c.supervisor.terminate();
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
        let managed = spawn_managed(&spec, &binary, &crash_log_dir);
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
        })
    }
}

/// The full spawn args for one child.
struct ChildSpec {
    name: String,
    role: String,
    model_id: String,
    args: Vec<String>,
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
        }
    }
}

/// Build the supervisor for one child, wire its lifecycle collector, and
/// spawn both tasks.
fn spawn_managed(spec: &ChildSpec, binary: &Path, crash_log_dir: &Path) -> ManagedChild {
    let config = SupervisorConfig {
        binary_path: binary.to_path_buf(),
        args: spec.args.clone(),
        working_dir: None,
        env: vec![],
        health: HealthTarget::StdoutHandshake {
            health_path: crate::wire::ROUTE_HEALTH.to_string(),
            handshake_deadline: Duration::from_secs(180),
        },
        crash_log_dir: crash_log_dir.to_path_buf(),
        heartbeat_interval: Duration::from_secs(2),
        heartbeat_timeout: Duration::from_secs(5),
        heartbeat_failure_threshold: 3,
        ready_deadline: Duration::from_secs(180),
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

    let supervisor = Arc::new(Supervisor::new(config));
    let (tx, rx) = watch::channel(ChildRuntimeState::starting());
    let collector = tokio::spawn(collect_lifecycle(
        spec.name.clone(),
        Arc::clone(&supervisor),
        tx,
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
        state_rx: rx,
        _run: run,
        _collector: collector,
    }
}

/// Translate a child's `SupervisorState` broadcast into the routable
/// [`ChildRuntimeState`], rebuilding the client when it becomes serving and
/// tracing every lifecycle transition (glassbox, target `compute_child`).
async fn collect_lifecycle(
    name: String,
    supervisor: Arc<Supervisor>,
    tx: watch::Sender<ChildRuntimeState>,
) {
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

        let prev = tx.borrow().lifecycle;
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

        if tx
            .send(ChildRuntimeState {
                lifecycle,
                pid,
                port,
                client,
                restarts,
                last_transition_reason: reason,
                last_exit: exit,
            })
            .is_err()
        {
            break;
        }
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
}

impl ComputeRoutedProvider {
    /// Wrap `inner` with routing to `manager`'s children.
    pub fn new(inner: Arc<dyn InferenceProvider>, manager: Arc<ComputeChildManager>) -> Self {
        Self {
            routes: manager.routes().clone(),
            embed_child: manager.embed_child().cloned(),
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
        }
    }

    /// The child a request addresses by `model_id`, if any.
    fn child_for(&self, request: &CompletionRequest) -> Option<&Arc<ChildProvider>> {
        request.model_id.as_deref().and_then(|m| self.routes.get(m))
    }

    /// The manager (for `/status`).
    pub fn manager(&self) -> Option<&Arc<ComputeChildManager>> {
        self.manager.as_ref()
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
        v
    }

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        self.inner.resident_slots()
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
    if !section.enabled || section.slot.is_empty() {
        return None;
    }
    let manager = ComputeChildManager::start(section, binary, crash_log_dir);
    if manager.routes().is_empty() {
        return None;
    }
    let facade = Arc::new(ComputeRoutedProvider::new(inner, Arc::clone(&manager)));
    Some((facade, manager))
}
