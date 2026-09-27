// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reaching `serve`, the model server (FIVE_PROGRAMS §2), from the svrn
//! daemon (pb-svrn-dials-serve): where it listens, and whether this daemon
//! dials it at all.

use sovereign_contracts::launch::RpcServe;
use sovereign_contracts::setup_config::{EntryBinding, NodeSection, SetupConfig};

static DECIDED: std::sync::OnceLock<ServingPath> = std::sync::OnceLock::new();

/// Where this daemon's inference is served from — THE one decider, read once
/// at boot after `apply_shared_model_role_to_env`, so the env contract it
/// reads is the one bootstrap.rs already translated (phase-b-24).
///
/// Mesh-distributed inference (RPC-worker discovery, the distributed-primary
/// respawn, the auto-warm orchestrator, the worker role) still needs the
/// loading process beside the roster, so a config that opts into it keeps the
/// in-process path until pb-serve-distributes moves it into serve. Every
/// other config dials serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingPath {
    /// The in-process engine, kept for an opt-in to distributed inference.
    /// `chosen_by` names the input that chose it, because four inputs are
    /// not predictable from outside (principle 1).
    InProcess { chosen_by: &'static str },
    /// Serving lives in `serve`, which this daemon dials.
    DialsServe,
}

impl ServingPath {
    /// Decide from each input's ONE existing reader, never a fresh `var_os`
    /// (principle 8): the subsystems the in-process path keeps gate on the
    /// same readers, so they and this decider cannot disagree on a spelling.
    pub fn decide(config: &SetupConfig) -> Self {
        let path = Self::from_inputs(
            &RpcServe::from_env(),
            crate::startup::rpc_discovery_armed(),
            &sovereign_inference::embedded::rpc_workers_from_env(),
            sovereign_inference::engine_factory::child_owns_primary(config),
        );
        tracing::info!(
            target: "serving_path",
            serving = %path.status_line(),
            owner = "pb-serve-distributes moves the in-process path into serve",
            "serving path decided (pb-svrn-dials-serve)"
        );
        // Kept as decided: `/status` reports the path this process booted on,
        // which a later config edit does not change.
        let _ = DECIDED.set(path.clone());
        path
    }

    /// The path this process decided at boot, `None` before (or without) a
    /// boot that decides.
    pub fn decided() -> Option<&'static ServingPath> {
        DECIDED.get()
    }

    /// The decision over the four inputs. A `Refused` RPC bind keeps today's
    /// path, so its refusal is still reported where it is today; an EMPTY
    /// `SOVEREIGN_RPC_SERVE` is `Off` at its reader and dials serve.
    pub fn from_inputs(
        rpc_serve: &RpcServe,
        discovery_armed: bool,
        rpc_workers: &[String],
        child_owns_primary: bool,
    ) -> Self {
        let chosen_by = if !matches!(rpc_serve, RpcServe::Off) {
            "SOVEREIGN_RPC_SERVE"
        } else if discovery_armed {
            "SOVEREIGN_RPC_DISCOVER"
        } else if !rpc_workers.is_empty() {
            "SOVEREIGN_RPC_WORKERS"
        } else if child_owns_primary {
            "[compute] distributed_primary"
        } else {
            return Self::DialsServe;
        };
        Self::InProcess { chosen_by }
    }

    /// How `svrn daemon status` names the path: `in-process (<input>)`, or
    /// `serve`.
    pub fn status_line(&self) -> String {
        match self {
            Self::InProcess { chosen_by } => format!("in-process ({chosen_by})"),
            Self::DialsServe => "serve".to_string(),
        }
    }
}

/// The base this daemon dials `serve` at when nothing says otherwise:
/// loopback, on the one port serve listens on by default
/// (`sovereign_contracts::venue::serve_port`, serve's reader too).
pub fn default_serve_base() -> String {
    format!(
        "http://127.0.0.1:{}",
        sovereign_contracts::venue::serve_port()
    )
}

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

/// How long boot waits for serve to answer, its bring-up included. The daemon
/// used to load its models inline at boot with no bound at all, so waiting on
/// serve's load here keeps that timing; ten minutes covers a cold load of the
/// largest single-node model this workspace runs, and past it the absence is
/// named rather than waited on forever.
pub const SERVE_BRING_UP_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// Make serve reachable at `serve`, at a user-action moment only (daemon
/// boot); never on a refused dial (`bring_up_decider`, quality/ARCH_LAYERS.toml).
/// The decision is [`ServingHost::ensure_reachable`]'s, the same one
/// `rails_client::ensure_rails` uses for cw-rails.
///
/// Only the default base brings serve up: the serve on this host, from the
/// config file the daemon read (serve reads `<data-dir>/config.toml`). An
/// operator-named `[node] entry` is someone else's process, so it is probed
/// and never started.
///
/// [`ServingHost::ensure_reachable`]: sovereign_turn_client::reach::ServingHost::ensure_reachable
pub async fn ensure_serve(
    serve: &ServeBase,
    config_path: &std::path::Path,
) -> Result<sovereign_turn_client::reach::Reached, String> {
    use sovereign_turn_client::reach::{locate_sibling, BundledBackend, ServingHost};
    let host = ServingHost::at(serve.base.as_str());
    let host = match serve.source {
        ServeBaseSource::NodeEntry => host,
        ServeBaseSource::Default => {
            let data_dir = config_path
                .parent()
                .filter(|dir| SetupConfig::path_in(dir) == config_path)
                .ok_or_else(|| {
                    format!(
                        "serve reads <data-dir>/config.toml, and this daemon's config is {}; \
                         start serve by hand with `sovereign-serve --data-dir <dir>`",
                        config_path.display()
                    )
                })?;
            let bin =
                locate_sibling("sovereign-serve", "SOVEREIGN_SERVE_BIN").ok_or_else(|| {
                    "no sovereign-serve binary: set SOVEREIGN_SERVE_BIN, or install it beside \
                 this program or on PATH"
                        .to_string()
                })?;
            host.bringing_up(
                BundledBackend::at(bin)
                    .arg("--data-dir")
                    .arg(data_dir.display().to_string())
                    .arg("--listen")
                    .arg(format!(
                        "127.0.0.1:{}",
                        sovereign_contracts::venue::serve_port()
                    ))
                    .log_to(data_dir.join("serve.log")),
            )
        }
    };
    let reached = host
        .ensure_reachable(SERVE_BRING_UP_WINDOW)
        .await
        .map_err(|e| e.to_string());
    match &reached {
        Ok(r) => {
            tracing::info!(target: "serving_path", serve_base = %serve.base, reached = ?r, "serve is reachable");
            record_bring_up(r, &crate::startup::serve_pid_path());
        }
        Err(e) => {
            tracing::warn!(target: "serving_path", serve_base = %serve.base, reason = %e, "serve is not reachable")
        }
    }
    reached
}

/// The `serve` this daemon brought up: its pid, and the loopback port it was
/// told to listen on. The port locates serve; the pid identifies it
/// (principle 8), so `svrn daemon stop` signals only a process this daemon
/// started, and only while it still listens where it was brought up. A serve
/// found already serving has no record and is never this daemon's to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeRecord {
    pub pid: u32,
    pub port: u16,
}

impl ServeRecord {
    /// `<pid> <port>`, one line.
    pub fn write_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        std::fs::write(path, format!("{} {}\n", self.pid, self.port))
    }

    /// The record at `path`: `Ok(None)` when there is none, an Err naming a
    /// record that exists and does not read, never a guessed pid.
    pub fn read_from(path: &std::path::Path) -> Result<Option<Self>, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut fields = raw.split_whitespace();
        match (
            fields.next().and_then(|p| p.parse().ok()),
            fields.next().and_then(|p| p.parse().ok()),
        ) {
            (Some(pid), Some(port)) => Ok(Some(Self { pid, port })),
            _ => Err(format!("{}: not `<pid> <port>`: {raw:?}", path.display())),
        }
    }
}

/// On a bring-up, record it where the stop reads it; a serve found already
/// serving clears any record, since a pid recorded before is not the one
/// answering now unless this daemon started it, and a stale record would
/// name a process the stop must not touch.
fn record_bring_up(reached: &sovereign_turn_client::reach::Reached, path: &std::path::Path) {
    use sovereign_turn_client::reach::Reached;
    match reached {
        Reached::BroughtUp { pid, .. } => {
            let record = ServeRecord {
                pid: *pid,
                port: sovereign_contracts::venue::serve_port(),
            };
            match record.write_to(path) {
                Ok(()) => {
                    tracing::info!(target: "serving_path", pid, port = record.port, path = %path.display(), "serve brought up by this daemon; recorded for daemon stop")
                }
                Err(e) => {
                    tracing::warn!(target: "serving_path", pid, path = %path.display(), error = %e, "serve brought up, but its record did not write; daemon stop will leave it running")
                }
            }
        }
        Reached::AlreadyServing { .. } => {
            let _ = std::fs::remove_file(path);
            tracing::info!(target: "serving_path", "serve was already serving; not this daemon's, so daemon stop leaves it running");
        }
    }
}

/// Read serve's self-report, bounded like every other status read of serve
/// (`sovereign_turn_client::reach::PROBE_TIMEOUT`).
pub async fn read_served_self(
    base: &str,
) -> Result<sovereign_contracts::engine_state::ServedSelf, String> {
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        sovereign_contracts::engine_state::SERVED_SELF_PATH
    );
    let read = async {
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("serve at {base} is not reachable: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "serve at {base} refused its self-report: HTTP {}",
                resp.status()
            ));
        }
        resp.json::<sovereign_contracts::engine_state::ServedSelf>()
            .await
            .map_err(|e| format!("serve at {base} answered an unreadable self-report: {e}"))
    };
    match tokio::time::timeout(sovereign_turn_client::reach::PROBE_TIMEOUT, read).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "serve at {base} did not answer its self-report within {:?}",
            sovereign_turn_client::reach::PROBE_TIMEOUT
        )),
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
) -> sovereign_inference::remote::SplitInferenceProvider {
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
    let provider = sovereign_inference::remote::SplitInferenceProvider::new(
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

/// How long a forwarded read waits on serve: the setup reads detect hardware
/// on a blocking thread, which takes well under this.
const FORWARD_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// One GET forwarded to serve, its status and body relayed. An unreachable
/// serve, one past [`FORWARD_WINDOW`], or an unreadable body is an Err naming
/// which, never a success-shaped answer.
pub async fn forward_get(
    base: &str,
    path: &str,
) -> Result<(axum::http::StatusCode, Vec<u8>), String> {
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(FORWARD_WINDOW)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                format!("serve at {base} did not answer {path} within {FORWARD_WINDOW:?}")
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
    tracing::debug!(target: "serving_path", serve_base = base, path, status = status.as_u16(), "read forwarded to serve");
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
mod tests {
    use super::*;

    fn node(entry: Option<&str>, entry_node: Option<&str>) -> NodeSection {
        NodeSection {
            entry: entry.map(str::to_string),
            entry_node: entry_node.map(str::to_string),
            ..NodeSection::default()
        }
    }

    fn decide(serve: Option<&str>, discover: bool, workers: &[&str], primary: bool) -> ServingPath {
        let workers: Vec<String> = workers.iter().map(|w| w.to_string()).collect();
        ServingPath::from_inputs(
            &RpcServe::resolve(serve, false),
            discover,
            &workers,
            primary,
        )
    }

    #[test]
    fn a_default_config_dials_serve() {
        assert_eq!(decide(None, false, &[], false), ServingPath::DialsServe);
    }

    #[test]
    fn an_empty_rpc_serve_is_off_and_dials_serve() {
        assert_eq!(decide(Some(""), false, &[], false), ServingPath::DialsServe);
    }

    #[test]
    fn each_opt_in_keeps_the_in_process_path_and_names_itself() {
        let cases = [
            (
                decide(Some("127.0.0.1:50052"), false, &[], false),
                "SOVEREIGN_RPC_SERVE",
            ),
            // A refused plaintext-LAN bind keeps today's path and its refusal.
            (
                decide(Some("0.0.0.0:50052"), false, &[], false),
                "SOVEREIGN_RPC_SERVE",
            ),
            (decide(None, true, &[], false), "SOVEREIGN_RPC_DISCOVER"),
            (
                decide(None, false, &["10.0.0.2:50052"], false),
                "SOVEREIGN_RPC_WORKERS",
            ),
            (
                decide(None, false, &[], true),
                "[compute] distributed_primary",
            ),
        ];
        for (path, input) in cases {
            assert_eq!(path, ServingPath::InProcess { chosen_by: input });
            assert_eq!(path.status_line(), format!("in-process ({input})"));
        }
    }

    /// A stub serve on a free loopback port whose engine-state route waits
    /// `hold` before it answers the empty view.
    async fn stub_serve(hold: std::time::Duration) -> String {
        use axum::routing::get;
        let app = axum::Router::new().route(
            sovereign_contracts::engine_state::ENGINE_STATE_PATH,
            get(move || async move {
                tokio::time::sleep(hold).await;
                axum::Json(sovereign_contracts::engine_state::EngineState::default())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        base
    }

    #[tokio::test]
    async fn a_serve_that_holds_the_route_past_the_bound_is_named_not_waited_on() {
        let base = stub_serve(std::time::Duration::from_secs(30)).await;
        let started = std::time::Instant::now();
        let read = read_engine_state(&base).await;
        let took = started.elapsed();
        assert_eq!(read, EngineStateRead::DidNotAnswerInTime);
        assert!(
            took < sovereign_turn_client::reach::PROBE_TIMEOUT + std::time::Duration::from_secs(1),
            "the read waited {took:?}, past the bound plus 1 s"
        );
    }

    #[tokio::test]
    async fn a_serve_that_answers_is_read_and_not_observed_yet_stays_none() {
        let base = stub_serve(std::time::Duration::ZERO).await;
        match read_engine_state(&base).await {
            EngineStateRead::Answered(state) => assert_eq!(state.device_memory, None),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_serve_at_the_base_is_unreachable_not_empty() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(matches!(
            read_engine_state(&base).await,
            EngineStateRead::Unreachable(_)
        ));
    }

    async fn stub(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        base
    }

    fn served() -> sovereign_contracts::engine_state::ServedSelf {
        sovereign_contracts::engine_state::ServedSelf {
            primary_model: "big".into(),
            medium_model: "big".into(),
            fast_model: "small".into(),
            embed_model: "Qwen3-Embedding-0.6B-Q8_0".into(),
            embed_family: sovereign_contracts::model_family::ModelFamily::Qwen3Embedding,
            context_size: Some(8192),
            ..Default::default()
        }
    }

    /// The loopback mode follows the base's source: this host's serve answers
    /// for this node's models, an operator-named entry never does.
    #[test]
    fn only_the_default_base_answers_for_this_nodes_models() {
        use sovereign_contracts::{InferenceProvider, Speed};
        for (source, expected) in [
            (ServeBaseSource::Default, "big"),
            (ServeBaseSource::NodeEntry, "primary"),
        ] {
            let serve = ServeBase {
                base: "http://127.0.0.1:1".into(),
                source,
            };
            let provider = loopback_provider(&serve, served(), 4096);
            assert_eq!(provider.model_id_for(Speed::Slow), expected, "{source:?}");
        }
    }

    /// A query embedded through the loopback provider carries the same
    /// instruction prefix the engine applies in process; the terminal arm's
    /// empty prefix would embed it as a document.
    #[tokio::test]
    async fn the_loopback_provider_prepares_a_query_the_way_the_engine_does() {
        use axum::routing::post;
        use sovereign_contracts::InferenceProvider;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen_in = std::sync::Arc::clone(&seen);
        let base = stub(axum::Router::new().route(
            "/v1/embeddings",
            post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen_in = std::sync::Arc::clone(&seen_in);
                async move {
                    *seen_in.lock().unwrap() = body["input"].as_str().unwrap_or("").to_string();
                    axum::Json(serde_json::json!({
                        "object": "list", "model": "e",
                        "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}]
                    }))
                }
            }),
        ))
        .await;
        let serve = ServeBase {
            base,
            source: ServeBaseSource::Default,
        };
        let provider = loopback_provider(&serve, served(), 4096);
        provider
            .embed_query("who wrote it")
            .await
            .expect("embedded");
        let input = seen.lock().unwrap().clone();
        let expected = sovereign_contracts::model_family::ModelFamily::Qwen3Embedding
            .default_quirks()
            .embed
            .expect("quirks")
            .query_instruction;
        assert!(input.starts_with(&expected), "query sent as {input:?}");
        assert_eq!(
            provider.model_id_for(sovereign_contracts::Speed::Slow),
            "big"
        );
    }

    #[tokio::test]
    async fn the_self_report_is_read_from_serve() {
        use axum::routing::get;
        let base = stub(axum::Router::new().route(
            sovereign_contracts::engine_state::SERVED_SELF_PATH,
            get(|| async { axum::Json(served()) }),
        ))
        .await;
        let read = read_served_self(&base).await.expect("read");
        assert_eq!(read.primary_model, "big");
    }

    /// A reload serve refuses is an Err naming the refusal, never a
    /// success-shaped reload.
    #[tokio::test]
    async fn a_refused_reload_is_named() {
        use axum::routing::post;
        let base = stub(axum::Router::new().route(
            sovereign_contracts::engine_state::RELOAD_PATH,
            post(|| async {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "reload: the serving assembly refused: no such file",
                )
            }),
        ))
        .await;
        let err = forward_reload(&base).await.expect_err("refused");
        assert!(
            err.contains("HTTP 503") && err.contains("no such file"),
            "{err}"
        );
    }

    /// A forwarded read relays serve's status and body as sent, query and
    /// refusal included, so a setup read answers alike from either process.
    #[tokio::test]
    async fn a_forwarded_read_relays_serves_status_and_body() {
        use axum::extract::RawQuery;
        use axum::routing::get;
        let base = stub(axum::Router::new().route(
            "/v1/admin/setup/catalog",
            get(|RawQuery(q): RawQuery| async move {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("{{\"error\":\"{}\"}}", q.unwrap_or_default()),
                )
            }),
        ))
        .await;
        let (status, body) = forward_get(&base, "/v1/admin/setup/catalog?profile=nope")
            .await
            .expect("answered");
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body, br#"{"error":"profile=nope"}"#);
    }

    #[tokio::test]
    async fn a_forwarded_read_to_no_serve_is_named() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = forward_get(&base, "/v1/admin/hardware")
            .await
            .expect_err("no serve");
        assert!(
            err.contains("not reachable") && err.contains("/v1/admin/hardware"),
            "{err}"
        );
    }

    /// The bring-up writes the record the stop reads; none is `None`, and a
    /// record that does not read is named, never a guessed pid.
    #[test]
    fn the_serve_record_round_trips_and_a_bad_one_is_named() {
        use sovereign_turn_client::reach::Reached;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serve.pid");
        assert_eq!(ServeRecord::read_from(&path), Ok(None));
        record_bring_up(
            &Reached::BroughtUp {
                pid: 4242,
                ready_after: std::time::Duration::ZERO,
            },
            &path,
        );
        assert_eq!(
            ServeRecord::read_from(&path),
            Ok(Some(ServeRecord {
                pid: 4242,
                port: sovereign_contracts::venue::serve_port()
            }))
        );
        record_bring_up(
            &Reached::AlreadyServing {
                waited: std::time::Duration::ZERO,
            },
            &path,
        );
        assert_eq!(ServeRecord::read_from(&path), Ok(None));
        std::fs::write(&path, "serve\n").unwrap();
        assert!(ServeRecord::read_from(&path).is_err());
    }

    #[test]
    fn no_entry_dials_the_default_base() {
        let resolved = resolve_serve_base(&node(None, None));
        assert_eq!(resolved.base, "http://127.0.0.1:9748");
        assert_eq!(resolved.source, ServeBaseSource::Default);
    }

    #[test]
    fn an_address_entry_is_the_base_without_its_v1() {
        for entry in ["http://127.0.0.1:18748/v1", "http://127.0.0.1:18748/v1/"] {
            let resolved = resolve_serve_base(&node(Some(entry), None));
            assert_eq!(resolved.base, "http://127.0.0.1:18748");
            assert_eq!(resolved.source, ServeBaseSource::NodeEntry);
        }
    }

    #[test]
    fn an_identity_binding_never_names_serve() {
        let resolved = resolve_serve_base(&node(None, Some("ab12")));
        assert_eq!(resolved.base, default_serve_base());
        assert_eq!(resolved.source, ServeBaseSource::Default);
    }
}
