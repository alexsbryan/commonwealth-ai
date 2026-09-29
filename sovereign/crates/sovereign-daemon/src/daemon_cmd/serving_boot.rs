// SPDX-License-Identifier: AGPL-3.0-or-later
//! The serving half of `run_daemon`'s boot — the RPC env contract, where
//! serving lives, and the provider (serve's, or a terminal's forwarder) — split
//! out of `boot.rs` at its arch-gate ceiling (pb-svrn-dials-serve).

use std::sync::Arc;

use sovereign_core::model_family::ModelFamily;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;

use crate::bootstrap;

/// What the serving boot hands the rest of `run_daemon`.
pub(super) struct ServingBoot {
    pub provider: Arc<dyn InferenceProvider>,
    pub resolved_embed_family: ModelFamily,
    /// The distribution over the engine a hosted serve loads (compute's
    /// `distribute`: warm orchestrator, self-manifest refresh, RPC-worker
    /// discovery), started once the mesh is up; `None` where no engine
    /// loads here (the dialing path, a terminal).
    pub distribute: Option<sovereign_serving_host::rpc_discovery::Distribute>,
    pub reload: crate::provider::ReloadSource,
    pub deferred_daemon: Arc<crate::DeferredDaemon>,
    pub path: crate::serve_client::ServingPath,
}

/// `Err(code)` is the exit code `run_daemon` returns.
pub(super) async fn boot_serving(
    config: &SetupConfig,
    args: &[String],
    config_override: &Option<std::path::PathBuf>,
    hosted: Option<crate::serve_client::HostedServe>,
) -> Result<ServingBoot, i32> {
    // Shared-model cluster role → RPC env contract. The desktop fleet
    // sets `[shared_model] role` instead of SOVEREIGN_RPC_* by hand;
    // translate it here, once, before any RPC consumer reads the env
    // (the inference serve call_once, the discovery loop below, and
    // commonwealth-api's /status advertise). An explicit env var wins.
    // `--rpc-worker` first: it is the operator saying it out loud on this
    // invocation, and the role translation below only fills in what is unset.
    bootstrap::apply_rpc_worker_flag(args);
    bootstrap::apply_shared_model_role_to_env(&config.shared_model);

    // Where serving lives, decided once, after the RPC env contract above
    // (`ServingPath::decide` traces it). A distribution that hosts serve here
    // (`hosted`) turns the dialing path into the hosted one; the decider stays
    // the one reader (pb-stock-binary). Every node but a terminal serves from
    // serve (pb-serve-distributes).
    let path = crate::serve_client::ServingPath::decide(config, hosted.is_some());
    let config_path_in_use = config_override
        .clone()
        .unwrap_or_else(sovereign_core::setup_config::SetupConfig::default_path);
    let deferred_daemon = Arc::new(crate::DeferredDaemon::new());
    if config.node_class() != sovereign_core::setup_config::NodeClass::Terminal {
        return match hosted {
            Some(hosted) if path == crate::serve_client::ServingPath::Hosted => {
                host_serve(config, &config_path_in_use, hosted, deferred_daemon, path).await
            }
            _ => dial_serve(config, deferred_daemon, path).await,
        };
    }
    if hosted.is_some() {
        tracing::info!(target: "serving_path", serving = %path.status_line(), "boot: a terminal forwards to its entry node; the distribution's serve composition is not run");
    }

    // ── Force-tool-calls config → process env ─────────────────────
    //
    // The inference adapter reads `SOVEREIGN_FORCE_TOOL_CALLS` per
    // request to decide whether to upgrade `tool_choice="auto"` to
    // `"required"` (which engages the JSON-Schema tool-envelope
    // grammar). When the operator sets `[daemon] force_tool_calls =
    // true` in setup_config.toml, we propagate that into the process
    // env at boot so the existing per-request lookup picks it up.
    // Caller-supplied env wins — `std::env::set_var` only overrides
    // when nothing was set on the CLI invocation. Operators who want
    // a one-shot test (`SOVEREIGN_FORCE_TOOL_CALLS=0 svrn daemon
    // run`) can still do so without editing the config file.
    if config.daemon.force_tool_calls && std::env::var("SOVEREIGN_FORCE_TOOL_CALLS").is_err() {
        std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
        tracing::info!(
            "daemon: force_tool_calls=true — grammar engaged on every \
             tools-using request (set via setup_config.toml)"
        );
    }

    // ── Alternation-grammar config → process env ──────────────────
    //
    // Same propagation pattern as force_tool_calls. The inference
    // adapter reads `SOVEREIGN_ALTERNATION_GRAMMAR` per request to
    // route tool-envelope requests through llguidance's canonical
    // `TopLevelGrammar::from_json_schema` path instead of the
    // in-house `JsonConstraint` mask. Caller-supplied env wins so
    // operators can A/B test (`SOVEREIGN_ALTERNATION_GRAMMAR=0
    // svrn daemon run` ignores the config).
    //
    // launchd-spawned daemons don't inherit caller env, so flipping
    // this in setup_config.toml is the load-bearing path on macOS
    // hosts running the daemon via `svrn daemon start`.
    if config.daemon.alternation_grammar && std::env::var("SOVEREIGN_ALTERNATION_GRAMMAR").is_err()
    {
        std::env::set_var("SOVEREIGN_ALTERNATION_GRAMMAR", "1");
        tracing::info!(
            "daemon: alternation_grammar=true — llguidance schema path \
             engaged on tools-using requests (set via setup_config.toml)"
        );
    }

    // A terminal holds no weights: its provider forwards to its entry node;
    // full rationale on `crate::build::inference::terminal_provider`.
    //
    // Minted HERE, before the provider, because a terminal's provider binds to
    // its entry node THROUGH this handle: the bind is a mesh identity, resolved
    // per turn, and the mesh view does not exist yet. `DeferredDaemon` answers
    // exactly as a commissioned-but-stopped daemon until `bind` — no peers — so
    // a terminal booting ahead of gossip reports its entry node unreachable
    // rather than inventing an address for it.
    let provider =
        match crate::build::inference::terminal_provider(config, Arc::clone(&deferred_daemon)) {
            Ok(p) => p,
            Err(()) => return Err(1),
        };
    Ok(ServingBoot {
        provider,
        resolved_embed_family: ModelFamily::Unknown,
        // No engine loads here, so nothing distributes.
        distribute: None,
        reload: crate::provider::ReloadSource::Terminal,
        deferred_daemon,
        path,
    })
}

/// The dialing path: serve holds the weights, and this daemon builds no
/// engine, and starts no serve: a standalone or remote serve is started by
/// whoever runs it (phase-b-29 Q2), and runs the llama log route and the VRAM
/// preflight on its own startup. A serve that cannot be reached or does not
/// report itself refuses boot by name, as a model that failed to load in
/// process refused it before.
async fn dial_serve(
    config: &SetupConfig,
    deferred_daemon: Arc<crate::DeferredDaemon>,
    path: crate::serve_client::ServingPath,
) -> Result<ServingBoot, i32> {
    let serve = crate::serve_client::resolve_serve_base(&config.node);
    if let Err(e) =
        crate::serve_client::ensure_serve(&serve, crate::serve_client::SERVE_BRING_UP_WINDOW).await
    {
        eprintln!("error: serve is not reachable at {}: {e}", serve.base);
        return Err(1);
    }
    let served = match crate::serve_client::read_served_self(&serve.base).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(1);
        }
    };
    if let Err(e) = crate::serve_client::install_serve_ner(&serve.base).await {
        eprintln!("error: {e}");
        return Err(1);
    }
    let config_context = config.effective_context_size();
    let resolved_embed_family = served.embed_family.clone();
    // One cell every reader shares, so a reload's rebuilt provider is seen by
    // both routers, `AppState`'s adapter and the runtime (phase-b-28).
    let cell = Arc::new(
        sovereign_contracts::reloadable_provider::ReloadableProvider::new(
            Arc::new(crate::serve_client::loopback_provider(
                &serve,
                served,
                config_context,
            )),
            resolved_embed_family.clone(),
        ),
    );
    let provider: Arc<dyn InferenceProvider> = Arc::clone(&cell) as Arc<_>;
    tracing::info!(target: "serving_path", serve_base = %serve.base, source = ?serve.source, "boot: serving is serve's; this daemon holds no engine");
    Ok(ServingBoot {
        provider,
        resolved_embed_family,
        distribute: None,
        reload: crate::provider::ReloadSource::Serve {
            base: serve,
            config_context,
            cell,
        },
        deferred_daemon,
        path,
    })
}

/// The hosted path (pb-stock-binary, phase-b-29 Q1): the distribution
/// assembles serve in this process and binds its router on serve's port, and
/// svrn holds the SAME cell serve's routes answer from, so one engine answers
/// both ports; the distribution over its engine comes back for svrn to start
/// with its mesh ports (pb-serve-distributes). Nothing is brought up, no self-report is read, no loopback
/// provider is built, and no NER handle is installed: that handle is one
/// process-global (`sovereign_compute::ner`) which serve's own `/v1/ner`
/// reads, so a `RemoteNer` here would dial itself. The first reader loads
/// through the kind registry serve's `bundles` registered.
async fn host_serve(
    config: &SetupConfig,
    config_path: &std::path::Path,
    hosted: crate::serve_client::HostedServe,
    deferred_daemon: Arc<crate::DeferredDaemon>,
    path: crate::serve_client::ServingPath,
) -> Result<ServingBoot, i32> {
    let crate::serve_client::HostedParts { cell, distribute } = match hosted
        .compose(config.data.dir.clone(), config_path.to_path_buf())
        .await
    {
        Ok(parts) => parts,
        Err(e) => {
            tracing::error!(target: "serving_path", error = %e, "boot: serve could not be hosted in this process");
            eprintln!("error: serve could not be hosted in this process: {e}");
            return Err(1);
        }
    };
    let resolved_embed_family = cell.embed_family();
    let provider: Arc<dyn InferenceProvider> = Arc::clone(&cell) as Arc<_>;
    tracing::info!(
        target: "serving_path",
        serve_base = %crate::serve_client::default_serve_base(),
        "boot: serving is serve's, hosted in this process; one engine answers both ports"
    );
    Ok(ServingBoot {
        provider,
        resolved_embed_family,
        distribute: Some(distribute),
        reload: crate::provider::ReloadSource::Hosted { cell },
        deferred_daemon,
        path,
    })
}
