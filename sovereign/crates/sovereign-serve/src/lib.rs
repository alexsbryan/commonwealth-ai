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

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use futures::StreamExt;
use host_kit::shell::RouteBundle;
use sovereign_compute::server::{openai_refusal, ChildMeta};
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
mod reload;
mod warm_cache;

/// The run lock's name inside the data root (`host_kit::RunLock`): one
/// `serve` per root. The daemon's and cw-rails' locks are their own.
pub const RUN_LOCK: &str = "serve";

/// Where `serve` listens unless `--listen` says otherwise: loopback, on the
/// one port the svrn daemon dials by default
/// (`sovereign_contracts::venue::DEFAULT_SERVE_PORT`).
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
        let mut listen: SocketAddr = DEFAULT_LISTEN.into();
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
    // The re-execs of this binary come first, before tracing or a runtime:
    // each builds its own (the same rule as the daemon's binary).
    match Launch::parse(args, Launch::Bare) {
        Launch::ComputeChild { args } => return sovereign_compute::child_main::run(&args),
        Launch::RpcWorker { args } => return sovereign_inference::rpc_worker_main::run(&args),
        _ => {}
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

/// The subcommands `run` routes before the server's arguments. The dispatcher
/// sends `svrn mesh <verb>` here for exactly these.
pub const WEIGHT_VERBS: &[&str] = &["warm-cache", "fetch-model"];

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
            _ => fetch_model::cmd_fetch_model(rest).await,
        }
    })
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "serve=info,sovereign_serve=info,sovereign_compute=info,sovereign_inference=info,\
             sovereign_serving_host=info,host_kit=info,served_kind=info,engine_factory=info,\
             serving_assembly=info,compute_child=info",
        )
    });
    let _ = fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
}

async fn serve(args: ServeArgs) -> i32 {
    info!(target: "serve", data_dir = %args.data_dir.display(), listen = %args.listen, "serve starting");
    let _run_lock = match host_kit::RunLock::acquire(&args.data_dir, RUN_LOCK) {
        Ok(lock) => lock,
        Err(e) => {
            error!(target: "serve", error = %e, "refusing to start");
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
    };
    let config_path = SetupConfig::path_in(&args.data_dir);
    let config = match SetupConfig::load_from(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(target: "serve", config = %config_path.display(), error = %e, "no config to serve from");
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
    };
    // The model-free engine, selectable by `[engine] kind = "mock"` and by
    // nothing else (the registry refuses an unknown id, never substitutes).
    if let Err(e) = sovereign_inference::engine_factory::register_engine(
        sovereign_compute::mock::MOCK_ENGINE,
        Arc::new(sovereign_compute::mock::MockEngine),
    ) {
        error!(target: "serve", error = %e, "engine registration refused");
        return 1;
    }
    let parts = match tokio::task::spawn_blocking(move || {
        sovereign_compute::assembly::assemble_serving(&config)
    })
    .await
    {
        Ok(Ok(parts)) => parts,
        Ok(Err(e)) => {
            error!(target: "serve", plan = ?e.plan, error = %e, "the serving assembly refused");
            eprintln!("sovereign-serve: {e}");
            return 1;
        }
        Err(e) => {
            error!(target: "serve", error = %e, "the serving assembly panicked");
            return 1;
        }
    };
    info!(target: "serve", plan = ?parts.plan, "serving assembly built");

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
    // Every route answers from one cell, so a reload swaps them all at once.
    let cell = Arc::new(reload::ReloadableProvider::new(parts.provider));
    let mut routes = bundles(Arc::clone(&cell) as Arc<dyn InferenceProvider>);
    routes.push(reload::bundle(cell, parts.reload_factory, config_path));
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
    vec![
        sovereign_compute::server::bundle(
            Arc::clone(&provider),
            Arc::new(AtomicBool::new(true)),
            meta,
        ),
        openai_bundle(adapter),
        RouteBundle::new("serve_engine_state").route(
            sovereign_contracts::engine_state::ENGINE_STATE_PATH,
            get(engine_state),
        ),
        RouteBundle::new("serve_self")
            .route(
                sovereign_contracts::engine_state::SERVED_SELF_PATH,
                get(served_self),
            )
            .with_state(provider),
    ]
}

/// What this process's provider says about itself, read at the moment of
/// the request. The svrn daemon's loopback terminal arm answers its own
/// `model_id_for`, `resident_slots` and `edit_slot_info` from this.
async fn served_self(
    State(provider): State<Arc<dyn InferenceProvider>>,
) -> Json<sovereign_contracts::engine_state::ServedSelf> {
    let this = sovereign_contracts::engine_state::ServedSelf {
        primary_model: provider.model_id_for(Speed::Slow),
        medium_model: provider.model_id_for(Speed::Medium),
        fast_model: provider.model_id_for(Speed::Fast),
        embed_model: provider.embed_model_id(),
        code_model: provider.code_model_id(),
        resident_slots: provider.resident_slots(),
        edit_slot: provider.edit_slot_info(),
    };
    debug!(target: "serve", primary = %this.primary_model, slots = this.resident_slots.len(), edit = this.edit_slot.is_some(), "served self: this process's provider");
    Json(this)
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
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/completions", post(completions))
        .route("/v1/models", get(list_models))
        .route("/oicp/v1/capabilities", get(capabilities))
        .with_state(adapter)
}

type AdapterState = State<Arc<SovereignInferenceAdapter>>;

async fn chat_completions(
    State(adapter): AdapterState,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
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
    let events = frames
        .map(move |frame| Ok::<_, std::convert::Infallible>(openai_http::sse_event(&header, frame)))
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
    Json(request): Json<EmbeddingRequest>,
) -> Response {
    match openai_http::embeddings_response(adapter.as_ref(), request).await {
        Ok(resp) => Json(resp).into_response(),
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

    #[tokio::test]
    async fn serve_describes_its_own_provider_on_the_self_route() {
        let provider: Arc<dyn InferenceProvider> =
            Arc::new(sovereign_compute::mock::MockProvider {
                tokens: 1,
                delay: std::time::Duration::ZERO,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(host_kit::shell::serve(
            [listener],
            bundles(provider),
            std::future::pending(),
        ));
        let this: sovereign_contracts::engine_state::ServedSelf = reqwest::get(format!(
            "{base}{}",
            sovereign_contracts::engine_state::SERVED_SELF_PATH
        ))
        .await
        .expect("answered")
        .json()
        .await
        .expect("a ServedSelf");
        assert_eq!(this.primary_model, sovereign_compute::mock::MOCK_MODEL);
        assert_eq!(this.resident_slots.len(), 1);
        assert_eq!(this.resident_slots[0].role, "primary");
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
