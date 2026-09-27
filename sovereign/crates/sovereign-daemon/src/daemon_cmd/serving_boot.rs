// SPDX-License-Identifier: AGPL-3.0-or-later
//! The serving half of `run_daemon`'s boot — the RPC env contract, llama.cpp's
//! log route, the VRAM preflight, the grammar env and the provider — split out
//! of `boot.rs` at its arch-gate ceiling (pb-svrn-dials-serve).

use std::sync::Arc;

use sovereign_core::model_family::ModelFamily;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

use crate::bootstrap;

/// What the serving boot hands the rest of `run_daemon`.
pub(super) struct ServingBoot {
    pub provider: Arc<dyn InferenceProvider>,
    pub engine_handle: Option<Arc<EmbeddedLlamaCpp>>,
    pub resolved_embed_family: ModelFamily,
    pub distributed_primary_slot: Option<Arc<sovereign_compute::manager::DynamicChildSlot>>,
    pub reload: crate::provider::ReloadSource,
    pub deferred_daemon: Arc<crate::DeferredDaemon>,
    /// Where serving lives: boot starts the in-process engine's subsystems
    /// (RPC discovery, the warm orchestrator) only on `InProcess`.
    pub path: crate::serve_client::ServingPath,
}

/// `Err(code)` is the exit code `run_daemon` returns.
pub(super) async fn boot_serving(
    config: &SetupConfig,
    args: &[String],
    config_override: &Option<std::path::PathBuf>,
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
    // (pb-svrn-dials-serve; `ServingPath::decide` traces it). A terminal
    // already holds no weights and dials its entry node, so it keeps that arm.
    let path = crate::serve_client::ServingPath::decide(config);
    let config_path_in_use = config_override
        .clone()
        .unwrap_or_else(sovereign_core::setup_config::SetupConfig::default_path);
    let deferred_daemon = Arc::new(crate::DeferredDaemon::new());
    if path == crate::serve_client::ServingPath::DialsServe
        && config.node_class() != sovereign_core::setup_config::NodeClass::Terminal
    {
        return dial_serve(config, &config_path_in_use, deferred_daemon, path).await;
    }

    // Route llama.cpp's internal log into our tracing layer. Without
    // this, gguf load failures and ggml backend diagnostics print to a
    // dropped stderr (the daemon's child-style stdio capture swallows
    // them) — the operator gets a bare "null result from llama cpp"
    // with no actionable detail. Installed exactly once per process.
    sovereign_inference::llama::install_log_tracing();

    // VRAM capacity preflight — ADVISORY by default: warns and starts
    // anyway on overcommit (so CPU-only / low-VRAM machines aren't
    // hard-blocked). Only refuses under SOVEREIGN_STRICT_VRAM_CHECK=1 or
    // when a model file is unreadable. Full rationale on
    // `build::preflight::check_vram`.
    // Name the config the operator actually passed, not the default one —
    // a `--config` start used to be told to edit a file it never read.
    if !crate::build::preflight::check_vram_reporting(&config, &config_path_in_use) {
        return Err(1);
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

    // Inference provider — load the embedded llama.cpp provider (3 GGUF
    // slots + extras/idle/rerank wiring); full rationale on
    // `crate::build::inference::load_provider`. `engine_handle` (concrete) feeds
    // the RPC-worker auto-reload path; `resolved_embed_family` feeds the
    // mesh embed-model advertisement.
    //
    // Minted HERE, before the provider, because a terminal's provider binds to
    // its entry node THROUGH this handle: the bind is a mesh identity, resolved
    // per turn, and the mesh view does not exist yet. `DeferredDaemon` answers
    // exactly as a commissioned-but-stopped daemon until `bind` — no peers — so
    // a terminal booting ahead of gossip reports its entry node unreachable
    // rather than inventing an address for it.
    let (provider, raw_engine, resolved_embed_family, distributed_primary_slot, reload_factory) =
        match crate::build::inference::load_provider(&config, Arc::clone(&deferred_daemon)) {
            Ok(t) => t,
            Err(()) => return Err(1),
        };
    // `None` whenever nothing in this process owns llama slots — TWO ways in
    // now, and the engine-only paths (RPC-worker auto-reload, slot hot-swap)
    // must see the absence rather than a stub either way:
    //   - a `terminal`, which holds no weights at all and forwards instead;
    //   - an engine configured with no local llama slots, where the
    //     RPC-worker reload below is llama's own and simply does not arm.
    // Already an `Option` before either existed; both make the `None` reachable.
    let engine_handle: Option<Arc<EmbeddedLlamaCpp>> = raw_engine;
    Ok(ServingBoot {
        provider,
        engine_handle,
        resolved_embed_family,
        distributed_primary_slot,
        reload: crate::provider::ReloadSource::Assembly(reload_factory),
        deferred_daemon,
        path,
    })
}

/// The dialing path: serve holds the weights, and this daemon builds no
/// engine. serve is brought up at this user-action moment only (daemon start),
/// never on a refused dial; it runs the llama log route and the VRAM
/// preflight on its own startup. A serve that cannot be reached or does not
/// report itself refuses boot by name, as a model that failed to load in
/// process refused it before.
async fn dial_serve(
    config: &SetupConfig,
    config_path: &std::path::Path,
    deferred_daemon: Arc<crate::DeferredDaemon>,
    path: crate::serve_client::ServingPath,
) -> Result<ServingBoot, i32> {
    let serve = crate::serve_client::resolve_serve_base(&config.node);
    if let Err(e) = crate::serve_client::ensure_serve(&serve, config_path).await {
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
    let config_context = config.effective_context_size();
    let resolved_embed_family = served.embed_family.clone();
    let provider: Arc<dyn InferenceProvider> = Arc::new(crate::serve_client::loopback_provider(
        &serve,
        served,
        config_context,
    ));
    tracing::info!(target: "serving_path", serve_base = %serve.base, source = ?serve.source, "boot: serving is serve's; this daemon holds no engine");
    Ok(ServingBoot {
        provider,
        engine_handle: None,
        resolved_embed_family,
        distributed_primary_slot: None,
        reload: crate::provider::ReloadSource::Serve {
            base: serve,
            config_context,
        },
        deferred_daemon,
        path,
    })
}
