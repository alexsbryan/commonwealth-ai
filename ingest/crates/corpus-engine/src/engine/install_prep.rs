// SPDX-License-Identifier: AGPL-3.0-or-later
//! The half of preparing an install that registry and recipe installs share:
//! resolve the parameters against the recipe, and hand back the ingest to
//! run. A sibling of `daemon_port.rs`, kept out of it so the port impl stays
//! under its size band.

use std::collections::BTreeMap;
use std::sync::Arc;

use corpus_index::ingest_port::daemon::{InstallRefusal, PreparedInstall};

use super::json_params_to_toml;
use crate::engine::CorpusEngine;
use crate::types::CorpusSpec;
use crate::Recipe;

/// What an install that carried its recipe owes the corpus after it lands:
/// the recipe in the local registry, where every later reader resolves it by
/// id (enrich init, recipe sharing, expand), and its sha256 stamped on the
/// index (ADDRESSED_TEXT §5.6).
pub(super) struct Stamp {
    /// The recipe as declared, before its parameters were resolved: what the
    /// registry keeps beside the text.
    pub(super) declared: Recipe,
    /// The TOML exactly as it arrived; the stamp names these bytes.
    pub(super) text: String,
}

/// Coerce `parameters` and resolve them against `recipe`, naming the step
/// that refused.
pub(super) fn with_parameters(
    corpus_id: &str,
    recipe: Recipe,
    parameters: &BTreeMap<String, serde_json::Value>,
) -> Result<Recipe, InstallRefusal> {
    let toml_params = json_params_to_toml(parameters).map_err(|e| {
        tracing::warn!(corpus = %corpus_id, error = %e, "spawn_corpus_install: parameter coercion failed");
        InstallRefusal::InvalidParameters(e)
    })?;
    let resolved = recipe.resolve_parameters(&toml_params).map_err(|e| {
        tracing::warn!(corpus = %corpus_id, error = %e, "spawn_corpus_install: parameter validation failed");
        InstallRefusal::InvalidParameters(e.to_string())
    })?;
    Ok(recipe.with_resolved_parameters(resolved))
}

/// The ingest of `recipe`, plus the registry write and the stamp when the
/// install carried its recipe. The stamp lands only after the ingest
/// succeeded, so a corpus is never stamped with a recipe it did not finish.
pub(super) fn prepared(
    engine: Arc<CorpusEngine>,
    recipe: Recipe,
    stamp: Option<Stamp>,
) -> PreparedInstall {
    let opts_out_of_auto_enrichment = recipe.opts_out_of_auto_enrichment();
    PreparedInstall {
        opts_out_of_auto_enrichment,
        run: Box::new(move |progress| {
            Box::pin(async move {
                let corpus_id = recipe.corpus.id.clone();
                if let Some(s) = &stamp {
                    engine
                        .registry()
                        .install_local_recipe(&s.declared, &s.text)?;
                }
                let result = engine
                    .ingest(&CorpusSpec::Inline(Box::new(recipe)), progress)
                    .await?;
                if let Some(s) = stamp {
                    let sha = corpus_index::corpus::recipe_sha256(&s.text);
                    let corpus =
                        corpus_index::corpus::Corpus::named(engine.index_dir(), &corpus_id);
                    match corpus.map(|c| c.stamp_recipe_sha256(&sha)) {
                        Some(Ok(())) => {
                            tracing::info!(corpus = %corpus_id, %sha, "install: recipe stamped")
                        }
                        Some(Err(e)) => tracing::warn!(
                            corpus = %corpus_id,
                            error = %e,
                            "install: ingested, but the recipe stamp did not write; a second \
                             identical install will reingest"
                        ),
                        None => tracing::warn!("install: an empty corpus id, nothing stamped"),
                    }
                }
                Ok(result)
            })
        }),
    }
}
