// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one serving assembly: config in, the provider a serving process
//! installs out.
//!
//! It owns everything between "this node holds weights" and "here is the
//! provider": admission ([`crate::containment`], fast ≠ primary), the engine
//! choice (`sovereign_inference::engine_factory::build_engine`, the registry),
//! llama's own slot installs and idle monitors, and the compute-child layer.
//! A terminal never reaches it; its forwarder is the daemon's.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_contracts::model_family::ModelFamily;
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::types::NextEditFormat;
use sovereign_contracts::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

/// What a serving process installs, plus the concrete handles its host keeps.
pub struct ServingParts {
    /// The `dyn`-erased view the host installs and advertises.
    pub provider: Arc<dyn InferenceProvider>,
    /// The same engine kept concrete, for the RPC-worker auto-reload path (the
    /// mesh discovery task force-reloads the primary when the worker set
    /// grows). `None` for an engine with no local llama slots.
    pub llama: Option<Arc<EmbeddedLlamaCpp>>,
    /// The manifest-resolved embed slot family; drives app-side pooling and
    /// the mesh advertisement's embed-model info.
    pub embed_family: ModelFamily,
    /// `Some` only under `[compute] distributed_primary`: the slot whose child
    /// owns the mesh-distributed primary. The worker-discovery loop respawns
    /// it on every worker-set change instead of calling
    /// `engine.reload_primary()`.
    pub distributed_primary: Option<Arc<crate::manager::DynamicChildSlot>>,
}

/// Env name for the automatic next-edit fallback. Declared in
/// `quality/env-flags.toml`; ledger row in `sovereign/DEFAULTS_LEDGER.md`.
const NEXT_EDIT_FALLBACK_ENV: &str = "SOVEREIGN_NEXT_EDIT_FALLBACK";

/// Is the automatic next-edit fallback armed?
///
/// **Default OFF, deliberately.** The fallback serves next-edit off
/// whichever model occupies the fast slot, and that model has not been
/// scored on the next-edit gen bank. The 21/30-useful / 0-wrong result
/// behind this feature was measured on a 35B-A3B chat primary; a small
/// fast model is a different model and its quality is an open question,
/// not an inherited one (ARCH §18.4 — validate the instrument before
/// the result). Flip condition and review-by date live in the ledger.
fn next_edit_fallback_enabled() -> bool {
    sovereign_inference::embedded::gates::env_flag_truthy(
        |n| std::env::var(n).ok(),
        NEXT_EDIT_FALLBACK_ENV,
    )
}

/// Build the provider a node that holds weights serves.
///
/// `Err` carries the operator-facing text; the caller prints it after
/// `error: ` (a `hint:` line, where there is one, is part of the text).
/// The containment refusal prints its own block before returning.
pub fn assemble_serving(config: &SetupConfig) -> Result<ServingParts, String> {
    // Past the terminal return, so this node holds weights and `[models]` must
    // be readable. Through the accessor, not the field: the refusal it returns
    // names WHY there are no slots, and a terminal has already left above.
    let models = config.models()?;

    // `[compute] distributed_primary` — the primary lives in a supervised
    // child, so this process must NOT also hold it. The factory derives this
    // the same way when it withholds the path; here it gates ADMISSION.
    let child_owns_primary = config.compute.enabled && config.compute.distributed_primary;
    // The other half of the same admission question: `child_owns_primary`
    // says the abort is contained; this says whether running WITHOUT that
    // containment is survivable on this node. The guard below fires when
    // containment IS armed and `fast` aliases `primary`; this one fires
    // when it is NOT armed and should be.
    if !crate::containment::check_containment(config, None) {
        return Err(
            "the distributed-primary containment guard refused this configuration (above)"
                .to_string(),
        );
    }
    if child_owns_primary && models.fast_path() == models.primary.as_path() {
        return Err(format!(
            "[compute] distributed_primary = true requires a DISTINCT small `fast` model.\n\
             hint: with no `[models].fast`, fast_path() falls back to the primary GGUF ({}), so \
             the daemon would load the distributed model locally as its fast slot — the exact \
             host-starving load this mode exists to prevent. Set `[models].fast` to a small GGUF.",
            models.primary.display()
        ));
    }
    if child_owns_primary {
        tracing::info!(
            target: "compute_child",
            primary = %models.primary.display(),
            "[compute] distributed_primary — the daemon withholds the primary; a compute child owns it"
        );
    }

    // WHICH engine — the one decision, made in one place
    // (`sovereign_inference::engine_factory`). Default `[engine] kind` is
    // `llama`, so a config.toml that names no engine builds exactly what
    // this function used to build unconditionally.
    let built = sovereign_inference::engine_factory::build_engine(config).map_err(|e| {
        format!(
            "{e}\nhint: verify paths and `[engine]` in {}",
            SetupConfig::default_path().display()
        )
    })?;
    let resolved_embed_family = built.embed_family.clone();

    // Everything below is llama's OWN surface — slot installs and idle
    // monitors that exist on `EmbeddedLlamaCpp` and on no other engine.
    // An engine that holds no local slots skips it entirely; the features
    // it configures report their own unavailability through the trait's
    // defaults rather than being faked (ARCH §18.3).
    if let Some(arc) = built.llama.as_ref() {
        // Wire the optional LRU memory budget BEFORE installing extras. With a
        // budget set, each `load_extra` call (including the eager startup loads
        // from `[models.extra]`) checks against it and evicts cold slots if
        // needed. Without a budget, eviction is disabled and slots persist.
        arc.set_extras_memory_budget(models.max_extras_memory_bytes())
            .map_err(|e| format!("failed to set extras memory budget: {e}"))?;
        // Idle-unload monitor for extras slots. Default 0 = disabled.
        arc.start_extras_idle_monitor(config.daemon.extras_idle_secs);
        // Operator-declared additional chat slots. Each `[models.extra]` entry
        // is loaded eagerly here; failures fail the daemon. Routing kicks in
        // when `/v1/chat/completions` arrives with a matching `model` field.
        if !models.extra.is_empty() {
            arc.install_extras(models.extra.clone(), models.effective_context_size())
                .map_err(|e| format!("failed to install extras slots: {e}"))?;
        }
        // The code-editing slot. Soft-fail like the reranker: a missing or
        // marker-less model must not block daemon startup — the routes
        // report their own unavailability, and the install logs the
        // actionable fix itself.
        match models.edit.as_ref() {
            // Operator chose an editing model. This always wins over the
            // fallback below.
            Some(edit) => {
                if let Err(e) = arc.install_edit_slot(edit, models.fast_path()) {
                    tracing::warn!(
                        target: "edit_slot",
                        error = %e,
                        "edit slot install failed — /v1/completions will 503 and next-edit \
                         is unavailable; check [models.edit].path in config.toml"
                    );
                }
            }
            // Nothing configured. Serve next-edit off the resident chat
            // model rather than serving nothing (`NEXT_EDIT.md` §graceful
            // degradation). Default OFF pending a bench baseline on the
            // fast slot — see `sovereign/DEFAULTS_LEDGER.md`.
            None if next_edit_fallback_enabled() => {
                if let Err(e) = arc.install_fallback_next_edit_slot(NextEditFormat::default()) {
                    tracing::warn!(
                        target: "edit_slot",
                        error = %e,
                        "next-edit fallback install failed — /v1/edit_predictions will \
                         report unavailable"
                    );
                }
            }
            None => {
                tracing::debug!(
                    target: "edit_slot",
                    "no [models.edit] configured and the next-edit fallback is off — \
                     next-edit and /v1/completions both unavailable. Set \
                     SOVEREIGN_NEXT_EDIT_FALLBACK=1 to serve next-edit off the \
                     resident chat model."
                );
            }
        }
        // Sourced from `[daemon].primary_idle_secs`. Default 300s suits a
        // desktop; batch workloads (atlas enrich) want 1800+ to skip the
        // 3–4 s reload tax between back-to-back short LLM calls.
        arc.start_idle_monitor(config.daemon.primary_idle_secs);
        // The two slots that used to be pinned from boot to process exit.
        // The daemon is a mesh node: it stays up and reachable for peers
        // whether or not anyone is using the desktop app, and an always-on
        // process must hold close to nothing while nobody is asking. Both
        // monitors release WEIGHTS only — nothing here stops the process,
        // and the next request on either slot reloads transparently.
        // Sourced from `[daemon].fast_idle_secs` / `[daemon].embed_idle_secs`
        // (both default 900s); `0` restores the old pinned behaviour.
        arc.start_fast_idle_monitor(config.daemon.fast_idle_secs);
        arc.start_embed_idle_monitor(config.daemon.embed_idle_secs);
        // Optional cross-encoder reranker from `SOVEREIGN_RERANK_MODEL_PATH`.
        // Soft-fail: a missing/broken reranker file must not block startup —
        // retrieval simply runs the baseline path.
        if let Ok(rerank_path) = std::env::var("SOVEREIGN_RERANK_MODEL_PATH") {
            let path = PathBuf::from(&rerank_path);
            match arc.install_rerank_slot(path, ModelFamily::Reranker) {
                Ok(model_id) => {
                    tracing::info!(
                        slot = "rerank",
                        model_id = %model_id,
                        "rerank slot installed from SOVEREIGN_RERANK_MODEL_PATH"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        path = %rerank_path,
                        error = %e,
                        "rerank slot install failed — running without reranker"
                    );
                }
            }
        }
    }

    let inner: Arc<dyn InferenceProvider> = Arc::clone(&built.provider);

    // Compute-child process boundary (DISTRIBUTED_PILOT_READINESS.md P1).
    // When `[compute]` declares pools, wrap the in-process engine in the
    // routing facade: requests whose `model_id` names a pool (or embeddings,
    // when a capturing embed pool is serving) route to supervised child
    // processes; everything else falls through to `inner`. The concrete
    // `arc` engine is still returned for the RPC-worker reload path. Default
    // OFF → `inner` is installed unchanged.
    let mut distributed_primary: Option<Arc<crate::manager::DynamicChildSlot>> = None;
    let provider: Arc<dyn InferenceProvider> = if config.compute.enabled
        && (!config.compute.slot.is_empty() || child_owns_primary)
    {
        // The child re-executes this binary with `--compute-child`;
        // `current_exe()`'s fallback is the daemon's [[bin]] name.
        let binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sovereign-daemon"));
        let crash_dir = config.data.dir.join("compute-crash-logs");
        // The distributed primary's identity: the shared-model id when the
        // node declares one (that is what peers address it by), else the
        // GGUF's own stem. Both are accepted as `model_id` on the way in.
        let distributed_spec = child_owns_primary.then(|| {
            let stem = models
                .primary
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "primary".to_string());
            let name = config
                .shared_model
                .model_id
                .clone()
                .unwrap_or_else(|| stem.clone());
            // The primary-role ALIASES must claim the child too, not just
            // the shared-model name and the GGUF stem.
            //
            // A request naming `commonwealth/primary` is asking for the
            // primary slot, and on a node with a distributed primary the
            // child IS that slot. `DistributedPrimaryRoute::claims` matched
            // only name and stem, so the alias fell through to the
            // in-process engine — whose primary is deliberately NOT resident
            // in this mode — and got served by the always-hot `fast` slot
            // instead. Measured live 2026-07-29 on RuggedFox, same prompt in
            // the same minute: `commonwealth/primary` returned 11 tokens at
            // ~111 tok/s (the 0.8B), while the GGUF stem returned 170 tokens
            // from the 122B. Every client using the advertised alias got the
            // small model and no error — including `svrn mesh bench`, which
            // filed the fast slot's rate under the 122B's name.
            //
            // Derived from `SLOT_ALIAS_POLICY` rather than spelled out here.
            // Resolution and mesh advertisement already drifted apart once
            // (slot_aliases.rs, 2026-05-19); routing is a third view of the
            // same policy and must not become a third place to forget.
            let model_ids = distributed_primary_model_ids(&stem);
            crate::manager::DistributedPrimarySpec {
                handoff_path: config
                    .data
                    .dir
                    .join("compute-distribution")
                    .join(format!("{name}.json")),
                name,
                model: models.primary.clone(),
                context_size: Some(models.effective_context_size()),
                n_gpu_layers: None,
                model_ids,
            }
        });
        match crate::manager::build_compute_layer_with_distributed(
            &config.compute,
            Arc::clone(&inner),
            binary,
            crash_dir,
            distributed_spec,
        ) {
            Some((facade, _manager)) => {
                distributed_primary = facade.distributed_slot();
                tracing::info!(
                    target: "compute_child",
                    slots = config.compute.slot.len(),
                    distributed_primary = distributed_primary.is_some(),
                    "compute-child routing facade installed"
                );
                // The facade holds the manager alive; children are
                // SIGTERM'd on daemon death via PR_SET_PDEATHSIG.
                facade as Arc<dyn InferenceProvider>
            }
            None => inner,
        }
    } else {
        inner
    };

    Ok(ServingParts {
        provider,
        llama: built.llama,
        embed_family: resolved_embed_family,
        distributed_primary,
    })
}

/// A compute child's `role=generate` engine, as its flags name it: one model
/// in the fast slot, or — with `--distribution` — the mesh's distributed
/// primary. Blocking (model load); the child calls it on `spawn_blocking`.
pub(crate) fn assemble_child_generate(
    path: &Path,
    ctx: u32,
    gpu_layers: Option<u32>,
    distribution: Option<PathBuf>,
) -> sovereign_contracts::Result<Arc<dyn InferenceProvider>> {
    let Some(handoff_path) = distribution else {
        // Single model into the fast slot (no separate primary).
        // Grammar/structured-output are honoured per-request by
        // build_sampler.
        let engine = EmbeddedLlamaCpp::load_dual(path, None, ctx, gpu_layers)?;
        return Ok(Arc::new(engine));
    };

    // Distributed primary. The daemon has already planned the shards
    // and warmed every worker's cache; we load across them. Install the
    // handoff FIRST — it is what makes `resolve_placement` see workers
    // at all, and what pins the daemon's plan so our `-ot` overrides cut
    // the blocks exactly where the warm caches expect.
    let handoff = crate::distribution::DistributionHandoff::read(&handoff_path).map_err(|e| {
        sovereign_contracts::Error::InvalidInput(format!(
            "--distribution {}: {e}",
            handoff_path.display()
        ))
    })?;
    tracing::info!(
        target: "compute_child",
        workers = handoff.endpoints.len(),
        endpoints = ?handoff.endpoints,
        handoff = %handoff_path.display(),
        "distributed primary: installing the daemon's worker set + shard plan"
    );
    handoff.install(path);
    let engine =
        EmbeddedLlamaCpp::load_single_distributed(path, ctx, gpu_layers, ModelFamily::Unknown)?;
    Ok(Arc::new(engine))
}

/// A compute child's `role=embed` engine. Blocking (model load).
pub(crate) fn assemble_child_embed(
    path: &Path,
) -> sovereign_contracts::Result<Arc<dyn InferenceProvider>> {
    let family = sovereign_inference::engine_factory::embed_family_for(path);
    let engine = sovereign_inference::embedded::EmbedOnlyProvider::load(path, family)?;
    Ok(Arc::new(engine))
}

/// Every `model_id` that must route to the distributed-primary child: the GGUF
/// stem plus every primary-role alias the daemon resolves.
///
/// Pure and separate from the assembly above so the set is testable — the defect
/// this replaces was a *missing member*, which no test of the surrounding
/// 250-line builder would have caught.
fn distributed_primary_model_ids(stem: &str) -> Vec<String> {
    let mut ids = vec![stem.to_string()];
    for alias in sovereign_contracts::venue::resolution_alias_keys("primary") {
        if !ids.contains(&alias) {
            ids.push(alias);
        }
    }
    ids
}

#[cfg(test)]
mod distributed_primary_routing_tests {
    use super::*;

    /// The literal string `svrn mesh bench` sends (`mesh_bench::PRIMARY_ALIAS`),
    /// and the one `build_self_manifest` advertises to mesh peers.
    const BENCH_AND_MESH_ALIAS: &str = "commonwealth/primary";

    #[test]
    fn the_child_claims_the_advertised_primary_alias() {
        let ids = distributed_primary_model_ids("Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003");

        assert!(
            ids.iter().any(|m| m == BENCH_AND_MESH_ALIAS),
            "a node whose primary is child-distributed advertises `{BENCH_AND_MESH_ALIAS}` \
             to peers and resolves it locally; if the child does not CLAIM it, the request \
             falls through to the in-process engine and the fast slot answers with a \
             different model and no error. Got: {ids:?}"
        );
        // The bare form too — OpenAI clients and opencode configs use it.
        assert!(ids.iter().any(|m| m == "primary"), "got: {ids:?}");
        // And the concrete id, which is how a peer addresses this exact GGUF.
        assert!(
            ids.iter()
                .any(|m| m == "Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003"),
            "got: {ids:?}"
        );
    }

    /// Whatever `SLOT_ALIAS_POLICY` says is resolvable for `primary` must be
    /// claimable. Adding a synonym there without this passing means that synonym
    /// silently reaches the wrong model.
    #[test]
    fn every_resolvable_primary_alias_is_claimable() {
        let ids = distributed_primary_model_ids("some-model");
        for alias in sovereign_contracts::venue::resolution_alias_keys("primary") {
            assert!(
                ids.contains(&alias),
                "`{alias}` resolves to the primary slot but would not route to the \
                 distributed child"
            );
        }
    }

    #[test]
    fn a_stem_that_collides_with_an_alias_is_not_duplicated() {
        let ids = distributed_primary_model_ids("primary");
        let count = ids.iter().filter(|m| *m == "primary").count();
        assert_eq!(count, 1, "got: {ids:?}");
    }
}
/// Every path that builds an inference provider must arm every idle
/// monitor the slot lineup has.
///
/// **The bug this is a gate for.** There are two provider-build paths in
/// this crate: cold start (`build/inference.rs`) and hot reload
/// (`provider.rs`). When `start_fast_idle_monitor` and
/// `start_embed_idle_monitor` were added, only the first was obvious —
/// the second was found by grepping, not by anything failing. A reloaded
/// daemon that armed three of four monitors would quietly re-acquire the
/// pinned-forever footprint the cold-start path had just given up, with
/// nothing red anywhere and no symptom except memory.
///
/// A type-level fix would be better and is not available: the monitors
/// are independent inherent methods on a provider the reload path
/// legitimately holds as `Arc<dyn InferenceProvider>` moments later. So
/// the invariant is enforced where it can be — over the source — in the
/// same shape `embedded::ffi_trace` already uses for the KV-clear rule.
#[cfg(test)]
mod idle_monitor_coverage {
    /// The cold-start provider build, by source. The hot-reload half of this
    /// gate is the daemon's (`sovereign-daemon/src/build/inference.rs`).
    const COLD_START_SRC: &str = include_str!("assembly.rs");

    /// Every idle monitor the embedded engine exposes. Adding a slot with
    /// an idle monitor means adding it here, which is the point: the list
    /// is the checklist.
    const MONITORS: [&str; 4] = [
        "start_idle_monitor(",
        "start_extras_idle_monitor(",
        "start_fast_idle_monitor(",
        "start_embed_idle_monitor(",
    ];

    /// A call, not a mention: comments explaining a monitor must not
    /// satisfy the gate. A gate that would pass with the ability it
    /// guards fully removed is not a gate.
    fn calls(src: &str, monitor: &str) -> bool {
        src.lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("///") && !t.starts_with('*')
            })
            .any(|l| l.contains(monitor))
    }

    #[test]
    fn the_cold_start_path_arms_every_idle_monitor() {
        let missing: Vec<&str> = MONITORS
            .iter()
            .copied()
            .filter(|m| !calls(COLD_START_SRC, m))
            .collect();
        assert!(
            missing.is_empty(),
            "cold-start provider build never calls: {missing:?} — those slots stay \
             resident for the life of the daemon"
        );
    }

    #[test]
    fn each_monitor_reads_its_own_configured_window() {
        // One knob per monitor, read from config — not a literal, and not
        // another slot's knob. A monitor wired to the wrong field looks
        // armed and sweeps on someone else's schedule.
        for (monitor, knob) in [
            ("start_idle_monitor(", "primary_idle_secs"),
            ("start_extras_idle_monitor(", "extras_idle_secs"),
            ("start_fast_idle_monitor(", "fast_idle_secs"),
            ("start_embed_idle_monitor(", "embed_idle_secs"),
        ] {
            let wired = COLD_START_SRC
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    !t.starts_with("//") && !t.starts_with("///")
                })
                .any(|l| l.contains(monitor) && l.contains(knob));
            assert!(
                wired,
                "`{monitor}` is never called with `{knob}` — it is either hardcoded or \
                 reading another slot's window"
            );
        }
    }
}
