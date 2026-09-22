// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas context bag — WRITE half: the ANN backfill.
//!
//! `load_atlas_context` + `LoadAtlasError` moved to the `corpus-engine-atlas-
//! reader` leaf (FIVE_PROGRAMS §12 decision 1 — the read bag needs no
//! engine); re-exported at the historical paths. What stays here is the one
//! write: `backfill_ann` embeds the bag and builds `atoms_ann.lance` through
//! the leaf's table port, deriving the population from the corpus's own
//! navigation map (`seed_population`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use understanding_vocab::atoms::{AtomEnvelope, AtomType};

use crate::enrichment::atlas::ann_store::{ann_table_dir, ANN_TABLE_DIRNAME};
use crate::enrichment::atlas::context::{
    build_persistent_ann_seed_table, render_atom_entry, AnnBuildStats, AtlasContext, AtlasEntry,
};
use crate::enrichment::atlas::seed_population::{seed_population, write_population_marker};
use crate::types::EmbedFn;

pub use super::context_filter::AtlasContextFilter;

// `load_atlas_context` + `LoadAtlasError` moved to the atlas-reader leaf
// (FIVE_PROGRAMS §12 decision 1 — the read bag needs no engine); re-exported
// at the historical paths.
pub use corpus_engine_atlas_reader::context_loader::{
    load_atlas_context, LoadAtlasError, ATLAS_ENTRY_CHAR_LIMIT,
};

/// What [`backfill_ann`] did for one corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillOutcome {
    /// The table at `atlas/atoms_ann.lance` was (re)written.
    Built(AnnBuildStats),
    /// The production grounding filter admitted no atom, so there is nothing
    /// to seed from — the table is not written and grounding for this corpus
    /// stays where it was. Not a failure: an atlas of Claims-only or
    /// structural atoms is a real shape (mirrors `migrate_all`'s `"none"`
    /// state, WITHOUT its relaxed-floor retry — one filter, the one the daemon
    /// seeds with).
    NoSeedableAtoms { min_description_chars: usize },
}

pub async fn backfill_ann(
    embed: &EmbedFn,
    atlas_dir: &Path,
    corpus_id: &str,
    filter: &AtlasContextFilter,
) -> Result<BackfillOutcome, String> {
    // The POPULATION is the map's decision, taken here — at the one writer —
    // rather than at each of the four call sites that reach it (the atlas
    // writer, `svrn atlas backfill-ann`, the `enrich build` Backfill step,
    // `atlas migrate-all`). None of them passes it, none of them can get it
    // wrong, and none of their signatures moved: the atlas dir is all the
    // derivation needs (ARCH §10.6, §19).
    let population = seed_population(atlas_dir);
    let filter = &AtlasContextFilter {
        seed_kinds: Some(population.kinds.clone()),
        ..filter.clone()
    };
    tracing::info!(
        corpus = corpus_id,
        population = %population
            .kinds
            .iter()
            .map(AtomType::label)
            .collect::<Vec<_>>()
            .join(","),
        source = %population.source.label(),
        "backfill-ann: seed population derived from the navigation map"
    );
    let ctx = match load_atlas_context(embed, atlas_dir, corpus_id, filter.top_k, filter).await {
        Ok(ctx) => ctx,
        Err(LoadAtlasError::FilterExcludedAll {
            min_description_chars,
            ..
        }) => {
            tracing::info!(
                corpus = corpus_id,
                min_description_chars,
                depth_allowlist = ?filter.depth_allowlist,
                "backfill-ann: no seedable atoms under the grounding filter; table not written"
            );
            return Ok(BackfillOutcome::NoSeedableAtoms {
                min_description_chars,
            });
        }
        Err(e) => return Err(e.to_string()),
    };
    let stats = build_persistent_ann_seed_table(atlas_dir, &ctx).await?;
    // The marker rides with the table, written second so it is never newer
    // than what it describes. Without it `ann_table_is_fresh` would keep an
    // Entity-only table that merely post-dates `atoms.json` — which is every
    // table on this box, and the reason a population change has to read as
    // STALENESS rather than as an operator's problem to remember.
    if let Err(e) = write_population_marker(atlas_dir, &population) {
        // Not fatal: the table is on disk and correct. A missing marker reads
        // as STALE, so the cost is a re-embed, never a wrong seed.
        tracing::warn!(
            corpus = corpus_id,
            error = %e,
            "backfill-ann: seed table written but its population marker was not; \
             the table will read as stale and be rebuilt"
        );
    }
    tracing::info!(
        corpus = corpus_id,
        resolved = stats.resolved,
        total = stats.total,
        population = %population
            .kinds
            .iter()
            .map(AtomType::label)
            .collect::<Vec<_>>()
            .join(","),
        table = %atlas_dir.join(ANN_TABLE_DIRNAME).display(),
        "backfill-ann: wrote ANN seed table"
    );
    Ok(BackfillOutcome::Built(stats))
}

/// Sync bridge for [`backfill_ann`] — the atlas writer
/// (`writer::write_atlas_full`) is sync at every lifecycle point that writes
/// the v2 store, and the seed table is written in that same write. Runs the
/// async backfill on a dedicated-thread runtime through the atlas module's ONE
/// such bridge (`store::run_blocking`, the same one `write_store_blocking`
/// uses), so it is safe whether or not an ambient tokio runtime exists.
///
/// One writer, one bridge: this is a thin wrapper over [`backfill_ann`], never
/// a second implementation of it (ARCH §10.6).
pub fn backfill_ann_blocking(
    embed: &EmbedFn,
    atlas_dir: &Path,
    corpus_id: &str,
    filter: &AtlasContextFilter,
) -> Result<BackfillOutcome, String> {
    let fut = backfill_ann(embed, atlas_dir, corpus_id, filter);
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            // Drive the backfill on the AMBIENT runtime, not a fresh one. The
            // embedder is a closure the caller built on that runtime -- an HTTP
            // client whose connection pool is bound to its reactor, or a
            // channel to a resident slot task -- and driving it from a foreign
            // reactor hangs or reports a dead IO driver. `block_in_place` hands
            // the worker back to the scheduler for the duration, so the caller
            // (the daemon, mid-ingest) keeps serving.
            tokio::task::block_in_place(|| h.block_on(fut))
        }
        // No ambient runtime (a plain sync caller), or a current-thread one
        // where `block_in_place` panics: the atlas module's own bridge, a
        // dedicated thread with its own reactor.
        _ => super::store::run_blocking(fut),
    }
}

/// Truncate atlas-entity text for embedding. Embed models cap context
/// somewhere around 8K tokens; entities with augmented descriptions
/// (questions + anchors aggregated across many sections) routinely run
/// 18KB chars. 3000 chars (~750 tokens) keeps headroom while still
/// covering the description and the strongest section signals.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::ann_store::{ann_table_is_fresh, ann_table_present};
    use std::sync::Arc;

    /// Embeds deterministically. The `InferenceProvider` impl this replaced
    /// carried three methods the loader never called — `complete` and
    /// `complete_stream` were `unreachable!()` and `capabilities` was filler —
    /// which is precisely the evidence that the parameter was only ever an
    /// embedder (ARCH §5.1: the trait was eight times wider than the use).
    fn unit_embed() -> EmbedFn {
        Arc::new(|text: &str| {
            let n = text.len() as f32;
            Box::pin(async move { Ok(vec![n, 1.0, 0.0, 0.0]) })
        })
    }

    /// The production grounding filter, spelled out so the test does not
    /// depend on the `SOVEREIGN_ATLAS_*` env knobs `Default` reads.
    fn grounding_filter() -> AtlasContextFilter {
        AtlasContextFilter {
            min_description_chars: 10,
            depth_allowlist: vec!["extracted".into()],
            max_entries: None,
            top_k: 3,
            include_claims: false,
            include_tensions: false,
            include_configurations: false,
            include_declared_claim_types: false,
            seed_kinds: None,
        }
    }

    /// One Entity envelope in the on-disk `atoms.json` shape (copied from a
    /// real maple-house atlas), at the given enrichment depth.
    fn atoms_json(depth: &str) -> String {
        format!(
            r#"{{"schema_version":"2","atoms":[{{"atom_type":"Entity","data":{{"id":"entity-0001","canonical_name":"guest logbook","entity_type":"work","first_appearance":{{"chunk_id":"sec_00001","passage_preview":"signed into the guest logbook"}},"description":"A physical record kept by the front door to track overnight guests.","salience":0.33,"enrichment_depth":"{depth}","provenance":{{"signal_kind":"llm_batch"}}}}}}]}}"#
        )
    }

    #[tokio::test]
    async fn backfill_ann_writes_a_fresh_table_for_an_extracted_entity() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(atlas.join("atoms.json"), atoms_json("extracted")).unwrap();

        let out = backfill_ann(&unit_embed(), &atlas, "t", &grounding_filter())
            .await
            .expect("backfill succeeds");
        assert_eq!(
            out,
            BackfillOutcome::Built(AnnBuildStats {
                resolved: 1,
                total: 1
            })
        );
        assert!(ann_table_present(&atlas));
        assert!(
            ann_table_is_fresh(&atlas),
            "a table written after atoms.json must read as fresh"
        );
    }

    /// The typed skip: an atlas whose atoms all sit outside the grounding
    /// filter's depth allowlist (structural-only) writes no table and says
    /// so, distinguishable from a failure without matching message text.
    #[tokio::test]
    async fn backfill_ann_reports_no_seedable_atoms_when_the_filter_admits_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        std::fs::write(atlas.join("atoms.json"), atoms_json("structural")).unwrap();

        let out = backfill_ann(&unit_embed(), &atlas, "t", &grounding_filter())
            .await
            .expect("an admitted-nothing filter is an outcome, not an error");
        assert_eq!(
            out,
            BackfillOutcome::NoSeedableAtoms {
                min_description_chars: 10
            }
        );
        assert!(!ann_table_present(&atlas), "no table may be written");
    }

    /// One Entity, one Claim and one Configuration in the on-disk shape, plus
    /// an `ontology.json` whose navigation map seeds on Claim and
    /// Configuration. Copied from real atlases (wessex-hoard's claim,
    /// brothers-karamazov-book-1's configuration) so the fixture is not a
    /// hopeful guess at the wire format.
    fn atlas_with_a_claim_and_configuration_map(atlas: &std::path::Path) {
        std::fs::create_dir_all(atlas).unwrap();
        std::fs::write(
            atlas.join("atoms.json"),
            r#"{"schema_version":"2","atoms":[
              {"atom_type":"Entity","data":{"id":"entity-0001","canonical_name":"guest logbook",
               "entity_type":"work","first_appearance":{"chunk_id":"sec_00001","passage_preview":"p"},
               "description":"A physical record kept by the front door.","salience":0.33,
               "enrichment_depth":"extracted"}},
              {"atom_type":"Claim","data":{"id":"claim-0001",
               "content":"Prior to Aldfrith, English coins named mints or moneyers, never the ruler.",
               "discourse_act":"assert","epistemic_status":"confident","scope":"universal",
               "evidence":[{"chunk_id":"sec_00001","passage_preview":"p"}],
               "anchor":"before him","claim_kind":"attribution","enrichment_depth":"extracted"}},
              {"atom_type":"Configuration","data":{"id":"config-0001",
               "label":"The Father as the Source of Structural Chaos",
               "description":"An entropic centre that generates the novel's conflicts.",
               "constituent_atoms":["entity-0001","claim-0001"],
               "evidence":[{"chunk_id":"sec_0003"}],"confidence":0.92,
               "interpretive_note":"An alternative reading makes him a passive victim.",
               "enrichment_depth":"extracted"}}]}"#,
        )
        .unwrap();
        std::fs::write(
            atlas.join("ontology.json"),
            r#"{"schema_version":"1","ontology_version":1,"pipeline_id":"custom_atlas",
              "policies":{"shape":{"types":[{"name":"attribution","kind":"claim"}]},
              "navigation":{
                "thematic":{"seed":{"kinds":["Configuration","Entity"]},"walk":[],"hops":2,"budget":12},
                "trajectory":{"seed":{"kinds":[]},"walk":[],"hops":2,"budget":12},
                "tension":{"seed":{"kinds":["Claim","Position"]},"walk":[],"hops":1,"budget":12},
                "enumeration":{"seed":{"kinds":[]},"walk":[],"hops":0,"budget":12},
                "lookup":{"seed":{"kinds":[]},"walk":[],"hops":1,"budget":12}}}}"#,
        )
        .unwrap();
    }

    /// ei-3c's whole point: the seed table's population is the corpus's
    /// navigation map, not the retrieval filter. The filter passed in is the
    /// PRODUCTION one — claims off, configurations off — and the map's
    /// `tension` and `thematic` rows put both kinds in the table anyway.
    ///
    /// Failing input: `seed_population` narrowed to the filter's admission, or
    /// `backfill_ann` not attaching the population — either drops the table to
    /// the one Entity, which is the Entity-only state ei-4 measured on every
    /// atlas on this box.
    #[tokio::test]
    async fn the_seed_table_population_is_the_maps_not_the_retrieval_filters() {
        use crate::enrichment::atlas::ann_store::AnnSeedTable;
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("atlas");
        atlas_with_a_claim_and_configuration_map(&atlas);

        let out = backfill_ann(&unit_embed(), &atlas, "t", &grounding_filter())
            .await
            .expect("backfill succeeds");
        assert_eq!(
            out,
            BackfillOutcome::Built(AnnBuildStats {
                resolved: 3,
                total: 3
            }),
            "entity + claim + configuration, all three seeded"
        );

        // Read the ids back OUT of the table, not off the stats: the done-when
        // is that those KINDS land in it.
        let table = AnnSeedTable::open_for_atlas(&atlas)
            .await
            .expect("table opens");
        let mut ids = table
            .nearest(&[1.0_f32, 1.0, 0.0, 0.0], 16)
            .await
            .expect("nearest");
        ids.sort();
        assert_eq!(ids, vec!["claim-0001", "config-0001", "entity-0001"]);

        // …and the table records the population it was built under, so a later
        // build that derives a different one rebuilds rather than trusting it.
        assert!(crate::enrichment::atlas::seed_population::population_marker_is_current(&atlas));
        assert!(ann_table_is_fresh(&atlas));
    }

    #[tokio::test]
    async fn load_atlas_context_missing_atlas_is_a_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas = tmp.path().join("nope").join("atlas");
        let err = load_atlas_context(&unit_embed(), &atlas, "t", 3, &grounding_filter())
            .await
            .err()
            .expect("missing atlas dir must be an error");
        assert!(matches!(err, LoadAtlasError::NoAtlas { .. }), "got {err:?}");
        assert!(err.to_string().contains("no atlas at"));
    }
}
