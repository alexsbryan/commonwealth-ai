// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reaching `serve`, the model server (FIVE_PROGRAMS §2), from the svrn
//! daemon (pb-svrn-dials-serve): where it listens, and whether this daemon
//! dials it at all.

use sovereign_contracts::setup_config::{EntryBinding, NodeSection, SetupConfig};

/// Does a provider serve the rerank kind: the one decider, asked here of a
/// [`loopback_provider`] by the clients that dial serve (cli-llm's
/// `serve_dial`, pb-cli-llm), which link no compute crate.
pub use sovereign_contracts::rerank_kind::serves_rerank;

static DECIDED: std::sync::OnceLock<ServingPath> = std::sync::OnceLock::new();

/// Where the path decided at boot reaches serve: `Some(base)`, or `None` on a
/// terminal, which dials its entry node and has no serve. Unset where no boot
/// decided.
static SERVE_TARGET: std::sync::OnceLock<Option<ServeBase>> = std::sync::OnceLock::new();

/// Where this daemon's inference is served from — THE one decider, read once
/// at boot.
///
/// Serving is serve's on every config (pb-serve-distributes): mesh-distributed
/// inference (RPC-worker discovery, the distributed-primary respawn, the
/// auto-warm orchestrator) runs in the process that loads the engine, so a
/// distribution that hosts serve here runs it here ([`ServingPath::with_hosting`]).
/// Every other config dials serve. The in-process path pb-svrn-dials-serve
/// kept for those opt-ins is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingPath {
    /// Serving lives in `serve`, which this daemon dials.
    DialsServe,
    /// Serving lives in `serve`, assembled in THIS process by the distribution
    /// that runs it (the stock binary, pb-stock-binary): svrn holds the same
    /// provider cell serve's routes answer from, and dials nothing.
    Hosted,
}

impl ServingPath {
    /// `hosting` says the process entry was handed a [`HostedServe`]. A path
    /// becomes [`ServingPath::Hosted`] only where serve would be this host's
    /// anyway: not a terminal (it dials its entry node) and no `[node] entry`
    /// address (an operator-named serve stays dialed).
    pub fn decide(config: &SetupConfig, hosting: bool) -> Self {
        let path = Self::DialsServe.with_hosting(hosting, config);
        tracing::info!(
            target: "serving_path",
            serving = %path.status_line(),
            hosting,
            "serving path decided"
        );
        // Kept as decided: `/status` reports the path this process booted on,
        // which a later config edit does not change.
        let _ = DECIDED.set(path.clone());
        let terminal =
            config.node_class() == sovereign_contracts::setup_config::NodeClass::Terminal;
        let _ = SERVE_TARGET.set((!terminal).then(|| resolve_serve_base(&config.node)));
        path
    }

    /// Where this process reaches serve, as decided at boot: `Some(Some(base))`,
    /// `Some(None)` on a terminal (no serve to reach), `None` where no boot
    /// decided. Readers name each absence apart (principle 6).
    pub fn decided_serve() -> Option<Option<&'static ServeBase>> {
        SERVE_TARGET.get().map(Option::as_ref)
    }

    /// [`ServingPath::DialsServe`] becomes [`ServingPath::Hosted`] when this
    /// process hosts serve and serve would be this host's anyway. Every other
    /// path is kept.
    pub fn with_hosting(self, hosting: bool, config: &SetupConfig) -> Self {
        let here = config.node_class() != sovereign_contracts::setup_config::NodeClass::Terminal
            && resolve_serve_base(&config.node).source == ServeBaseSource::Default;
        match self {
            Self::DialsServe if hosting && here => Self::Hosted,
            path => path,
        }
    }

    /// The path this process decided at boot, `None` before (or without) a
    /// boot that decides.
    pub fn decided() -> Option<&'static ServingPath> {
        DECIDED.get()
    }

    /// How `svrn daemon status` names the path: `serve`, or
    /// `serve (this process)`.
    pub fn status_line(&self) -> String {
        match self {
            Self::DialsServe => "serve".to_string(),
            Self::Hosted => "serve (this process)".to_string(),
        }
    }
}

/// The provider cell a hosted serve's routes answer from.
pub type HostedCell = std::sync::Arc<sovereign_contracts::reloadable_provider::ReloadableProvider>;

/// Pushes svrn's slot-alias map into the router that ranks this node's turns.
pub type SlotAliasSink =
    std::sync::Arc<dyn Fn(std::collections::HashMap<String, String>) + Send + Sync>;

/// What hosting serve in this process hands svrn: the cell every route
/// answers from, and the distribution over serve's engine and slot (the warm
/// orchestrator, the self-manifest refresh, RPC-worker discovery), which svrn
/// starts once its mesh is up (pb-serve-distributes).
pub struct HostedParts {
    pub cell: HostedCell,
    pub distribute: StartMesh,
}

/// How the distribution starts serve's distribution over this daemon's mesh:
/// the composition root builds the mesh ports from the daemon it is handed
/// (pb-serve-ranks-discovery), so svrn names neither the ports nor the
/// discovery they carry. svrn calls it once, with its mesh router.
pub type StartMesh = Box<
    dyn FnOnce(
            std::sync::Arc<crate::EmbeddedDaemon>,
            std::sync::Arc<sovereign_serving_host::peer_inference::InferenceRouter>,
        ) + Send,
>;

type Compose = Box<
    dyn FnOnce(
            std::path::PathBuf,
            std::path::PathBuf,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<HostedParts, String>> + Send>,
        > + Send,
>;

/// The composition a distribution hands svrn's process entry (phase-b-29 Q1):
/// how to host serve in this process, and serve's tracing targets. svrn calls
/// `compose` at most once, from `boot_serving`, and only where
/// [`ServingPath::decide`] says [`ServingPath::Hosted`]; that path builds no
/// engine of its own, so the process never holds two.
pub struct HostedServe {
    filter: String,
    compose: Compose,
    env_contract: Option<EnvContract>,
    ner: Option<NerSource>,
    rpc_warmer: Option<std::sync::Arc<dyn sovereign_contracts::rpc_warm::RpcShardWarmer>>,
    rpc_workers: Option<crate::mesh_http::RpcWorkerRows>,
}

/// The distribution's in-process NER kind: the one handle per process, loaded
/// on first ask (`sovereign_compute::ner::served_ner`).
pub type NerSource = Box<
    dyn Fn() -> Option<std::sync::Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>>
        + Send
        + Sync,
>;

/// The loader's RPC env contract: the `--rpc-worker` flag and the
/// `[shared_model]` role, translated into the env its consumers read
/// (`sovereign_compute::distributed_role`, pb-serve-distributes).
type EnvContract =
    Box<dyn Fn(&[String], &sovereign_contracts::setup_config::SharedModelSection) + Send + Sync>;

impl HostedServe {
    /// `filter` is serve's tracing allowlist, unioned with svrn's. `compose`
    /// gets the data root and the config path svrn booted on, and assembles
    /// serve, binds its router on serve's port and returns its parts; an `Err`
    /// names why, and refuses boot.
    pub fn new<F, Fut>(filter: String, compose: F) -> Self
    where
        F: FnOnce(std::path::PathBuf, std::path::PathBuf) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<HostedParts, String>> + Send + 'static,
    {
        Self {
            filter,
            compose: Box::new(move |data_dir, config_path| {
                Box::pin(compose(data_dir, config_path))
            }),
            env_contract: None,
            ner: None,
            rpc_warmer: None,
            rpc_workers: None,
        }
    }

    /// The RPC-worker rows svrn's `/v1/mesh/status` reports: serve's
    /// eligibility view in this process (pb-serve-ranks-discovery).
    pub fn rpc_workers<F>(mut self, rows: F) -> Self
    where
        F: Fn() -> Vec<serde_json::Value> + Send + Sync + 'static,
    {
        self.rpc_workers = Some(std::sync::Arc::new(rows));
        self
    }

    /// The RPC-worker rows this distribution handed, if any.
    pub fn rpc_worker_rows(&self) -> Option<crate::mesh_http::RpcWorkerRows> {
        self.rpc_workers.clone()
    }

    /// The loader's worker-side warmer, which svrn's `/internal/rpc-warm`
    /// hands each warm request (with the reach it resolves from its mesh)
    /// until the flip gives the route to serve.
    pub fn rpc_warmer(
        mut self,
        warmer: std::sync::Arc<dyn sovereign_contracts::rpc_warm::RpcShardWarmer>,
    ) -> Self {
        self.rpc_warmer = Some(warmer);
        self
    }

    /// The warmer this distribution handed, if any.
    pub fn warmer(
        &self,
    ) -> Option<std::sync::Arc<dyn sovereign_contracts::rpc_warm::RpcShardWarmer>> {
        self.rpc_warmer.clone()
    }

    /// The distribution's in-process NER kind, which svrn's boot takes its
    /// handle from wherever this process loads for itself: the hosted path,
    /// where serve's `/v1/ner` reads the same handle, and a terminal.
    pub fn ner<F>(mut self, handle: F) -> Self
    where
        F: Fn() -> Option<std::sync::Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>>
            + Send
            + Sync
            + 'static,
    {
        self.ner = Some(Box::new(handle));
        self
    }

    /// The in-process NER kind, taken before `compose` consumes this value, so
    /// the hosted path asks it after serve is assembled.
    pub fn take_ner(&mut self) -> Option<NerSource> {
        self.ner.take()
    }

    /// The in-process NER handle, `None` when this distribution handed no
    /// kind or the kind has no model installed.
    pub fn ner_handle(
        &self,
    ) -> Option<std::sync::Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>> {
        self.ner.as_ref().and_then(|handle| handle())
    }

    /// How the loader translates this invocation's `--rpc-worker` flag and
    /// the config's `[shared_model]` role into its env contract. svrn applies
    /// it once at boot, on every path, before anything reads that env: the
    /// hosted engine's worker bind and discovery, and svrn's own router and
    /// `/status` in the same process.
    pub fn env_contract<F>(mut self, apply: F) -> Self
    where
        F: Fn(&[String], &sovereign_contracts::setup_config::SharedModelSection)
            + Send
            + Sync
            + 'static,
    {
        self.env_contract = Some(Box::new(apply));
        self
    }

    /// Apply the loader's env contract, if this distribution handed one.
    /// `false` when it did not, so the caller names the absence.
    pub fn apply_env_contract(
        &self,
        args: &[String],
        shared_model: &sovereign_contracts::setup_config::SharedModelSection,
    ) -> bool {
        match &self.env_contract {
            Some(apply) => {
                apply(args, shared_model);
                true
            }
            None => false,
        }
    }

    /// serve's tracing allowlist.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Run the composition.
    pub async fn compose(
        self,
        data_dir: std::path::PathBuf,
        config_path: std::path::PathBuf,
    ) -> Result<HostedParts, String> {
        (self.compose)(data_dir, config_path).await
    }
}

/// Serve's default base and self-report reader, at their historical paths
/// (moved to the turn-client leaf at pb-meshapp-rest).
pub use sovereign_turn_client::serve_self::{default_serve_base, read_served_self};

/// Where a [`ServeBase`] came from. The switch chooses the terminal arm's
/// loopback mode from this, never by comparing the base to the default or
/// re-reading `[node]` (seat, reviewing d66686a89).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeBaseSource {
    /// [`default_serve_base`]: the serve on this host.
    Default,
    /// `[node] entry`, an address the operator named.
    NodeEntry,
}

/// The base this daemon dials `serve` at, without `/v1`, and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeBase {
    pub base: String,
    pub source: ServeBaseSource,
}

/// `[node] entry` when it is set, else [`default_serve_base`]. THE one
/// reader of "where is serve".
///
/// An identity binding (`[node] entry_node`) names a mesh peer, not a
/// process on this host, so it never names `serve`; it answers the default
/// and says so.
pub fn resolve_serve_base(node: &NodeSection) -> ServeBase {
    let resolved = match node.binding() {
        Some(EntryBinding::Address(url)) => ServeBase {
            base: url
                .trim_end_matches('/')
                .trim_end_matches("/v1")
                .to_string(),
            source: ServeBaseSource::NodeEntry,
        },
        Some(EntryBinding::Node(id)) => {
            tracing::debug!(entry_node = %id, "an identity binding names a peer, not serve");
            ServeBase {
                base: default_serve_base(),
                source: ServeBaseSource::Default,
            }
        }
        None => ServeBase {
            base: default_serve_base(),
            source: ServeBaseSource::Default,
        },
    };
    tracing::debug!(serve_base = %resolved.base, source = ?resolved.source, "serve base resolved");
    resolved
}

/// What reading serve's engine state found. The absences stay apart
/// (principle 6): "not observed yet" is an [`EngineStateRead::Answered`]
/// whose `device_memory` is `None`, and it is never the same answer as a
/// serve that is not there or one that did not answer in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStateRead {
    /// serve answered its cached view.
    Answered(sovereign_contracts::engine_state::EngineState),
    /// Nothing answered at the base, or what answered refused or was unreadable.
    Unreachable(String),
    /// serve did not answer within the bound.
    DidNotAnswerInTime,
}

/// Read serve's engine state, bounded by the bound every serving-host probe
/// already uses (`sovereign_turn_client::reach::PROBE_TIMEOUT`, 2 s). A
/// status endpoint must not block on another process, and a cached view on
/// serve's side does not bound the dial to it (the rule `/v1/mesh/status`
/// minted on 2026-07-30).
pub async fn read_engine_state(base: &str) -> EngineStateRead {
    let bound = sovereign_turn_client::reach::PROBE_TIMEOUT;
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        sovereign_contracts::engine_state::ENGINE_STATE_PATH
    );
    let read = async {
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("serve at {base} is not reachable: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "serve at {base} refused its engine state: HTTP {}",
                resp.status()
            ));
        }
        resp.json::<sovereign_contracts::engine_state::EngineState>()
            .await
            .map_err(|e| format!("serve at {base} answered an unreadable engine state: {e}"))
    };
    let outcome = match tokio::time::timeout(bound, read).await {
        Ok(Ok(state)) => EngineStateRead::Answered(state),
        Ok(Err(why)) => EngineStateRead::Unreachable(why),
        Err(_) => EngineStateRead::DidNotAnswerInTime,
    };
    tracing::debug!(serve_base = base, bound_ms = bound.as_millis() as u64, outcome = ?outcome, "engine state read from serve");
    outcome
}

/// How long boot waits for serve to answer. The daemon used to load its
/// models inline at boot with no bound at all, so waiting on serve's load here
/// keeps that timing; ten minutes covers a cold load of the largest
/// single-node model this workspace runs, and past it the absence is named
/// rather than waited on forever.
pub const SERVE_BRING_UP_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// Wait up to `window` for serve to answer at `serve`, and name the absence
/// when it never does. svrn brings nothing up (phase-b-29 Q2): the stock
/// install hosts serve in its own process (`sovereign-stock`,
/// [`ServingPath::Hosted`]), and a standalone or remote serve is started by
/// whoever runs it. The wait is [`ServingHost::ensure_reachable`]'s, with no
/// backend to bring up.
///
/// [`ServingHost::ensure_reachable`]: sovereign_turn_client::reach::ServingHost::ensure_reachable
pub async fn ensure_serve(
    serve: &ServeBase,
    window: std::time::Duration,
) -> Result<sovereign_turn_client::reach::Reached, String> {
    let reached = sovereign_turn_client::reach::ServingHost::at(serve.base.as_str())
        .ensure_reachable(window)
        .await
        .map_err(|e| e.to_string());
    match &reached {
        Ok(r) => {
            tracing::info!(target: "serving_path", serve_base = %serve.base, reached = ?r, "serve is reachable")
        }
        Err(e) => {
            tracing::warn!(target: "serving_path", serve_base = %serve.base, reason = %e, "serve is not reachable; svrn starts no serve")
        }
    }
    reached
}

/// The slot-alias map on the dialing path: serve's residency, role to model
/// id, through the one alias policy (`venue::resolution_alias_keys`) that
/// `register_local_model_slots` applies to `[models]` where no boot decided.
/// So the map names only models serve holds, never svrn's reading of serve's
/// sections (seat, reviewing c0c39be03).
pub fn served_slot_aliases(
    slots: &[sovereign_contracts::oicp::ResidentSlot],
) -> std::collections::HashMap<String, String> {
    slots
        .iter()
        .flat_map(|slot| {
            sovereign_contracts::venue::resolution_alias_keys(&slot.role)
                .into_iter()
                .map(move |key| (key, slot.model_id.clone()))
        })
        .collect()
}

/// How long [`resolve_serve_ner`] waits between probes that erred.
const NER_PROBE_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// Ask serve's NER route which extractor answers, retrying a probe that errs
/// until `window` has passed. `Ok(None)` only when serve answered that it
/// has no NER model; an Err when it never answered.
pub async fn resolve_serve_ner(
    base: &str,
    window: std::time::Duration,
) -> Result<Option<std::sync::Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>>, String> {
    use oicp_client::RemoteNer;
    let deadline = std::time::Instant::now() + window;
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match RemoteNer::connect(base).await {
            Ok(Some(remote)) => {
                tracing::info!(target: "serving_path", serve_base = base, attempts, model_id = %sovereign_contracts::ner::LabeledEntityExtractor::model_id(&remote), "NER is serve's; this daemon dials it");
                return Ok(Some(std::sync::Arc::new(remote)));
            }
            Ok(None) => {
                tracing::info!(target: "serving_path", serve_base = base, attempts, "serve has no NER model installed; no entity extractor this process");
                return Ok(None);
            }
            Err(e) if std::time::Instant::now() + NER_PROBE_RETRY < deadline => {
                tracing::debug!(target: "serving_path", serve_base = base, attempts, error = %e, "serve's NER route did not answer; retrying");
                tokio::time::sleep(NER_PROBE_RETRY).await;
            }
            Err(e) => {
                tracing::warn!(target: "serving_path", serve_base = base, attempts, error = %e, "serve's NER route did not answer within the bring-up's bound");
                return Err(format!(
                    "serve's NER route did not answer within {window:?} ({attempts} probes): {e}"
                ));
            }
        }
    }
}

/// The terminal arm in its loopback mode, dialing serve at `serve`: chat and
/// embeddings go to serve's `/v1`, and this node's model facts are serve's
/// (`SplitInferenceProvider::with_served`). The query-instruction prefix is
/// `embed_family.default_quirks().embed`, the decider the engine applies in
/// process, so a query embedded over the wire is prepared the same way.
///
/// The loopback mode is taken from the base's SOURCE, never from comparing
/// addresses: the default base is this host's serve, whose models are this
/// node's; an operator-named `[node] entry` is someone else's process, so its
/// provider keeps the terminal arm's empty manifest (build/inference.rs:72-75).
///
/// `config_context` is used only when serve's provider reports no context
/// window (the model-free mock engine), and the substitution is traced.
pub fn loopback_provider(
    serve: &ServeBase,
    served: sovereign_contracts::engine_state::ServedSelf,
    config_context: u32,
) -> oicp_client::SplitInferenceProvider {
    let query_instruction = served
        .embed_family
        .default_quirks()
        .embed
        .map(|q| q.query_instruction)
        .unwrap_or_default();
    let context = match served.context_size {
        Some(n) => n,
        None => {
            tracing::info!(target: "serving_path", config_context, "serve reports no context window; the config's is used");
            config_context
        }
    };
    let provider = oicp_client::SplitInferenceProvider::new(
        &format!("{}/v1", serve.base),
        "primary".to_string(),
        served.embed_model.clone(),
        context,
        query_instruction,
    );
    match serve.source {
        ServeBaseSource::Default => provider.with_served(served),
        ServeBaseSource::NodeEntry => {
            tracing::info!(target: "serving_path", serve_base = %serve.base, "serve at [node] entry: not this node's models, so the provider advertises none");
            provider
        }
    }
}

/// The dialing path's reload: serve rebuilds through its own ReloadFactory,
/// then the loopback provider is rebuilt from serve's new self-report and
/// stored into `cell`, the one boot wrapped, so every reader that holds it
/// (both routers, `AppState`'s adapter, the runtime) sees what serve holds
/// now (phase-b-28). Returns the self-report for the alias map.
pub async fn reload_through_serve(
    serve: &ServeBase,
    cell: &sovereign_contracts::reloadable_provider::ReloadableProvider,
    config_context: u32,
) -> Result<sovereign_contracts::engine_state::ServedSelf, String> {
    let reloaded = forward_reload(&serve.base).await?;
    let served = read_served_self(&serve.base)
        .await
        .map_err(|e| format!("reload: serve reloaded, then {e}"))?;
    tracing::info!(target: "serving_path", serve_base = %serve.base, resident = ?reloaded.resident_models, primary = %served.primary_model, "reload: serve rebuilt; the loopback provider in the boot cell is rebuilt from its self-report");
    cell.swap(
        std::sync::Arc::new(loopback_provider(serve, served.clone(), config_context)),
        served.embed_family.clone(),
    );
    Ok(served)
}

/// How long a forwarded read waits on serve: the setup reads detect hardware
/// on a blocking thread, which takes well under this.
const FORWARD_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// One GET forwarded to serve, its status and body relayed (see [`forward`]).
pub async fn forward_get(
    base: &str,
    path: &str,
) -> Result<(axum::http::StatusCode, Vec<u8>), String> {
    forward(base, axum::http::Method::GET, path, None).await
}

/// One GET forwarded to serve with its body STREAMED back — a model file can
/// be tens of GB — and `pass` request headers sent through (a byte range).
/// serve's status and the response headers a fetcher reads (length, range,
/// the integrity digest) are relayed. Unbounded by [`FORWARD_WINDOW`]: only the
/// connect is bounded, since a whole-GGUF body takes as long as it takes. An
/// unreachable serve is an Err naming it.
pub async fn forward_stream(
    base: &str,
    path: &str,
    pass: &axum::http::HeaderMap,
) -> Result<axum::response::Response, String> {
    use axum::response::IntoResponse;
    const RELAYED: [&str; 5] = [
        "content-type",
        "content-length",
        "content-range",
        "accept-ranges",
        "x-sha256",
    ];
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .connect_timeout(FORWARD_WINDOW)
        .build()
        .map_err(|e| format!("cannot build a client to reach serve: {e}"))?;
    let mut request = client.get(&url);
    if let Some(range) = pass.get(axum::http::header::RANGE) {
        request = request.header(reqwest::header::RANGE, range.as_bytes());
    }
    let resp = request
        .send()
        .await
        .map_err(|e| format!("serve at {base} is not reachable for {path}: {e}"))?;
    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    let mut headers = axum::http::HeaderMap::new();
    for name in RELAYED {
        if let Some(value) = resp.headers().get(name) {
            if let Ok(value) = axum::http::HeaderValue::from_bytes(value.as_bytes()) {
                headers.insert(axum::http::HeaderName::from_static(name), value);
            }
        }
    }
    tracing::debug!(target: "serving_path", serve_base = base, path, status = status.as_u16(), "streamed request forwarded to serve");
    let body = axum::body::Body::from_stream(resp.bytes_stream());
    Ok((status, headers, body).into_response())
}

/// One request forwarded to serve, its status and body relayed; `body` goes
/// as JSON when present. An unreachable serve, one past [`FORWARD_WINDOW`],
/// or an unreadable body is an Err naming which, never a success-shaped
/// answer.
pub async fn forward(
    base: &str,
    method: axum::http::Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<(axum::http::StatusCode, Vec<u8>), String> {
    forward_within(base, method, path, body, FORWARD_WINDOW).await
}

/// [`forward`] with its own window, for a request serve answers only after
/// work of its own (a worker's shard warm reads or fetches gigabytes).
pub async fn forward_within(
    base: &str,
    method: axum::http::Method,
    path: &str,
    body: Option<Vec<u8>>,
    window: std::time::Duration,
) -> Result<(axum::http::StatusCode, Vec<u8>), String> {
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|e| format!("{method} is not a method serve can be asked: {e}"))?;
    let mut request = reqwest::Client::new()
        .request(method.clone(), &url)
        .timeout(window);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let resp = request.send().await.map_err(|e| {
        if e.is_timeout() {
            format!("serve at {base} did not answer {path} within {window:?}")
        } else {
            format!("serve at {base} is not reachable for {path}: {e}")
        }
    })?;
    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("serve at {base} answered {path} unreadably: {e}"))?;
    tracing::debug!(target: "serving_path", serve_base = base, %method, path, status = status.as_u16(), "request forwarded to serve");
    Ok((status, body.to_vec()))
}

/// Forward a reload to serve, which rebuilds through its own ReloadFactory.
/// Unbounded by a probe timeout, because a reload loads models; an
/// unreachable serve or a refused rebuild is an Err naming it, never a
/// success-shaped reload.
pub async fn forward_reload(
    base: &str,
) -> Result<sovereign_contracts::engine_state::EngineReloaded, String> {
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        sovereign_contracts::engine_state::RELOAD_PATH
    );
    let resp = reqwest::Client::new()
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("reload: serve at {base} is not reachable: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "reload: serve at {base} refused (HTTP {status}): {body}"
        ));
    }
    let reloaded = resp
        .json::<sovereign_contracts::engine_state::EngineReloaded>()
        .await
        .map_err(|e| format!("reload: serve at {base} answered an unreadable reload: {e}"))?;
    tracing::info!(target: "serving_path", serve_base = base, resident = ?reloaded.resident_models, "reload forwarded to serve");
    Ok(reloaded)
}

#[cfg(test)]
#[path = "serve_client_tests.rs"]
mod tests;
