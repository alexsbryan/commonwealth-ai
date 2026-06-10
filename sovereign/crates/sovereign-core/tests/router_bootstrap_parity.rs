//! Parity guard for the shared router bootstrap.
//!
//! The bench-vs-desktop divergence (2026-06-09) happened because the classifier
//! stack was wired in only one of three call sites. The structural fix is the
//! single [`router_bootstrap::build_llm_router`] path + baked exemplars. These
//! tests guard the two ways that fix could silently rot:
//!
//! 1. A broken bake (wrong `include_str!` path, malformed TOML, an empty class)
//!    would make a classifier fail to load — the router would fall back and the
//!    desktop would silently under-route again. `baked_*` tests assert every
//!    baked set parses + builds.
//! 2. `build_llm_router` must wire ALL FOUR classifiers on a healthy boot —
//!    `all_wired()` is the invariant every surface relies on.
//!
//! Uses `DeterministicInference` (no GPU/network/model, ARCH §12.4). Its `embed`
//! returns a fixed zero vector, so the centroids are degenerate — irrelevant
//! here: these tests check *that the stack assembles*, not classification
//! quality (which the live benches cover).

use std::sync::Arc;

use sovereign_core::current_info_classifier::CurrentInfoClassifier;
use sovereign_core::effort_classifier::EffortClassifier;
use sovereign_core::router_bootstrap::{
    build_llm_router, ExemplarOverrides, BAKED_CURRENT_INFO_EXAMPLES, BAKED_EFFORT_EXAMPLES,
    BAKED_ROUTER_EXEMPLARS, BAKED_SCOPE_EXAMPLES,
};
use sovereign_core::router_embed::EmbedRouter;
use sovereign_core::scope_classifier::PersonalScopeClassifier;
use sovereign_core::skills::SkillRegistry;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_store::sqlite::SqliteStateStore;

mod harness;
use harness::DeterministicInference;

fn inference() -> Arc<dyn InferenceProvider> {
    Arc::new(DeterministicInference)
}

#[tokio::test]
async fn baked_embed_exemplars_parse_and_build() {
    let er = EmbedRouter::from_toml_str(BAKED_ROUTER_EXEMPLARS, inference())
        .await
        .expect("baked exemplars.toml must parse + embed");
    // The on-disk file ships well over 100 exemplars; guard against a truncated
    // or wrong-file bake.
    assert!(
        er.exemplar_count() > 50,
        "expected a substantial exemplar set, got {}",
        er.exemplar_count()
    );
}

#[tokio::test]
async fn baked_scope_examples_parse_and_build() {
    let c = PersonalScopeClassifier::from_toml_str(BAKED_SCOPE_EXAMPLES, inference())
        .await
        .expect("baked scope_examples.toml must parse + embed");
    assert!(c.personal_count() > 0 && c.external_count() > 0);
}

#[tokio::test]
async fn baked_effort_examples_parse_and_build() {
    let c = EffortClassifier::from_toml_str(BAKED_EFFORT_EXAMPLES, inference())
        .await
        .expect("baked effort_examples.toml must parse + embed");
    assert!(c.high_count() > 0 && c.low_count() > 0);
}

#[tokio::test]
async fn baked_current_info_examples_parse_and_build() {
    let c = CurrentInfoClassifier::from_toml_str(BAKED_CURRENT_INFO_EXAMPLES, inference())
        .await
        .expect("baked current_info_examples.toml must parse + embed");
    assert!(c.current_count() > 0 && c.evergreen_count() > 0);
}

/// The core parity invariant: a healthy boot from the baked defaults wires all
/// four classifiers. If a future edit drops a `with_*` call or breaks a bake,
/// `all_wired()` flips false here long before a user notices desktop chat
/// regressing.
#[tokio::test]
async fn build_llm_router_wires_full_stack_from_baked() {
    let store: Arc<dyn StateStore> = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let skills = Arc::new(SkillRegistry::new());
    // `default()` forces the baked set (all overrides None), independent of CWD.
    let (_router, report) =
        build_llm_router(inference(), store, skills, &ExemplarOverrides::default()).await;
    assert!(
        report.all_wired(),
        "classifier stack must be fully wired from baked exemplars; got {report:?}"
    );
}
