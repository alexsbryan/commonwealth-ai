// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the on-demand recipe guard in `CorpusEngine::ingest`.
//!
//! On-demand recipes (e.g. `gutenberg-work`) carry a placeholder
//! `[corpus] id` and a placeholder acquire URL. Running them by id
//! without any override would write to a junk corpus dir and try to
//! download the placeholder. The guard fires before any side effects
//! and rejects with a clear actionable error.
//!
//! Companion: an Inline-spec ingest (used by `CatalogIngestService`)
//! is exercised end-to-end in the catalog ingest e2e tests; here we
//! cover the rejection path because it has no embed / network deps
//! and pins the contract independent of the rest of the ingest
//! pipeline.

use std::pin::Pin;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, Error};

/// Tiny no-op embed fn — never called by the on-demand guard, so the
/// values don't matter.
fn dummy_embed_fn() -> EmbedFn {
    Arc::new(
        |_text: &str| -> Pin<
            Box<
                dyn std::future::Future<
                        Output = std::result::Result<Vec<f32>, corpus_engine::Error>,
                    > + Send,
            >,
        > { Box::pin(async { Ok(vec![0.0_f32; 768]) }) },
    )
}

#[tokio::test]
async fn ingesting_on_demand_recipe_by_id_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    std::fs::create_dir_all(tmp.path().join("recipes")).unwrap();
    let engine = CorpusEngine::new(
        tmp.path().join("recipes"),
        tmp.path().join("indexes"),
        dummy_embed_fn(),
    )
    .with_embedding_model("dummy-embed-768");

    let result: corpus_engine::Result<corpus_engine::IngestResult> = engine
        .ingest(&CorpusSpec::Builtin("gutenberg-work".to_string()), None)
        .await;

    let err = result.expect_err(
        "on-demand recipe ingested by id must be rejected — \
         a misclick on `gutenberg-work` would otherwise blast the \
         placeholder URL into a real corpus dir",
    );

    match err {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("on_demand"),
                "rejection message should mention on_demand; got: {msg}"
            );
            assert!(
                msg.contains("gutenberg-work"),
                "rejection message should name the offending recipe; got: {msg}"
            );
            assert!(
                msg.contains("CorpusSpec::Inline"),
                "rejection message should point at the Inline override path; got: {msg}"
            );
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // No partial index dir should have been created — the guard
    // fires before any disk side effects.
    let indexes = std::fs::read_dir(tmp.path().join("indexes"))
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(
        indexes, 0,
        "on-demand guard must fire before any index dir is created"
    );
}
