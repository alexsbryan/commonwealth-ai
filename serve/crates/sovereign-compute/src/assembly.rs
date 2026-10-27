// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one serving assembly: config in, the provider a serving process
//! installs out.
//!
//! It owns everything between "this node holds weights" and "here is the
//! provider": the engine choice (`crate::engine_factory::build_engine`, the
//! registry), the router, and the compute-child layer. A terminal never reaches it; its forwarder is the
//! daemon's.
//!
//! The in-process engine's arm (its slot installs and idle monitors) and the
//! generate and embed child engines were deleted with it (ENGINE_SWAP_PLAN
//! G2).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::engine_factory;
use sovereign_contracts::model_family::ModelFamily;
use sovereign_contracts::setup_config::{EngineKind, EngineSection, SetupConfig};
use sovereign_contracts::InferenceProvider;

use crate::llama_router::live::LiveRouter;
use crate::llama_router::placement::RouterPlacement;
use crate::llama_router::provider::RouterProvider;
use crate::llama_router::RouterPreset;
use crate::manager::{ComputeChildManager, ComputeRoutedProvider};
use crate::rpc_peer::{self, RpcPeerChild};

/// What a serving process installs, plus the concrete handles its host keeps.
pub struct ServingParts {
    /// The `dyn`-erased view the host installs and advertises.
    pub provider: Arc<dyn InferenceProvider>,
    /// The manifest-resolved embed slot family; drives app-side pooling and
    /// the mesh advertisement's embed-model info.
    pub embed_family: ModelFamily,
    /// What was built.
    pub plan: ServingPlan,
    /// Rebuilds this process's provider on a hot reload, through this same
    /// assembly and against the compute generation this build started.
    pub reload_factory: Arc<ReloadFactory>,
}

/// One slot a serving process loads or installs in-process. The in-process
/// engine's chat, code, extra, edit and rerank slots went with it
/// (ENGINE_SWAP_PLAN G2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlannedSlot {
    /// The embedding model.
    Embed,
}

/// What one config asks a serving process to hold: its slot set. Computed
/// from the config before anything loads, so a build that fails still names
/// what it was building.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingPlan {
    /// The engine `[engine] kind` selects.
    pub engine: EngineKind,
    /// Slots held in this process; empty for an engine with none.
    pub in_process: BTreeSet<PlannedSlot>,
    /// Compute children, by name and the GGUF each hosts.
    pub children: BTreeSet<(String, PathBuf)>,
    /// The embed slot's family.
    pub embed_family: ModelFamily,
    /// `kind = "llama-server"`: the router's models, as the preset it runs.
    pub router: Option<RouterPreset>,
}

/// A refused or failed build: the operator-facing reason, and the plan when
/// the config got far enough to have one.
#[derive(Debug)]
pub struct AssemblyError {
    /// What the build was asked to hold, if the config could be planned.
    pub plan: Option<ServingPlan>,
    /// Operator-facing text; a `hint:` line, where there is one, is part of it.
    pub reason: String,
}

impl std::fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Is this config the hosted pair `svrn setup --hosted` writes — a vendor
/// chat with an `embed_path` GGUF and no `[models]`, on an engine that does
/// not hold chat weights?
///
/// The one test for the config SHAPE, beside the one mover
/// ([`EngineSection::hosted_router_section`], which decides the section): its
/// no-`[models]` half is what makes the pair a one-section router rather
/// than a preset beside a vendor chat, so the two questions are asked in
/// their own places (ARCH 8).
fn is_hosted_pair(config: &SetupConfig) -> bool {
    config.engine.hosted_router_section().is_some() && config.models.is_none()
}

/// The `[engine]` section this node is PLANNED and SERVED with: the pair's
/// moved one when `config` is a hosted pair, else its own.
///
/// Every caller that asks "which engine is this" reads this rather than
/// `config.engine.kind`, so the plan, the boot and the reload cannot
/// disagree about a file nobody rewrote (ARCH 8).
fn hosted_pair_plan(config: &SetupConfig) -> EngineSection {
    match is_hosted_pair(config) {
        true => config
            .engine
            .hosted_router_section()
            .expect("is_hosted_pair asked the same decider"),
        false => config.engine.clone(),
    }
}

/// The slot set `config` asks for. Pure apart from its env reads (the served
/// kind's path) and, for a router, the GGUF headers its preset reads.
///
/// A hosted pair — `kind = "remote"` with `[engine] embed_path`, no
/// `[models]` — is planned as the router node it is moved to
/// (`EngineSection::hosted_router_section`, ENGINE_SWAP_PLAN A6), so this
/// function's answer and the boot's agree on a file nobody has rewritten.
pub fn plan_serving(config: &SetupConfig) -> Result<ServingPlan, String> {
    // Through the accessor, not the field: the refusal it returns names WHY
    // there are no slots (a terminal routes instead; an unconfigured node
    // runs `svrn setup`). The ENGINE is the one `hosted_pair_plan` answers:
    // a hosted pair is planned as the one-section router it is served by.
    let engine = hosted_pair_plan(config);
    // A remote engine holds no chat slots, so it needs no `[models]`. Neither
    // does a router node: its models are the preset's, and a hosted node has
    // none at all (its chat is at the vendor and its embedder is the one
    // section [`RouterPreset::embed_only`] builds).
    let models_may_be_absent =
        !engine.kind.needs_models() || engine.kind == EngineKind::LlamaServer;
    let models = if models_may_be_absent {
        // A vendor chat with an `embed_path` anyway (so `hosted_pair_plan`
        // did not move it, `embed_endpoint` is taken, and there is no
        // `[models]`) names no route: the router's preset is `[models]`.
        // Refused by name, so the boot never answers for a file it dropped a
        // key from (ARCH 6).
        if let Some(key) = engine.unhostable_on_router() {
            return Err(format!(
                "[engine] `{key}` beside `embed_path` names no route: the router builds its \
                 preset from `[models]`, and a node whose chat is at a vendor names none. \
                 Name `embed_endpoint`, name `[models]`, or name both `endpoint` and \
                 `embed_path` (which is served as the router's one section)."
            ));
        }
        config.models().ok()
    } else {
        Some(config.models()?)
    };
    let mut in_process = BTreeSet::new();
    // No engine holds its chat slots here: a remote engine embeds on the
    // server it names (`embed_endpoint`, or the vendor), and a router node's
    // embedder is the preset's section. Their family is `Unknown` where
    // nothing loads locally (`BuiltEngine::external`). `kind = "llama"`
    // plans nothing; `build_engine` refuses it by name.
    let embed_family = if let (EngineKind::LlamaServer, Some(models)) = (&engine.kind, models) {
        engine_factory::embed_family_for(&models.embed)
    } else if let (EngineKind::LlamaServer, None, Some(_)) =
        (&engine.kind, models, engine.own_embed_path())
    {
        // A router node with no `[models]` embeds from `[engine] embed_path`,
        // when it names one; the router serves it as its only section, so
        // the slot is NOT this process's.
        engine
            .own_embed_path()
            .map_or(ModelFamily::Unknown, engine_factory::embed_family_for)
    } else if let Some(path) = engine.own_embed_path() {
        in_process.insert(PlannedSlot::Embed);
        engine_factory::embed_family_for(path)
    } else {
        ModelFamily::Unknown
    };
    let mut children = BTreeSet::new();
    if config.compute.enabled {
        children.extend(
            config
                .compute
                .slot
                .iter()
                .map(|s| (s.name.clone(), s.model.clone())),
        );
    }
    let router = match (&engine.kind, models) {
        (EngineKind::LlamaServer, Some(models)) => Some(
            RouterPreset::from_models(
                models,
                crate::llama_router::served_rerank(models)?,
                &config.daemon,
                sovereign_inference::gguf_meta::read_mtp_draft_layers,
            )?
            // The split is `[compute] rpc_peers`'s, and every section of a
            // distributed preset names the same peers (ENGINE_SWAP_PLAN F,
            // f2): the section that loads across them is the one that
            // carries `rpc`, and a preset whose sections disagreed would
            // make `-ts` mean two things. Attached here, once, so the digest
            // (`with_placement` is part of the render) sees it and a reload
            // naming other peers is refused like any other preset change.
            .with_placement(RouterPlacement::from_config(config)?),
        ),
        // No `[models]`: a hosted node's router serves only its embedder.
        (EngineKind::LlamaServer, None) => engine
            .own_embed_path()
            .map(|path| RouterPreset::embed_only(path, &config.daemon))
            .transpose()?,
        _ => None,
    };
    Ok(ServingPlan {
        engine: engine.kind,
        in_process,
        children,
        embed_family,
        router,
    })
}

/// The one rerank decider, in sovereign-contracts since pb-serve-distributes.
pub use sovereign_contracts::rerank_kind::serves_rerank;

/// The compute generation a factory started: what the children were planned
/// as, and the manager running them (`None` when the layer came up empty).
struct LiveCompute {
    children: BTreeSet<(String, PathBuf)>,
    manager: Option<Arc<ComputeChildManager>>,
    /// The llama-server router the first build started, for a router engine.
    router: Option<Arc<LiveRouter>>,
    /// The `ggml-rpc-server` the first build started, under `[compute] rpc_peer`.
    rpc_peer: Option<Arc<RpcPeerChild>>,
}

/// Builds a serving process's provider, at cold start and again on every
/// reload, so the two cannot build different things.
///
/// It remembers the compute generation its first build started. A reload
/// re-wraps the new in-process engine around those RUNNING children rather
/// than spawning a second set beside them: `ComputeChildManager` has no
/// `Drop`, and its supervisors are spawned tasks, so a fresh layer would leave
/// the old children serving (for a distributed primary, two copies of the
/// model). A reload whose config asks for different children refuses by name.
#[derive(Default)]
pub struct ReloadFactory {
    live: Mutex<Option<LiveCompute>>,
}

/// Build the provider a node that holds weights serves, starting its compute
/// children if the config declares any.
pub fn assemble_serving(config: &SetupConfig) -> Result<ServingParts, AssemblyError> {
    Arc::new(ReloadFactory::default()).build(config)
}

impl ReloadFactory {
    /// Plan, admit and build.
    pub fn build(self: &Arc<Self>, config: &SetupConfig) -> Result<ServingParts, AssemblyError> {
        // A hosted pair is SERVED, by the router: every model process is the
        // router's now (ENGINE_SWAP_PLAN A6), so the vendor chat stays remote
        // and the embedding GGUF becomes its one preset section. Moved here,
        // once, before anything is planned or built — by the same accessor
        // `plan_serving` and the class check read.
        let moved;
        let config = if is_hosted_pair(config) {
            let mut moved_config = config.clone();
            moved_config.engine = hosted_pair_plan(config);
            tracing::info!(
                target: "serving_assembly",
                endpoint = ?config.engine.endpoint,
                embed_path = ?config.engine.embed_path,
                "a hosted pair is served by a one-section router: its embedder is the section, \
                 its chat stays at the vendor"
            );
            moved = moved_config;
            &moved
        } else {
            config
        };
        let plan = plan_serving(config).map_err(|reason| AssemblyError { plan: None, reason })?;
        tracing::info!(target: "serving_assembly", plan = ?plan, "assembling the serving provider");
        let fail = |reason: String| AssemblyError {
            plan: Some(plan.clone()),
            reason,
        };
        // The FIRST refusal: a router node must plan a model at all.
        // `plan_serving` answers a router with `[models]` or `[engine]
        // embed_path`, and nothing else, so a hosted pair (a vendor chat, no
        // `[models]`) reaches here with no preset — the daemon this row
        // replaced used to load its embedder in this process (ENGINE_SWAP_PLAN
        // A6). The class check refuses that file where it is read
        // (`EngineSection::unhostable_on_router`, naming both keys); this is
        // the boot's own answer for a config constructed in memory (ARCH 6).
        if config.engine.kind == EngineKind::LlamaServer && plan.router.is_none() {
            tracing::error!(
                target: "llama_router",
                "refused: kind = \"llama-server\" planned no model section"
            );
            return Err(fail(
                "[engine] kind = \"llama-server\" plans no model for this config: it serves \
                 `[models]` as the router's preset, and with none it serves `[engine] \
                 embed_path` as its one section. This node names neither. Add `[models]`, \
                 or run kind = \"remote\" to use a hosted chat model."
                    .to_string(),
            ));
        }
        // Required by every engine but a remote one (`plan_serving`) or a hosted pair.
        let models = if !config.engine.kind.needs_models() || is_hosted_pair(config) {
            config.models().ok()
        } else {
            Some(config.models().map_err(fail)?)
        };
        let peer_bind = rpc_peer::configured_bind(config).map_err(fail)?;

        // The generate and embed roles hosted the embedded engine, deleted
        // with it (ENGINE_SWAP_PLAN A7, G2): the router runs every model in
        // its own supervised child. Refused here, before anything spawns,
        // naming each slot.
        let retired = crate::compute_roles::retired_slots(&config.compute);
        if !retired.is_empty() {
            tracing::error!(
                target: "compute_child",
                slots = ?retired,
                "refused: retired compute-child roles"
            );
            return Err(fail(format!(
                "[compute] declares retired process roles:\n  {}\n\
                 The router loads, supervises and restarts every model itself.",
                retired.join("\n  ")
            )));
        }
        // The distributed primary's child carried the embedded engine, so the
        // mode went with it; the router places a distributed model in its own
        // supervised child (the section's `rpc` key and `-ts` split).
        if config.compute.distributed_primary {
            tracing::error!(
                target: "compute_child",
                engine = %config.engine.kind,
                "refused: [compute] distributed_primary is retired"
            );
            return Err(fail(
                "[compute] distributed_primary is retired: its compute child carried the \
                 embedded engine, which was deleted. The router places a distributed model in \
                 its own supervised child (the section's `rpc` key and `-ts` split, from \
                 `[compute] rpc_peers`). Remove the key."
                    .to_string(),
            ));
        }

        // One build at a time, and the children check before any load.
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = live.as_ref() {
            if running.children != plan.children {
                tracing::warn!(
                    target: "compute_child",
                    running = ?running.children,
                    asked = ?plan.children,
                    "reload refused: the config asks for different compute children; \
                     a reload never respawns them, a daemon restart does"
                );
                return Err(fail(format!(
                    "the compute children running are {:?}, and this config asks for {:?}. A \
                     reload re-wraps the running children and never respawns them — restart \
                     the daemon to apply a change to [compute] or to a child's model.",
                    running.children, plan.children
                )));
            }
        }

        // The router, like the children, belongs to the first build and is
        // re-wrapped by every reload; a reload asking for another preset
        // refuses by name.
        let running_router = live.as_ref().and_then(|r| r.router.clone());
        let asked = plan.router.as_ref().map(RouterPreset::digest);
        if live.is_some() && running_router.as_ref().map(|r| r.digest().to_string()) != asked {
            tracing::warn!(
                target: "llama_router",
                running = ?running_router.as_ref().map(|r| r.digest()),
                ?asked,
                "reload refused: the config asks for another llama-server preset"
            );
            return Err(fail(
                "the running llama-server router serves another preset than this config asks \
                 for. A reload never restarts the router — restart the daemon to apply a change \
                 to [models] or [engine] kind."
                    .to_string(),
            ));
        }
        // The peer child likewise: kept across reloads, never restarted by one.
        let running_peer = live
            .as_ref()
            .and_then(|r| r.rpc_peer.as_ref().map(|p| p.bind()));
        if live.is_some() && running_peer != peer_bind {
            tracing::warn!(target: "rpc_peer", running = ?running_peer, asked = ?peer_bind, "reload refused: the config asks for another rpc peer bind");
            return Err(fail(format!(
                "the running rpc peer child serves {running_peer:?}, and this config asks for \
                 {peer_bind:?}. A reload never restarts it — restart the daemon to apply a \
                 change to [compute] rpc_peer."
            )));
        }
        let started_router = match (&plan.router, &running_router) {
            (Some(preset), None) => Some(LiveRouter::start(preset, config).map_err(fail)?),
            _ => None,
        };
        let started_peer = match peer_bind.filter(|_| live.is_none()) {
            Some(bind) => match RpcPeerChild::start(bind, config) {
                Ok(child) => Some(child),
                Err(e) => {
                    if let Some(r) = &started_router {
                        r.terminate();
                    }
                    return Err(fail(e));
                }
            },
            None => None,
        };
        let router = running_router.or_else(|| started_router.clone());
        // The remote half is built from the live router's section; capture,
        // a property of the node, carries over.
        let engine_config = match (&router, &plan.router) {
            (Some(live_router), Some(preset)) => {
                let mut derived = config.clone();
                let capture = derived.engine.capture.take();
                derived.engine = match is_hosted_pair(config) {
                    true => preset.hosted_pair_section(&live_router.endpoint_v1(), &config.engine),
                    false => preset.engine_section(&live_router.endpoint_v1()),
                };
                derived.engine.capture = capture;
                // A one-section preset serves no chat model: 0 is "no chat context here",
                // the key `unhostable_on_router` reads as a served one-section preset.
                if preset.primary().is_none() {
                    derived.engine.context_size = 0;
                }
                Some(derived)
            }
            _ => None,
        };
        let built = build_in_process(engine_config.as_ref().unwrap_or(config));
        let (inner, embed_family) = match built {
            Ok(parts) => parts,
            Err(reason) => {
                // A router this build started must not outlive the build
                // that failed; the next one would start another.
                if let Some(r) = &started_router {
                    r.terminate();
                }
                if let Some(p) = &started_peer {
                    p.terminate();
                }
                return Err(fail(reason));
            }
        };

        let provider: Arc<dyn InferenceProvider> = match live.as_ref() {
            // A reload: the running generation serves the new engine too.
            Some(running) => match &running.manager {
                Some(manager) => {
                    let facade = Arc::new(ComputeRoutedProvider::new(
                        Arc::clone(&inner),
                        Arc::clone(manager),
                    ));
                    tracing::info!(
                        target: "compute_child",
                        children = running.children.len(),
                        "reload: the new engine is wrapped around the running compute children"
                    );
                    facade
                }
                None => inner,
            },
            // The first build: start the generation (or record that there is
            // none), so every later reload is measured against it.
            None => {
                let (provider, manager) = start_compute_layer(config, inner);
                *live = Some(LiveCompute {
                    children: plan.children.clone(),
                    manager,
                    router: router.clone(),
                    rpc_peer: started_peer,
                });
                provider
            }
        };

        let provider: Arc<dyn InferenceProvider> = match (&router, &plan.router) {
            (Some(r), Some(preset)) => Arc::new(RouterProvider::new(
                provider,
                Arc::clone(r),
                preset.clone(),
                models.and_then(|m| m.edit.as_ref()),
            )),
            _ => provider,
        };

        Ok(ServingParts {
            provider,
            embed_family,
            plan,
            reload_factory: Arc::clone(self),
        })
    }
}

/// The engine the registry selects.
fn build_in_process(
    config: &SetupConfig,
) -> Result<(Arc<dyn InferenceProvider>, ModelFamily), String> {
    // WHICH engine — the one decision, made in one place
    // (`crate::engine_factory`).
    let built = engine_factory::build_engine(config).map_err(|e| {
        format!(
            "{e}\nhint: verify paths and `[engine]` in {}",
            SetupConfig::default_path().display()
        )
    })?;
    Ok((built.provider, built.embed_family))
}

/// Compute-child process boundary (DISTRIBUTED_PILOT_READINESS.md P1).
///
/// When `[compute]` declares pools, wrap the in-process engine in the
/// routing facade: requests whose `model_id` names a pool (or embeddings,
/// when a capturing embed pool is serving) route to supervised child
/// processes; everything else falls through to `inner`. Default OFF →
/// `inner` is returned unchanged.
fn start_compute_layer(
    config: &SetupConfig,
    inner: Arc<dyn InferenceProvider>,
) -> (Arc<dyn InferenceProvider>, Option<Arc<ComputeChildManager>>) {
    if !(config.compute.enabled && !config.compute.slot.is_empty()) {
        return (inner, None);
    }
    // The child re-executes this binary with `--compute-child`;
    // `current_exe()`'s fallback is the daemon's [[bin]] name.
    let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sovereign-daemon"));
    let crash_dir = config.data.dir.join("compute-crash-logs");
    match crate::manager::build_compute_layer(
        &config.compute,
        Arc::clone(&inner),
        binary,
        crash_dir,
    ) {
        Some((facade, manager)) => {
            tracing::info!(
                target: "compute_child",
                slots = config.compute.slot.len(),
                "compute-child routing facade installed"
            );
            // The facade holds the manager alive; children are
            // SIGTERM'd on daemon death via PR_SET_PDEATHSIG.
            (facade, Some(manager))
        }
        None => (inner, None),
    }
}

// A sibling file: inline, it put this file past the 800-line band (ARCH §3.1).
#[cfg(test)]
#[path = "assembly/reload_builds_tests.rs"]
mod reload_builds_tests;
#[cfg(test)]
#[path = "assembly/reload_tests.rs"]
mod reload_tests;
#[cfg(test)]
#[path = "assembly/remote_plan_tests.rs"]
mod remote_plan_tests;
#[cfg(test)]
#[path = "assembly/serves_rerank_tests.rs"]
mod serves_rerank_tests;
