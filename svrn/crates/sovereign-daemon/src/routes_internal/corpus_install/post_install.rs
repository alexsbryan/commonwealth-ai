// SPDX-License-Identifier: AGPL-3.0-or-later
//! The post-install hook's two lookups: which CLI runs the deep Tier-2
//! pass, and whether a peer's atlas is worth pulling instead. Moved whole
//! from `corpus_install.rs` to keep it out of the arch-gate approach band;
//! behaviour-preserving.

use crate::state::AppState;

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
pub(super) fn resolve_enrich_cli() -> Option<std::path::PathBuf> {
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
pub(super) async fn gather_peer_atlas_advice(
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
