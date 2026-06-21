//! Single source of truth for assembling a fully-wired [`LlmRouter`].
//!
//! Every product surface — the CLI/bench bootstrap, the desktop app, and the
//! served daemon — MUST build its router through [`build_llm_router`] so the
//! classifier stack is **identical everywhere**. This is the structural fix for
//! the bench-vs-desktop divergence found 2026-06-09: the embed router + scope +
//! effort classifiers had been wired ONLY in the CLI/bench bootstrap, so desktop
//! chat and the served daemon silently under-routed to the fast slot while the
//! benches (which DID wire them) reported steady improvements — "desktop kind of
//! sucks even as the benches get better." Parity is now by construction: there
//! is one wiring path, and it lives here.
//!
//! Exemplars are **baked** into the binary (`include_str!`) so the stack works
//! regardless of CWD or `.app`-bundle layout; an on-disk file (env var or
//! repo-relative) overrides the baked default when present (mirrors the recipe
//! SSOT pattern). Each classifier soft-degrades independently — a load failure
//! logs a warning and leaves that one pre-check off rather than failing boot.

use std::path::PathBuf;
use std::sync::Arc;

use crate::current_info_classifier::CurrentInfoClassifier;
use crate::effort_classifier::EffortClassifier;
use crate::error::{Error, Result};
use crate::router::LlmRouter;
use crate::router_embed::EmbedRouter;
use crate::scope_classifier::PersonalScopeClassifier;
use crate::skills::SkillRegistry;
use crate::traits::{InferenceProvider, StateStore};

/// Baked exemplar defaults — the same bytes as the editable on-disk files under
/// `sovereign/router/`, vendored at compile time so every binary has them.
pub const BAKED_ROUTER_EXEMPLARS: &str = include_str!("../../../router/exemplars.toml");
pub const BAKED_SCOPE_EXAMPLES: &str = include_str!("../../../router/scope_examples.toml");
pub const BAKED_EFFORT_EXAMPLES: &str = include_str!("../../../router/effort_examples.toml");
pub const BAKED_CURRENT_INFO_EXAMPLES: &str =
    include_str!("../../../router/current_info_examples.toml");

/// Every `(method, text)` pair the four boot classifiers embed, in boot order.
/// `method` is the embed-cache key space: `"q"` (instruction-prefixed
/// `embed_query`) for the embed-router / scope / current-info classifiers,
/// `"d"` (unprefixed `embed`) for the effort classifier (see
/// `effort_classifier::compute_centroid` for why effort is unprefixed). This is
/// the SSOT for the router-embed cache freshness gate: each text comes from the
/// classifier's own parse-only `exemplar_texts`, so the gate can never drift
/// from what `build_llm_router` actually caches. Takes the four TOML bodies so
/// one code path serves both the binary-baked exemplars and an on-disk working
/// tree (the release/bump hook checking uncommitted edits).
pub fn exemplar_specs(
    router: &str,
    scope: &str,
    effort: &str,
    current_info: &str,
) -> Result<Vec<(&'static str, String)>> {
    let mut specs = Vec::new();
    for t in EmbedRouter::exemplar_texts(router)? {
        specs.push(("q", t));
    }
    for t in PersonalScopeClassifier::exemplar_texts(scope)? {
        specs.push(("q", t));
    }
    for t in EffortClassifier::exemplar_texts(effort)? {
        specs.push(("d", t));
    }
    for t in CurrentInfoClassifier::exemplar_texts(current_info)? {
        specs.push(("q", t));
    }
    Ok(specs)
}

/// [`exemplar_specs`] over the binary-baked exemplar TOMLs — the set the shipped
/// runtime serves and the CI freshness test gates against.
pub fn baked_exemplar_specs() -> Result<Vec<(&'static str, String)>> {
    exemplar_specs(
        BAKED_ROUTER_EXEMPLARS,
        BAKED_SCOPE_EXAMPLES,
        BAKED_EFFORT_EXAMPLES,
        BAKED_CURRENT_INFO_EXAMPLES,
    )
}

/// Optional on-disk overrides for each exemplar set. `None` → use the baked
/// default. [`Self::from_env_and_repo`] is the standard resolver and works for
/// every surface: a missing repo-relative file (a packaged app) simply falls
/// through to the baked default.
#[derive(Debug, Default, Clone)]
pub struct ExemplarOverrides {
    pub router: Option<PathBuf>,
    pub scope: Option<PathBuf>,
    pub effort: Option<PathBuf>,
    pub current_info: Option<PathBuf>,
}

impl ExemplarOverrides {
    /// Resolve each set from its `SOVEREIGN_*` env var, then a repo-relative
    /// `sovereign/router/*.toml` (present in a dev checkout, absent in a
    /// packaged app). Anything unresolved stays `None` → the baked default.
    pub fn from_env_and_repo() -> Self {
        Self {
            router: resolve(
                "SOVEREIGN_ROUTER_EXEMPLARS",
                "sovereign/router/exemplars.toml",
            ),
            scope: resolve(
                "SOVEREIGN_SCOPE_EXAMPLES",
                "sovereign/router/scope_examples.toml",
            ),
            effort: resolve(
                "SOVEREIGN_EFFORT_EXAMPLES",
                "sovereign/router/effort_examples.toml",
            ),
            current_info: resolve(
                "SOVEREIGN_CURRENT_INFO_EXAMPLES",
                "sovereign/router/current_info_examples.toml",
            ),
        }
    }
}

fn resolve(env_key: &str, repo_relative: &str) -> Option<PathBuf> {
    if let Ok(v) = std::env::var(env_key) {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
        tracing::warn!(
            target: "router.bootstrap",
            env = env_key,
            path = %p.display(),
            "exemplar override set but file missing; falling through to repo/baked"
        );
    }
    let p = PathBuf::from(repo_relative);
    p.is_file().then_some(p)
}

/// Where a wired classifier's exemplars came from — for the glassbox boot log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierSource {
    Path(PathBuf),
    Baked,
}

/// Glassbox report of which classifiers the router got. `Some` = wired; `None` =
/// load failed and the router falls back to its non-classifier path for that
/// pre-check. Logged at boot and asserted by the parity test.
#[derive(Debug, Default, Clone)]
pub struct RouterBuildReport {
    pub embed_router: Option<ClassifierSource>,
    pub scope: Option<ClassifierSource>,
    pub effort: Option<ClassifierSource>,
    pub current_info: Option<ClassifierSource>,
}

impl RouterBuildReport {
    /// True when every classifier wired — the parity invariant each surface
    /// expects on a healthy boot.
    pub fn all_wired(&self) -> bool {
        self.embed_router.is_some()
            && self.scope.is_some()
            && self.effort.is_some()
            && self.current_info.is_some()
    }
}

/// Assemble a fully-wired [`LlmRouter`]: embed router → scope → effort →
/// current-info, each from its override path when present else the baked
/// default, each soft-degrading independently. Returns the router plus a
/// [`RouterBuildReport`] for the caller to log.
pub async fn build_llm_router(
    inference: Arc<dyn InferenceProvider>,
    store: Arc<dyn StateStore>,
    skills: Arc<SkillRegistry>,
    overrides: &ExemplarOverrides,
) -> (LlmRouter, RouterBuildReport) {
    let mut router = LlmRouter::new(Arc::clone(&inference), store, skills);
    let mut report = RouterBuildReport::default();

    // Exemplar embeddings are static per (text, embed model); without
    // the cache the four classifiers below re-embed ~310 strings
    // sequentially at every boot (~5.7s of desktop splash, measured
    // 2026-06-10). Validity is a sentinel cosine probe inside `open`.
    let mut embed_cache = crate::router_embed_cache::BootEmbedCache::open(&*inference).await;

    // 1. Embed router — the primary, deterministic intent pre-check. When it
    //    returns a confident verdict the router skips the heuristic + LLM
    //    cascade entirely.
    match load_embed(&overrides.router, &inference, &mut embed_cache).await {
        Ok((er, src)) => {
            tracing::info!(target: "router.bootstrap", exemplars = er.exemplar_count(), source = ?src, "embed router wired");
            router = router.with_embed_router(Arc::new(er));
            report.embed_router = Some(src);
        }
        Err(e) => tracing::warn!(target: "router.bootstrap", error = %e,
            "embed router unavailable; heuristic + LLM fallback"),
    }

    // 2. Personal-scope classifier — populates `RouterClassification.scope`.
    match load_scope(&overrides.scope, &inference, &mut embed_cache).await {
        Ok((c, src)) => {
            tracing::info!(target: "router.bootstrap", personal = c.personal_count(), external = c.external_count(), source = ?src, "scope classifier wired");
            router = router.with_scope_classifier(Arc::new(c));
            report.scope = Some(src);
        }
        Err(e) => tracing::warn!(target: "router.bootstrap", error = %e,
            "scope classifier unavailable; routing without personal-scope bias"),
    }

    // 3. Effort classifier — escalates a high-effort referential `Answer` to
    //    `DeepQuery` so exhaustive asks reach the primary slot.
    match load_effort(&overrides.effort, &inference, &mut embed_cache).await {
        Ok((c, src)) => {
            tracing::info!(target: "router.bootstrap", high = c.high_count(), low = c.low_count(), source = ?src, "effort classifier wired");
            router = router.with_effort_classifier(Arc::new(c));
            report.effort = Some(src);
        }
        Err(e) => tracing::warn!(target: "router.bootstrap", error = %e,
            "effort classifier unavailable; routing without effort-tier escalation"),
    }

    // 4. Current-info classifier — drives the `force_action` pre-check
    //    (time-sensitivity) instead of the keyword heuristic.
    match load_current_info(&overrides.current_info, &inference, &mut embed_cache).await {
        Ok((c, src)) => {
            tracing::info!(target: "router.bootstrap", current = c.current_count(), evergreen = c.evergreen_count(), source = ?src, "current-info classifier wired");
            router = router.with_current_info_classifier(Arc::new(c));
            report.current_info = Some(src);
        }
        Err(e) => tracing::warn!(target: "router.bootstrap", error = %e,
            "current-info classifier unavailable; force_action falls back to keyword heuristic"),
    }

    embed_cache.flush();

    tracing::info!(
        target: "router.bootstrap",
        embed = report.embed_router.is_some(),
        scope = report.scope.is_some(),
        effort = report.effort.is_some(),
        current_info = report.current_info.is_some(),
        all_wired = report.all_wired(),
        "router classifier stack assembled"
    );
    (router, report)
}

/// Read an override file for the cached constructors below. Same
/// error surface the classifiers' own `load` produces.
fn read_override(p: &PathBuf, what: &str) -> Result<String> {
    std::fs::read_to_string(p)
        .map_err(|e| Error::InvalidInput(format!("read {what} {}: {e}", p.display())))
}

async fn load_embed(
    over: &Option<PathBuf>,
    inf: &Arc<dyn InferenceProvider>,
    cache: &mut crate::router_embed_cache::BootEmbedCache,
) -> Result<(EmbedRouter, ClassifierSource)> {
    Ok(match over {
        Some(p) => (
            EmbedRouter::from_toml_str_cached(
                &read_override(p, "exemplars")?,
                Arc::clone(inf),
                Some(cache),
            )
            .await?,
            ClassifierSource::Path(p.clone()),
        ),
        None => (
            EmbedRouter::from_toml_str_cached(BAKED_ROUTER_EXEMPLARS, Arc::clone(inf), Some(cache))
                .await?,
            ClassifierSource::Baked,
        ),
    })
}

async fn load_scope(
    over: &Option<PathBuf>,
    inf: &Arc<dyn InferenceProvider>,
    cache: &mut crate::router_embed_cache::BootEmbedCache,
) -> Result<(PersonalScopeClassifier, ClassifierSource)> {
    Ok(match over {
        Some(p) => (
            PersonalScopeClassifier::from_toml_str_cached(
                &read_override(p, "scope examples")?,
                Arc::clone(inf),
                Some(cache),
            )
            .await?,
            ClassifierSource::Path(p.clone()),
        ),
        None => (
            PersonalScopeClassifier::from_toml_str_cached(
                BAKED_SCOPE_EXAMPLES,
                Arc::clone(inf),
                Some(cache),
            )
            .await?,
            ClassifierSource::Baked,
        ),
    })
}

async fn load_effort(
    over: &Option<PathBuf>,
    inf: &Arc<dyn InferenceProvider>,
    cache: &mut crate::router_embed_cache::BootEmbedCache,
) -> Result<(EffortClassifier, ClassifierSource)> {
    Ok(match over {
        Some(p) => (
            EffortClassifier::from_toml_str_cached(
                &read_override(p, "effort examples")?,
                Arc::clone(inf),
                Some(cache),
            )
            .await?,
            ClassifierSource::Path(p.clone()),
        ),
        None => (
            EffortClassifier::from_toml_str_cached(
                BAKED_EFFORT_EXAMPLES,
                Arc::clone(inf),
                Some(cache),
            )
            .await?,
            ClassifierSource::Baked,
        ),
    })
}

async fn load_current_info(
    over: &Option<PathBuf>,
    inf: &Arc<dyn InferenceProvider>,
    cache: &mut crate::router_embed_cache::BootEmbedCache,
) -> Result<(CurrentInfoClassifier, ClassifierSource)> {
    Ok(match over {
        Some(p) => (
            CurrentInfoClassifier::from_toml_str_cached(
                &read_override(p, "current-info examples")?,
                Arc::clone(inf),
                Some(cache),
            )
            .await?,
            ClassifierSource::Path(p.clone()),
        ),
        None => (
            CurrentInfoClassifier::from_toml_str_cached(
                BAKED_CURRENT_INFO_EXAMPLES,
                Arc::clone(inf),
                Some(cache),
            )
            .await?,
            ClassifierSource::Baked,
        ),
    })
}
