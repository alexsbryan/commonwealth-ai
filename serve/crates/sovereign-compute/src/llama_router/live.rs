// SPDX-License-Identifier: AGPL-3.0-or-later
//! The running router: a supervised `llama-server --models-preset` child,
//! warmed before the first build returns, and its state for `/status`.
//!
//! Started once per serving process, on the first build; a reload re-wraps
//! it and never respawns it (`assembly::ReloadFactory`). It dies with the
//! process: the supervisor gives every child `PR_SET_PDEATHSIG` and kills
//! its process group, which holds the router's one process per model.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sovereign_contracts::oicp::ComputeChildStatus;
use sovereign_contracts::setup_config::SetupConfig;

use super::build_pin::{self, BuildPin};
use super::placement::RouterPlacement;
use super::RouterPreset;
use crate::supervisor::{HealthTarget, Supervisor, SupervisorConfig, SupervisorState};

/// The binary's name beside the daemon's own executable.
pub use sovereign_inference::gpu_probe::SERVER_BINARY;

/// The router answers `/health` once it is listening; the models load after.
const ROUTER_READY: Duration = Duration::from_secs(60);

/// One model's load, the 35B MTP being the slowest (the battery allowed 600s
/// per forced load and never needed it).
const MODEL_LOAD: Duration = Duration::from_secs(600);

/// SIGTERM to SIGKILL: the router stops one child per model (the battery
/// waited up to 60s for every llama-server to exit).
const TERMINATE_GRACE: Duration = Duration::from_secs(60);

/// Where the binary came from: the one shipped beside the daemon, or the
/// `[engine] server_binary` override, which `/status` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinarySource {
    /// Beside the daemon's executable.
    Shipped,
    /// `[engine] server_binary`.
    Override,
}

/// A router this process started and supervises.
pub struct LiveRouter {
    digest: String,
    port: u16,
    binary: PathBuf,
    source: BinarySource,
    supervisor: Arc<Supervisor>,
    state: Arc<Mutex<SupervisorState>>,
    models: Arc<Mutex<Vec<(String, String)>>>,
    /// The repair each model was owed the moment it was first seen in a
    /// terminal state, by model name: the first `/models` value read after a
    /// model died, and the time the repair loop acted on it. Empty for a
    /// healthy router.
    repairs: Arc<Mutex<BTreeMap<String, ModelRepair>>>,
    /// Where this router's models run (es-f2). [`RouterPlacement::Local`] on
    /// every single-card node; a distributed one is what lets `/status` name
    /// a peer that is gone rather than only the child it aborted.
    placement: RouterPlacement,
    /// The router's own log (`--log-file`), where each model child's output
    /// lands. `/status` reads its tail for the RPC abort marker a peer loss
    /// leaves, so naming the peer costs a local file read and never a
    /// network connect (ARCH 1: a status field may not block).
    log_path: Arc<PathBuf>,
}

/// What happened to one router model, as `/status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRepair {
    /// The `/models` status that demanded the repair (`failed (exit N)`,
    /// `unloaded` after a load).
    pub observed: String,
    /// When the repair loop re-issued `/models/load`.
    pub reload_at: Option<Instant>,
    /// The last reload's error, when it did not reach `loaded`.
    pub error: Option<String>,
}

/// How often the repair loop reads `/models` and reloads a dead model.
const REPAIR_POLL: Duration = Duration::from_secs(5);

/// How often the follow poller re-reads `/models` and re-probes the peers.
const PEER_POLL: Duration = Duration::from_secs(5);

/// Backoff between reload attempts for one model: the first retry is
/// immediate enough to be a restart, the last never floods a crash-looping
/// model with load requests.
const REPAIR_BACKOFF: [Duration; 4] = [
    Duration::from_secs(0),
    Duration::from_secs(2),
    Duration::from_secs(10),
    Duration::from_secs(60),
];

/// A `/models` status that means the model's process is GONE, not merely
/// idle: llama.cpp marks a load that failed `failed (exit N)` and leaves a
/// model it unloaded `unloaded`. `sleeping` is not one of these — its
/// process is alive and a request wakes it.
fn demands_repair(status: &str) -> bool {
    status.starts_with("failed") || status == "unloaded"
}

impl std::fmt::Debug for LiveRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveRouter")
            .field("digest", &self.digest)
            .field("port", &self.port)
            .field("binary", &self.binary)
            .field("source", &self.source)
            .finish()
    }
}

/// The binary to run: the override, else `llama-server` beside this
/// executable. Absent is a refusal that says where it looked.
pub fn resolve_binary(config: &SetupConfig) -> Result<(PathBuf, BinarySource), String> {
    if let Some(path) = &config.engine.server_binary {
        return match path.is_file() {
            true => Ok((path.clone(), BinarySource::Override)),
            false => Err(format!(
                "[engine] server_binary {} is not a file",
                path.display()
            )),
        };
    }
    let beside = sovereign_inference::gpu_probe::shipped_server_binary()?;
    match beside.is_file() {
        true => Ok((beside, BinarySource::Shipped)),
        false => Err(format!(
            "kind = \"llama-server\" needs {SERVER_BINARY} beside the daemon ({}), and it is not \
             there.\nhint: install it with the daemon, or set [engine] server_binary for a dev build",
            beside.display()
        )),
    }
}

/// A shipped binary must be the build this daemon was compiled against
/// ([`build_pin::verify`]); an override is the operator's dev build and is
/// run as it is, which `/status` says.
fn check_build(binary: &Path, source: &BinarySource) -> Result<(), String> {
    match source {
        BinarySource::Shipped => {
            let pin = BuildPin::compiled()?;
            build_pin::verify(binary, &pin)?;
            tracing::info!(target: "llama_router", %pin, "llama-server build verified against the compiled pin");
        }
        BinarySource::Override => {
            tracing::info!(
                target: "llama_router",
                binary = %binary.display(),
                "llama-server build not checked: [engine] server_binary overrides the shipped one"
            );
        }
    }
    Ok(())
}

/// The preset's models that fit this machine's GPU together
/// ([`RouterPreset::models_that_fit`]), or the refusal
/// [`RouterPreset::vram_if_every_section_loaded`] returns when one card no
/// longer holds the whole preset. With no GPU budget to size against (none,
/// or the probe could not read one) every model is warmed, as before, and the
/// log says the count was not sized.
///
/// The whole-preset check comes first because the repair loop reloads each
/// section after a death with no eviction to make room for it
/// (ENGINE_SWAP_PLAN A7): a preset the card cannot hold is a configuration
/// this node cannot serve, refused by name instead of warmed in part.
fn warm_within_memory(preset: &RouterPreset) -> Result<Vec<&str>, String> {
    let hw = crate::hardware::detect_hardware();
    if !hw.gpu_available && !hw.is_unified_memory {
        tracing::info!(target: "llama_router", "router: no GPU budget to size --models-max against; warming every model");
        return Ok(preset.warm_models());
    }
    let fits = |slots: &[_]| sovereign_inference::capacity::check_fit(slots, &hw).fits;
    preset.vram_if_every_section_loaded(fits)?;
    let warm = preset.models_that_fit(fits);
    if warm.len() < preset.warm_models().len() {
        tracing::warn!(
            target: "llama_router",
            ?warm,
            all = ?preset.warm_models(),
            vram_gb = hw.effective_vram_gb(),
            "router: not every model fits together; the rest load on demand, evicting the least recently used"
        );
    }
    Ok(warm)
}

fn free_port() -> Result<u16, String> {
    // Bound and released: another process could take it before the router
    // binds, and the router would then fail to start and be restarted.
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("choosing a port for the router: {e}"))
}

impl LiveRouter {
    /// Start the router for `preset` and return once every model reports
    /// `loaded`. Blocking: call it off the async workers (serving assembly
    /// runs on `spawn_blocking`).
    pub fn start(preset: &RouterPreset, config: &SetupConfig) -> Result<Arc<Self>, String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "the llama-server router is started from a tokio runtime".to_string())?;
        let (binary, source) = resolve_binary(config)?;
        check_build(&binary, &source)?;
        let dir = config.data.dir.join("llama-server");
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        // A distributed preset is started exactly as a single-card one is:
        // the router comes up (it loads nothing itself), `warm` asks it to
        // load each model, and a model that cannot reach a peer fails —
        // `/models` says `failed (exit N)` and the repair loop re-issues the
        // load with backoff until the peer answers. That loop IS the head's
        // half of restart-on-peer-loss; the peer's half is its own supervisor
        // restarting `ggml-rpc-server` (`rpc_peer::RpcPeerChild`). No probe
        // gates the spawn: a TCP connect to a peer that is busy says nothing
        // (its accept loop has a backlog of 1, `transport.cpp:677`, so the
        // head's own model holds the only slot), and a connect that times
        // out would refuse a boot the repair loop can recover on its own.
        let placement = preset.placement();
        if !placement.peers().is_empty() {
            tracing::info!(
                target: "llama_router",
                placement = %placement,
                "router: a distributed preset; its models load across these peers"
            );
        }
        let preset_path = dir.join("router.ini");
        std::fs::write(&preset_path, preset.render())
            .map_err(|e| format!("writing {}: {e}", preset_path.display()))?;
        let port = free_port()?;
        // The card must hold the whole preset (the repair loop reloads every
        // section, `warm_within_memory`), so an overcommitted one refuses here
        // — before anything is written or spawned — naming what it asked.
        let warm: Vec<String> = warm_within_memory(preset)?
            .iter()
            .map(|m| m.to_string())
            .collect();
        let models_max = warm.len().to_string();
        let args = [
            "--models-preset",
            &preset_path.to_string_lossy(),
            "--models-max",
            &models_max,
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--slots",
            "--log-file",
            &dir.join("router.log").to_string_lossy(),
        ]
        .map(str::to_string)
        .to_vec();
        tracing::info!(
            target: "llama_router",
            binary = %binary.display(),
            ?source,
            port,
            preset = %preset_path.display(),
            digest = %preset.digest(),
            models_max = %models_max,
            placement = %placement,
            ?warm,
            "starting the llama-server router"
        );
        let supervisor = Arc::new(Supervisor::new(SupervisorConfig {
            binary_path: binary.clone(),
            args,
            working_dir: Some(dir.clone()),
            // The router lists every model in llama.cpp's download cache
            // beside its preset (`load_from_cache`); an empty cache of its
            // own keeps the preset the one list of what it serves.
            //
            // `LLAMA_SERVER_SLOTS_DEBUG` makes `/slots` describe the task,
            // grammar included; in its metrics-only form (the default) the
            // key is absent, and the battery's grammar rows can see nothing
            // of what the router enforced (`slot_sampler`'s
            // SLOTS_METRICS_ONLY). The daemon already serves the router's
            // slots for the sampler facet, so the description is on by
            // default; `SOVEREIGN_ROUTER_SLOTS_DEBUG=0` turns it off.
            env: vec![
                (
                    "LLAMA_CACHE".into(),
                    dir.join("cache").to_string_lossy().into_owned(),
                ),
                (
                    "LLAMA_SERVER_SLOTS_DEBUG".into(),
                    if std::env::var("SOVEREIGN_ROUTER_SLOTS_DEBUG").as_deref() == Ok("0") {
                        "0".to_string()
                    } else {
                        "1".to_string()
                    },
                ),
            ],
            health: HealthTarget::Fixed(format!("http://127.0.0.1:{port}/health")),
            crash_log_dir: config.data.dir.join("compute-crash-logs"),
            heartbeat_interval: Duration::from_secs(2),
            heartbeat_timeout: Duration::from_secs(5),
            heartbeat_failure_threshold: 3,
            ready_deadline: ROUTER_READY,
            backoff_schedule: [1, 2, 10, 60].map(Duration::from_secs).to_vec(),
            healthy_reset_after: Duration::from_secs(60),
            crash_loop_max: 5,
            stderr_ring_lines: 200,
            terminate_grace: TERMINATE_GRACE,
        }));
        let live = Arc::new(Self {
            digest: preset.digest(),
            port,
            binary,
            source,
            supervisor: Arc::clone(&supervisor),
            state: Arc::new(Mutex::new(SupervisorState::Starting)),
            models: Arc::new(Mutex::new(Vec::new())),
            repairs: Arc::new(Mutex::new(BTreeMap::new())),
            placement: placement.clone(),
            log_path: Arc::new(dir.join("router.log")),
        });
        let mut states = supervisor.subscribe();
        runtime.spawn({
            let supervisor = Arc::clone(&supervisor);
            async move { supervisor.run().await }
        });
        let started = runtime.block_on(async {
            live.await_healthy(&mut states).await?;
            live.warm(&warm).await
        });
        if let Err(e) = started {
            supervisor.terminate();
            return Err(format!(
                "{e}\nhint: the router's log is {}",
                dir.join("router.log").display()
            ));
        }
        live.follow(states, warm.clone(), &runtime);
        live.repair_loop(&runtime);
        Ok(live)
    }

    /// Stop the router and the models it runs.
    pub fn terminate(&self) {
        let stopped = self.supervisor.terminate();
        tracing::info!(target: "llama_router", stopped, "router: terminate requested");
    }

    /// The `/v1` base the remote provider talks to.
    pub fn endpoint_v1(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// The router's own base, where its native routes (`/completion`,
    /// `/tokenize`) are.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The preset this router was started with.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    async fn await_healthy(
        &self,
        states: &mut tokio::sync::broadcast::Receiver<SupervisorState>,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + ROUTER_READY;
        loop {
            let state = tokio::time::timeout_at(deadline, states.recv())
                .await
                .map_err(|_| {
                    format!("the router did not answer /health within {ROUTER_READY:?}")
                })?;
            match state {
                Ok(s @ SupervisorState::Healthy { .. }) => {
                    *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
                    return Ok(());
                }
                Ok(SupervisorState::Failed { reason, .. }) => {
                    return Err(format!("the router failed to start: {reason}"))
                }
                Ok(s) => *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s,
                Err(e) => {
                    tracing::debug!(target: "llama_router", error = %e, "supervisor state lagged")
                }
            }
        }
    }

    /// Ask the router to load each model, then wait for every one to report
    /// `loaded` in `GET /models`.
    async fn warm(&self, names: &[String]) -> Result<(), String> {
        let http = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}", self.port);
        for name in names {
            let resp = http
                .post(format!("{base}/models/load"))
                .json(&json!({ "model": name }))
                .send()
                .await
                .map_err(|e| format!("asking the router to load {name}: {e}"))?;
            // A model already loading or loaded is not an error to wait on.
            tracing::info!(target: "llama_router", model = %name, status = %resp.status(), "router: load requested");
        }
        let started = Instant::now();
        loop {
            let models = list_models(&http, &base).await?;
            *self.models.lock().unwrap_or_else(PoisonError::into_inner) = models.clone();
            let status_of = |n: &str| {
                models
                    .iter()
                    .find(|(m, _)| m == n)
                    .map_or("absent from /models", |(_, s)| s.as_str())
            };
            let pending: Vec<(&str, &str)> = names
                .iter()
                .map(|n| (n.as_str(), status_of(n)))
                .filter(|(_, s)| *s != "loaded")
                .collect();
            if pending.is_empty() {
                tracing::info!(target: "llama_router", models = names.len(), elapsed_s = started.elapsed().as_secs(), "router: every model loaded");
                return Ok(());
            }
            // A failed load is final; `loading` (set before the load
            // request returns) is waited on.
            if let Some((name, status)) = pending
                .iter()
                .find(|(_, s)| s.starts_with("failed") || *s == "absent from /models")
            {
                return Err(format!("the router reports {name}: {status}"));
            }
            // A sleeping model is still running, so `/models/load` refuses
            // it as already running; a request is what wakes it, and
            // `/tokenize` is one the server makes wait for the reload.
            for (name, _) in pending.iter().filter(|(_, s)| *s == "sleeping") {
                tracing::info!(target: "llama_router", model = %name, "router: waking a sleeping model");
                let woke = tokio::time::timeout(
                    MODEL_LOAD.saturating_sub(started.elapsed()),
                    self.token_count(name, ""),
                )
                .await;
                if let Ok(Err(e)) = &woke {
                    tracing::warn!(target: "llama_router", model = %name, error = %e, "router: waking request failed");
                }
            }
            if started.elapsed() > MODEL_LOAD {
                return Err(format!(
                    "models still loading after {MODEL_LOAD:?}: {pending:?}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Keep `/status` current: the supervisor's state, and each model's
    /// status every few seconds. A router that restarted is warmed again.
    fn follow(
        self: &Arc<Self>,
        mut states: tokio::sync::broadcast::Receiver<SupervisorState>,
        warm: Vec<String>,
        runtime: &tokio::runtime::Handle,
    ) {
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            while let Ok(state) = states.recv().await {
                let Some(live) = weak.upgrade() else { return };
                let rewarm = matches!(state, SupervisorState::Healthy { .. });
                tracing::info!(target: "llama_router", state = ?state, "router: lifecycle");
                *live.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
                if rewarm {
                    if let Err(e) = live.warm(&warm).await {
                        tracing::warn!(target: "llama_router", error = %e, "router: re-warm after restart failed");
                    }
                }
            }
        });
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            let http = reqwest::Client::new();
            loop {
                tokio::time::sleep(PEER_POLL).await;
                let Some(live) = weak.upgrade() else { return };
                let base = format!("http://127.0.0.1:{}", live.port);
                match list_models(&http, &base).await {
                    Ok(models) => *live.models.lock().unwrap_or_else(PoisonError::into_inner) = models,
                    Err(e) => tracing::debug!(target: "llama_router", error = %e, "router: /models unreadable"),
                }
            }
        });
    }

    /// Reload a model whose process died (ENGINE_SWAP_PLAN A7: the router
    /// replaces the generate and embed compute children, so it owns their
    /// restart). llama.cpp runs each model in a child its parent supervises
    /// for a clean unload only: a SIGKILLed or aborted model process leaves
    /// `/models` reporting `failed (exit N)` for good. This loop reads
    /// `/models` on [`REPAIR_POLL`], and for every model in a terminal state
    /// re-issues `/models/load` on [`REPAIR_BACKOFF`], recording each attempt
    /// in [`Self::repairs`] so `/status` reports the death AND the repair
    /// rather than the death alone.
    ///
    /// The loop is per model, not per router: one dead model does not stop
    /// another from serving, and it does not restart the router — which
    /// would drop every model. A model that keeps failing is retried at the
    /// floor of [`REPAIR_BACKOFF`], never in a tight loop, and each attempt
    /// is an event at `llama_router`.
    fn repair_loop(self: &Arc<Self>, runtime: &tokio::runtime::Handle) {
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            let http = reqwest::Client::new();
            let mut attempts: BTreeMap<String, u32> = BTreeMap::new();
            let mut next_try: BTreeMap<String, Instant> = BTreeMap::new();
            loop {
                tokio::time::sleep(REPAIR_POLL).await;
                let Some(live) = weak.upgrade() else { return };
                let models = live
                    .models
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                let now = Instant::now();
                for (name, status) in models {
                    if !demands_repair(&status) {
                        attempts.remove(&name);
                        next_try.remove(&name);
                        continue;
                    }
                    if next_try.get(&name).is_some_and(|at| *at > now) {
                        continue;
                    }
                    let attempt = attempts.entry(name.clone()).or_insert(0);
                    *attempt += 1;
                    let backoff = REPAIR_BACKOFF[(*attempt as usize - 1).min(REPAIR_BACKOFF.len() - 1)];
                    next_try.insert(name.clone(), now + backoff);
                    {
                        let mut repairs =
                            live.repairs.lock().unwrap_or_else(PoisonError::into_inner);
                        let entry = repairs.entry(name.clone()).or_insert_with(|| ModelRepair {
                            observed: status.clone(),
                            reload_at: None,
                            error: None,
                        });
                        entry.observed = status.clone();
                        entry.reload_at = Some(now);
                        entry.error = None;
                    }
                    tracing::warn!(
                        target: "llama_router",
                        model = %name,
                        status = %status,
                        attempt = *attempt,
                        ?backoff,
                        "router model died; asking the router to load it again"
                    );
                    let sent = http
                        .post(format!("http://127.0.0.1:{}/models/load", live.port))
                        .json(&json!({ "model": name }))
                        .send()
                        .await;
                    let error = match sent {
                        Ok(resp) if resp.status().is_success() => None,
                        Ok(resp) => Some(format!("POST /models/load: HTTP {}", resp.status())),
                        Err(e) => Some(format!("POST /models/load: {e}")),
                    };
                    if let Some(error) = error {
                        tracing::warn!(target: "llama_router", model = %name, %error, "router model reload failed");
                        if let Some(entry) = live
                            .repairs
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get_mut(&name)
                        {
                            entry.error = Some(error);
                        }
                    } else {
                        tracing::info!(target: "llama_router", model = %name, "router model reload requested");
                    }
                }
            }
        });
    }

    /// `model`'s status as the router last listed it in `GET /models`
    /// (`loaded`, `loading`, `unloaded`, `sleeping`, `failed (exit N)`), or
    /// `None` before the first listing names it. Never asks the router: the
    /// listing is the one [`Self::follow`] keeps current every 5s.
    pub fn model_status(&self, model: &str) -> Option<String> {
        self.models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(m, _)| m == model)
            .map(|(_, s)| s.clone())
    }

    /// Load `model` through the router's `POST /models/load` and return once
    /// it reports `loaded`; a failed load is an error naming the status.
    pub async fn load(&self, model: &str) -> Result<(), String> {
        self.warm(&[model.to_string()]).await
    }

    /// How many tokens `model`'s own tokenizer makes of `text`, from the
    /// router's `POST /tokenize` (routed by model, as a completion is).
    pub async fn token_count(&self, model: &str, text: &str) -> Result<u32, String> {
        let body: Value = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/tokenize", self.port))
            .json(&json!({ "model": model, "content": text }))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("POST /tokenize for {model}: {e}"))?
            .json()
            .await
            .map_err(|e| format!("POST /tokenize for {model}: {e}"))?;
        let tokens = body
            .get("tokens")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("POST /tokenize for {model} returned no tokens: {body}"))?;
        tracing::debug!(target: "llama_router", model, chars = text.len(), tokens = tokens.len(), "router: tokenized");
        Ok(tokens.len() as u32)
    }

    /// `reason` prefixed with the peer loss, when the router's log shows this
    /// model's process EXITING (`instance name=<model> exited with status N`,
    /// llama.cpp `server-models.cpp`) on a distributed preset. `None`
    /// otherwise, so a model that failed some other way keeps its own
    /// reason and the line never claims a peer loss it cannot see.
    ///
    /// The exit is the whole of the evidence a peer loss leaves here: the
    /// child aborts on the dead peer's next RPC touch (`ggml-rpc.cpp:30`
    /// through `:647`, "Remote RPC server crashed"), and llama.cpp's router
    /// records the exit, not the abort — measured 2026-10-24, the abort text
    /// goes to the child's inherited stdout and the log carries only
    /// `exited with status 1`. A distributed model cannot exit without its
    /// peer behind it, which is why the preset's peers are named with it.
    fn peer_loss_from_log(&self, model: &str, reason: &str) -> Option<String> {
        if self.placement.peers().is_empty() {
            return None;
        }
        let log = std::fs::read_to_string(self.log_path.as_path()).ok()?;
        let exited = format!("instance name={model} exited with status");
        log.lines().find(|l| l.contains(&exited)).map(|line| {
            let status = line.rsplit_once("status ").map_or("?", |(_, s)| s.trim());
            format!(
                "{reason}; peer loss: {model} is distributed across {:?}, and its process \
                     exited (status {status}) — an RPC touch of a peer that has stopped \
                     answering aborts it (ggml-rpc.cpp:30, upstream never reconnects); the repair \
                     loop reloads it with backoff once the peer's supervisor has restarted \
                     ggml-rpc-server",
                self.placement.peers()
            )
        })
    }

    /// The router and each of its models, as `/status` lists compute children.
    pub fn statuses(&self) -> Vec<ComputeChildStatus> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (lifecycle, reason) = lifecycle_of(&state);
        let source = match self.source {
            BinarySource::Shipped => "shipped, build verified",
            BinarySource::Override => "override, build unchecked",
        };
        // `self.digest` is part of the model_id below, so the router line
        // already names the preset; peer loss is named on the MODEL's line
        // (`peer_loss` in the loop), where the failed child is.
        let mut out = vec![ComputeChildStatus {
            name: SERVER_BINARY.into(),
            role: "router".into(),
            model_id: format!(
                "{} ({source}, preset {})",
                self.binary.display(),
                self.digest
            ),
            lifecycle: lifecycle.into(),
            port: Some(self.port),
            restarts: self.supervisor.total_restarts(),
            last_transition_reason: reason,
            last_exit: None,
        }];
        for (name, status) in self
            .models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            // A model whose process died is `failed`; `/status` also says
            // what is being done about it, so the death is not the last word
            // (the repair loop reloads it, `Self::repair_loop`).
            let repair = self
                .repairs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned();
            let (lifecycle, mut reason) = match &repair {
                Some(ModelRepair {
                    reload_at: Some(_),
                    error: None,
                    ..
                }) => (
                    "restarting".to_string(),
                    format!("{status}; reload requested, waiting for /models"),
                ),
                Some(ModelRepair {
                    reload_at: Some(_),
                    error: Some(e),
                    ..
                }) => (
                    "restarting".to_string(),
                    format!("{status}; reload failed: {e}"),
                ),
                _ => (status.clone(), format!("GET /models: {status}")),
            };
            // Peer loss (es-f2): a distributed section whose model process
            // exited names its peers, because upstream never reconnects and
            // the child's death is otherwise only "exit N". Reported whether
            // or not the model has since been reloaded — the exit HAPPENED,
            // and the repair loop reloading it does not un-happen the peer
            // that went away. Read from the router's own log, a local file.
            if let Some(lost) = self.peer_loss_from_log(name, &reason) {
                reason = lost;
            }
            out.push(ComputeChildStatus {
                name: name.clone(),
                role: "router-model".into(),
                model_id: name.clone(),
                lifecycle,
                port: Some(self.port),
                restarts: repair.map_or(0, |r| u32::from(r.reload_at.is_some())),
                last_transition_reason: reason,
                last_exit: None,
            });
        }
        out
    }
}

fn lifecycle_of(state: &SupervisorState) -> (&'static str, String) {
    match state {
        SupervisorState::Starting => ("starting", "spawned".into()),
        SupervisorState::Warming { .. } => ("warming", "announced its port".into()),
        SupervisorState::Healthy { .. } => ("serving", "answering /health".into()),
        SupervisorState::Unhealthy {
            consecutive_failures,
            ..
        } => (
            "degraded",
            format!("{consecutive_failures} failed health checks"),
        ),
        SupervisorState::Restarting { reason, .. } => ("restarting", reason.clone()),
        SupervisorState::Failed { reason, .. } => ("failed", reason.clone()),
        SupervisorState::GateHeld { reason } => ("restarting", reason.clone()),
    }
}

/// `GET /models` as (id, status) pairs. The router lists every preset model
/// with a status string (`unloaded`, `loading`, `loaded`, `sleeping`, ...;
/// llama.cpp `server-models.cpp`).
async fn list_models(http: &reqwest::Client, base: &str) -> Result<Vec<(String, String)>, String> {
    let body: Value = http
        .get(format!("{base}/models"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("GET /models: {e}"))?
        .json()
        .await
        .map_err(|e| format!("GET /models: {e}"))?;
    Ok(parse_models(&body))
}

/// The router's `/models` body: `{"data": [{"id", "status": {"value",
/// "failed"?, "exit_code"?}}]}` (llama.cpp `server-models.cpp`,
/// `get_router_models`). A model whose load failed reads `failed (exit N)`.
pub(super) fn parse_models(body: &Value) -> Vec<(String, String)> {
    body.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?;
            let status = m.get("status")?;
            let value = status
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("unreported");
            let shown = match status.get("failed").and_then(Value::as_bool) {
                Some(true) => format!(
                    "failed (exit {})",
                    status
                        .get("exit_code")
                        .map(Value::to_string)
                        .unwrap_or_default()
                ),
                _ => value.to_string(),
            };
            Some((id.to_string(), shown))
        })
        .collect()
}
