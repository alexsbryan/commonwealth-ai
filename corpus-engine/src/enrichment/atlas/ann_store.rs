// SPDX-License-Identifier: AGPL-3.0-or-later
//! ANN seed table FRESHNESS + the atlas-write seeding contract.
//!
//! The table port itself (schema, build/append, the read paths) lives in the
//! `corpus-engine-atlas-reader` leaf since 2026-09-21 (FIVE_PROGRAMS §12
//! decision 1) and is re-exported below at its historical paths. What stays
//! here is the WRITE side's vocabulary: how a lifecycle point declares its
//! seeding intent ([`AtlasSeeding`], what it reports back ([`SeedOutcome`]),
//! and the rebuild decider [`ann_table_is_fresh`] — which is TWO questions
//! (mtime + population) and the population half belongs beside
//! `seed_population`, the deriver that defines that currency (ARCH §10.6).

use std::path::Path;

pub use corpus_engine_atlas_reader::ann_store::{
    ann_table_dir, ann_table_mtime_ms, ann_table_present, ann_table_rows, AnnSeedTable,
    ANN_TABLE_DIRNAME,
};

/// Whether the ANN seed table is at least as new as `atoms.json` — the
/// "already built" test the `enrich build` Backfill step and the daemon's
/// post-write hook share (ontology-v1 P0). [`ann_table_present`] answers only
/// "is there a table"; a table older than the atlas it was embedded from
/// seeds grounding with atoms the last resolve renamed or deleted, which is
/// worse than no table (ATLAS_STORAGE_V2 3b keys the table on atom-id). Same
/// mtime idiom as `store::store_needs_build`.
///
/// Absent table → `false`. Table present but no `atoms.json` → `true`
/// (nothing newer exists to embed). An unreadable mtime → `false` (rebuild).
///
/// Since ei-3c freshness is TWO questions, not one. mtime answers "was it
/// embedded from these atoms"; it cannot answer "was it embedded from these
/// KINDS", and that is the one that mattered — the seed population moved from
/// the retrieval filter's Entity-only admission to the corpus's navigation map
/// (`super::seed_population`), so every table on disk is fresh by mtime and
/// wrong by population. A table whose recorded population is not the one this
/// build derives is stale, and the check is deliberately cheap (one small read,
/// two mtimes, no `ontology.json` parse) because the daemon runs it once per
/// installed atlas at boot.
pub fn ann_table_is_fresh(atlas_dir: &Path) -> bool {
    if !super::seed_population::population_marker_is_current(atlas_dir) {
        return false;
    }
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let Some(table) = mtime(&ann_table_dir(atlas_dir)) else {
        return false;
    };
    // `atoms.json` is the file `writer::write_atlas_full` stamps last.
    match mtime(&atlas_dir.join("atoms.json")) {
        Some(atoms) => table >= atoms,
        None => true,
    }
}

/// How a lifecycle point that writes an atlas supplies the atom embeddings the
/// seed table needs.
///
/// There is no default and no `Option`. `atoms_ann.lance` is a MANDATORY ingest
/// artifact beside `atoms.lance` and `edges.csr` (EPISTEMIC_INDEX §1, Ideas
/// row), and the failure this type exists to make impossible is the quiet one:
/// an atlas written with a store and no seed table loads, enumerates, and
/// reports no error, while every grounded answer over it silently falls back to
/// cosine over chunks. Before this, seeding was a separate step three surfaces
/// remembered to call and every other write forgot — SEP is the evidence, 22 of
/// 1,770 atlases seeded (ARCH §7: make it structural, not remembered).
///
/// So every caller of [`super::writer::write_atlas_full`] states which of the
/// two this write is, both are traced, and [`SeedOutcome`] carries the answer
/// back so no caller can mistake one for the other (ARCH §18.3).
#[derive(Clone)]
pub enum AtlasSeeding {
    /// Embed the atoms and write the seed table in this same write, through
    /// [`super::context_loader::backfill_ann`] — the ONE writer (ARCH §10.6).
    /// A failure here fails the atlas write, exactly as an unwritable
    /// `atoms.lance` does.
    ///
    /// The embedder MUST be the query-side one: the table and the queries that
    /// search it share one vector space, and a document-side embed degrades
    /// grounding without failing anything.
    With(crate::types::EmbedFn),
    /// This lifecycle point has no embedder, and says why. The reason is
    /// traced and rides back in [`SeedOutcome::Deferred`]; the atlas is left
    /// seedless and `svrn atlas backfill-ann <id>` completes it.
    ///
    /// Legitimate cases: a partial step-3a write whose step-3b sibling seeds, a
    /// test fixture with no inference, and a host that seeds on its own async
    /// runtime after the write returns. Not a way to skip the artifact.
    Deferred(&'static str),
}

impl std::fmt::Debug for AtlasSeeding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::With(_) => write!(f, "AtlasSeeding::With(<embed>)"),
            Self::Deferred(why) => write!(f, "AtlasSeeding::Deferred({why:?})"),
        }
    }
}

/// What an atlas write did about the seed table. Three outcomes, each in its
/// own words (ARCH §18.3) so a caller can report the absence rather than print
/// a zero: the table was written; the grounding filter admitted no atom, so
/// there was nothing to seed from; or the write deferred and named why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// `atoms_ann.lance` written with `rows` of `of` bag entries resolved to
    /// atom ids.
    Written { rows: usize, of: usize },
    /// The production grounding filter admitted nothing — a real shape for an
    /// atlas carrying only structural or Claims-only atoms. No table.
    NoSeedableAtoms { min_description_chars: usize },
    /// No embedder at this lifecycle point; the reason as given.
    Deferred(&'static str),
}

impl SeedOutcome {
    /// One line for an operator surface. Never renders an absence as a zero.
    pub fn describe(&self) -> String {
        match self {
            Self::Written { rows, of } => {
                format!("{ANN_TABLE_DIRNAME}: {rows}/{of} atoms embedded")
            }
            Self::NoSeedableAtoms {
                min_description_chars,
            } => format!(
                "{ANN_TABLE_DIRNAME} not written — the grounding filter \
                 (min_chars={min_description_chars}) admitted no atom"
            ),
            Self::Deferred(why) => {
                format!("{ANN_TABLE_DIRNAME} deferred — {why}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Freshness fixture: an `atlas/` dir with `atoms.json` stamped at
    /// `atoms_secs` and (optionally) an ANN table dir stamped at
    /// `table_secs`, both relative to the epoch so the order is explicit.
    fn freshness_fixture(atoms_secs: Option<u64>, table_secs: Option<u64>) -> tempfile::TempDir {
        use std::time::{Duration, UNIX_EPOCH};
        let tmp = tempfile::tempdir().unwrap();
        if let Some(a) = atoms_secs {
            let p = tmp.path().join("atoms.json");
            std::fs::write(&p, "{}").unwrap();
            std::fs::File::open(&p)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(a))
                .unwrap();
        }
        if let Some(t) = table_secs {
            let d = ann_table_dir(tmp.path());
            std::fs::create_dir_all(&d).unwrap();
            // A table is written WITH its population marker since ei-3c, so a
            // fixture exercising the MTIME clause has to write one too — else
            // every case below short-circuits on the population clause and the
            // mtime rule is never actually tested (§18.1).
            let pop = super::super::seed_population::seed_population(tmp.path());
            super::super::seed_population::write_population_marker(tmp.path(), &pop).unwrap();
            std::fs::File::open(&d)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(t))
                .unwrap();
        }
        tmp
    }

    /// The population clause, watched failing: a table with the right mtime and
    /// NO marker is the state every atlas on disk was in before ei-3c —
    /// Entity-only, and fresh by mtime. It must read as stale.
    #[test]
    fn ann_table_is_fresh_table_without_a_population_marker_is_stale() {
        let tmp = freshness_fixture(Some(1_000), Some(2_000));
        std::fs::remove_file(super::super::seed_population::population_marker_path(
            tmp.path(),
        ))
        .unwrap();
        assert!(!ann_table_is_fresh(tmp.path()));
    }

    #[test]
    fn ann_table_is_fresh_absent_table_is_not_fresh() {
        let tmp = freshness_fixture(Some(2_000), None);
        assert!(!ann_table_is_fresh(tmp.path()));
    }

    #[test]
    fn ann_table_is_fresh_table_newer_than_atoms_is_fresh() {
        let tmp = freshness_fixture(Some(1_000), Some(2_000));
        assert!(ann_table_is_fresh(tmp.path()));
    }

    /// The falsifier `ann_table_present` cannot pass: a table that exists
    /// but predates the atoms.json it should have been embedded from.
    #[test]
    fn ann_table_is_fresh_table_older_than_atoms_is_stale() {
        let tmp = freshness_fixture(Some(2_000), Some(1_000));
        assert!(!ann_table_is_fresh(tmp.path()));
    }

    #[test]
    fn ann_table_is_fresh_table_without_atoms_json_is_fresh() {
        let tmp = freshness_fixture(None, Some(1_000));
        assert!(ann_table_is_fresh(tmp.path()));
    }
}
