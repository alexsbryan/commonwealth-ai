// SPDX-License-Identifier: AGPL-3.0-or-later
//! # sovereign-serve — `serve`, the model server (FIVE_PROGRAMS §2)
//!
//! One binary that holds weights and answers the OpenAI wire:
//! `/v1/chat/completions`, `/v1/embeddings`, `/v1/rerank` and `/v1/models`,
//! with no mesh, no knowledge server and no cw-rails. It owns nothing it
//! wires (compose, never re-own, §2c):
//!
//! - the engine and its slots come from the ONE serving assembly
//!   (`sovereign_compute::assembly::assemble_serving`), so an `[engine]`
//!   registration, a `[models.kinds]` kind or the `[models.edit]` slot is
//!   the same here as in the daemon;
//! - the compute child's server is promoted whole
//!   (`sovereign_compute::server::bundle`): the native wire, `/health`, and
//!   every served kind's route through the one kind mount;
//! - the OpenAI translation is serving-host's (`inference_adapter`,
//!   `openai_http`), shared with the daemon's routes;
//! - `/v1/models` is built from this process's OWN provider manifest
//!   (`build_self_manifest` over `resident_slots()`), never from a ledger
//!   another process keeps;
//! - the data-root lock and the server shell are the host kit's.
//!
//! The compute child and the RPC worker re-exec `current_exe()`, which is
//! this binary when `serve` spawns them, so it routes both launches first.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::HttpBody;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Extension;
use axum::Json;
use futures::StreamExt;
use host_kit::shell::RouteBundle;
use sovereign_compute::server::{openai_refusal, ChildMeta};
use sovereign_contracts::engine_state::{Lap, LATENCY_TARGET};
use sovereign_contracts::launch::Launch;
use sovereign_contracts::oicp::openai_types::{
    ChatCompletionRequest, CompletionsRequestWire, EmbeddingRequest, ModelListResponse, StopParam,
};
use sovereign_contracts::oicp::{FimCompletionRequest, LocalInferenceError};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::traits::LocalInferenceService;
use sovereign_contracts::venue::resolution_alias_keys;
use sovereign_contracts::{InferenceProvider, Speed};
use sovereign_serving_host::fim_http;
use sovereign_serving_host::inference_adapter::SovereignInferenceAdapter;
use sovereign_serving_host::openai_http::{self, ChunkHeader};
use sovereign_serving_host::slot_manifest::CoreSlotManifest;
use tracing::{debug, error, info, warn};

mod fetch_model;
mod fetch_ner;
mod reload;
mod self_report;
mod warm_cache;

/// The run lock's name inside the data root (`host_kit::RunLock`): one
/// `serve` per root. The daemon's and cw-rails' locks are their own.
pub const RUN_LOCK: &str = "serve";

/// Where `serve` listens unless `--listen` or `SOVEREIGN_SERVE_PORT` says
/// otherwise: loopback, on the one port the svrn daemon dials by default
/// (`sovereign_contracts::venue::DEFAULT_SERVE_PORT`). The parse reads the
/// port through `venue::serve_port`, the daemon's reader too.
pub const DEFAULT_LISTEN: ([u8; 4], u16) = (
    [127, 0, 0, 1],
    sovereign_contracts::venue::DEFAULT_SERVE_PORT,
);

/// How this process names itself in `/v1/models` `advertised_by`.
const LOCAL_HOLDER: &str = "local";

/// The launch this process was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeArgs {
    /// The data root: its `config.toml` is read and its run lock is held.
    pub data_dir: PathBuf,
    /// Where the OpenAI wire listens.
    pub listen: SocketAddr,
}

impl ServeArgs {
    /// `[--data-dir <dir>] [--listen <addr:port>]`. The data root defaults to
    /// the branded root the daemon reads (`rebrand::svrnmesh_root`); the
    /// listener to [`DEFAULT_LISTEN`], printed once bound.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut data_dir = None;
        let mut listen: SocketAddr =
            (DEFAULT_LISTEN.0, sovereign_contracts::venue::serve_port()).into();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let mut value = |flag: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} requires a value"))
            };
            match arg.as_str() {
                "--data-dir" => data_dir = Some(PathBuf::from(value("--data-dir")?)),
                "--listen" => {
                    let v = value("--listen")?;
                    listen = v
                        .parse()
                        .map_err(|e| format!("--listen {v}: not an addr:port ({e})"))?;
                }
                other => {
                    return Err(format!(
                        "unknown argument `{other}`\nusage: sovereign-serve [--data-dir <dir>] [--listen <addr:port>]"
                    ))
                }
            }
        }
        Ok(Self {
            data_dir: data_dir.unwrap_or_else(sovereign_contracts::rebrand::svrnmesh_root),
            listen,
        })
    }
}

/// The process entry: argv without `argv[0]`, returning the exit code.
pub fn run(args: &[String]) -> i32 {
    if let Some(code) = child_launch(args) {
        return code;
    }
    // The weight verbs, spelled `svrn mesh <verb>` through the dispatcher
    // (phase-b-22). No tracing subscriber, as under sovereign-cli-mesh.
    if let Some((verb, rest)) = args.split_first() {
        if WEIGHT_VERBS.contains(&verb.as_str()) {
            return run_weight_verb(verb, rest);
        }
    }
    let parsed = match ServeArgs::parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sovereign-serve: {e}");
            return 2;
        }
    };
    init_tracing();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        // llama.cpp call chains are deep; the daemon and the child use 8 MiB.
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-serve-rt")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("sovereign-serve: cannot build runtime: {e}");
            return 1;
        }
    };
    let code = runtime.block_on(serve(parsed));
    // Skip the ggml static destructors on the way out (the teardown SIGABRT
    // the compute child dodges the same way).
    sovereign_inference::fast_exit_skip_destructors(code)
}

/// The re-execs of a binary that loads weights: the compute child and the RPC
/// worker re-exec `current_exe()`, so a process hosting serve's assembly
/// routes them first, before tracing or a runtime (each builds its own).
/// `Some(exit code)` when `args` is one of them. serve's `run` and the stock
/// distribution's main both call it (FIVE_PROGRAMS §2c).
pub fn child_launch(args: &[String]) -> Option<i32> {
    match Launch::parse(args, Launch::Bare) {
        Launch::ComputeChild { args } => Some(sovereign_compute::child_main::run(&args)),
        Launch::RpcWorker { args } => Some(sovereign_inference::rpc_worker_main::run(&args)),
        _ => None,
    }
}

/// The subcommands `run` routes before the server's arguments. The dispatcher
/// sends `svrn mesh <verb>` here for exactly these.
pub const WEIGHT_VERBS: &[&str] = &["warm-cache", "fetch-model", "fetch-ner"];

fn run_weight_verb(verb: &str, rest: &[String]) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("sovereign-serve: cannot build runtime: {e}");
            return 1;
        }
    };
    runtime.block_on(async {
        match verb {
            "warm-cache" => warm_cache::cmd_warm_cache(rest).await,
            "fetch-ner" => fetch_ner::cmd_fetch_ner(rest).await,
            _ => fetch_model::cmd_fetch_model(rest).await,
        }
    })
}

/// serve's log filter when `RUST_LOG` is unset: an allowlist of targets.
/// `llama_cpp` is the target every ggml line rides (sovereign-inference
/// llama.rs), so a failed GGUF load names its cause (the daemon's filter
/// carries the same directive).
pub const DEFAULT_FILTER: &str = "serve=info,sovereign_serve=info,sovereign_compute=info,\
     sovereign_inference=info,sovereign_serving_host=info,host_kit=info,served_kind=info,\
     engine_factory=info,serving_assembly=info,compute_child=info,llama_cpp=info";

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let _ = fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
}

/// What [`assemble`] built: the provider cell every route answers from, every
/// route, and serve's run lock, held for as long as the value lives.
pub struct ServeAssembly {
    /// The one cell: a reload swaps it, so every route and every in-process
    /// reader sees the new engine at once.
    pub cell: Arc<reload::ReloadableProvider>,
    /// Every route serve answers, [`bundles`] plus the self-report and reload
    /// bundles.
    pub routes: Vec<RouteBundle>,
    /// Where serve listens unless told otherwise: loopback on
    /// `venue::serve_port()`, the port every client of serve dials, so a host
    /// of this assembly binds where they look.
    pub listen: SocketAddr,
    /// serve's hold on the data root; drop it only when serve stops.
    pub run_lock: host_kit::RunLock,
}

/// serve's assembly, from its data root's lock to its last route: the lock,
/// the config at `config_path`, llama.cpp's log route, the VRAM preflight,
/// the mock engine's registration, the ONE serving assembly
/// (`assemble_serving`), the cell and every bundle. serve's own `run` and the
/// stock distribution both call it, so a stock install serves exactly what a
/// standalone serve does (phase-b-29 Q1). `Err` names the refusal, already
/// traced.
pub async fn assemble(
    data_dir: &std::path::Path,
    config_path: &std::path::Path,
) -> Result<ServeAssembly, String> {
    let run_lock = host_kit::RunLock::acquire(data_dir, RUN_LOCK).map_err(|e| {
        error!(target: "serve", error = %e, "refusing to start");
        e.to_string()
    })?;
    let config = SetupConfig::load_from(config_path).map_err(|e| {
        error!(target: "serve", config = %config_path.display(), error = %e, "no config to serve from");
        e.to_string()
    })?;
    // serve is the loader, so the daemon's pre-load steps are its own
    // (pb-svrn-dials-serve): llama.cpp's log reaches tracing, so a failed GGUF
    // load names its cause, and the VRAM preflight reads serve's own sections.
    sovereign_inference::llama::install_log_tracing();
    if !sovereign_compute::preflight::check_vram_reporting(&config, config_path) {
        error!(target: "serve", config = %config_path.display(), "the VRAM preflight refused");
        return Err(format!(
            "the VRAM preflight refused {}",
            config_path.display()
        ));
    }
    // The model-free engine, selectable by `[engine] kind = "mock"` and by
    // nothing else (the registry refuses an unknown id, never substitutes).
    if let Err(e) = sovereign_inference::engine_factory::register_engine(
        sovereign_compute::mock::MOCK_ENGINE,
        Arc::new(sovereign_compute::mock::MockEngine),
    ) {
        error!(target: "serve", error = %e, "engine registration refused");
        return Err(e.to_string());
    }
    let parts = match tokio::task::spawn_blocking(move || {
        sovereign_compute::assembly::assemble_serving(&config)
    })
    .await
    {
        Ok(Ok(parts)) => parts,
        Ok(Err(e)) => {
            error!(target: "serve", plan = ?e.plan, error = %e, "the serving assembly refused");
            return Err(e.to_string());
        }
        Err(e) => {
            error!(target: "serve", error = %e, "the serving assembly panicked");
            return Err(format!("the serving assembly panicked: {e}"));
        }
    };
    info!(target: "serve", plan = ?parts.plan, "serving assembly built");
    // Every route answers from one cell, so a reload swaps them all at once.
    let cell = Arc::new(reload::ReloadableProvider::new(
        parts.provider,
        parts.embed_family,
    ));
    let mut routes = bundles(Arc::clone(&cell) as Arc<dyn InferenceProvider>);
    routes.push(self_report::bundle(Arc::clone(&cell)));
    routes.push(reload::bundle(
        Arc::clone(&cell),
        parts.reload_factory,
        config_path.to_path_buf(),
    ));
    Ok(ServeAssembly {
        cell,
        routes,
        listen: (DEFAULT_LISTEN.0, sovereign_contracts::venue::serve_port()).into(),
        run_lock,
    })
}

async fn serve(args: ServeArgs) -> i32 {
    info!(target: "serve", data_dir = %args.data_dir.display(), listen = %args.listen, "serve starting");
    let config_path = SetupConfig::path_in(&args.data_dir);
    let assembly = match assemble(&args.data_dir, &config_path).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
    };

    let listener = match host_kit::shell::bind_with_retry(args.listen, "serve").await {
        Ok(l) => l,
        Err(e) => {
            error!(target: "serve", listen = %args.listen, error = %e, "cannot bind");
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
    };
    let bound = listener
        .local_addr()
        .map_or_else(|_| args.listen.to_string(), |a| a.to_string());
    println!("sovereign-serve: listening on http://{bound}");

    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = tokio::signal::ctrl_c() => {}
                    }
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        info!(target: "serve", "shutdown signal received");
    };
    let ServeAssembly {
        routes,
        run_lock: _run_lock,
        ..
    } = assembly;
    match host_kit::shell::serve([listener], routes, shutdown).await {
        Ok(()) => 0,
        Err(e) => {
            error!(target: "serve", error = %e, "the listener stopped");
            1
        }
    }
}

/// Every route `serve` answers, as the shell's named bundles: the compute
/// child's server, promoted (native wire, `/health`, the served kinds), and
/// the OpenAI translation in front of the same provider.
pub fn bundles(provider: Arc<dyn InferenceProvider>) -> Vec<RouteBundle> {
    let meta = ChildMeta {
        role: "serve".to_string(),
        model_id: provider.model_id_for(Speed::Slow),
    };
    let adapter = Arc::new(SovereignInferenceAdapter::new(
        Arc::clone(&provider),
        Arc::new(CoreSlotManifest),
    ));
    // NER registers on its first load; registered here, the kind mount below
    // serves its route before any request loads it (pb-svrn-dials-serve).
    if let Err(e) = sovereign_compute::ner::register() {
        tracing::warn!(target: "served_kind", error = %e, "NER kind did not register; serve has no /v1/ner");
    }
    vec![
        sovereign_compute::server::bundle(
            Arc::clone(&provider),
            Arc::new(AtomicBool::new(true)),
            meta,
        ),
        openai_bundle(adapter),
        sovereign_compute::setup_reads::bundle(),
        RouteBundle::new("serve_engine_state").route(
            sovereign_contracts::engine_state::ENGINE_STATE_PATH,
            get(engine_state),
        ),
    ]
}

/// The loader's CACHED view: the device memory it read the last time it
/// planned a distributed load, and the pinned block split. Never sampled
/// here — sampling an RPC device can stall on a busy worker (the 2026-07-30
/// hang) — so this answers as fast as the daemon's own `/v1/mesh/status`
/// did when the loader lived there (pb-svrn-dials-serve).
async fn engine_state() -> Json<sovereign_contracts::engine_state::EngineState> {
    use sovereign_contracts::engine_state::{DeviceBytes, DeviceMemoryReading, EngineState};
    use sovereign_inference::embedded::{DeviceMemory, DeviceMemorySnapshot};
    // Destructured exhaustively: a field added to the loader's reading is a
    // compile error here, never a field that silently stops at serve.
    let device_memory = sovereign_inference::embedded::last_device_memory().map(
        |DeviceMemorySnapshot {
             observed_unix,
             devices,
         }| DeviceMemoryReading {
            observed_unix,
            devices: devices
                .into_iter()
                .map(
                    |DeviceMemory {
                         endpoint,
                         free_bytes,
                         total_bytes,
                         reserve_bytes,
                     }| DeviceBytes {
                        endpoint,
                        free_bytes,
                        total_bytes,
                        reserve_bytes,
                    },
                )
                .collect(),
        },
    );
    let state = EngineState {
        device_memory,
        rpc_block_split_pin: sovereign_inference::embedded::pinned_block_split_raw(),
    };
    debug!(target: "serve", observed = state.device_memory.is_some(), pinned = state.rpc_block_split_pin.is_some(), "engine state: the loader's cached view");
    Json(state)
}

/// The OpenAI routes, over the adapter.
fn openai_bundle(adapter: Arc<SovereignInferenceAdapter>) -> RouteBundle {
    RouteBundle::new("serve_openai")
        .route(
            "/v1/chat/completions",
            post(chat_completions).layer(from_fn(|r, n| accept_lap("chat", r, n))),
        )
        .route(
            "/v1/embeddings",
            post(embeddings).layer(from_fn(|r, n| accept_lap("embed", r, n))),
        )
        .route("/v1/completions", post(completions))
        .route("/v1/models", get(list_models))
        .route("/oicp/v1/capabilities", get(capabilities))
        .with_state(adapter)
}

type AdapterState = State<Arc<SovereignInferenceAdapter>>;

/// Start a request's [`Lap`] before its body is read (`accept`), so the
/// handler's `parsed` mark prices the extractor.
async fn accept_lap(op: &'static str, mut request: Request, next: Next) -> Response {
    let lap = Lap::start("serve", op);
    lap.mark("accept");
    request.extensions_mut().insert(Arc::new(lap));
    next.run(request).await
}

async fn chat_completions(
    State(adapter): AdapterState,
    Extension(lap): Extension<Arc<Lap>>,
    connect: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Response {
    lap.mark("parsed");
    let peer = connect.map(|Extension(axum::extract::ConnectInfo(p))| p);
    sovereign_serving_host::turn_admission::honour_turn_admission(
        &mut request,
        sovereign_serving_host::turn_admission::from_this_host(peer, None),
        "serve",
    );
    if !request.stream.unwrap_or(false) {
        return match adapter.chat_completion(request).await {
            Ok(resp) => Json(resp).into_response(),
            Err(e) => chat_refusal(e, "local inference"),
        };
    }
    let header = ChunkHeader::new(request.model.clone());
    let frames = match adapter.chat_completion_stream(request).await {
        Ok(s) => s,
        Err(e) => return chat_refusal(e, "local stream"),
    };
    lap.mark("adapter returned");
    let events = frames
        .map(move |frame| {
            lap.first("first frame to body");
            Ok::<_, std::convert::Infallible>(openai_http::sse_event(&header, frame))
        })
        .chain(futures::stream::once(async {
            Ok(Event::default().data(openai_http::DONE))
        }));
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// A shed is backpressure and carries `Retry-After` (serving-host's one
/// shed renderer); any other failure is a 503 naming what failed.
fn chat_refusal(err: LocalInferenceError, what: &'static str) -> Response {
    match err {
        LocalInferenceError::Shed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        } => {
            warn!(target: "serve", queue_position = position, predicted_wait_ms, retry_after_secs, "chat_completions: local queue shed");
            sovereign_serving_host::admission::local_queue_shed_response(
                position,
                predicted_wait_ms,
                retry_after_secs,
            )
        }
        e => {
            warn!(target: "serve", error = %e, "chat_completions: {what} failed");
            openai_refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("{what} failed: {e}"),
                "backend_error",
            )
        }
    }
}

/// `/v1/completions`, the FIM model call over this process's adapter. The
/// svrn daemon's editor door keeps its context assembly and dials here for
/// the model (phase-b-23); a `raw_prompt` is the next-edit lane's verbatim
/// prompt. Rendered by the one FIM renderer both doors share.
async fn completions(
    State(adapter): AdapterState,
    Json(wire): Json<CompletionsRequestWire>,
) -> Response {
    let Some(prefix) = wire
        .effective_prefix()
        .map(str::to_string)
        .or_else(|| wire.raw_prompt.as_ref().map(|_| String::new()))
    else {
        return openai_refusal(
            StatusCode::BAD_REQUEST,
            "missing `prefix` (or legacy `prompt`, or `raw_prompt`)".to_string(),
            "invalid_request",
        );
    };
    let debug = wire.debug.unwrap_or(false);
    let stream = wire.stream.unwrap_or(false);
    let request = FimCompletionRequest {
        prefix,
        suffix: wire.suffix.unwrap_or_default(),
        path: wire.path,
        language: wire.language,
        max_tokens: wire.max_tokens,
        temperature: wire.temperature,
        stop: wire.stop.map(StopParam::into_vec).unwrap_or_default(),
        debug,
        raw_prompt: wire.raw_prompt,
    };
    debug!(target: "serve", raw = request.raw_prompt.is_some(), stream, "completions: FIM model call");
    match adapter.fim_completion_stream(request).await {
        Ok(start) if stream => fim_http::serve_fim_sse(start, debug, wire.model),
        Ok(start) => fim_http::serve_fim_aggregated(start, debug, wire.model).await,
        Err(e) => {
            warn!(target: "serve", error = %e, "completions: FIM unavailable");
            openai_refusal(StatusCode::SERVICE_UNAVAILABLE, e, "fim_unavailable")
        }
    }
}

async fn embeddings(
    State(adapter): AdapterState,
    Extension(lap): Extension<Arc<Lap>>,
    Json(request): Json<EmbeddingRequest>,
) -> Response {
    lap.mark("parsed");
    match openai_http::embeddings_response(adapter.as_ref(), request).await {
        Ok(resp) => {
            lap.mark("adapter returned");
            let response = Json(resp).into_response();
            lap.mark("serialized");
            let bytes = response.body().size_hint().exact();
            debug!(target: LATENCY_TARGET, op = "embed", bytes, "embed response size");
            response
        }
        Err(refusal) => openai_refusal(refusal.status, refusal.message, refusal.error_type),
    }
}

/// `/v1/models` from this process's own manifest: the slots it holds, and
/// the aliases (`primary`, `commonwealth/primary`, …) its resident slots
/// answer to (`venue::resolution_alias_keys`, the one alias vocabulary).
async fn list_models(State(adapter): AdapterState) -> Response {
    let holders = adapter
        .provider_manifest()
        .map(|m| m.models)
        .unwrap_or_default()
        .into_iter()
        .map(|m| (LOCAL_HOLDER.to_string(), m))
        .collect();
    let mut aliases: HashMap<String, String> = HashMap::new();
    for slot in adapter.resident_slots() {
        for key in resolution_alias_keys(&slot.role) {
            aliases.insert(key, slot.model_id.clone());
        }
    }
    // `primary` and `fast` bind the way the manifest advertises them
    // (`build_self_manifest`): to the model each speed resolves to.
    for (role, speed) in [("primary", Speed::Slow), ("fast", Speed::Fast)] {
        let id = adapter.model_id_for(speed);
        if !id.is_empty() && id != "unknown" {
            for key in resolution_alias_keys(role) {
                aliases.insert(key, id.clone());
            }
        }
    }
    let mut data = openai_http::model_rows(holders, &aliases, LOCAL_HOLDER);
    data.sort_by(|a, b| a.id.cmp(&b.id));
    info!(target: "serve", models = data.len(), "listed models from this process's own manifest");
    Json(ModelListResponse {
        object: "list".into(),
        data,
    })
    .into_response()
}

/// `/oicp/v1/capabilities`: this process's own provider manifest, the source
/// its `/v1/models` reads. The svrn daemon's loopback terminal arm reads it as
/// its own manifest and resident slots, so its peers still see this node's
/// models after the daemon stops holding them (phase-b-23, option (a)).
async fn capabilities(State(adapter): AdapterState) -> Response {
    match adapter.provider_manifest() {
        Some(manifest) => {
            debug!(target: "serve", models = manifest.models.len(), "capabilities: this process's own manifest");
            Json(manifest).into_response()
        }
        None => {
            warn!(target: "serve", "capabilities: the provider builds no manifest");
            openai_refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "this serve's provider builds no manifest".to_string(),
                "no_manifest",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    /// Without `llama_cpp` in the allowlist, the log route serve installs
    /// before loading delivers nothing and a failed load is a bare error.
    #[test]
    fn serve_filter_carries_llama_cpp_target() {
        assert!(
            DEFAULT_FILTER.contains("llama_cpp=info"),
            "llama_cpp must be allowlisted: {DEFAULT_FILTER}"
        );
        tracing_subscriber::EnvFilter::builder()
            .parse(DEFAULT_FILTER)
            .expect("serve's default filter must parse");
    }

    use super::*;

    #[test]
    fn parse_takes_the_data_dir_and_the_listener() {
        let args: Vec<String> = ["--data-dir", "/srv/serve", "--listen", "127.0.0.1:8080"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = ServeArgs::parse(&args).expect("valid");
        assert_eq!(parsed.data_dir, PathBuf::from("/srv/serve"));
        assert_eq!(parsed.listen, "127.0.0.1:8080".parse().unwrap());
    }

    #[test]
    fn with_no_listener_named_it_listens_where_svrn_dials() {
        let parsed = ServeArgs::parse(&[]).expect("valid");
        assert_eq!(parsed.listen, "127.0.0.1:9748".parse().unwrap());
    }

    /// A provider with a FIM-capable edit slot that records the request it
    /// was asked to decode.
    struct EditSlotStub {
        seen: std::sync::Mutex<Option<sovereign_contracts::CompletionRequest>>,
    }

    #[async_trait::async_trait]
    impl InferenceProvider for EditSlotStub {
        async fn complete(
            &self,
            _r: &sovereign_contracts::CompletionRequest,
        ) -> sovereign_contracts::Result<sovereign_contracts::CompletionResponse> {
            unimplemented!("stream-only stub")
        }
        async fn complete_stream(
            &self,
            _r: &sovereign_contracts::CompletionRequest,
        ) -> sovereign_contracts::Result<
            std::pin::Pin<
                Box<dyn futures::Stream<Item = sovereign_contracts::Result<String>> + Send>,
            >,
        > {
            unimplemented!("with_finish-only stub")
        }
        async fn complete_stream_with_finish(
            &self,
            request: &sovereign_contracts::CompletionRequest,
        ) -> sovereign_contracts::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = sovereign_contracts::StreamFrame> + Send>>,
        > {
            *self.seen.lock().unwrap() = Some(request.clone());
            Ok(Box::pin(futures::stream::iter(vec![
                sovereign_contracts::StreamFrame::Token("return a + b;".to_string()),
                sovereign_contracts::StreamFrame::Finish {
                    reason: sovereign_contracts::FinishReason::Stop,
                    usage: None,
                },
            ])))
        }
        async fn embed(&self, _t: &str) -> sovereign_contracts::Result<Vec<f32>> {
            unimplemented!()
        }
        fn capabilities(&self) -> sovereign_contracts::ProviderCapabilities {
            sovereign_contracts::ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: sovereign_contracts::Depth::Shallow,
            }
        }
        fn edit_slot_info(&self) -> Option<sovereign_contracts::EditSlotInfo> {
            Some(sovereign_contracts::EditSlotInfo {
                slot: "edit".into(),
                model_id: "coder".into(),
                aliased_to_fast: false,
                degraded: false,
                next_edit: None,
                fim: Some(sovereign_contracts::FimLane {
                    style: sovereign_contracts::FimStyle::QwenCoder,
                    max_tokens: 48,
                    temperature: 0.2,
                    max_prefix_chars: 4096,
                    max_suffix_chars: 4096,
                }),
            })
        }
    }

    async fn serving(provider: Arc<dyn InferenceProvider>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(host_kit::shell::serve(
            [listener],
            bundles(provider),
            std::future::pending(),
        ));
        base
    }

    #[tokio::test]
    async fn serve_decodes_the_next_edit_lanes_raw_prompt_verbatim() {
        let stub = Arc::new(EditSlotStub {
            seen: std::sync::Mutex::new(None),
        });
        let base = serving(Arc::clone(&stub) as Arc<dyn InferenceProvider>).await;
        let raw = "<|editable_region_start|>fn add(a, b) {}<|editable_region_end|>";
        let body: serde_json::Value = reqwest::Client::new()
            .post(format!("{base}/v1/completions"))
            .json(&serde_json::json!({ "raw_prompt": raw, "max_tokens": 16 }))
            .send()
            .await
            .expect("answered")
            .json()
            .await
            .expect("a text_completion");
        assert_eq!(body["choices"][0]["text"], "return a + b;", "{body}");
        let seen = stub.seen.lock().unwrap().clone().expect("decoded");
        assert_eq!(seen.prompt, raw, "the raw prompt was not decoded verbatim");
        assert_eq!(seen.model_id.as_deref(), Some("coder"));
    }

    #[tokio::test]
    async fn serve_without_an_edit_slot_refuses_fim_by_name() {
        let base = serving(Arc::new(sovereign_compute::mock::MockProvider {
            tokens: 1,
            delay: std::time::Duration::ZERO,
        }))
        .await;
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/completions"))
            .json(&serde_json::json!({ "prefix": "fn main() {" }))
            .send()
            .await
            .expect("answered");
        assert_eq!(resp.status(), 503);
        assert!(resp
            .text()
            .await
            .unwrap_or_default()
            .contains("fim_unavailable"));
    }

    #[test]
    fn parse_refuses_an_unknown_argument_by_name() {
        let err = ServeArgs::parse(&["--mesh".to_string()]).expect_err("refused");
        assert!(err.contains("--mesh"), "got: {err}");
    }

    #[test]
    fn the_weight_verbs_route_before_the_server_arguments() {
        // Unrouted, `warm-cache` would reach ServeArgs::parse and exit 2.
        for verb in WEIGHT_VERBS {
            assert_eq!(run(&[verb.to_string(), "--help".into()]), 0, "{verb}");
        }
    }
}
