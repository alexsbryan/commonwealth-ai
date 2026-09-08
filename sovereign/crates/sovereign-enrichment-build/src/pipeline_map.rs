// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pipeline's map, written beside the atoms — at build time by the
//! resolve step, and AFTER the fact by `svrn atlas migrate-all` for every
//! atlas built before the map existed (map-conversion rung 3, 2026-09-08).
//!
//! Convert, never regenerate: an installed atlas already has its atoms,
//! edges, store and seed table; the ONE artefact it lacks is
//! `atlas/ontology.json`, and that file is a DECLARATION the pipeline
//! carries (`Pipeline::declared_ontology`), not something a model extracts.
//! Writing it is a file transform over the corpus's own `config.json`
//! (which names the pipeline) — minutes over 1,770 SEP siblings, no daemon.
//!
//! One writer, two callers ([`write_pipeline_map`]): the resolve step and the
//! converter go through it, so a converted atlas and a freshly built one
//! carry byte-identical maps for the same pipeline (ARCH §10.6).

use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::{
    read_atlas_ontology, write_atlas_ontology, AtlasOntologyFile,
};
use corpus_engine::enrichment::pipeline::Pipeline;
use corpus_engine::Result;

use super::config::EnrichConfig;
use super::pipeline_resolve::resolve_pipeline;

/// What one write of the map produced.
#[derive(Debug, Clone, PartialEq)]
pub struct MapWrite {
    pub path: PathBuf,
    pub pipeline: String,
    pub ontology_version: u32,
    pub declared_types: usize,
}

/// The ontology version a config's map is written under: the recipe's for
/// the custom path, [`AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION`] otherwise.
fn version_for(cfg: &EnrichConfig) -> u32 {
    cfg.ontology
        .as_ref()
        .map(|spec| spec.ontology_version)
        .unwrap_or(AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION)
}

/// Write `atlas/ontology.json` for `cfg`'s pipeline: the pipeline's own map
/// (`declared_ontology`), under [`version_for`]'s version.
pub fn write_pipeline_map(
    atlas_dir: &Path,
    cfg: &EnrichConfig,
    pipeline: &dyn Pipeline,
) -> std::io::Result<MapWrite> {
    let map = pipeline.declared_ontology();
    let ontology_version = version_for(cfg);
    let path = write_atlas_ontology(atlas_dir, pipeline.id(), ontology_version, &map)?;
    Ok(MapWrite {
        path,
        pipeline: pipeline.id().to_string(),
        ontology_version,
        declared_types: map.shape.types.len(),
    })
}

/// The converter's verdict on one atlas — every outcome named, none
/// defaulted (ARCH §18.3).
#[derive(Debug, Clone, PartialEq)]
pub enum MapConversion {
    /// No map was on disk; the pipeline's is now.
    Written(MapWrite),
    /// A built-in map was on disk and differed from what its pipeline
    /// declares today (rows added since it was written, a version bump); it
    /// is rewritten. Author-declared maps are never touched — see below.
    Refreshed(MapWrite),
    /// The map on disk is what the pipeline declares. Nothing written.
    Current { pipeline: String },
    /// `ontology.json` was declared by a recipe author (`custom_atlas`). The
    /// converter does not overwrite an author's declaration with anything.
    AuthorDeclared,
    /// No `enrichment/<corpus>/config.json`: nothing says which pipeline
    /// built this atlas, so no map can be claimed for it.
    NoConfig,
    /// The config names a pipeline the registry does not know.
    Unregistered(String),
}

impl MapConversion {
    /// The one-word column for a status row.
    pub fn state(&self) -> &'static str {
        match self {
            MapConversion::Written(_) => "written",
            MapConversion::Refreshed(_) => "refresh",
            MapConversion::Current { .. } => "current",
            MapConversion::AuthorDeclared => "author",
            MapConversion::NoConfig => "no-cfg",
            MapConversion::Unregistered(_) => "unreg",
        }
    }
}

/// Give `atlas_dir` its pipeline's map if it lacks one or carries a stale
/// built-in one. Idempotent: a second run over the same atlas is `Current`.
pub fn ensure_pipeline_map(atlas_dir: &Path, corpus_id: &str) -> Result<MapConversion> {
    let Some(cfg) = EnrichConfig::load(corpus_id)? else {
        return Ok(MapConversion::NoConfig);
    };
    let Some(pipeline) = resolve_pipeline(&cfg) else {
        return Ok(MapConversion::Unregistered(cfg.pipeline_id.clone()));
    };
    let current = |file: &AtlasOntologyFile| {
        file.pipeline_id == pipeline.id()
            && file.ontology_version == version_for(&cfg)
            && file.policies == pipeline.declared_ontology()
    };
    match read_atlas_ontology(atlas_dir) {
        Some(file) if file.is_author_declared() => Ok(MapConversion::AuthorDeclared),
        Some(file) if current(&file) => Ok(MapConversion::Current {
            pipeline: pipeline.id().to_string(),
        }),
        Some(_) => Ok(MapConversion::Refreshed(write_pipeline_map(
            atlas_dir, &cfg, &*pipeline,
        )?)),
        None => Ok(MapConversion::Written(write_pipeline_map(
            atlas_dir, &cfg, &*pipeline,
        )?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::scoped_home;
    use corpus_engine::enrichment::ontology::OntologyPolicies;
    use corpus_engine::enrichment::pipeline::PipelineRegistry;
    use sovereign_enrichment_catalog::config::CONFIG_SCHEMA_VERSION;
    use sovereign_enrichment_catalog::paths;

    fn config(corpus_id: &str, pipeline_id: &str) -> EnrichConfig {
        let cfg = EnrichConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            corpus_id: corpus_id.into(),
            pipeline_id: pipeline_id.into(),
            source_path: PathBuf::from("/nonexistent"),
            chapter_regex: String::new(),
            chat_model: "test-chat".into(),
            chat_models: None,
            embed_model: "test-embed".into(),
            base_url: "http://localhost:9741".into(),
            embed_base_url: None,
            min_section_body_words: 0,
            toc_markers: None,
            max_output_tokens: 4096,
            phase1b_max_output_tokens: None,
            phase_overrides: None,
            ontology: None,
            created_at: "2026-09-08T00:00:00Z".into(),
        };
        std::fs::create_dir_all(paths::enrichment_root(corpus_id)).unwrap();
        cfg.save().unwrap();
        cfg
    }

    /// THE CONVERSION, as an installed SEP article sees it: atoms and no map,
    /// `config.json` naming `philosophy_atlas`. One call writes the pipeline's
    /// map with its rows; a second call is `Current`; a map written by an
    /// older binary (rows differ) is refreshed; an author's map is never
    /// touched; no config, no claim. Failing input: drop the `policies`
    /// comparison from `current` and the stale-rows case reads as current.
    #[test]
    fn converts_writes_refreshes_and_leaves_authors_alone() {
        let home = scoped_home();
        let atlas_dir = home.path().join("indexes/sep-x/atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();

        assert_eq!(
            ensure_pipeline_map(&atlas_dir, "sep-x").unwrap(),
            MapConversion::NoConfig
        );
        config("sep-x", "philosophy_atlas");

        let MapConversion::Written(w) = ensure_pipeline_map(&atlas_dir, "sep-x").unwrap() else {
            panic!("no map on disk means written");
        };
        assert_eq!(w.pipeline, "philosophy_atlas");
        assert_eq!(w.ontology_version, AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION);
        let file = read_atlas_ontology(&atlas_dir).unwrap();
        let expected = PipelineRegistry::builtin()
            .get("philosophy_atlas")
            .unwrap()
            .declared_ontology();
        assert_eq!(file.policies, expected);
        assert!(!file.policies.navigation.tension.exemplars.is_empty());

        assert_eq!(
            ensure_pipeline_map(&atlas_dir, "sep-x").unwrap(),
            MapConversion::Current {
                pipeline: "philosophy_atlas".into()
            }
        );

        // A map from before rung 2: same pipeline, default rows.
        let mut stale = expected.clone();
        stale.navigation = Default::default();
        write_atlas_ontology(
            &atlas_dir,
            "philosophy_atlas",
            AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION,
            &stale,
        )
        .unwrap();
        assert!(matches!(
            ensure_pipeline_map(&atlas_dir, "sep-x").unwrap(),
            MapConversion::Refreshed(_)
        ));
        assert_eq!(read_atlas_ontology(&atlas_dir).unwrap().policies, expected);

        // An author's declaration is left exactly as it is.
        let authored = OntologyPolicies::default();
        write_atlas_ontology(&atlas_dir, "custom_atlas", 1, &authored).unwrap();
        assert_eq!(
            ensure_pipeline_map(&atlas_dir, "sep-x").unwrap(),
            MapConversion::AuthorDeclared
        );
        assert_eq!(read_atlas_ontology(&atlas_dir).unwrap().policies, authored);

        // A pipeline the registry does not know is named, not defaulted.
        config("odd", "no_such_atlas");
        assert_eq!(
            ensure_pipeline_map(&atlas_dir, "odd").unwrap(),
            MapConversion::Unregistered("no_such_atlas".into())
        );
    }
}
