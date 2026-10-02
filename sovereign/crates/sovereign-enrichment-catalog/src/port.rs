// SPDX-License-Identifier: AGPL-3.0-or-later
//! The catalog as the implementor of ingest's enrichment-config port
//! (`corpus_index::ingest_port::enrich_config`, pb-ingest-dial-tools-close).
//! svrn reads a corpus's config and writes a watched folder's through the
//! port; a distribution that composes ingest hands svrn [`CatalogEnrichConfig`].

use std::path::PathBuf;

use corpus_index::ingest_port::enrich_config::{
    EnrichConfigPort, EnrichConfigSummary, WatchedEnrichConfig,
};
use corpus_index::Result;

use crate::{paths, EnrichConfig};

/// The enrichment catalog, as the port's one implementor.
#[derive(Debug, Clone, Copy, Default)]
pub struct CatalogEnrichConfig;

impl EnrichConfigPort for CatalogEnrichConfig {
    fn load(&self, corpus_id: &str) -> Result<Option<EnrichConfigSummary>> {
        Ok(
            EnrichConfig::load(corpus_id)?.map(|cfg| EnrichConfigSummary {
                declares_ontology: cfg.ontology.is_some(),
                pipeline_id: cfg.pipeline_id,
            }),
        )
    }

    /// `EnrichConfig::save` is the atomic writer (tmp + rename), and the
    /// path is the one accessor the CLI's `enrich build` reads.
    fn write_watched(&self, config: &WatchedEnrichConfig<'_>) -> Result<PathBuf> {
        let cfg = synthesize_watched_config(config);
        cfg.save()?;
        Ok(paths::config_path(&cfg.corpus_id))
    }
}

/// Synthesize a watched-folder enrich config.
///
/// The SCHEMA is [`EnrichConfig`] — the one the CLI reads and the desktop
/// lists. `sovereign-tools` used to carry a hand-written
/// mirror of it whose doc comment read "Mirrors
/// `sovereign_cli::enrich_cmd::config::EnrichConfig` field-for-field. Kept
/// separate so this crate doesn't depend on the CLI." It had drifted four
/// fields behind (`toc_markers`, `phase1b_max_output_tokens`,
/// `phase_overrides`, `ontology`), which is what a mirror does. The schema now
/// lives BELOW both, so there is nothing to mirror.
///
/// What lives here is the watched-folder POLICY, the driver's product
/// decision and not the schema's; it moved here with the write
/// (pb-ingest-dial-tools-close), so svrn hands over only the inputs:
///
/// - `chapter_regex = "^.*$"` — every doc is its own chapter. Watched folders
///   have no per-doc section structure for the pipeline to discover; treating
///   each file as one chapter matches how the chunker already segments them.
/// - `min_section_body_words = 0` — bypass the section-body floor that's
///   meaningful for SEP-style index pages but spurious for arbitrary file
///   collections.
/// - `max_output_tokens = 16_384` — covers thinking-model traces.
/// - `chat_models = None` — no per-phase overrides. Operators who care can
///   hand-edit the config later.
/// - `created_at = now (RFC3339)`.
fn synthesize_watched_config(config: &WatchedEnrichConfig<'_>) -> EnrichConfig {
    EnrichConfig {
        // The CLI refuses a config whose `schema_version` exceeds its own
        // build, so the driver must stay at the version the shared crate
        // declares — which is now literally the same constant, not a copy.
        schema_version: crate::CONFIG_SCHEMA_VERSION,
        corpus_id: config.corpus_id.to_string(),
        pipeline_id: config.pipeline_id.to_string(),
        source_path: config.source_path.to_path_buf(),
        chapter_regex: "^.*$".to_string(),
        chat_model: config.chat_model.to_string(),
        chat_models: None,
        embed_model: config.embed_model.to_string(),
        base_url: config.base_url.to_string(),
        embed_base_url: None,
        min_section_body_words: 0,
        toc_markers: None,
        max_output_tokens: 16_384,
        phase1b_max_output_tokens: None,
        phase_overrides: None,
        ontology: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn watched<'a>(
        corpus_id: &'a str,
        pipeline_id: &'a str,
        source: &'a str,
    ) -> WatchedEnrichConfig<'a> {
        WatchedEnrichConfig {
            corpus_id,
            pipeline_id,
            source_path: Path::new(source),
            chat_model: "qwen3-32b",
            embed_model: "qwen3-embedding-0.6b",
            base_url: "http://localhost:9741",
        }
    }

    #[test]
    fn synthesize_defaults_match_v1_posture() {
        let cfg =
            synthesize_watched_config(&watched("test-corpus", "philosophy_atlas", "/tmp/notes"));
        assert_eq!(cfg.corpus_id, "test-corpus");
        assert_eq!(cfg.pipeline_id, "philosophy_atlas");
        assert_eq!(cfg.source_path, PathBuf::from("/tmp/notes"));
        // §3.3 watched-folder defaults: every doc as its own chapter,
        // no section-body floor, no per-phase overrides.
        assert_eq!(cfg.chapter_regex, "^.*$");
        assert_eq!(cfg.min_section_body_words, 0);
        assert!(cfg.chat_models.is_none());
        assert_eq!(cfg.max_output_tokens, 16_384);
    }

    #[test]
    fn synthesize_round_trips_cli_compatible_json() {
        // The CLI's EnrichConfig::load deserializes from this same
        // shape; pin field names + camelCase-vs-snake conventions
        // so a refactor that breaks JSON compatibility surfaces
        // before the subprocess reads the file.
        let cfg = synthesize_watched_config(&watched("c1", "literary_atlas", "/tmp/x"));
        let json = serde_json::to_value(&cfg).unwrap();
        // CLI required fields:
        for field in [
            "schema_version",
            "corpus_id",
            "pipeline_id",
            "source_path",
            "chapter_regex",
            "chat_model",
            "embed_model",
            "base_url",
            "min_section_body_words",
            "max_output_tokens",
            "created_at",
        ] {
            assert!(
                json.get(field).is_some(),
                "missing required field {field} in {json}"
            );
        }
        // Skip-if-none fields must be absent on default:
        assert!(
            json.get("chat_models").is_none(),
            "chat_models must be skipped when None: {json}"
        );
    }
}
