// SPDX-License-Identifier: AGPL-3.0-or-later
//! A node's shared-model role, translated into the RPC env contract the
//! loader's consumers read (discovery, the anchor eligibility profile, the
//! quorum and shard-fetch knobs). The worker bind (`SOVEREIGN_RPC_SERVE`) and
//! `--rpc-worker` were deleted with the mesh fabric (es-g4-delete-mesh-fabric). Moved
//! from the daemon's bootstrap (pb-serve-distributes): the process that
//! loads the engine applies it before it decides anything.

/// Translate `[shared_model] role` into the RPC env contract that the
/// three decoupled RPC consumers already read — the inference serve
/// (`serve_rpc_worker_if_configured`), this module's discovery loop, and
/// commonwealth-api's `/status` advertise. This is the desktop-friendly
/// source of the role (the app writes the config; no hand-set env vars);
/// an explicitly-set env var always wins, so CLI/power users are
/// unaffected. One traceable place where role → RPC wiring happens.
///
/// - `Host`   → discover peers' workers AND serve (a host also anchors).
/// - `Anchor` → serve this node's GPU into the layer-split.
/// - `Consumer` (default) → neither; the node only queries the shared
///   model (that routing is wired separately, not via these env vars).
pub fn apply_shared_model_role_to_env(cfg: &sovereign_contracts::setup_config::SharedModelSection) {
    use sovereign_contracts::setup_config::SharedModelRole;
    // The shared model this node routes its primary turns into (any role that
    // names one — consumers query it, anchors/host also serve it). Read by the
    // mesh inference provider via SOVEREIGN_SHARED_MODEL_ID.
    if let Some(id) = cfg.model_id.as_deref() {
        if !id.is_empty() && std::env::var_os("SOVEREIGN_SHARED_MODEL_ID").is_none() {
            std::env::set_var("SOVEREIGN_SHARED_MODEL_ID", id);
            tracing::info!(
                model_id = id,
                "shared-model: primary routes to SOVEREIGN_SHARED_MODEL_ID"
            );
        }
    }
    let serve = matches!(cfg.role, SharedModelRole::Anchor | SharedModelRole::Host);
    // Host failover: EVERY anchor spawns the discovery loop, not just the
    // statically-designated host — so any anchor can take over the host role the
    // instant it is elected leader (`partition::should_host`). The loop stays
    // dormant (it discovers + keeps worker eligibility warm but does NOT
    // distribute) until this node is the host. So "discover" now means
    // "participates in the host election", which every anchor does.
    let discover = serve;
    // Anchor-tier worker eligibility: a host treats its fellow anchors with the
    // stricter `EligibilityConfig::anchor` profile (slower settle, quarantine on
    // first flap), since a flapping anchor can GGML_ABORT the host mid-decode.
    // Set the env knobs the eligibility gate reads; an explicit env always wins.
    // Applied for any serving role so a future failover host (an anchor that
    // becomes leader) already carries the right profile.
    if serve {
        if std::env::var_os("SOVEREIGN_RPC_WORKER_SETTLE_SECS").is_none() {
            std::env::set_var(
                "SOVEREIGN_RPC_WORKER_SETTLE_SECS",
                sovereign_serving_host::worker_eligibility::ANCHOR_SETTLE_SECS.to_string(),
            );
        }
        if std::env::var_os("SOVEREIGN_RPC_WORKER_FLAP_THRESHOLD").is_none() {
            std::env::set_var(
                "SOVEREIGN_RPC_WORKER_FLAP_THRESHOLD",
                sovereign_serving_host::worker_eligibility::ANCHOR_FLAP_THRESHOLD.to_string(),
            );
        }
    }
    if discover && std::env::var_os("SOVEREIGN_RPC_DISCOVER").is_none() {
        std::env::set_var("SOVEREIGN_RPC_DISCOVER", "1");
        tracing::info!(role = ?cfg.role, "shared-model: anchor spawns the host-election discovery loop");
    }
    // The operator's optional designated-host pin. Published so every anchor's
    // `should_host` check honours it while it's an eligible anchor, and fails
    // over to election (min NodeId) when it drops out.
    if let Some(pin) = cfg.host_node_id.as_deref() {
        if !pin.is_empty() && std::env::var_os("SOVEREIGN_SHARED_MODEL_HOST_NODE_ID").is_none() {
            std::env::set_var("SOVEREIGN_SHARED_MODEL_HOST_NODE_ID", pin);
            tracing::info!(pin, "shared-model: designated-host pin published");
        }
    }
    // The host enforces the quorum + pooled-memory gate before distributing, so it
    // carries those knobs into the RPC env contract too (env wins if already set).
    if discover {
        if std::env::var_os("SOVEREIGN_RPC_QUORUM_ANCHORS").is_none() {
            std::env::set_var(
                "SOVEREIGN_RPC_QUORUM_ANCHORS",
                cfg.quorum_anchors.to_string(),
            );
        }
        if let Some(gb) = cfg.min_pooled_gb {
            if std::env::var_os("SOVEREIGN_RPC_MIN_POOLED_GB").is_none() {
                std::env::set_var("SOVEREIGN_RPC_MIN_POOLED_GB", gb.to_string());
            }
        }
        if let Some(h) = cfg.headroom {
            if std::env::var_os("SOVEREIGN_RPC_HEADROOM").is_none() {
                std::env::set_var("SOVEREIGN_RPC_HEADROOM", h.to_string());
            }
        }
        // Shard-fetch mode (host-side orchestrator reads this). The fleet
        // default is `ranges` — each anchor pulls only its slice, the only way
        // a model bigger than one node's disk distributes. Set for any serving
        // role so a failover host already carries it. Env wins if pre-set.
        if std::env::var_os("SOVEREIGN_RPC_SHARD_FETCH").is_none() {
            std::env::set_var("SOVEREIGN_RPC_SHARD_FETCH", cfg.shard_fetch.as_env());
        }
    }
}
