// SPDX-License-Identifier: AGPL-3.0-or-later
//! Project a corpus's RAPTOR summaries into `Summary` atoms in its atlases —
//! ei-7a's write side.
//!
//! **There is no new RAPTOR pass here** (operator, 2026-09-04: "we already
//! have the RAPTOR summaries"). The summaries already exist, in
//! `raptor_summaries.lance`, with their embeddings; this reads those rows and
//! gives each one a face in the atlas the walk already reads, so a summary
//! stops arriving through a separate retrieval-time injector
//! (`raptor_grounding.rs`) that runs beside the walk and knows nothing about
//! it.
//!
//! It lives in `sovereign-tools` for the same reason
//! [`raptor_index`](crate::raptor_index) does: this crate is where the
//! RAPTOR-side handles and the corpus-engine handles meet. Every mechanism it
//! uses is somebody else's and is reached, not reimplemented (ARCH §19):
//!
//! | What | Whose |
//! |---|---|
//! | reading the summary rows | `corpus_engine::scan_raptor_summaries` |
//! | `conv_uuid` → article title | `corpus_engine::raptor_article_title` |
//! | title → per-article atlas id | `ground::candidate_atlas_ids` |
//! | the tree (children, evidence chunks) | `RaptorCheckpointHandle::load_all_nodes` |
//! | writing atoms + edges | `atlas::append_atoms_and_edges` |
//! | the seed row | `AnnSeedTable::append_rows` |
//! | which kinds the table seeds | `seed_population::seed_population` |
//!
//! ## The tree is mostly gone, and that is REPORTED, not defaulted
//!
//! `raptor_summaries.lance` carries no tree columns — node_id, conv_uuid,
//! level, summary, embedding and nothing else — and `conv_raptor_nodes`, which
//! did carry the tree, is EMPTY on this box (verified 2026-09-04: 0 rows in
//! both `~/.sovereign/sovereign.db` and `~/.svrnmesh/sovereign.db`). The only
//! surviving tree is the `_raptor_checkpoint` directory, and for SEP that is
//! 37 nodes of ONE article.
//!
//! So a Summary atom gets its `evidence` and `children` when the checkpoint
//! has that node and NOT OTHERWISE, and the count of each is named in
//! [`SummaryProjection`] and printed by the caller (ARCH §18.3). An atom
//! without evidence is still a seed the walk can reach and still orients a
//! query; it just cannot hand back a chunk of its own. Reporting that number
//! is the difference between "the tree is thin here" and "the writer dropped
//! it".
//!
//! ## No `EvidenceFor` edge is emitted, and that is not an omission
//!
//! The order says "`EvidenceFor` edges to their chunks". An [`Edge`] is
//! atom → atom, and a chunk is not an atom, so the summary → chunk relation
//! has no edge to be. It rides on the atom instead, in `Summary::evidence`,
//! which is exactly where [`AtomEnvelope::evidence`] reads it and where the
//! walk's `resolve_evidence` looks — the same field every other kind uses for
//! the same purpose. `Composes` (summary → child summary) IS atom → atom and
//! IS emitted.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::ann_store::{ann_table_dir, AnnSeedTable};
use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomType, ChunkRef, Summary};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use corpus_engine::enrichment::atlas::ground::candidate_atlas_ids;
use corpus_engine::enrichment::atlas::seed_population::{seed_population, write_population_marker};
use corpus_engine::enrichment::atlas::{append_atoms_and_edges, read_atlas_atoms};
use corpus_engine::enrichment::pipeline::atlas::EnrichmentDepth;
use corpus_engine::{raptor_article_title, scan_raptor_summaries};

use crate::raptor_checkpoint::RaptorCheckpointHandle;

/// What one projection did, in the terms an operator has to judge it by.
///
/// Every field is a COUNT the caller prints. `atoms_written` alone cannot tell
/// a thin tree from a dropped one, which is why `with_evidence` /
/// `with_children` / `no_tree_row` are all here and separate.
#[derive(Debug, Clone, Default)]
pub struct SummaryProjection {
    /// Rows in `raptor_summaries.lance`.
    pub rows_read: usize,
    /// Distinct atlases that got at least one atom.
    pub atlases_written: usize,
    /// `Summary` atoms appended.
    pub atoms_written: usize,
    /// `Composes` edges appended (summary → child summary).
    pub edges_written: usize,
    /// Rows added to the atlases' `atoms_ann.lance` seed tables.
    pub seeds_written: usize,
    /// Atoms that carry at least one evidence chunk.
    pub with_evidence: usize,
    /// Atoms that carry at least one child.
    pub with_children: usize,
    /// Rows whose node the checkpoint does not have — so no evidence and no
    /// children for that atom. The headline degradation.
    pub no_tree_row: usize,
    /// Rows already projected by an earlier run (same atom id) and skipped.
    /// Idempotence: re-running this must not double the atlas.
    pub skipped_already_present: usize,
    /// Rows whose article resolved to no atlas directory on disk.
    pub unresolved_article: usize,
    /// Rows with an empty stored embedding — no seed row possible.
    pub no_embedding: usize,
    /// Every degradation, in a sentence, in the order it was decided.
    pub degradations: Vec<String>,
}

impl SummaryProjection {
    /// The operator line. Never renders an absence as a zero without saying
    /// what the zero is (ARCH §18.3).
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} summary rows -> {} atoms in {} atlases ({} with evidence, {} with children), \
             {} Composes edges, {} seed rows",
            self.rows_read,
            self.atoms_written,
            self.atlases_written,
            self.with_evidence,
            self.with_children,
            self.edges_written,
            self.seeds_written,
        );
        if self.skipped_already_present > 0 {
            s.push_str(&format!(
                "; {} already projected (skipped)",
                self.skipped_already_present
            ));
        }
        for d in &self.degradations {
            s.push_str("\n  degraded: ");
            s.push_str(d);
        }
        s
    }
}

/// The atlas directory a summary of `title` belongs in, or `None`.
///
/// The candidate LIST is `candidate_atlas_ids`' — the one home of the
/// chunk → atlas-id derivation, reused here so the write side cannot pick a
/// different atlas than the read side walks. What is decided HERE is only the
/// preference between candidates: **most specific first**. That matters
/// concretely for SEP, where `sep/atlas/atoms.json` exists and is EMPTY (0
/// atoms, a stub from the pre-per-article layout) while the real atlas of an
/// entry is `sep-<slug>/atlas`. Taking the first existing candidate would put
/// all 11,181 summaries in the stub.
fn atlas_dir_for(index_root: &Path, corpus_id: &str, title: &str) -> Option<PathBuf> {
    candidate_atlas_ids(corpus_id, Some(title))
        .into_iter()
        .rev()
        .map(|id| index_root.join(id).join("atlas"))
        .find(|dir| dir.join("atoms.json").exists())
}

/// The checkpoint's tree, keyed by node id: `(evidence chunk row ids, child
/// node ids)`. Empty when there is no checkpoint — which is a legitimate
/// shape, not an error, and shows up in the report as `no_tree_row` for every
/// row.
fn load_tree(corpus_dir: &Path) -> BTreeMap<String, (Vec<u32>, Vec<String>)> {
    // `load_all_nodes` walks `level-*/cluster-*.json` and never consults the
    // manifest's input hash, so the empty hash here is not a lie about
    // freshness — it is simply unused on this path.
    let handle = RaptorCheckpointHandle::at(corpus_dir, String::new());
    match handle.load_all_nodes() {
        Ok(nodes) => nodes
            .into_iter()
            .map(|n| (n.node_id, (n.evidence_chunk_ids, n.children_node_ids)))
            .collect(),
        Err(e) => {
            tracing::warn!(
                dir = %corpus_dir.display(),
                error = %e,
                "summary-atoms: checkpoint unreadable — projecting without tree edges"
            );
            BTreeMap::new()
        }
    }
}

/// Project `corpus_id`'s RAPTOR summaries into its atlases.
///
/// `index_root` is the indexes directory (`CorpusEngine::index_dir()`), so the
/// corpus's own dir is `<index_root>/<corpus_id>` and its per-article atlases
/// are `<index_root>/<corpus_id>-<title>/atlas`.
///
/// **Idempotent.** A node whose atom id is already in the target atlas is
/// skipped, so a second run adds nothing — which is what lets the seed rows be
/// APPENDED (`AnnSeedTable::append_rows`) rather than rebuilt: a key written
/// twice would be two rows and the walk would see the atom twice.
///
/// **Never re-embeds.** The seed row reuses the vector already stored beside
/// the summary. A fresh embed would be a second decider for the seed space
/// (ARCH §10.6) and the cosine sample this order carries is what proves the
/// two agree.
pub async fn write_summary_atoms(
    index_root: &Path,
    corpus_id: &str,
) -> Result<SummaryProjection, String> {
    let corpus_dir = index_root.join(corpus_id);
    let mut report = SummaryProjection::default();

    let rows = scan_raptor_summaries(&corpus_dir)
        .await
        .map_err(|e| format!("read raptor_summaries.lance({corpus_id}): {e}"))?;
    report.rows_read = rows.len();
    if rows.is_empty() {
        report.degradations.push(format!(
            "{corpus_id} has no raptor_summaries.lance rows — nothing to project \
             (run `sovereign enrich raptor-index {corpus_id}` first)"
        ));
        return Ok(report);
    }

    let tree = load_tree(&corpus_dir);
    if tree.is_empty() {
        report.degradations.push(format!(
            "no readable RAPTOR checkpoint under {} — every Summary atom is written \
             WITHOUT evidence chunks and without Composes edges",
            corpus_dir.display()
        ));
    }

    // Group by ATLAS DIRECTORY — not by article — so each atlas is read and
    // written exactly ONCE.
    //
    // The atlas write is a read-modify-write of `atoms.json` + `edges.json`
    // that also rebuilds `atoms.lance`, so the grouping key has to be the
    // thing being rewritten. Article looks like the natural key and is not:
    // several articles resolve to ONE atlas whenever the corpus is
    // self-hosted, and grouping by article would then rewrite that same
    // growing file once per article — 284 rebuilds of one table on the ei-7a
    // subset fixture, each larger than the last.
    let mut by_atlas: BTreeMap<PathBuf, Vec<corpus_engine::RaptorSummaryRow>> = BTreeMap::new();
    for row in rows {
        let title = raptor_article_title(&row.conv_uuid);
        match atlas_dir_for(index_root, corpus_id, &title) {
            Some(dir) => by_atlas.entry(dir).or_default().push(row),
            None => report.unresolved_article += 1,
        }
    }

    // node_id -> atom id, across ALL articles: a `Composes` child may in
    // principle be listed before its parent's article is reached, and the edge
    // target must be the same id the child's own atom got.
    let atom_id_of = |node_id: &str| AtomId::summary_content_hash(node_id, corpus_id);

    for (atlas_dir, article_rows) in by_atlas {
        let existing = read_atlas_atoms(&atlas_dir)
            .map_err(|e| format!("read {}: {e}", atlas_dir.join("atoms.json").display()))?;
        let present: HashSet<String> = existing
            .atoms
            .iter()
            .map(|a| a.id().as_str().to_string())
            .collect();
        // `EdgeId::new(i)` is positional, so a second run must not restart at
        // 0 and collide with the edges already on disk. Read it or REFUSE —
        // an `unwrap_or(0)` here would mint `edge-00000` on top of an existing
        // one whenever `edges.json` failed to parse, which is a silent
        // substitution of a wrong id for an unreadable one (§18.3).
        // `append_atoms_and_edges` needs the file anyway, so failing here
        // fails earlier and says why.
        let mut next_edge_ix = corpus_engine::enrichment::atlas::read_atlas_edges(&atlas_dir)
            .map(|f| f.edges.len())
            .map_err(|e| {
                format!(
                    "read {}: {e} (an atlas with atoms.json needs an edges.json beside it)",
                    atlas_dir.join("edges.json").display()
                )
            })?;

        let mut atoms: Vec<AtomEnvelope> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut seeds: Vec<(String, Vec<f32>)> = Vec::new();

        for row in article_rows {
            let id = atom_id_of(&row.node_id);
            if present.contains(id.as_str()) {
                report.skipped_already_present += 1;
                continue;
            }
            let (evidence_ids, child_ids) = match tree.get(&row.node_id) {
                Some((e, c)) => (e.clone(), c.clone()),
                None => {
                    report.no_tree_row += 1;
                    (Vec::new(), Vec::new())
                }
            };
            let evidence: Vec<ChunkRef> = evidence_ids
                .iter()
                // The row id, as a decimal string: `ChunkSelector::parse`
                // reads a numeric `chunk_id` back as a `RowId`, which is the
                // direct-key fetch the walk's `by_row` performs. Not a format
                // invented here — it is the one every atlas already uses.
                .map(|c| ChunkRef::new(c.to_string(), None))
                .collect();
            let children: Vec<AtomId> = child_ids.iter().map(|c| atom_id_of(c)).collect();
            if !evidence.is_empty() {
                report.with_evidence += 1;
            }
            if !children.is_empty() {
                report.with_children += 1;
            }
            for child in &children {
                edges.push(Edge {
                    id: EdgeId::new(next_edge_ix),
                    edge_type: EdgeType::Composes,
                    source: id.clone(),
                    target: child.clone(),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    // Read off the tree, not inferred: the builder recorded
                    // this parentage, so there is no extraction confidence to
                    // discount.
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
                next_edge_ix += 1;
            }
            if row.embedding.is_empty() {
                report.no_embedding += 1;
            } else {
                seeds.push((id.as_str().to_string(), row.embedding.clone()));
            }
            atoms.push(AtomEnvelope::Summary(Summary {
                id,
                node_id: row.node_id,
                level: row.level.max(0) as u32,
                text: row.summary,
                evidence,
                children,
                // The RAPTOR pass EXTRACTED this text from the chunks it
                // clustered — it is a model reading passages, which is what
                // `Extracted` names. Not `Structural`: nothing about a
                // summary comes from the document's shape.
                enrichment_depth: EnrichmentDepth::extracted_default(),
            }));
        }

        if atoms.is_empty() && edges.is_empty() {
            continue;
        }
        report.atoms_written += atoms.len();
        report.edges_written += edges.len();
        report.atlases_written += 1;
        append_atoms_and_edges(&atlas_dir, &atoms, &edges)
            .map_err(|e| format!("append atoms to {}: {e}", atlas_dir.display()))?;

        if !seeds.is_empty() {
            match AnnSeedTable::append_rows(&ann_table_dir(&atlas_dir), &seeds).await {
                Ok(n) => report.seeds_written += n,
                Err(e) => {
                    // NAMED, not swallowed: an atlas whose atoms landed and
                    // whose seeds did not is an atlas the walk can enumerate
                    // and cannot reach by vector, and the two look identical
                    // in an atom count.
                    report.degradations.push(format!(
                        "{}: atoms written but the seed table refused them ({e}) — \
                         these summaries are unreachable by vector seeding",
                        atlas_dir.display()
                    ));
                }
            }
        }
        // The population marker records WHICH KINDS the table was built from.
        // ei-7a bumped `SEED_POPULATION_SCHEMA` 1 -> 2 (Summary joined the
        // `thematic` row), so every marker on disk is stale by definition —
        // and a stale marker makes `ann_table_is_fresh` false, which sends the
        // daemon's backfill through `build_persistent_ann_seed_table`, which
        // REPLACES the table and would delete the rows just appended. Stamping
        // it here is what makes the append durable.
        if let Err(e) = write_population_marker(&atlas_dir, &seed_population(&atlas_dir)) {
            report.degradations.push(format!(
                "{}: population marker not written ({e}) — the next backfill will \
                 rebuild this seed table and drop the appended Summary rows",
                atlas_dir.display()
            ));
        }
    }

    if report.unresolved_article > 0 {
        report.degradations.push(format!(
            "{} summary rows belong to articles with no atlas directory under {} — \
             not written",
            report.unresolved_article,
            index_root.display()
        ));
    }
    if report.no_tree_row > 0 {
        report.degradations.push(format!(
            "{} of {} summaries have no checkpoint node, so they carry no evidence \
             chunks and no Composes edges (the RAPTOR tree survives only in \
             `_raptor_checkpoint`, and `conv_raptor_nodes` is empty)",
            report.no_tree_row, report.rows_read
        ));
    }
    if report.no_embedding > 0 {
        report.degradations.push(format!(
            "{} summaries have no stored embedding — written as atoms, absent from \
             the seed table",
            report.no_embedding
        ));
    }
    tracing::info!(
        corpus = corpus_id,
        rows = report.rows_read,
        atoms = report.atoms_written,
        edges = report.edges_written,
        seeds = report.seeds_written,
        with_evidence = report.with_evidence,
        with_children = report.with_children,
        no_tree_row = report.no_tree_row,
        "summary-atoms: projection ledger"
    );
    // The whole point of the degradation list is that it reaches a human even
    // when nobody is reading the trace (ARCH §18.3).
    for d in &report.degradations {
        eprintln!("summary-atoms: degraded: {d}");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::AtomsFile;
    use corpus_engine::{build_raptor_index, RaptorSummaryRow};
    use tempfile::tempdir;

    fn emb(i: usize) -> Vec<f32> {
        (0..8usize)
            .map(|d| ((i * 131 + d * 977 + 7) % 1000) as f32 / 500.0 - 1.0)
            .collect()
    }

    /// An atlas directory with an EMPTY but valid `atoms.json` — the shape a
    /// per-article atlas has before anything has been written into it.
    fn empty_atlas(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let atoms = AtomsFile {
            schema_version: AtomsFile::SCHEMA_VERSION.to_string(),
            atoms: Vec::new(),
        };
        std::fs::write(
            dir.join("atoms.json"),
            serde_json::to_vec_pretty(&atoms).unwrap(),
        )
        .unwrap();
        // Both files, because both are the real shape: an atlas with
        // `atoms.json` and no `edges.json` is a torn write, and the writer
        // refuses it rather than restarting the edge ids at zero.
        std::fs::write(
            dir.join("edges.json"),
            br#"{"schema_version":"2.0","edges":[]}"#,
        )
        .unwrap();
    }

    /// The SEP trap, as a test. `sep/atlas/atoms.json` EXISTS and is a
    /// zero-atom stub; the article's real atlas is `sep-<slug>/atlas`. A
    /// most-general-first preference would put all 11,181 summaries in the
    /// stub, where no walk that scopes to the article would ever see them —
    /// and every count in the report would still look right.
    #[test]
    fn the_per_article_atlas_wins_over_the_parent_stub() {
        let root = tempdir().unwrap();
        empty_atlas(&root.path().join("sep").join("atlas"));
        empty_atlas(&root.path().join("sep-abduction").join("atlas"));

        let picked = atlas_dir_for(root.path(), "sep", "abduction").expect("an atlas");
        assert!(
            picked.ends_with("sep-abduction/atlas"),
            "expected the per-article atlas, got {}",
            picked.display()
        );
    }

    /// The other direction of the same decision: with no per-article atlas on
    /// disk, the parent is the right answer rather than a silent drop — a
    /// self-hosted corpus (one atlas, no article children) is a real shape.
    #[test]
    fn the_parent_atlas_is_used_when_the_article_has_none() {
        let root = tempdir().unwrap();
        empty_atlas(&root.path().join("wessex-hoard").join("atlas"));
        let picked =
            atlas_dir_for(root.path(), "wessex-hoard", "chapter-3").expect("the parent atlas");
        assert!(picked.ends_with("wessex-hoard/atlas"));
        // And an article of a corpus with NO atlas at all is reported as
        // unresolved, never guessed at.
        assert!(atlas_dir_for(root.path(), "nothing-here", "x").is_none());
    }

    /// End-to-end over a real `raptor_summaries.lance`, run TWICE.
    ///
    /// The second run is the assertion: this writer APPENDS to `atoms.json`
    /// and to the ANN seed table, so a non-idempotent projection does not
    /// error — it silently doubles every atom and every seed row, and the walk
    /// then sees each summary twice. The skip is keyed on the atom id, which
    /// is `hash(node_id | corpus_id)` and therefore stable across runs.
    #[tokio::test]
    async fn projecting_twice_writes_the_atoms_once() {
        let root = tempdir().unwrap();
        let corpus_dir = root.path().join("sep");
        std::fs::create_dir_all(&corpus_dir).unwrap();
        empty_atlas(&corpus_dir.join("atlas"));
        empty_atlas(&root.path().join("sep-abduction").join("atlas"));

        let rows: Vec<RaptorSummaryRow> = (0..4)
            .map(|i| RaptorSummaryRow {
                node_id: format!("node-{i}"),
                conv_uuid: "https://plato.stanford.edu/entries/abduction/".into(),
                level: (i % 2) as i64,
                summary: format!("rollup {i}"),
                embedding: emb(i),
            })
            .collect();
        build_raptor_index(&corpus_dir, &rows, 1).await.unwrap();

        let first = write_summary_atoms(root.path(), "sep").await.unwrap();
        assert_eq!(first.rows_read, 4);
        assert_eq!(first.atoms_written, 4);
        assert_eq!(first.seeds_written, 4);
        assert_eq!(first.atlases_written, 1);
        // No checkpoint in this fixture, so every atom is tree-less — and
        // that is REPORTED rather than passed off as "no evidence exists".
        assert_eq!(first.no_tree_row, 4);
        assert!(first
            .degradations
            .iter()
            .any(|d| d.contains("no checkpoint node") || d.contains("checkpoint")));

        let second = write_summary_atoms(root.path(), "sep").await.unwrap();
        assert_eq!(second.atoms_written, 0, "second run must add no atom");
        assert_eq!(second.seeds_written, 0, "second run must add no seed row");
        assert_eq!(second.skipped_already_present, 4);

        let article_atlas = root.path().join("sep-abduction").join("atlas");
        let on_disk = read_atlas_atoms(&article_atlas).unwrap();
        assert_eq!(on_disk.atoms.len(), 4, "atoms.json doubled");

        // The marker, and it is not bookkeeping. ei-7a bumped
        // `SEED_POPULATION_SCHEMA` 1 -> 2, so every marker on disk is stale by
        // definition; a stale marker makes `ann_table_is_fresh` false, which
        // sends the daemon's backfill through `build_persistent_ann_seed_table`
        // — which REPLACES the table. Without this stamp the appended Summary
        // seeds are deleted at the next boot, with nothing erroring and no
        // count looking wrong.
        assert!(
            corpus_engine::enrichment::atlas::seed_population::population_marker_is_current(
                &article_atlas
            ),
            "the population marker must be stamped, or the next backfill drops these seeds"
        );
        // And the projected atoms are of a kind the derived population admits
        // — a Summary written into an atlas whose table is not built from
        // Summary is a seed row nothing will ever look at.
        assert!(
            corpus_engine::enrichment::atlas::seed_population::seed_population(&article_atlas)
                .kinds
                .contains(&AtomType::Summary),
            "the seed population must admit Summary"
        );
        // The stub parent stayed empty: nothing leaked into `sep/atlas`.
        let stub = read_atlas_atoms(&corpus_dir.join("atlas")).unwrap();
        assert!(stub.atoms.is_empty());
    }

    /// A corpus with no summary table is an ABSENCE, reported in words — not
    /// an error, and not a zero that reads like "the projection ran clean".
    #[tokio::test]
    async fn a_corpus_with_no_summary_table_is_named_not_zeroed() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("bare")).unwrap();
        let report = write_summary_atoms(root.path(), "bare").await.unwrap();
        assert_eq!(report.rows_read, 0);
        assert!(report
            .degradations
            .iter()
            .any(|d| d.contains("nothing to project")));
    }
}
