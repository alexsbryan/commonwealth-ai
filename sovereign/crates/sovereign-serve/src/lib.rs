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

mod engine_state;
mod fetch_model;
mod fetch_ner;
/// The measurement codec on cw-rails' `mesh-measurements` journal, and the
/// reconcile loop that keeps the local file on it (pb-serve-placement).
pub mod measurements_rail;
/// `svrn mesh bench`: measure a placement's decode rate under the probe's
/// validity guards and file the record (pb-serve-placement).
mod mesh_bench;
/// Placement measurements: what a placement was observed to do, keyed by
/// model fingerprint × placement digest × machine witness (phase-b-22).
pub mod mesh_measurements;
/// `svrn mesh plan`: dry-run a model's split across a mesh, with what the
/// placement measurements know about its speed (pb-serve-placement).
mod mesh_plan;
/// The CLI's side of measurement travel: publish a run, read peers' runs.
mod mesh_travel;
mod reload;
/// A model named by URL: fetched header-only so `plan` can read its tensors.
mod remote_gguf;
mod self_report;
/// serve's own process body (`run` -> `standalone::serve`).
mod standalone;
mod warm_cache;

/// Exit past ggml's static destructors (the teardown SIGABRT, and on macOS
/// the ggml-metal device sweeper's assertion): the loader's, so a process
/// that hosts serve's assembly calls it on its way out, as `run` does
/// (pb-serve-distributes).
pub use sovereign_inference::fast_exit_skip_destructors;

/// The loader's RPC env contract: `--rpc-worker` and the `[shared_model]`
/// role, translated into the env the worker bind, discovery and the host's
/// knobs read. A process hosting serve's assembly applies it before anything
/// reads that env (pb-serve-distributes).
pub use sovereign_compute::distributed_role::{
    apply_rpc_worker_flag, apply_shared_model_role_to_env,
};

/// The NER kind's one handle per process, loaded on first ask: what a process
/// hosting serve's assembly hands svrn for its own ingest and retrieval
/// (pb-serve-distributes), the same handle serve's `/v1/ner` reads.
pub use sovereign_compute::ner::served_ner;

/// The worker side of distributed-inference auto-warm (the contracts port
/// `RpcShardWarmer`): what a process hosting serve's assembly hands svrn's
/// `/internal/rpc-warm` until the flip (pb-serve-distributes).
pub use sovereign_compute::distributed_warm::MeshRpcShardWarmer;

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
    // serve's placement measurements stay on cw-rails' journal whether or not
    // this process ever binds (pb-serve-placement).
    measurements_rail::spawn_reconcile(Some(SetupConfig::path_in(&parsed.data_dir)));
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
    let code = runtime.block_on(standalone::serve(parsed));
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
/// `plan` and `bench` are placement measurement's (pb-serve-placement).
pub const WEIGHT_VERBS: &[&str] = &["warm-cache", "fetch-model", "fetch-ner", "plan", "bench"];

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
            "plan" => mesh_plan::cmd_plan(rest).await,
            "bench" => mesh_bench::cmd_bench(rest).await,
            _ => fetch_model::cmd_fetch_model(rest).await,
        }
    })
}

/// serve's allowlisted targets, without `llama_cpp`, whose level
/// [`tracing_filter`] decides; a host takes the whole filter from there.
pub const DEFAULT_FILTER: &str = "serve=info,sovereign_serve=info,sovereign_compute=info,\
     sovereign_inference=info,sovereign_serving_host=info,host_kit=info,served_kind=info,\
     engine_factory=info,serving_assembly=info,compute_child=info";

/// serve's log filter when `RUST_LOG` is unset, and the one a distribution
/// hosting serve unions with its own: an allowlist of targets.
///
/// Also carries `llama_cpp`, the LITERAL target every ggml/llama.cpp log line
/// rides (`sovereign_inference::llama::ggml_log_cb`). Its absence silently
/// defeated both the model-load-failure surface (a failed load reaching the
/// operator as a bare "null result from llama cpp") and every
/// `GGML_RPC_DEBUG=1` investigation (2026-07-27 distributed-inference crash
/// hunt). [`llama_debug_requested`] cranks it to `debug`; otherwise `info`
/// keeps routine load chatter out while WARN/ERROR still surface. The loader's
/// knob, so the loader's filter (pb-serve-distributes; it was svrn's).
pub fn tracing_filter() -> String {
    tracing_filter_for(llama_debug_requested())
}

fn tracing_filter_for(llama_debug: bool) -> String {
    let llama_lvl = if llama_debug { "debug" } else { "info" };
    format!("{DEFAULT_FILTER},llama_cpp={llama_lvl}")
}

/// True when the operator has asked for verbose ggml/llama.cpp output, by
/// either our own knob (`SOVEREIGN_LLAMA_LOGS=1`) or llama.cpp's own
/// documented RPC knob (`GGML_RPC_DEBUG`). Honouring the latter here is what
/// makes `GGML_RPC_DEBUG=1` behave the way its upstream documentation
/// promises: the var alone gates `LOG_DBG` inside ggml-rpc.cpp, but those
/// lines are `GGML_LOG_DEBUG` and would still die at our callback and again
/// at this filter. One env var, all three gates.
pub fn llama_debug_requested() -> bool {
    sovereign_inference::llama_logs::LlamaLogs::from_env().is_verbose()
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(tracing_filter()));
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
    /// The distribution over this assembly's engine and distributed-primary
    /// slot (compute's `distributed_discovery::distribute`), which a hosting
    /// process starts once its mesh is up (pb-serve-distributes). A
    /// standalone serve drops it, as it did before.
    pub distribute: sovereign_serving_host::rpc_discovery::Distribute,
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
    // The files peers may fetch from this node: every shard of each slot it
    // advertises, published before the engine loads (model transfer is
    // serve's, phase-b-19; pb-serve-distributes).
    let servable = sovereign_serving_host::state::ServableModelFilesReader::default();
    let files = sovereign_compute::model_transfer::servable_for(config.models.as_ref());
    info!(target: "serve", files = files.len(), "publishing servable model files for peer fetch");
    servable.publish(files);
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
    // serve's weights: the NER read and the download job, into this root.
    routes.push(sovereign_compute::assets::bundle(data_dir.join("models")));
    // Model transfer: peers fetch the files above, whole or by byte range.
    routes.push(sovereign_compute::model_transfer::bundle(servable));
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
        distribute: sovereign_compute::distributed_discovery::distribute(
            parts.llama,
            parts.distributed_primary,
        ),
    })
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
            get(engine_state::engine_state),
        ),
    ]
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

// serve's crate-root tests live in a sibling file, which keeps lib.rs out of
// arch-gate's approach band (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
