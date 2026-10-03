// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for [`super`] — the RAPTOR-summary projection.
//!
//! In a sibling file because the fixtures (a real `raptor_summaries.lance`, a
//! written checkpoint tree) put `summary_atoms.rs` into arch-gate's 800-1200
//! approach band. `#[path]`, so every test name is unchanged.

use super::*;
use corpus_engine_atlas_reader::ports::double::AtlasPortDouble;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use understanding_vocab::atoms::AtomsFile;

/// The atlas port as the projection asks it: `rows` are the corpus's
/// `raptor_summaries.lance`, every SEP url names the `abduction` article,
/// and the atlas writes land as the plain `atoms.json` / `edges.json` the
/// leaf reads back. The atom-store rebuild, the raptor-index round trip and
/// the population marker are ingest's, proven on `IngestAtlas`
/// (corpus-engine's atlas_port_parity
/// `summary_projection_writes_are_idempotent_and_keep_the_seeds`). Each
/// population-marker request is recorded in `marked`.
fn port(rows: Vec<RaptorSummaryRow>, marked: Arc<Mutex<Vec<PathBuf>>>) -> AtlasPortDouble {
    AtlasPortDouble::new()
        .on_scan_raptor_summaries(move |_| Ok(rows.clone()))
        .on_raptor_article_title(|_| "abduction".into())
        .on_write_atlas_edges(|dir, edges| {
            let path = dir.join("edges.json");
            std::fs::write(&path, serde_json::to_vec_pretty(edges).unwrap())?;
            Ok(path)
        })
        .on_write_atlas_atoms(|dir, atoms| {
            let path = dir.join("atoms.json");
            std::fs::write(&path, serde_json::to_vec_pretty(atoms).unwrap())?;
            Ok(path)
        })
        .on_write_population_marker(move |dir| {
            marked.lock().unwrap().push(dir.to_path_buf());
            Ok(())
        })
}

fn emb(i: usize) -> Vec<f32> {
    (0..8usize)
        .map(|d| ((i * 131 + d * 977 + 7) % 1000) as f32 / 500.0 - 1.0)
        .collect()
}

/// An atlas directory with an EMPTY but valid `atoms.json` — the shape a
/// per-article atlas has before anything has been written into it.
fn empty_atlas(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let atoms = AtomsFile::new(Vec::new());
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
    let picked = atlas_dir_for(root.path(), "wessex-hoard", "chapter-3").expect("the parent atlas");
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
    let marked: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let atlas = port(rows, Arc::clone(&marked));

    let first = write_summary_atoms(&atlas, root.path(), "sep")
        .await
        .unwrap();
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

    let second = write_summary_atoms(&atlas, root.path(), "sep")
        .await
        .unwrap();
    assert_eq!(second.atoms_written, 0, "second run must add no atom");
    assert_eq!(second.seeds_written, 0, "second run must add no seed row");
    assert_eq!(second.skipped_already_present, 4);

    let article_atlas = root.path().join("sep-abduction").join("atlas");
    let on_disk = read_atlas_atoms(&article_atlas).unwrap();
    assert_eq!(on_disk.atoms().len(), 4, "atoms.json doubled");

    // The marker, and it is not bookkeeping. ei-7a bumped
    // `SEED_POPULATION_SCHEMA` 1 -> 2, so every marker on disk is stale by
    // definition; a stale marker makes `ann_table_is_fresh` false, which
    // sends the daemon's backfill through `build_persistent_ann_seed_table`
    // — which REPLACES the table. Without this stamp the appended Summary
    // seeds are deleted at the next boot, with nothing erroring and no
    // count looking wrong.
    assert!(
        marked.lock().unwrap().contains(&article_atlas),
        "the population marker must be stamped, or the next backfill drops these seeds"
    );
    // That a stamped marker is current, and that the derived population
    // admits Summary, is ingest's (proven on `IngestAtlas`).
    // The stub parent stayed empty: nothing leaked into `sep/atlas`.
    let stub = read_atlas_atoms(&corpus_dir.join("atlas")).unwrap();
    assert!(stub.atoms().is_empty());
}

/// A rebuilt tree replaces the one it rebuilt.
///
/// Every rebuild mints new node ids, and the projection only appended: after
/// one `enrich raptor --force` on the pilot the atlas held 14 Summary atoms
/// of the old tree beside 14 of the new, and the walk reached both.
#[tokio::test]
async fn a_rebuilt_tree_retires_the_summaries_it_replaced() {
    let root = tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("sep")).unwrap();
    empty_atlas(&root.path().join("sep").join("atlas"));
    empty_atlas(&root.path().join("sep-abduction").join("atlas"));
    let tree = |first: usize| -> Vec<RaptorSummaryRow> {
        (first..first + 3)
            .map(|i| RaptorSummaryRow {
                node_id: format!("node-{i}"),
                conv_uuid: "https://plato.stanford.edu/entries/abduction/".into(),
                level: 0,
                summary: format!("rollup {i}"),
                embedding: emb(i),
            })
            .collect()
    };
    let marked: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    write_summary_atoms(&port(tree(0), Arc::clone(&marked)), root.path(), "sep")
        .await
        .unwrap();
    let rebuilt = write_summary_atoms(&port(tree(10), marked), root.path(), "sep")
        .await
        .unwrap();
    assert_eq!(rebuilt.retired, 3);

    let article_atlas = root.path().join("sep-abduction").join("atlas");
    let want: HashSet<String> = (10..13)
        .map(|i| {
            AtomId::summary_content_hash(&format!("node-{i}"), "sep")
                .as_str()
                .to_string()
        })
        .collect();
    let atoms: HashSet<String> = read_atlas_atoms(&article_atlas)
        .unwrap()
        .atoms()
        .iter()
        .map(|a| a.id().as_str().to_string())
        .collect();
    assert_eq!(atoms, want, "atoms.json holds a tree that is not installed");
    let seeds: HashSet<String> = AnnSeedTable::open(&ann_table_dir(&article_atlas))
        .await
        .unwrap()
        .all_rows()
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(seeds, want, "the seed table still reaches the old tree");
}

/// The atoms the idempotence check used to strand.
///
/// Failing input, named: project with no checkpoint (a legitimate, REPORTED
/// outcome — and the shape the tree-slot bug produced on both RAPTOR
/// corpora), then make the tree readable and re-run. Under the old
/// predicate the second run skips all four on id alone and the atoms stay
/// uncitable forever; the tool cannot repair what the tool wrote.
#[tokio::test]
async fn a_tree_less_projection_is_repaired_when_the_tree_arrives() {
    let root = tempdir().unwrap();
    let corpus_dir = root.path().join("sep");
    std::fs::create_dir_all(&corpus_dir).unwrap();
    empty_atlas(&corpus_dir.join("atlas"));
    empty_atlas(&root.path().join("sep-abduction").join("atlas"));
    let conv = "https://plato.stanford.edu/entries/abduction/";

    let rows: Vec<RaptorSummaryRow> = (0..4)
        .map(|i| RaptorSummaryRow {
            node_id: format!("node-{i}"),
            conv_uuid: conv.into(),
            level: (i % 2) as i64,
            summary: format!("rollup {i}"),
            embedding: emb(i),
        })
        .collect();
    let marked: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let atlas = port(rows, Arc::clone(&marked));

    let first = write_summary_atoms(&atlas, root.path(), "sep")
        .await
        .unwrap();
    assert_eq!(first.atoms_written, 4);
    assert_eq!(first.no_tree_row, 4, "no checkpoint yet");
    assert_eq!(first.with_evidence, 0, "uncitable, by construction");

    // The tree arrives. `node-3` composes `node-0`, so the repair has a
    // `Composes` edge to write as well as evidence to attach.
    let handle = RaptorCheckpointHandle::at_note(&corpus_dir, conv, "h");
    for i in 0..4u32 {
        let mut node = sovereign_core::types::RaptorNode {
            node_id: format!("node-{i}"),
            level: (i % 2) as u8,
            summary: format!("rollup {i}"),
            summary_embedding: vec![0.1, 0.2, 0.3],
            centroid_embedding: vec![0.4, 0.5, 0.6],
            children_node_ids: Vec::new(),
            direct_member_chunk_ids: vec![i * 10, i * 10 + 1],
            evidence_chunk_ids: vec![i * 10, i * 10 + 1],
            quote_spans: Vec::new(),
            primary_entities: Vec::new(),
            cluster_coherence: 0.8,
            created_at: chrono::Utc::now(),
            prompt_version: String::new(),
            summarizer_model: String::new(),
        };
        if i == 3 {
            node.children_node_ids = vec!["node-0".into()];
        }
        handle
            .write_cluster_node(node.level, i as usize, &node)
            .unwrap();
    }

    let second = write_summary_atoms(&atlas, root.path(), "sep")
        .await
        .unwrap();
    assert_eq!(second.repaired, 4, "every stranded atom must be repaired");
    assert_eq!(second.atoms_written, 0, "a repair adds no atom");
    assert_eq!(
        second.seeds_written, 0,
        "a repair keeps its id, so its seed row is already right — a second \
         append would be two rows under one key"
    );
    assert_eq!(second.with_evidence, 4);
    assert_eq!(second.edges_written, 1, "node-3 Composes node-0");

    let article_atlas = root.path().join("sep-abduction").join("atlas");
    let on_disk = read_atlas_atoms(&article_atlas).unwrap();
    assert_eq!(on_disk.atoms().len(), 4, "repair must REPLACE, not append");
    for a in on_disk.atoms() {
        let AtomEnvelope::Summary(s) = a else {
            panic!("only Summary atoms in this fixture")
        };
        assert!(
            !s.evidence.is_empty(),
            "{} is still uncitable after the repair",
            s.node_id
        );
    }
    let edges = read_atlas_edges(&article_atlas).unwrap();
    assert_eq!(edges.edges.len(), 1);
    assert_eq!(edges.edges[0].edge_type, EdgeType::Composes);

    // Still idempotent: the repair filled the fields the predicate reads,
    // so a third run has nothing to do.
    let third = write_summary_atoms(&atlas, root.path(), "sep")
        .await
        .unwrap();
    assert_eq!(third.repaired, 0);
    assert_eq!(third.atoms_written, 0);
    assert_eq!(third.skipped_already_present, 4);
    assert_eq!(
        read_atlas_edges(&article_atlas).unwrap().edges.len(),
        1,
        "a third run must not re-mint the Composes edge"
    );
}

/// A corpus with no summary table is an ABSENCE, reported in words — not
/// an error, and not a zero that reads like "the projection ran clean".
#[tokio::test]
async fn a_corpus_with_no_summary_table_is_named_not_zeroed() {
    let root = tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("bare")).unwrap();
    let report = write_summary_atoms(
        &AtlasPortDouble::new().on_scan_raptor_summaries(|_| Ok(Vec::new())),
        root.path(),
        "bare",
    )
    .await
    .unwrap();
    assert_eq!(report.rows_read, 0);
    assert!(report
        .degradations
        .iter()
        .any(|d| d.contains("nothing to project")));
}

fn person(id: usize, name: &str, aliases: &[&str]) -> AtomEnvelope {
    AtomEnvelope::Entity(understanding_vocab::atoms::Entity {
        id: AtomId::entity(id),
        canonical_name: name.into(),
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        entity_type: understanding_vocab::taxonomy::EntityType::Concept,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: "x".into(),
        defining_quote: None,
        salience: 0.5,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: vec![],
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    })
}

fn summary(node: &str, text: &str) -> AtomEnvelope {
    AtomEnvelope::Summary(Summary {
        id: AtomId::summary_content_hash(node, "pilot"),
        node_id: node.into(),
        level: 0,
        text: text.into(),
        evidence: Vec::new(),
        children: Vec::new(),
        enrichment_depth: EnrichmentDepth::extracted_default(),
    })
}

/// A summary is linked to every entity its text names, by alias as well as
/// canonical name, weighted by mentions over the most-mentioned entity's; an
/// entity it does not name gets no edge, a summary naming nobody is counted,
/// and a second pass over the linked atlas adds nothing. Failing inputs: a
/// projector that writes no `Involves` (the pre-2026-10-03 shape), weights
/// every mention 1.0, or relinks on every run.
#[test]
fn a_summary_involves_the_entities_its_text_names_once() {
    let dir = tempdir().unwrap();
    let s1 = summary(
        "n1",
        "Elizabeth refuses Carl; Elizabeth waits for Salve Kristiansen.",
    );
    let s2 = summary("n2", "The harbour freezes over in a hard winter.");
    let kept = vec![
        person(1, "Elizabeth Raklev", &["Elizabeth"]),
        person(2, "Salve Kristiansen", &[]),
        person(3, "Fru Beck", &[]),
        s1.clone(),
        s2.clone(),
    ];
    let installed: HashSet<String> = [&s1, &s2]
        .iter()
        .map(|a| a.id().as_str().to_string())
        .collect();
    let mut next = 7;
    let mut report = SummaryProjection::default();
    let edges = involves_edges(
        dir.path(),
        &kept,
        &[],
        &installed,
        &[],
        &mut next,
        &mut report,
    );
    let got: Vec<(&str, &str, f32)> = edges
        .iter()
        .map(|e| (e.source.as_str(), e.target.as_str(), e.confidence))
        .collect();
    let s1_id = s1.id().as_str();
    assert_eq!(
        got,
        vec![
            (s1_id, AtomId::entity(1).as_str(), 1.0),
            (s1_id, AtomId::entity(2).as_str(), 0.5),
        ]
    );
    assert!(edges.iter().all(|e| e.edge_type == EdgeType::Involves));
    assert_eq!(
        (report.involves_written, report.unlinked_summaries, next),
        (2, 1, 9)
    );

    let again = involves_edges(
        dir.path(),
        &kept,
        &[],
        &installed,
        &edges,
        &mut next,
        &mut report,
    );
    assert!(
        again.iter().all(|e| e.source.as_str() != s1_id),
        "linked twice"
    );
}
