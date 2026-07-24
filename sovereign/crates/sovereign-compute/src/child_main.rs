// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compute child's entrypoint — reached by re-executing the daemon
//! binary with `--compute-child` (no separate artifact ships).
//!
//! Lifecycle: bind `127.0.0.1:0` → print the stdout handshake (the
//! supervisor parses the port → `Warming`) → serve immediately (so
//! `/health` answers 503 while the model loads) → load the model on a
//! blocking task → flip `ready` (→ `Healthy`) → serve until SIGTERM →
//! `fast_exit_skip_destructors` past the ggml teardown SIGABRT.
//!
//! Roles: `generate` (EmbeddedLlamaCpp), `embed` (EmbedOnlyProvider),
//! `mock` (canned frames with a per-token delay — model-free, for the
//! crash-isolation e2e).

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, Depth, Error, FinishReason, InferenceProvider,
    ProviderCapabilities, Result, Speed, StreamFrame,
};
use sovereign_core::model_family::ModelFamily;
use sovereign_inference::embedded::{EmbedOnlyProvider, EmbeddedLlamaCpp};
use sovereign_inference::fast_exit_skip_destructors;
use tracing::{error, info};

use crate::server::{router, ChildMeta};
use crate::supervisor::HANDSHAKE_PREFIX;

/// The kind of provider a child hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Full generative engine (`EmbeddedLlamaCpp`), serves `/internal/complete*`.
    Generate,
    /// Embedding-only engine (`EmbedOnlyProvider`).
    Embed,
    /// Model-free canned provider for the crash-isolation e2e.
    Mock,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Generate => "generate",
            Role::Embed => "embed",
            Role::Mock => "mock",
        }
    }
}

/// Parsed child CLI. Flags are visible in `ps` (glassbox) — no temp config.
struct ChildArgs {
    role: Role,
    name: String,
    model: Option<PathBuf>,
    ctx: u32,
    gpu_layers: Option<u32>,
    bind: String,
    mock_tokens: usize,
    mock_token_delay_ms: u64,
}

fn take_value<'a>(
    it: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> std::result::Result<String, String> {
    it.next()
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

impl ChildArgs {
    fn parse(args: &[String]) -> std::result::Result<Self, String> {
        let mut role: Option<Role> = None;
        let mut name: Option<String> = None;
        let mut model: Option<PathBuf> = None;
        let mut ctx: u32 = 4096;
        let mut gpu_layers: Option<u32> = None;
        let mut bind = "127.0.0.1:0".to_string();
        let mut mock_tokens: usize = 32;
        let mut mock_token_delay_ms: u64 = 0;

        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--role" => {
                    role = Some(match take_value(&mut it, "--role")?.as_str() {
                        "generate" => Role::Generate,
                        "embed" => Role::Embed,
                        "mock" => Role::Mock,
                        other => return Err(format!("unknown --role: {other}")),
                    })
                }
                "--name" => name = Some(take_value(&mut it, "--name")?),
                "--model" => model = Some(PathBuf::from(take_value(&mut it, "--model")?)),
                "--ctx" => {
                    ctx = take_value(&mut it, "--ctx")?
                        .parse()
                        .map_err(|_| "invalid --ctx".to_string())?
                }
                "--gpu-layers" => {
                    let v = take_value(&mut it, "--gpu-layers")?;
                    gpu_layers = if v == "auto" {
                        None
                    } else {
                        Some(v.parse().map_err(|_| "invalid --gpu-layers".to_string())?)
                    };
                }
                "--bind" => bind = take_value(&mut it, "--bind")?,
                "--mock-tokens" => {
                    mock_tokens = take_value(&mut it, "--mock-tokens")?
                        .parse()
                        .map_err(|_| "invalid --mock-tokens".to_string())?
                }
                "--mock-token-delay-ms" => {
                    mock_token_delay_ms = take_value(&mut it, "--mock-token-delay-ms")?
                        .parse()
                        .map_err(|_| "invalid --mock-token-delay-ms".to_string())?
                }
                other => return Err(format!("unknown flag: {other}")),
            }
        }

        Ok(ChildArgs {
            role: role.ok_or_else(|| "--role is required".to_string())?,
            name: name.unwrap_or_else(|| "compute-child".to_string()),
            model,
            ctx,
            gpu_layers,
            bind,
            mock_tokens,
            mock_token_delay_ms,
        })
    }

    /// Addressable model id: the gguf file stem, or the slot name for mock.
    fn model_id(&self) -> String {
        match self.role {
            Role::Mock => "mock".to_string(),
            _ => self
                .model
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.name.clone()),
        }
    }
}

/// Entrypoint: `<daemon-binary> --compute-child --role … --model …`.
/// Returns a process exit code (the success path never returns — it
/// `fast_exit`s past the ggml destructors on SIGTERM).
pub fn run(args: &[String]) -> i32 {
    let cfg = match ChildArgs::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("compute-child: {e}");
            return 2;
        }
    };

    // Die if the daemon (our parent) dies — even on an uncatchable daemon
    // crash, so a compute child never outlives the daemon and leaks a port.
    // The delivered SIGTERM hits the graceful-shutdown path below.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }

    init_child_tracing();

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        // Match the daemon's 8 MiB worker stack — llama.cpp call chains are deep.
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-compute-child-rt")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("compute-child: cannot build runtime: {e}");
            return 1;
        }
    };
    rt.block_on(serve(cfg))
}

fn init_child_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("sovereign_compute=info,sovereign_inference=info,compute_child=info")
    });
    // try_init: never panic if a subscriber is somehow already set.
    let _ = fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
}

async fn serve(cfg: ChildArgs) -> i32 {
    let listener = match tokio::net::TcpListener::bind(&cfg.bind).await {
        Ok(l) => l,
        Err(e) => {
            error!(target: "compute_child", bind = %cfg.bind, error = %e, "cannot bind");
            return 1;
        }
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            error!(target: "compute_child", error = %e, "cannot read local_addr");
            return 1;
        }
    };
    let pid = std::process::id();

    // Handshake FIRST (before the model load) so the supervisor learns the
    // port and enters Warming immediately. Must go to stdout as a single line.
    println!("{HANDSHAKE_PREFIX}{{\"port\":{port},\"pid\":{pid}}}");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let ready = Arc::new(AtomicBool::new(false));
    let slot: Arc<RwLock<Option<Arc<dyn InferenceProvider>>>> = Arc::new(RwLock::new(None));
    let lazy: Arc<dyn InferenceProvider> = Arc::new(LazyProvider {
        slot: Arc::clone(&slot),
        name: cfg.name.clone(),
    });
    let meta = ChildMeta {
        role: cfg.role.as_str().to_string(),
        model_id: cfg.model_id(),
    };

    // Load the model off the runtime's worker (blocking) so `/health`
    // stays responsive (503 loading) throughout. On failure, exit non-zero
    // → the supervisor respawns / eventually latches Failed.
    {
        let ready = Arc::clone(&ready);
        let slot = Arc::clone(&slot);
        let name = cfg.name.clone();
        let role = cfg.role;
        let model = cfg.model.clone();
        let ctx = cfg.ctx;
        let gpu = cfg.gpu_layers;
        let mock_tokens = cfg.mock_tokens;
        let mock_delay = cfg.mock_token_delay_ms;
        tokio::spawn(async move {
            let loaded = tokio::task::spawn_blocking(move || {
                load_provider(role, model, ctx, gpu, mock_tokens, mock_delay)
            })
            .await;
            match loaded {
                Ok(Ok(provider)) => {
                    if let Ok(mut w) = slot.write() {
                        *w = Some(provider);
                    }
                    ready.store(true, Ordering::Relaxed);
                    info!(target: "compute_child", child = %name, "model loaded; serving");
                }
                Ok(Err(e)) => {
                    error!(target: "compute_child", child = %name, error = %e, "model load failed; exiting");
                    fast_exit_skip_destructors(1);
                }
                Err(e) => {
                    error!(target: "compute_child", child = %name, error = %e, "model load task panicked; exiting");
                    fast_exit_skip_destructors(1);
                }
            }
        });
    }

    let app = router(lazy, ready, meta);
    info!(
        target: "compute_child",
        child = %cfg.name,
        role = cfg.role.as_str(),
        port,
        "compute child serving; awaiting model load"
    );

    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    term.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        error!(target: "compute_child", error = %e, "axum serve error");
        return 1;
    }

    // SIGTERM path — skip the ggml static destructors (the teardown SIGABRT).
    info!(target: "compute_child", child = %cfg.name, "SIGTERM received; fast-exiting");
    fast_exit_skip_destructors(0)
}

/// Build the role's provider. Blocking (model load) — call on `spawn_blocking`.
fn load_provider(
    role: Role,
    model: Option<PathBuf>,
    ctx: u32,
    gpu_layers: Option<u32>,
    mock_tokens: usize,
    mock_token_delay_ms: u64,
) -> Result<Arc<dyn InferenceProvider>> {
    match role {
        Role::Generate => {
            let path = model
                .ok_or_else(|| Error::InvalidInput("--model required for role=generate".into()))?;
            // Single model into the fast slot (no separate primary). Grammar/
            // structured-output are honoured per-request by build_sampler.
            let engine = EmbeddedLlamaCpp::load_dual(&path, None, ctx, gpu_layers)?;
            Ok(Arc::new(engine))
        }
        Role::Embed => {
            let path = model
                .ok_or_else(|| Error::InvalidInput("--model required for role=embed".into()))?;
            let engine = EmbedOnlyProvider::load(&path, ModelFamily::Unknown)?;
            Ok(Arc::new(engine))
        }
        Role::Mock => Ok(Arc::new(MockProvider {
            tokens: mock_tokens.max(1),
            delay: Duration::from_millis(mock_token_delay_ms),
        })),
    }
}

/// Delegates to the loaded provider once ready; returns `ComputeUnavailable`
/// (→ HTTP 503) while the model is still loading, so `/health` and
/// `/internal/*` answer fast instead of hanging.
struct LazyProvider {
    slot: Arc<RwLock<Option<Arc<dyn InferenceProvider>>>>,
    name: String,
}

impl LazyProvider {
    fn current(&self) -> Option<Arc<dyn InferenceProvider>> {
        // Recover from a poisoned lock rather than panic — the guard is only
        // ever held for a clone.
        self.slot.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn unavailable(&self) -> Error {
        Error::ComputeUnavailable {
            slot: self.name.clone(),
            reason: "model still loading".to_string(),
        }
    }
}

#[async_trait]
impl InferenceProvider for LazyProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        match self.current() {
            Some(p) => p.complete(request).await,
            None => Err(self.unavailable()),
        }
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        match self.current() {
            Some(p) => p.complete_stream(request).await,
            None => Err(self.unavailable()),
        }
    }

    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        match self.current() {
            Some(p) => p.complete_stream_with_finish(request).await,
            None => Err(self.unavailable()),
        }
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        match self.current() {
            Some(p) => p.embed(text).await,
            None => Err(self.unavailable()),
        }
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        match self.current() {
            Some(p) => p.embed_batch(texts).await,
            None => Err(self.unavailable()),
        }
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        match self.current() {
            Some(p) => p.embed_query(query).await,
            None => Err(self.unavailable()),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        match self.current() {
            Some(p) => p.capabilities(),
            None => ProviderCapabilities {
                max_context_tokens: 0,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            },
        }
    }
}

/// Model-free provider: streams `tokens` canned tokens with `delay` between
/// them (so a crash-isolation test can `kill -9` mid-stream), and answers
/// `complete`/`embed` with fixed values.
struct MockProvider {
    tokens: usize,
    delay: Duration,
}

#[async_trait]
impl InferenceProvider for MockProvider {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: "mock response".to_string(),
            tokens_used: self.tokens,
            prompt_tokens: 0,
            model_id: "mock".to_string(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: Some(FinishReason::Stop),
            completion_tokens: Some(self.tokens as u32),
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let n = self.tokens;
        let delay = self.delay;
        let s = futures::stream::unfold(0usize, move |i| async move {
            if i >= n {
                return None;
            }
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }
            Some((Ok(format!("tok{i} ")), i + 1))
        });
        Ok(Box::pin(s))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}
