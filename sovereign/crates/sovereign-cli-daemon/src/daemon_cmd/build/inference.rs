//! Inference provider construction — extracted from `run_daemon` (§3.3).
//!
//! Builds the embedded llama.cpp provider from the three GGUF slots.
//! Synchronous — load happens inline; model files are mmapped so
//! cold-start latency is dominated by disk I/O on first reference.
//!
//! **Family resolution.** The embed slot's family identity decides its
//! app-side pooling strategy, normalisation, and the document / query
//! instruction prefixes via `EmbedQuirks`. After the llama-cpp-4 0.2.x
//! migration the C-side pooling type is forced to `None` in
//! `EmbedSlot::load` (the binding returns null from `embeddings_seq_ith`
//! on every gguf whose header says NONE, and setting any other type
//! ggml_aborts the context constructor for Qwen3-Embedding); pooling
//! moved into Rust against the per-token `embeddings_ith` reads. The
//! family lookup is therefore what selects the right strategy (Last for
//! Qwen3-Embedding, Mean for BERT-style) and the right text prep on the
//! input — keeping it resolved here means the slot loader and the
//! mesh-advertisement path read from a single source of truth.

use std::path::PathBuf;
use std::sync::Arc;

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

use sovereign_core::model_family::ModelFamily;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::NextEditFormat;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

/// Returns `(provider, engine, embed_family)` on success, or `Err(())`
/// when a slot fails to load or configure (the caller returns 1; the
/// operator-facing diagnostics are printed here).
///
/// - `provider` — the `dyn`-erased view the daemon installs + advertises.
/// - `engine` — the same object kept concrete, captured for the
///   RPC-worker auto-reload path (the mesh discovery task force-reloads
///   the primary when the worker set grows).
/// - `embed_family` — the manifest-resolved embed slot family; drives
///   app-side pooling + the mesh advertisement's embed-model info.
/// - `distributed_primary` — `Some` only under `[compute] distributed_primary`:
///   the slot whose child owns the mesh-distributed primary. The worker-
///   discovery loop respawns it on every worker-set change instead of calling
///   `engine.reload_primary()`.
pub(crate) fn load_provider(
    config: &SetupConfig,
) -> Result<
    (
        Arc<dyn InferenceProvider>,
        Option<Arc<EmbeddedLlamaCpp>>,
        ModelFamily,
        Option<Arc<sovereign_compute::manager::DynamicChildSlot>>,
    ),
    (),
> {
    // `[compute] distributed_primary` — the primary lives in a supervised
    // child, so the daemon must NOT also hold it. The factory derives this
    // the same way when it withholds the path; here it gates ADMISSION,
    // which stays with the daemon because only the daemon owns the
    // operator-facing diagnostics below.
    let child_owns_primary = config.compute.enabled && config.compute.distributed_primary;
    // The other half of the same admission question: `child_owns_primary`
    // says the abort is contained; this says whether running WITHOUT that
    // containment is survivable on this node. The guard below fires when
    // containment IS armed and `fast` aliases `primary`; this one fires
    // when it is NOT armed and should be.
    if !super::containment::check_containment(config, None) {
        return Err(());
    }
    if child_owns_primary && config.models.fast_path() == config.models.primary.as_path() {
        eprintln!(
            "error: [compute] distributed_primary = true requires a DISTINCT small `fast` model."
        );
        eprintln!(
            "hint: with no `[models].fast`, fast_path() falls back to the primary GGUF ({}), so \
             the daemon would load the distributed model locally as its fast slot — the exact \
             host-starving load this mode exists to prevent. Set `[models].fast` to a small GGUF.",
            config.models.primary.display()
        );
        return Err(());
    }
    if child_owns_primary {
        tracing::info!(
            target: "compute_child",
            primary = %config.models.primary.display(),
            "[compute] distributed_primary — the daemon withholds the primary; a compute child owns it"
        );
    }

    // WHICH engine — the one decision, made in one place
    // (`sovereign_inference::engine_factory`). Default `[engine] kind` is
    // `llama`, so a config.toml that names no engine builds exactly what
    // this function used to build unconditionally.
    let built = match sovereign_inference::engine_factory::build_engine(config) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!(
                "hint: verify paths and `[engine]` in {}",
                SetupConfig::default_path().display()
            );
            return Err(());
        }
    };
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
        if let Err(e) = arc.set_extras_memory_budget(config.models.max_extras_memory_bytes()) {
            eprintln!("error: failed to set extras memory budget: {e}");
            return Err(());
        }
        // Idle-unload monitor for extras slots. Default 0 = disabled.
        arc.start_extras_idle_monitor(config.daemon.extras_idle_secs);
        // Operator-declared additional chat slots. Each `[models.extra]` entry
        // is loaded eagerly here; failures fail the daemon. Routing kicks in
        // when `/v1/chat/completions` arrives with a matching `model` field.
        if !config.models.extra.is_empty() {
            if let Err(e) = arc.install_extras(
                config.models.extra.clone(),
                config.models.effective_context_size(),
            ) {
                eprintln!("error: failed to install extras slots: {e}");
                return Err(());
            }
        }
        // The code-editing slot. Soft-fail like the reranker: a missing or
        // marker-less model must not block daemon startup — the routes
        // report their own unavailability, and the install logs the
        // actionable fix itself.
        match config.models.edit.as_ref() {
            // Operator chose an editing model. This always wins over the
            // fallback below.
            Some(edit) => {
                if let Err(e) = arc.install_edit_slot(edit, config.models.fast_path()) {
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
    let mut distributed_primary: Option<Arc<sovereign_compute::manager::DynamicChildSlot>> = None;
    let provider: Arc<dyn InferenceProvider> =
        if config.compute.enabled && (!config.compute.slot.is_empty() || child_owns_primary) {
            let binary =
                std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sovereign-cli-daemon"));
            let crash_dir = config.data.dir.join("compute-crash-logs");
            // The distributed primary's identity: the shared-model id when the
            // node declares one (that is what peers address it by), else the
            // GGUF's own stem. Both are accepted as `model_id` on the way in.
            let distributed_spec = child_owns_primary.then(|| {
                let stem = config
                    .models
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
                sovereign_compute::manager::DistributedPrimarySpec {
                    handoff_path: config
                        .data
                        .dir
                        .join("compute-distribution")
                        .join(format!("{name}.json")),
                    name,
                    model: config.models.primary.clone(),
                    context_size: Some(config.models.effective_context_size()),
                    n_gpu_layers: None,
                    model_ids,
                }
            });
            match sovereign_compute::manager::build_compute_layer_with_distributed(
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

    Ok((provider, built.llama, resolved_embed_family, distributed_primary))
}

/// Every `model_id` that must route to the distributed-primary child: the GGUF
/// stem plus every primary-role alias the daemon resolves.
///
/// Pure and separate from the assembly above so the set is testable — the defect
/// this replaces was a *missing member*, which no test of the surrounding
/// 250-line builder would have caught.
fn distributed_primary_model_ids(stem: &str) -> Vec<String> {
    let mut ids = vec![stem.to_string()];
    for alias in sovereign_mesh::slot_aliases::resolution_alias_keys("primary") {
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
        for alias in sovereign_mesh::slot_aliases::resolution_alias_keys("primary") {
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
