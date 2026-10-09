// SPDX-License-Identifier: AGPL-3.0-or-later
//! Spawning one corpus install on this node: the shared helper every install
//! path comes through (the internal and OICP install routes, mesh
//! auto-ingest, auto-resume, the collaborate path on partition-receiver
//! peers), its typed outcome, and the post-install hooks it runs.
//!
//! Moved whole from `corpus_ingest.rs`, which is over its arch-gate pin, so
//! the install outcome can grow (ADDRESSED_TEXT §5.6) without growing that
//! file. Behaviour-preserving; the names are unchanged and re-exported
//! through the module facade.

use corpus_index::ingest_port::daemon::InstallRefusal;
use oicp_types::activity::ActivityEventKind;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

use super::corpus_ingest::{clear_stale_failure, ingest_progress_callback, record_failure};

/// Resolve the CLI binary that can actually run `enrich init/extract`
/// for the deep Tier-2 referential-atlas pass.
///
/// `enrich` is owned ONLY by `sovereign-cli-llm`. **No daemon process
/// is that binary**, so we must NEVER self-exec `std::env::current_exe()`
/// here (the historical bug):
/// - Standalone daemon → `current_exe()` is `sovereign-cli-daemon`,
///   whose dispatcher rejects `enrich` (exit 2) — a dead pass.
/// - Desktop embedded in-process daemon → `current_exe()` is the Tauri
///   GUI binary (`sovereign-desktop`), which has no arg-parser and no
///   single-instance guard: self-execing it **mis-launches a second GUI
///   window** and blocks this post-install task on it.
///
/// So the deep pass is opt-in via an explicit `$SOVEREIGN_CLI_LLM_BIN`
/// pointing at a real `sovereign-cli-llm`. When it is unset (every
/// default deployment) we skip — the structural atlas + inline tiered
/// enrichment have already landed by this point, so the referential
/// atlas is an optional publisher-side deepening, not a prerequisite for
/// retrieval or Explore. We deliberately do NOT auto-discover a sibling
/// binary or fall back to `which`: that would silently turn on a heavy
/// LLM pass on the standalone daemon (which has never run it), changing
/// behaviour no operator asked for.
fn resolve_enrich_cli() -> Option<std::path::PathBuf> {
    let raw = std::env::var("SOVEREIGN_CLI_LLM_BIN").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(trimmed);
    if path.exists() {
        Some(path)
    } else {
        tracing::warn!(
            configured = %path.display(),
            "SOVEREIGN_CLI_LLM_BIN is set but the path does not exist — skipping tier-2 deep extraction"
        );
        None
    }
}

/// Phase C3 — gather peer atlas advice for `corpus_id`.
///
/// Walks the live mesh, builds [`RemoteAtlasView`]s from each peer's
/// `hosted_corpora`, reads the local atlas summary, and returns the
/// best pull candidate (if any) per the rule in
/// [`evaluate_peer_atlas_advice`].
///
/// Returns `None` when no peer is worth pulling from — the post-
/// install hook then proceeds with the local Tier-2 launch as
/// usual. Best-effort: any I/O hiccup falls through to "no advice"
/// rather than blocking the install.
async fn gather_peer_atlas_advice(
    state: &AppState,
    corpus_id: &str,
    indexes_dir: &std::path::Path,
) -> Option<sovereign_tools::atlas_peer_advice::AtlasPullLead> {
    use sovereign_tools::atlas_peer_advice::{evaluate_peer_atlas_advice, RemoteAtlasView};

    // Local view: atom counts come from the cached summary; embed
    // model from our own member record (populated by gossip).
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    let Some(atlas) = state.inner.node.atlas.as_ref() else {
        tracing::debug!(
            corpus_id,
            "peer atlas advice: no ingest atlas port; no advice"
        );
        return None;
    };
    let local_summary = atlas.atlas_summary(&atlas_dir).ok().flatten();
    let local_tier2_count = local_summary.as_ref().map(|s| s.tier2_count).unwrap_or(0);
    let local_fingerprint = local_summary.as_ref().map(|s| s.fingerprint.as_str());

    let self_node_id = state.inner.fabric.identity.current();
    let members = state.membership().members().await;
    let my_embed_model = members
        .iter()
        .find(|m| m.node_id == self_node_id)
        .and_then(|m| m.capabilities.embed_model.as_ref())
        .map(|m| m.model_id.clone());

    let mut peer_views: Vec<RemoteAtlasView> = Vec::new();
    for member in &members {
        if member.node_id == self_node_id {
            continue;
        }
        let model = member
            .capabilities
            .embed_model
            .as_ref()
            .map(|m| m.model_id.clone());
        if let Some(view) = RemoteAtlasView::from_member(
            member.name.clone(),
            model,
            corpus_id,
            &member.capabilities.hosted_corpora,
        ) {
            peer_views.push(view);
        }
    }

    evaluate_peer_atlas_advice(
        local_tier2_count,
        local_fingerprint,
        my_embed_model.as_deref(),
        &peer_views,
    )
}

/// Where an install's recipe comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// The registry resolves the recipe by the corpus id.
    Registry,
    /// The caller supplied the recipe TOML (OICP v0.5 `ingest:recipe`,
    /// ADDRESSED_TEXT §5.6): installed under the corpus id it must name,
    /// and stamped with the sha256 of these bytes.
    Recipe(String),
}

/// `true` only when a new task was spawned: the projection the mesh
/// auto-ingest and auto-resume callers want. Every HTTP caller maps the full
/// [`InstallOutcome`] through [`install_status`] instead.
pub async fn spawn_corpus_install(state: AppState, corpus_id: String) -> bool {
    matches!(
        spawn_corpus_install_outcome(state, corpus_id, std::collections::BTreeMap::new()).await,
        InstallOutcome::Spawned
    )
}

/// Like [`spawn_corpus_install`] but threads recipe parameters and
/// returns the full [`InstallOutcome`] instead of a bool — the HTTP
/// handler needs to distinguish a failure from a benign no-op. Most
/// callers want the bool projection [`spawn_corpus_install_with_parameters`].
///
/// The recipe is fetched up front, its parameter schema validated via
/// [`Recipe::resolve_parameters`], and the stamped recipe passed to
/// `engine.ingest` via [`CorpusSpec::Inline`] so the runtime carries the
/// resolved values into `http_api` URL/body interpolation. Mismatched /
/// missing parameters — and an unresolvable recipe — surface as a
/// typed failure here, *before* the background task spawns, so the
/// caller sees a 4xx on the install POST instead of a silent "ingest
/// failed" three minutes later.
pub async fn spawn_corpus_install_outcome(
    state: AppState,
    corpus_id: String,
    parameters: std::collections::BTreeMap<String, serde_json::Value>,
) -> InstallOutcome {
    spawn_install(state, corpus_id, parameters, InstallSource::Registry).await
}

/// [`spawn_corpus_install_outcome`] from either [`InstallSource`].
///
/// A supplied recipe is idempotent by its bytes: the same `(corpus_id,
/// recipe_sha256)` as the stamp on an installed corpus is
/// [`InstallOutcome::AlreadyInstalled`], and a different one reingests the
/// corpus — validated first, so a recipe that does not load never costs the
/// installed corpus.
pub async fn spawn_install(
    state: AppState,
    corpus_id: String,
    parameters: std::collections::BTreeMap<String, serde_json::Value>,
    source: InstallSource,
) -> InstallOutcome {
    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        tracing::warn!(
            corpus = %corpus_id,
            "spawn_corpus_install: no corpus engine — ignoring"
        );
        return InstallOutcome::NoEngine;
    };
    // Ingest's atlas port, for the post-install structural atlas; composed
    // beside the engine, so `None` only where a host slots an engine alone.
    let atlas = state.inner.node.atlas.clone();

    {
        let mut active = state.inner.ingest.active_ingests.write().await;
        if active.contains(&corpus_id) {
            tracing::info!(
                corpus = %corpus_id,
                "spawn_corpus_install: already active — not spawning a second task"
            );
            return InstallOutcome::AlreadyActive;
        }
        active.insert(corpus_id.clone());
    }

    // What is installed under this id now, and from which recipe bytes.
    let installed = corpus_index::corpus::Corpus::named(engine.index_dir(), &corpus_id);
    let installed = installed.filter(|c| c.is_installed());
    if let InstallSource::Recipe(toml) = &source {
        let sha = corpus_index::corpus::recipe_sha256(toml);
        let stamped = installed
            .as_ref()
            .and_then(|c| corpus_index::corpus::Corpus::recipe_sha256_in(c.root()));
        tracing::debug!(corpus = %corpus_id, %sha, stamped = ?stamped, "spawn_corpus_install: recipe install");
        if stamped.as_deref() == Some(sha.as_str()) {
            release(&state, &corpus_id).await;
            return InstallOutcome::AlreadyInstalled;
        }
    }

    clear_stale_failure(&state, &corpus_id).await;

    // Resolve the recipe + apply parameters BEFORE spawning the
    // background task so a parameter mismatch surfaces as a
    // synchronous failure instead of a silent crash later. The port
    // loads (a supplied recipe) or fetches (by id), coerces and resolves
    // in that order and names which step refused.
    let prepared = match &source {
        InstallSource::Registry => {
            engine
                .clone()
                .prepare_registry_install(&corpus_id, &parameters)
                .await
        }
        InstallSource::Recipe(toml) => {
            engine
                .clone()
                .prepare_recipe_install(&corpus_id, toml, &parameters)
                .await
        }
    };
    let prepared = match prepared {
        Ok(p) => p,
        Err(refusal) => {
            // Roll back the active_ingests insert so a subsequent
            // retry isn't blocked.
            release(&state, &corpus_id).await;
            return match refusal {
                InstallRefusal::RecipeNotFound(e) => InstallOutcome::RecipeNotFound(e),
                InstallRefusal::InvalidParameters(e) => InstallOutcome::InvalidParameters(e),
                InstallRefusal::InvalidRecipe(e) => InstallOutcome::InvalidRecipe(e),
            };
        }
    };
    // A different recipe for an installed corpus reingests it: the new one
    // validated above, so only now is the old index removed.
    if let (InstallSource::Recipe(_), Some(old)) = (&source, &installed) {
        tracing::info!(corpus = %old, "spawn_corpus_install: a different recipe, reingesting");
        if let Err(e) = engine.remove_corpus_everything(&corpus_id) {
            release(&state, &corpus_id).await;
            return InstallOutcome::ReplaceFailed(e.to_string());
        }
    }

    let state_for_task = state.clone();
    let corpus_id_for_task = corpus_id.clone();
    tokio::spawn(async move {
        // Progress callback: latest-wins per corpus, except that a
        // terminal failure is never clobbered. See
        // `ingest_progress_callback`.
        let progress_cb =
            ingest_progress_callback(state_for_task.clone(), corpus_id_for_task.clone());

        // Respect a recipe's explicit retrieval-only opt-out: a recipe with
        // `[enrichment] enabled = false` skips the default post-install
        // structural-atlas + Tier-2 RAPTOR pass below, keeping retrieval sealed
        // to its own chunks (e.g. the chaos-monkey bench corpus). A recipe with
        // NO [enrichment] keeps the default-on hook.
        let recipe_opts_out_of_auto_enrichment = prepared.opts_out_of_auto_enrichment;
        let result = (prepared.run)(Some(progress_cb)).await;

        state_for_task
            .inner
            .ingest
            .active_ingests
            .write()
            .await
            .remove(&corpus_id_for_task);

        match result {
            Ok(info) => {
                tracing::info!(
                    corpus = %corpus_id_for_task,
                    chunks = info.chunks_created,
                    duration_secs = info.duration_secs,
                    "spawn_corpus_install: ingest complete"
                );
                // Record the ingest on the local Activity ledger — the
                // headline "your import did real work" signal. Embedding
                // thousands of chunks is heavy local resource use that
                // never crosses a peer boundary, so the contribution
                // ledger never sees it; this is where it becomes visible.
                if let Err(e) = state_for_task
                    .inner
                    .node
                    .activity_emitter
                    .record(ActivityEventKind::ChunksIngested {
                        corpus_id: corpus_id_for_task.clone(),
                        chunks: info.chunks_created,
                        duration_secs: info.duration_secs,
                    })
                    .await
                {
                    tracing::warn!(
                        corpus = %corpus_id_for_task, error = %e,
                        "spawn_corpus_install: the activity record did not reach the store"
                    );
                }
                // Post-install hook: build the structural atlas the
                // moment chunks are committed. Detached so the route
                // handler that triggered the install isn't held up
                // by the atlas pass; idempotent — a re-install or
                // restart is a no-op once `atlas/atoms.json` exists.
                let cid = corpus_id_for_task.clone();
                // structure_first doesn't read recipes — it walks chunks
                // by metadata. Pass the same path for both to satisfy
                // the CorpusEngine constructor without a recipe lookup.
                let indexes = engine.index_dir().to_path_buf();
                let recipes = indexes.clone();
                let enrich_activity = state_for_task.inner.node.activity_emitter.clone();
                // The SEC filings corpus's typed fact store moves into
                // the index dir HERE, synchronously, BEFORE the detached
                // block below. The `sec_facts` tool resolves a corpus by
                // the presence of that store, so any window where the
                // corpus is installed and the store is not yet placed is
                // a window where the tool reports "no installed SEC
                // corpus" for a corpus the user just watched install.
                // The atlas pass below is detached because it is
                // expensive; a file copy is not. No-op for every corpus
                // that has no staged store.
                if let Err(e) = sovereign_tools::sec_edgar::install_fact_sidecar(&cid, &indexes) {
                    tracing::warn!(
                        corpus = %cid, error = %e,
                        "post-install: typed fact store could not be placed — financial \
                         figures will refuse for this corpus until it is"
                    );
                }
                tokio::spawn(async move {
                    // Recipe opted out of auto-enrichment (retrieval-only):
                    // skip the structural-atlas + Tier-2 RAPTOR pass entirely
                    // so retrieval stays sealed to the source chunks.
                    if recipe_opts_out_of_auto_enrichment {
                        tracing::info!(
                            corpus = %cid,
                            "post-install: recipe is retrieval-only ([enrichment] enabled=false) — skipping structural atlas + Tier-2 RAPTOR"
                        );
                        return;
                    }
                    let Some(atlas) = atlas else {
                        tracing::warn!(
                            corpus = %cid,
                            "post-install: no ingest atlas port in this process — skipping \
                             structural atlas + Tier-2 RAPTOR"
                        );
                        return;
                    };
                    use corpus_index::enrichment_state::{EnrichmentPhase, EnrichmentStateFile};
                    use sovereign_tools::atlas_postinstall::{
                        build_structural_atlas, build_triage_candidates, effective_tier2_budget,
                        StructuralAtlasOutcome, TriageOutcome,
                    };
                    tracing::info!(corpus = %cid, "post-install: structural atlas — start");
                    // Generic enrichment state stamp so every corpus's
                    // post-install gets a row in
                    // `_enrichment_state.json` and the desktop chip
                    // can render "Extracting atoms" → "Saving" →
                    // "complete". Daemon restart leaves a Stalled
                    // entry for the sweeper to pick up.
                    let corpus_index_dir = indexes.join(&cid);
                    let _ = EnrichmentStateFile::stamp(
                        &corpus_index_dir,
                        &cid,
                        Some("structural_atlas"),
                        EnrichmentPhase::AtomExtraction,
                        0,
                        0,
                        Some("walking chunks for structural atom extraction"),
                    );
                    let atlas_ok = match build_structural_atlas(
                        atlas.as_ref(),
                        &cid,
                        indexes.clone(),
                        recipes,
                    )
                    .await
                    {
                        StructuralAtlasOutcome::Built {
                            atoms_path,
                            edges_path,
                            elapsed_secs,
                        } => {
                            tracing::info!(
                                corpus = %cid,
                                atoms = %atoms_path.display(),
                                edges = %edges_path.display(),
                                elapsed_s = elapsed_secs,
                                "post-install: structural atlas — built"
                            );
                            let _ = EnrichmentStateFile::stamp(
                                &corpus_index_dir,
                                &cid,
                                Some("structural_atlas"),
                                EnrichmentPhase::Complete,
                                0,
                                0,
                                Some(&format!("structural atlas built in {elapsed_secs}s")),
                            );
                            // Glassbox: enrichment is heavy local
                            // inference work — record it so the
                            // Activity surface shows "enriched <corpus>"
                            // distinct from the raw ingest embed pass.
                            if let Err(e) = enrich_activity
                                .record(ActivityEventKind::CorpusEnriched {
                                    corpus_id: cid.clone(),
                                    atoms: 0,
                                    duration_secs: elapsed_secs as u64,
                                })
                                .await
                            {
                                tracing::warn!(
                                    corpus = %cid, error = %e,
                                    "post-install: the activity record did not reach the store"
                                );
                            }
                            true
                        }
                        StructuralAtlasOutcome::AlreadyPresent { atoms_path } => {
                            tracing::info!(
                                corpus = %cid,
                                atoms = %atoms_path.display(),
                                "post-install: structural atlas — already present"
                            );
                            let _ = EnrichmentStateFile::stamp(
                                &corpus_index_dir,
                                &cid,
                                Some("structural_atlas"),
                                EnrichmentPhase::Complete,
                                0,
                                0,
                                Some("structural atlas already present"),
                            );
                            true
                        }
                        StructuralAtlasOutcome::Failed { reason } => {
                            tracing::warn!(
                                corpus = %cid,
                                reason,
                                "post-install: structural atlas — failed (atlas grounding stays off until rebuilt)"
                            );
                            let _ = EnrichmentStateFile::fail(
                                &corpus_index_dir,
                                &cid,
                                &format!("structural atlas: {reason}"),
                            );
                            false
                        }
                    };

                    // Triage: rank in-corpus articles by centrality
                    // and persist the top-N for Tier-2 enrichment.
                    // Output is consumable by `sovereign enrich init
                    // --include-articles <path>` so the manual flow
                    // and the future daemon-side scheduler share one
                    // source of truth.
                    if atlas_ok {
                        // Honour per-corpus override (Phase B3) —
                        // operators set this via `sovereign atlas
                        // budget <corpus> <n>`. Default is 1000
                        // articles, which fits L1+L2+L3 with tier
                        // headroom on a wiki-scale atlas.
                        let budget = effective_tier2_budget(&indexes, &cid);
                        tracing::info!(
                            corpus = %cid,
                            budget,
                            "post-install: triage — start"
                        );
                        let triage_path_for_tier2 = match build_triage_candidates(
                            atlas.as_ref(),
                            &cid,
                            indexes.clone(),
                            budget,
                        )
                        .await
                        {
                            TriageOutcome::Built {
                                path,
                                in_corpus_picked,
                                elapsed_secs,
                            } => {
                                tracing::info!(
                                    corpus = %cid,
                                    path = %path.display(),
                                    articles = in_corpus_picked,
                                    elapsed_s = elapsed_secs,
                                    "post-install: triage — built"
                                );
                                Some(path)
                            }
                            TriageOutcome::NoAtlas => {
                                tracing::warn!(
                                    corpus = %cid,
                                    "post-install: triage skipped (atlas missing)"
                                );
                                None
                            }
                            TriageOutcome::Failed { reason } => {
                                tracing::warn!(
                                    corpus = %cid,
                                    reason,
                                    "post-install: triage failed"
                                );
                                None
                            }
                        };

                        // Tier-2 extraction: kick off the long-running
                        // background job that runs Phase 1 over every
                        // chapter of every triaged article. Detached
                        // subprocess — daemon doesn't block on it,
                        // logs go to <workspace>/extraction.log, and
                        // restart safety comes from the per-chapter
                        // checkpoint inherited from `enrich extract
                        // --resume`.
                        if let Some(triage_path) = triage_path_for_tier2 {
                            use sovereign_tools::atlas_postinstall::{
                                launch_tier2_extraction_with_advice, Tier2LaunchOutcome,
                            };
                            // Deep Tier-2 needs a real `sovereign-cli-llm`.
                            // If none is configured, skip the pass rather
                            // than self-execing the daemon/GUI binary (see
                            // `resolve_enrich_cli`). Nothing runs after this
                            // block in the spawned task, so an early return
                            // just ends the (already-detached) task cleanly;
                            // the structural atlas + inline tiered enrichment
                            // stamped above stay intact — we do NOT mark the
                            // corpus failed for skipping an optional pass.
                            let Some(cli_bin) = resolve_enrich_cli() else {
                                tracing::info!(
                                    corpus = %cid,
                                    "post-install: tier-2 deep extraction skipped — no `sovereign-cli-llm` configured (set $SOVEREIGN_CLI_LLM_BIN to run the referential-atlas pass); structural atlas + inline tiered enrichment already complete"
                                );
                                return;
                            };
                            let enrich_dir = indexes
                                .parent()
                                .unwrap_or(std::path::Path::new("."))
                                .join("enrichment");

                            // Phase C3: walk the live mesh and ask
                            // whether any peer already has a deeper
                            // atlas. If so, skip local extraction
                            // and log the recommendation — operator
                            // pulls via the canonical-sync surface.
                            let peer_advice =
                                gather_peer_atlas_advice(&state_for_task, &cid, &indexes).await;
                            if let Some(advice) = peer_advice.as_ref() {
                                tracing::info!(
                                    corpus = %cid,
                                    peer = %advice.peer_name,
                                    peer_tier2 = advice.peer_tier2_count,
                                    local_tier2 = advice.local_tier2_count,
                                    "post-install: tier-2 extraction — deferring to peer (Phase C3)"
                                );
                            } else {
                                tracing::info!(
                                    corpus = %cid,
                                    "post-install: tier-2 extraction — launching background"
                                );
                            }
                            match launch_tier2_extraction_with_advice(
                                &cid,
                                triage_path,
                                cli_bin,
                                enrich_dir,
                                indexes.clone(),
                                peer_advice,
                            )
                            .await
                            {
                                Tier2LaunchOutcome::Spawned {
                                    workspace_id,
                                    log_path,
                                    pid,
                                } => tracing::info!(
                                    corpus = %cid,
                                    workspace = %workspace_id,
                                    log = %log_path.display(),
                                    pid,
                                    "post-install: tier-2 extraction — spawned (tail extraction.log for progress)"
                                ),
                                Tier2LaunchOutcome::AlreadyComplete {
                                    workspace_id,
                                    chapters_done,
                                    chapters_total,
                                } => tracing::info!(
                                    corpus = %cid,
                                    workspace = %workspace_id,
                                    chapters_done,
                                    chapters_total,
                                    "post-install: tier-2 extraction — already complete"
                                ),
                                Tier2LaunchOutcome::DeferredToPeer {
                                    peer_name,
                                    peer_tier2_count,
                                    local_tier2_count,
                                } => tracing::info!(
                                    corpus = %cid,
                                    peer = %peer_name,
                                    peer_tier2 = peer_tier2_count,
                                    local_tier2 = local_tier2_count,
                                    "post-install: tier-2 extraction — deferred to peer (run `sovereign mesh canonical-pull {cid} --from {peer_name}` to fetch)"
                                ),
                                Tier2LaunchOutcome::InitFailed { reason }
                                | Tier2LaunchOutcome::SpawnFailed { reason } => tracing::warn!(
                                    corpus = %cid,
                                    reason,
                                    "post-install: tier-2 extraction — launch failed"
                                ),
                            }
                        }
                    }
                });
            }
            Err(corpus_index::Error::Cancelled(_)) => {
                // Cancel route handles the wipe; we only clean up
                // the progress map so the UI returns to
                // "not_installed" on the next poll.
                state_for_task
                    .inner
                    .ingest
                    .corpus_progress
                    .write()
                    .await
                    .remove(&corpus_id_for_task);
                tracing::info!(
                    corpus = %corpus_id_for_task,
                    "spawn_corpus_install: ingest cancelled"
                );
            }
            Err(e) => {
                // Recorded, not merely logged — see `record_failure`.
                // A log-only handler here is the bug that made every
                // ingest failure render as a completed install.
                record_failure(&state_for_task, &corpus_id_for_task, e.to_string()).await;
                tracing::warn!(
                    corpus = %corpus_id_for_task,
                    error = %e,
                    "spawn_corpus_install: ingest failed"
                );
            }
        }
    });
    InstallOutcome::Spawned
}

/// Take `corpus_id` back out of `active_ingests` on a path that spawned
/// nothing, so a retry is not blocked.
async fn release(state: &AppState, corpus_id: &str) {
    state
        .inner
        .ingest
        .active_ingests
        .write()
        .await
        .remove(corpus_id);
}

/// THE mapping from an install's outcome to its HTTP answer, shared by the
/// internal and the OICP install routes: `Ok(spawned)` for a start or a
/// benign no-op, and a named 4xx/5xx for everything that is a failure.
/// Until 2026-10-08 the OICP route projected the outcome to a `bool`, so an
/// invalid recipe or parameters answered 200 `spawned: false`
/// (ADDRESSED_TEXT appendix defect 2; OICP v0.4 §5.1 requires the 400).
pub fn install_status(
    outcome: InstallOutcome,
    corpus_id: &str,
) -> Result<bool, (axum::http::StatusCode, axum::Json<super::ErrorBody>)> {
    use axum::http::StatusCode;
    let refuse = |status: StatusCode, error: String| {
        tracing::info!(corpus = %corpus_id, %status, %error, "install: refused");
        Err((status, axum::Json(super::ErrorBody { error })))
    };
    match outcome {
        InstallOutcome::Spawned => Ok(true),
        InstallOutcome::AlreadyActive | InstallOutcome::AlreadyInstalled => Ok(false),
        InstallOutcome::NoEngine => refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            crate::hosted_ingest::NO_INGEST.into(),
        ),
        InstallOutcome::RecipeNotFound(reason) => refuse(
            StatusCode::NOT_FOUND,
            format!("cannot install '{corpus_id}': {reason}"),
        ),
        InstallOutcome::InvalidParameters(reason) => refuse(
            StatusCode::BAD_REQUEST,
            format!("invalid parameters for '{corpus_id}': {reason}"),
        ),
        InstallOutcome::InvalidRecipe(reason) => refuse(
            StatusCode::BAD_REQUEST,
            format!("invalid recipe for '{corpus_id}': {reason}"),
        ),
        InstallOutcome::ReplaceFailed(reason) => refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not remove the installed '{corpus_id}' to reingest it: {reason}"),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub corpus_id: String,
    /// Recipe-parameter values supplied by the user at install time.
    /// Validated against the recipe's `[recipe.parameters]` schema
    /// before the ingest task spawns, so a missing required param
    /// fails the request rather than silently producing an empty
    /// corpus. JSON shape: `{"name": value, ...}` where value can
    /// be a string, integer, or string array.
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct InstallResponse {
    pub corpus_id: String,
    /// True when a new task was spawned, false when an ingest for
    /// this corpus was already running on this node.
    pub spawned: bool,
}

/// The outcome of an install attempt, richer than the `bool` the
/// mesh/OICP callers consume. It exists so the HTTP handler can tell a
/// *benign* idempotent no-op (`AlreadyActive`) apart from a *genuine
/// failure* (`RecipeNotFound` / `InvalidParameters`) — the former is a
/// 200 with `spawned:false`, the latter a 4xx with a reason. Before this
/// split, every non-spawn collapsed to `spawned:false` + HTTP 200, so a
/// mistyped corpus id or a private recipe the daemon can't resolve looked
/// identical to "already running" and the CLI printed "Install requested"
/// over a silent failure. Glassbox: the caller now sees why nothing ran.
pub enum InstallOutcome {
    /// A new background ingest task was started.
    Spawned,
    /// An ingest for this corpus was already in flight — no new task.
    AlreadyActive,
    /// No corpus engine is wired on this node.
    NoEngine,
    /// The recipe could not be resolved: no local override, no catalog
    /// entry, and no bundled fallback. Carries the resolver's message.
    RecipeNotFound(String),
    /// Supplied parameters failed JSON→TOML coercion or schema
    /// validation against the recipe's `[recipe.parameters]` block.
    InvalidParameters(String),
    /// The corpus is installed from these exact recipe bytes already: the
    /// same `(corpus_id, recipe_sha256)` twice is a no-op.
    AlreadyInstalled,
    /// A supplied recipe did not load, or names another corpus.
    InvalidRecipe(String),
    /// A different recipe validated, and the installed index it replaces
    /// would not remove.
    ReplaceFailed(String),
}
