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
//! | writing atoms + edges | `atlas::write_atlas_edges` + `write_atlas_atoms` |
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
use corpus_engine::enrichment::atlas::atoms::{
    AtomEnvelope, AtomId, AtomType, AtomsFile, ChunkRef, Summary,
};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use corpus_engine::enrichment::atlas::ground::candidate_atlas_ids;
use corpus_engine::enrichment::atlas::seed_population::{seed_population, write_population_marker};
use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, write_atlas_atoms, write_atlas_edges,
};
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
    /// Atoms already on disk with neither evidence nor children, REPLACED in
    /// place because the tree now has both. Separate from `atoms_written`: the
    /// atlas gains no atom and no seed row, it gains provenance.
    pub repaired: usize,
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
        if self.repaired > 0 {
            s.push_str(&format!(
                "; {} already-projected atoms repaired in place (evidence and children \
                 attached to summaries that had neither)",
                self.repaired
            ));
        }
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
    // `load_corpus_nodes`, NOT `at(..).load_all_nodes()`: the latter reads only
    // the shared per-corpus slot, and a corpus built per-note keeps its nodes
    // in `_raptor_checkpoint/note-<hash>/`. Reading the parent matched no
    // `level-` dir and returned Ok(empty), which is indistinguishable from
    // "this corpus has no tree" — so every Summary atom was written with
    // `evidence: []` and `children: []`. Neither hash is consulted on this
    // path, so the empty one is unused rather than a freshness claim.
    match RaptorCheckpointHandle::load_corpus_nodes(corpus_dir) {
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
/// **Idempotent, and repairing.** A node whose atom id is already in the target
/// atlas is skipped, so a second run adds nothing — which is what lets the seed
/// rows be APPENDED (`AnnSeedTable::append_rows`) rather than rebuilt: a key
/// written twice would be two rows and the walk would see the atom twice.
///
/// The one exception is the atom the skip used to strand: a `Summary` already
/// on disk with neither evidence nor children, whose node the tree DOES have.
/// That atom is replaced in place, keeping its id and therefore its seed row.
/// Without this, a projection made while the tree was unreadable could never be
/// repaired by re-running the tool that made it — the atoms stay uncitable for
/// the life of the corpus, which is the state `chaos-secret-agent` (19) and
/// `raptor-pilot-and-his-wife` (14) were in after 2026-09-21's tree-slot fix.
/// Still idempotent: a second repair run finds nothing repairable, because the
/// first filled the fields the predicate reads.
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
        // Two situations wear the same empty map, and they are not the same
        // thing. If slots exist on disk, the tree is THERE and we failed to
        // read it — refuse, before a single atom is written, rather than emit
        // a corpus of summaries no reader can trace to a source. That failure
        // shipped until 2026-09-21: `load_tree` read the shared per-corpus
        // slot while the nodes lived in per-note slots one directory down, so
        // `raptor-pilot-and-his-wife` (14 atoms) and `chaos-secret-agent` (19)
        // were both projected entirely uncitable, under a degradation line
        // that blamed an empty `conv_raptor_nodes` — which held 14 rows.
        //
        // Refusing here and not after the write loop is deliberate: an Err
        // returned once the atlas is on disk leaves the damage behind and
        // still calls itself a failure.
        if RaptorCheckpointHandle::corpus_has_slots(&corpus_dir) {
            // This return is before the projection ledger at the end of the
            // function, so without this event a refusal is invisible at
            // `tracing=debug` — the decision that stops the whole projection
            // would be the one decision that left no trace.
            tracing::warn!(
                corpus = corpus_id,
                dir = %corpus_dir.display(),
                rows = report.rows_read,
                "summary-atoms: refusing — checkpoint slots present but tree read empty"
            );
            return Err(format!(
                "summary-atoms: {}/_raptor_checkpoint has node slots on disk, but the \
                 tree read back empty. Every one of the {} summaries would be written \
                 without evidence chunks and without Composes edges — uncitable. \
                 This is a tree-READ failure, not an absent tree; refusing rather than \
                 projecting provenance-free summaries.",
                corpus_dir.display(),
                report.rows_read
            ));
        }
        report.degradations.push(format!(
            "no RAPTOR checkpoint under {} at all — every Summary atom is written \
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
        // Presence alone is the WRONG skip predicate, and that is what left
        // both RAPTOR corpora permanently uncitable. A `Summary` atom with
        // neither evidence nor children is the damage shape of a projection
        // that ran while the tree could not be read (the bug fixed above it,
        // and the `no RAPTOR checkpoint at all` degradation that is still a
        // legitimate outcome). Its id is on disk, so every later run — including
        // every run made AFTER the tree became readable — skipped it. The fix
        // could not reach the data it fixed.
        //
        // So: skip a present id when the tree has nothing to ADD to it, and
        // REPAIR it when the tree does. Both are idempotent, which is the
        // property that mattered; a second repair run finds nothing repairable
        // because the first one filled the fields the predicate reads.
        let mut present: HashSet<String> = HashSet::new();
        let mut repairable: HashSet<String> = HashSet::new();
        for a in existing.atoms() {
            present.insert(a.id().as_str().to_string());
            if let AtomEnvelope::Summary(s) = a {
                if s.evidence.is_empty() && s.children.is_empty() {
                    repairable.insert(s.id.as_str().to_string());
                }
            }
        }
        // `EdgeId::new(i)` is positional, so a second run must not restart at
        // 0 and collide with the edges already on disk. Read it or REFUSE —
        // an `unwrap_or(0)` here would mint `edge-00000` on top of an existing
        // one whenever `edges.json` failed to parse, which is a silent
        // substitution of a wrong id for an unreadable one (§18.3). The write
        // below needs the file anyway, so failing here fails earlier and says
        // why.
        let mut edges_file = read_atlas_edges(&atlas_dir).map_err(|e| {
            format!(
                "read {}: {e} (an atlas with atoms.json needs an edges.json beside it)",
                atlas_dir.join("edges.json").display()
            )
        })?;
        let mut next_edge_ix = edges_file.edges.len();

        let mut atoms: Vec<AtomEnvelope> = Vec::new();
        let mut repaired: Vec<AtomEnvelope> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut seeds: Vec<(String, Vec<f32>)> = Vec::new();

        for row in article_rows {
            let id = atom_id_of(&row.node_id);
            // Looked up BEFORE the skip and counted after it: `no_tree_row` is
            // a degradation of what this run WROTE, and a re-run that writes
            // nothing must not report every skipped row as a missing node.
            let tree_entry = tree.get(&row.node_id);
            let is_repair = if present.contains(id.as_str()) {
                let upgradable = repairable.contains(id.as_str())
                    && tree_entry.is_some_and(|(e, c)| !e.is_empty() || !c.is_empty());
                if !upgradable {
                    report.skipped_already_present += 1;
                    continue;
                }
                true
            } else {
                false
            };
            let (evidence_ids, child_ids) = match tree_entry {
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
            // Seeds are for NEW atoms only. The atom id is the content hash of
            // `node_id`, so a repaired atom keeps the id its seed row is
            // already keyed by — appending again would be two rows under one
            // key and the walk would see the atom twice.
            if !is_repair {
                if row.embedding.is_empty() {
                    report.no_embedding += 1;
                } else {
                    seeds.push((id.as_str().to_string(), row.embedding.clone()));
                }
            }
            let atom = AtomEnvelope::Summary(Summary {
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
            });
            if is_repair {
                report.repaired += 1;
                repaired.push(atom);
            } else {
                atoms.push(atom);
            }
        }

        if atoms.is_empty() && repaired.is_empty() && edges.is_empty() {
            continue;
        }
        report.atoms_written += atoms.len();
        report.edges_written += edges.len();
        report.atlases_written += 1;

        // ONE write path for both cases, not an append plus a replace: a
        // repair substitutes an atom under an id the file already holds, which
        // `append_atoms_and_edges` cannot express, and with no repairs the
        // merged set is exactly what appending produces.
        let mut merged: Vec<AtomEnvelope> = existing.atoms().to_vec();
        for fixed in repaired {
            match merged
                .iter_mut()
                .find(|a| a.id().as_str() == fixed.id().as_str())
            {
                Some(slot) => *slot = fixed,
                // Unreachable: `repairable` was built from this same vector.
                // A silent drop would be an atom the report counted and the
                // file does not have (§18.3).
                None => {
                    return Err(format!(
                        "repair target {} vanished from {} between read and write",
                        fixed.id().as_str(),
                        atlas_dir.display()
                    ))
                }
            }
        }
        merged.extend(atoms.iter().cloned());
        edges_file.edges.extend(edges.iter().cloned());
        // Edges FIRST: `write_atlas_atoms` rebuilds `atoms.lance` from the
        // atoms it is handed and the edges it reads back off disk, so the
        // other order would build the store without the new `Composes` edges.
        write_atlas_edges(&atlas_dir, &edges_file)
            .map_err(|e| format!("write edges to {}: {e}", atlas_dir.display()))?;
        write_atlas_atoms(
            &atlas_dir,
            &AtomsFile::from_atoms(existing.schema_version.clone(), merged),
        )
        .map_err(|e| format!("write atoms to {}: {e}", atlas_dir.display()))?;

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
        // The tree's children + evidence chunk ids live ONLY in
        // `_raptor_checkpoint` — `conv_raptor_nodes` does not carry them — so a
        // summary with no checkpoint node is a summary a reader cannot trace
        // back to a source. Say which of the two situations this is; the
        // previous wording asserted "`conv_raptor_nodes` is empty", which was
        // not checked and was false on both corpora that hit it.
        report.degradations.push(format!(
            "{} of {} summaries resolved no checkpoint node, so they carry no \
             evidence chunks and no Composes edges — they are uncitable",
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
        // A repair writes no atom and skips nothing, so every other field in
        // this event reads as a run that did nothing. Without this one the
        // decision that replaced 33 uncitable atoms leaves no trace (§9.1).
        repaired = report.repaired,
        skipped = report.skipped_already_present,
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

// The tests live in a sibling file: their fixtures build a real
// `raptor_summaries.lance` and a written checkpoint tree, and keeping them
// here put this file into arch-gate's 800-1200 approach band (ARCH §3.1).
// `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "summary_atoms/tests.rs"]
mod tests;
