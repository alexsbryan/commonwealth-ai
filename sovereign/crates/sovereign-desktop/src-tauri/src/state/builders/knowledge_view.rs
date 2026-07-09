// SPDX-License-Identifier: AGPL-3.0-or-later
//! KnowledgeView manager construction — extracted verbatim from
//! `bootstrap_with_progress` (§3.3). The desktop mirror of the
//! server/CLI KnowledgeView wire-up. Narrowed to the values it needs
//! (not `&AppState`, ARCH_PRINCIPLES §5.2): it produces the optional
//! `KnowledgeViewManager` the Runtime later receives as its
//! landscape-digest provider, and installs the manager as the SQLite
//! store's write observer as its only side effect.
//!
//! Construction is model-free (the `InferenceFn` is captured, not
//! called, until enrichment runs), so the whole builder is exercised
//! in CI against a stub provider + a temp corpus engine — see tests.

use std::sync::Arc;

use corpus_engine::CorpusEngine;
use sovereign_core::observer::SharedStateStoreObserver;
use sovereign_core::SkillRegistry;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::knowledge_view::KnowledgeViewManager;
use tokio::sync::RwLock;

use crate::state::DesktopConfig;

/// Build the desktop-side `KnowledgeViewManager`, gated (in precedence
/// order) on:
///
/// 1. **Attach mode** — when a CLI daemon at `:9741` is the source of
///    truth, IT owns the `KnowledgeViewManager` (see
///    `sovereign-cli/src/daemon_cmd.rs`). Constructing one here too
///    means a duplicate observer fires on every conversation write, two
///    debouncers race to ingest the same view, and two enrichment loops
///    compete for the chat slot. Skip entirely. (Consequence: landscape
///    digests are NOT spliced into prompts on the desktop side in attach
///    mode — the daemon has the digest data but no HTTP endpoint exposes
///    it yet. TODO: add `/v1/knowledge/landscape_digest` on the daemon +
///    a thin client-side `LandscapeDigestProvider` that fetches over
///    HTTP, then wire that into the runtime.)
///
/// 2. **Settings → Knowledge → Enable KnowledgeView** — when the user
///    has explicitly disabled the feature, svrnmesh behaves exactly as
///    it did before KnowledgeView existed.
///
/// 3. Otherwise (Local / CliSetup mode, feature on) build the manager:
///    the Runtime gets a landscape-digest provider and the manager is
///    installed as the SQLite store's observer so it wires into writes.
///
/// The toggle is read once at startup — changing the Settings toggle or
/// the bootstrap mode requires a desktop restart, because the Runtime is
/// built once with or without the provider.
pub(crate) async fn build_knowledge_view(
    is_attach_mode: bool,
    config: &DesktopConfig,
    skills: &SkillRegistry,
    corpus_engine: &Arc<CorpusEngine>,
    inference_fn: &corpus_engine::InferenceFn,
    inference: &Arc<dyn sovereign_core::traits::InferenceProvider>,
    sqlite_store: &RwLock<Option<Arc<SqliteStateStore>>>,
) -> Option<Arc<KnowledgeViewManager>> {
    if is_attach_mode {
        tracing::info!(
            "knowledge_view: attach mode — CLI daemon owns enrichment, \
             skipping desktop-side construction. Landscape digests in \
             chat splice are deferred until the daemon exposes an HTTP \
             endpoint."
        );
        None
    } else if config.knowledge_view_enabled {
        let knowledge_view_db_path = config.data_dir.join("sovereign.db");
        // Resolve local_only skill ids from the registry loaded above.
        // Mirror of the server/CLI paths.
        let local_only_skill_ids = skills.local_only_skill_ids();
        tracing::info!(
            local_only_skills = ?local_only_skill_ids,
            "knowledge_view: enabled; skills excluded from conversational corpus"
        );
        // Project-local ATOS paths — same `.sovereign/` layout as the
        // CLI / server bootstraps. Optional; the splice path's strategic
        // block falls through gracefully when either path is missing.
        let project_sov_dir = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".sovereign");
        let features_db_path = project_sov_dir.join("features.db");
        let project_toml_path = project_sov_dir.join("project.toml");
        let mut mgr = KnowledgeViewManager::new(
            Arc::clone(corpus_engine),
            inference_fn.clone(),
            knowledge_view_db_path,
            local_only_skill_ids,
        )
        .await;
        if features_db_path.exists() {
            mgr = mgr.with_features_db_path(features_db_path);
        }
        if project_toml_path.exists() {
            mgr = mgr.with_project_toml_path(project_toml_path);
        }
        let mgr = Arc::new(mgr);
        if let Some(concrete) = sqlite_store.read().await.as_ref() {
            concrete.set_observer(mgr.clone() as SharedStateStoreObserver);
            // Memory-pool RAPTOR rebuild (T3 tiered-retrieval memory
            // port): the debouncer's MemoryTouched window rebuilds the
            // per-scope memory trees alongside the personal view.
            mgr.install_memory_atlas(
                concrete.clone() as Arc<dyn sovereign_core::traits::StateStore>,
                Arc::clone(inference),
            )
            .await;
        } else {
            tracing::warn!(
                "KnowledgeView: desktop store was not SQLite-backed; \
                 observer not installed (memory-mode fallback?)"
            );
        }
        Some(mgr)
    } else {
        tracing::info!(
            "knowledge_view: disabled via Settings — landscape digests \
             skipped, no ingest will run"
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::builders::test_support::{temp_corpus_engine, StubInference};
    use sovereign_core::traits::InferenceProvider;

    fn stub_inference_fn() -> corpus_engine::InferenceFn {
        sovereign_tools::corpus::inference_to_inference_fn(
            Arc::new(StubInference) as Arc<dyn InferenceProvider>
        )
    }

    /// Attach mode hands enrichment to the CLI daemon — the desktop must
    /// NOT construct a second manager (the duplicate-observer hazard).
    #[tokio::test]
    async fn attach_mode_skips_construction() {
        let (_tmp, corpus_engine) = temp_corpus_engine();
        let inference_fn = stub_inference_fn();
        let config = DesktopConfig::default();
        let skills = SkillRegistry::new();
        let sqlite_store = RwLock::new(None);

        let mgr = build_knowledge_view(
            /* is_attach_mode */ true,
            &config,
            &skills,
            &corpus_engine,
            &inference_fn,
            &(Arc::new(StubInference) as Arc<dyn InferenceProvider>),
            &sqlite_store,
        )
        .await;

        assert!(mgr.is_none(), "attach mode must skip desktop construction");
    }

    /// Feature toggled off → behaves as pre-KnowledgeView (no manager).
    #[tokio::test]
    async fn disabled_via_settings_returns_none() {
        let (_tmp, corpus_engine) = temp_corpus_engine();
        let inference_fn = stub_inference_fn();
        let mut config = DesktopConfig::default();
        config.knowledge_view_enabled = false;
        let skills = SkillRegistry::new();
        let sqlite_store = RwLock::new(None);

        let mgr = build_knowledge_view(
            false,
            &config,
            &skills,
            &corpus_engine,
            &inference_fn,
            &(Arc::new(StubInference) as Arc<dyn InferenceProvider>),
            &sqlite_store,
        )
        .await;

        assert!(mgr.is_none(), "disabled feature must not build a manager");
    }

    /// Local mode + feature on builds the manager with no model loaded —
    /// the `InferenceFn` is captured, not called, at construction. A
    /// `None` sqlite_store exercises the "store not SQLite-backed →
    /// observer not installed" warn branch without a real store.
    #[tokio::test]
    async fn enabled_builds_manager_without_a_model() {
        let (tmp, corpus_engine) = temp_corpus_engine();
        let inference_fn = stub_inference_fn();
        let mut config = DesktopConfig::default();
        config.knowledge_view_enabled = true;
        config.data_dir = tmp.path().to_path_buf();
        let skills = SkillRegistry::new();
        let sqlite_store = RwLock::new(None);

        let mgr = build_knowledge_view(
            false,
            &config,
            &skills,
            &corpus_engine,
            &inference_fn,
            &(Arc::new(StubInference) as Arc<dyn InferenceProvider>),
            &sqlite_store,
        )
        .await;

        assert!(
            mgr.is_some(),
            "Local mode + feature on must construct the manager model-free"
        );
    }
}
