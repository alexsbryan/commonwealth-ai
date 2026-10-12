// SPDX-License-Identifier: AGPL-3.0-or-later
//! The running router: a supervised `llama-server --models-preset` child,
//! warmed before the first build returns, and its state for `/status`.
//!
//! Started once per serving process, on the first build; a reload re-wraps
//! it and never respawns it (`assembly::ReloadFactory`). It dies with the
//! process: the supervisor gives every child `PR_SET_PDEATHSIG` and kills
//! its process group, which holds the router's one process per model.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sovereign_contracts::oicp::ComputeChildStatus;
use sovereign_contracts::setup_config::SetupConfig;

use super::build_pin::{self, BuildPin};
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
        let preset_path = dir.join("router.ini");
        std::fs::write(&preset_path, preset.render())
            .map_err(|e| format!("writing {}: {e}", preset_path.display()))?;
        let port = free_port()?;
        let models_max = preset.warm_models().len().to_string();
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
            "starting the llama-server router"
        );
        let supervisor = Arc::new(Supervisor::new(SupervisorConfig {
            binary_path: binary.clone(),
            args,
            working_dir: Some(dir.clone()),
            // The router lists every model in llama.cpp's download cache
            // beside its preset (`load_from_cache`); an empty cache of its
            // own keeps the preset the one list of what it serves.
            env: vec![(
                "LLAMA_CACHE".into(),
                dir.join("cache").to_string_lossy().into_owned(),
            )],
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
        });
        let mut states = supervisor.subscribe();
        runtime.spawn({
            let supervisor = Arc::clone(&supervisor);
            async move { supervisor.run().await }
        });
        let warm: Vec<String> = preset.warm_models().iter().map(|m| m.to_string()).collect();
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
        live.follow(states, warm, &runtime);
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
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(live) = weak.upgrade() else { return };
                let base = format!("http://127.0.0.1:{}", live.port);
                match list_models(&http, &base).await {
                    Ok(models) => *live.models.lock().unwrap_or_else(PoisonError::into_inner) = models,
                    Err(e) => tracing::debug!(target: "llama_router", error = %e, "router: /models unreadable"),
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
            out.push(ComputeChildStatus {
                name: name.clone(),
                role: "router-model".into(),
                model_id: name.clone(),
                lifecycle: status.clone(),
                port: Some(self.port),
                restarts: 0,
                last_transition_reason: format!("GET /models: {status}"),
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
