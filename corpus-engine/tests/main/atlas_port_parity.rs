// SPDX-License-Identifier: AGPL-3.0-or-later
//! `impl AtlasPort for IngestAtlas`, proven where it lives (phase-b-47).
//!
//! sovereign-tools' tests drive their own code against
//! `corpus_engine_atlas_reader::ports::double::AtlasPortDouble`; each
//! real-engine assertion they used to make through `IngestAtlas` is made
//! here instead, on the implementor, over the same fixtures. The name of
//! each test is the one the svrn-side test cites.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::seed_population::{
    population_marker_is_current, seed_population,
};
use corpus_engine::IngestAtlas;
use corpus_engine_atlas_reader::citation::SourceCitation;
use corpus_engine_atlas_reader::fixtures::ARGUMENTATIVE_ENVELOPE;
use corpus_engine_atlas_reader::ports::{ArgumentativeResponse, AtlasPort};
use corpus_engine_atlas_reader::raptor_read::RaptorSummaryRow;
use corpus_index::types::EmbedFn;
use understanding_vocab::atoms::{
    AtomEnvelope, AtomId, AtomType, AtomsFile, ChunkRef, Entity, Summary,
};
use understanding_vocab::edges::EdgesFile;
use understanding_vocab::read::read_atlas_atoms;
use understanding_vocab::taxonomy::{EnrichmentDepth, EntityType};

fn entity(id: usize, name: &str) -> AtomEnvelope {
    AtomEnvelope::Entity(Entity {
        id: AtomId::entity(id),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Concept,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: "x".into(),
        salience: 1.0,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        defining_quote: None,
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    })
}

fn write_atoms(atlas_dir: &Path, atoms: Vec<AtomEnvelope>) {
    std::fs::create_dir_all(atlas_dir).unwrap();
    std::fs::write(
        atlas_dir.join("atoms.json"),
        serde_json::to_vec_pretty(&AtomsFile::new(atoms)).unwrap(),
    )
    .unwrap();
}

/// atlas_status' and atlas_view's summaries: `None` without an
/// `atoms.json`, counts by type (an extracted entity is a tier-2 atom), and
/// the `_summary.json` sidecar serves a repeat read even when `atoms.json`
/// becomes unreadable at the same size and mtime (the cache key).
#[test]
fn atlas_summary_counts_by_type_and_caches_the_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("wikipedia").join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    assert!(IngestAtlas.atlas_summary(&atlas_dir).unwrap().is_none());

    write_atoms(&atlas_dir, vec![entity(1, "Earth")]);
    let one = IngestAtlas.atlas_summary(&atlas_dir).unwrap().unwrap();
    assert_eq!(one.tier2_count, 1);

    write_atoms(&atlas_dir, vec![entity(1, "Earth"), entity(2, "Mars")]);
    let first = IngestAtlas.atlas_summary(&atlas_dir).unwrap().unwrap();
    assert_eq!(first.atom_count, 2);
    assert!(atlas_dir.join("_summary.json").exists());

    let atoms = atlas_dir.join("atoms.json");
    let original = std::fs::metadata(&atoms).unwrap();
    std::fs::write(&atoms, vec![0u8; original.len() as usize]).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&atoms)
        .unwrap()
        .set_modified(original.modified().unwrap())
        .unwrap();
    let second = IngestAtlas.atlas_summary(&atlas_dir).unwrap().unwrap();
    assert_eq!(second.atom_count, 2);
    assert_eq!(second.atom_counts.get(&AtomType::Entity).copied(), Some(2));
}

/// atlas_view's build report and the context manager's load path: a fresh
/// atlas has no current ANN table.
#[test]
fn ann_table_is_not_fresh_on_a_fresh_atlas() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!IngestAtlas.ann_table_is_fresh(tmp.path()));
}

/// atlas_gaps: read → detect → write, in the schema the downstream reads.
#[test]
fn write_deterministic_gaps_writes_the_schema_the_downstream_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("c1").join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    let (n, path) = IngestAtlas
        .write_deterministic_gaps(&atlas_dir, &[], &[])
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(path, atlas_dir.join("gaps.json"));
    let g: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(g["gaps"].as_array().unwrap().len(), 0);
    assert_eq!(g["schema_version"], "2.0");
}

/// atlas_tensions: select → write, in the schema the classifier reads.
#[test]
fn write_tension_candidates_writes_the_schema_the_classifier_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("c1").join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    let (n, path) = IngestAtlas
        .write_tension_candidates(&atlas_dir, &[])
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(path, atlas_dir.join("tension_candidates.json"));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["candidates"].as_array().unwrap().len(), 0);
    assert_eq!(v["schema_version"], "2.0");
}

/// atlas_postinstall's triage: Earth is the one vital article among the
/// synthetic names (level 1), and a title and its underscored,
/// case-folded spelling normalize to one key.
#[test]
fn vital_tier_and_normalize_title_answer_triage_s_fixture_names() {
    assert_eq!(IngestAtlas.vital_tier("Earth"), Some(1));
    let mut off_list: Vec<String> = vec!["Alpha thing".into(), "Beta thing".into()];
    off_list.extend((0..5).map(|i| format!("Random Page {i}")));
    off_list.extend(["A", "B", "C", "D"].map(|c| format!("Neighbour {c}")));
    off_list.extend((1..=5).map(|i| format!("Noise {i}")));
    for name in &off_list {
        assert_eq!(IngestAtlas.vital_tier(name), None, "{name}");
    }
    assert_eq!(
        IngestAtlas.normalize_title("Beta thing"),
        IngestAtlas.normalize_title("beta_Thing")
    );
}

/// typed_extension's prompt bodies: the recovery block names the excerpts
/// and the naming discipline, truncates an over-long quote, and carries no
/// excerpt heading when there are none.
#[test]
fn render_source_recovery_block_carries_the_naming_discipline() {
    let block = IngestAtlas.render_source_recovery_block(&[
        "The practice known as spread pricing lets PBMs charge payers more.",
        "FTC documented $1.4B per year in spread pricing income.",
    ]);
    assert!(block.contains("Verbatim source excerpts"), "{block}");
    assert!(block.contains("$1.4B"));
    assert!(block.contains("Atom-naming discipline"));
    assert!(block.contains("Prefer verbatim phrasings"));

    let long = "a".repeat(2_000);
    assert!(IngestAtlas
        .render_source_recovery_block(&[long.as_str()])
        .contains('…'));

    assert!(!IngestAtlas
        .render_source_recovery_block(&[])
        .contains("Verbatim source excerpts"));
}

/// The response typed_extension's double counts as atoms parses under the
/// real parser into atoms, whole and cross-leaf only.
#[test]
fn argumentative_atom_count_parses_the_canned_envelope() {
    assert!(
        IngestAtlas
            .argumentative_atom_count(ARGUMENTATIVE_ENVELOPE, false)
            .unwrap()
            >= 5
    );
    assert!(
        IngestAtlas
            .argumentative_atom_count(ARGUMENTATIVE_ENVELOPE, true)
            .unwrap()
            >= 2
    );
    assert!(IngestAtlas
        .argumentative_atom_count("not json", false)
        .is_err());
}

/// A deterministic 4-d embedder: typed_extension's canned provider.
fn embed() -> EmbedFn {
    Arc::new(|text: &str| {
        let n = text.len() as f32;
        Box::pin(async move { Ok(vec![n, 1.0, 0.0, 0.0]) })
    })
}

fn response(section_id: &str, cross_leaf_only: bool) -> ArgumentativeResponse {
    ArgumentativeResponse {
        section_id: section_id.into(),
        response_text: ARGUMENTATIVE_ENVELOPE.into(),
        cross_leaf_only,
    }
}

/// typed_extension's `end_to_end_writes_atoms_and_manifest`: two Pass A
/// leaves and two Pass B themes, as the pass hands them over, land all
/// FOUR artifacts, populate every axis, and carry content-hash ids.
#[test]
fn typed_extension_write_lands_every_artifact_with_content_hash_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let responses = vec![
        response("n-leaf-1", false),
        response("n-leaf-2", false),
        response("theme:theme-1", true),
        response("theme:theme-2", true),
    ];
    let citations: HashMap<String, SourceCitation> = responses
        .iter()
        .map(|r| {
            (
                r.section_id.clone(),
                SourceCitation::from_primary(&r.section_id, None),
            )
        })
        .collect();
    let per_kind = IngestAtlas
        .write_typed_extension(
            "test-corpus-e2e",
            &atlas_dir,
            &responses,
            Vec::new(),
            &citations,
            embed(),
        )
        .unwrap();

    for artifact in ["atoms.json", "atoms.lance", "edges.csr", "atoms_ann.lance"] {
        assert!(atlas_dir.join(artifact).exists(), "{artifact} missing");
    }
    for kind in [
        "mechanism",
        "named_position",
        "evidence",
        "opposition",
        "concession",
    ] {
        assert!(
            per_kind.get(kind).copied().unwrap_or(0) >= 1,
            "{kind} must populate: {per_kind:?}"
        );
    }

    let atoms = read_atlas_atoms(&atlas_dir).unwrap();
    assert!(!atoms.atoms().is_empty());
    for atom in atoms.atoms() {
        let id = atom.id().as_str();
        let (prefix, suffix) = id.split_once('-').expect("atom id carries a `-`");
        assert!(!prefix.is_empty(), "{id}");
        assert_eq!(suffix.len(), 16, "{id} must use the 16-hex content hash");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }
}

/// typed_extension's `atoms_carry_primary_source_citations_when_quote_spans_present`:
/// a response keyed on `chunk:<id>` with that chunk's verbatim sentence as
/// its citation puts both on every atom it produces.
#[test]
fn typed_extension_write_carries_primary_source_citations() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let primary_quote =
        "Spread pricing lets PBMs charge payers more than they reimburse pharmacies.";
    let citation = SourceCitation::from_primary("n-cite-1", Some((7777, primary_quote)));
    assert_eq!(citation.section_id, "chunk:7777");
    let responses = vec![response(&citation.section_id, false)];
    let citations = HashMap::from([(citation.section_id.clone(), citation)]);
    IngestAtlas
        .write_typed_extension(
            "test-corpus-citations",
            &atlas_dir,
            &responses,
            Vec::new(),
            &citations,
            embed(),
        )
        .unwrap();

    let raw = std::fs::read_to_string(atlas_dir.join("atoms.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let atoms = parsed["atoms"].as_array().unwrap();
    assert!(!atoms.is_empty());
    let mut with_preview = 0usize;
    for atom in atoms {
        let data = &atom["data"];
        let first = data
            .get("first_appearance")
            .or_else(|| data["evidence"].as_array().and_then(|a| a.first()))
            .expect("each atom carries a source citation");
        assert_eq!(first["chunk_id"].as_str(), Some("chunk:7777"));
        if let Some(preview) = first["passage_preview"].as_str() {
            assert_eq!(preview, primary_quote);
            with_preview += 1;
        }
    }
    assert!(with_preview > 0, "source recovery is structurally broken");
}

fn summary_atom(node: usize, corpus_id: &str) -> AtomEnvelope {
    let node_id = format!("node-{node}");
    AtomEnvelope::Summary(Summary {
        id: AtomId::summary_content_hash(&node_id, corpus_id),
        node_id,
        level: (node % 2) as u32,
        text: format!("rollup {node}"),
        evidence: Vec::new(),
        children: Vec::new(),
        enrichment_depth: EnrichmentDepth::Extracted,
    })
}

/// summary_atoms' `projecting_twice_writes_the_atoms_once`, the engine half:
/// the raptor index reads back the rows it was built from, a SEP url names
/// its article, rewriting the same atoms REPLACES them (and rebuilds the
/// atom store), and the population marker is current and admits Summary.
#[tokio::test]
async fn summary_projection_writes_are_idempotent_and_keep_the_seeds() {
    let tmp = tempfile::tempdir().unwrap();
    let corpus_dir = tmp.path().join("sep");
    std::fs::create_dir_all(&corpus_dir).unwrap();
    let conv = "https://plato.stanford.edu/entries/abduction/";
    let rows: Vec<RaptorSummaryRow> = (0..4)
        .map(|i| RaptorSummaryRow {
            node_id: format!("node-{i}"),
            conv_uuid: conv.into(),
            level: (i % 2) as i64,
            summary: format!("rollup {i}"),
            embedding: (0..8usize)
                .map(|d| ((i * 131 + d * 977 + 7) % 1000) as f32 / 500.0 - 1.0)
                .collect(),
        })
        .collect();
    assert_eq!(
        IngestAtlas
            .build_raptor_index(&corpus_dir, &rows, 1)
            .await
            .unwrap(),
        4
    );
    let mut scanned = IngestAtlas
        .scan_raptor_summaries(&corpus_dir)
        .await
        .unwrap();
    scanned.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    assert_eq!(
        scanned
            .iter()
            .map(|r| r.node_id.as_str())
            .collect::<Vec<_>>(),
        rows.iter().map(|r| r.node_id.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(IngestAtlas.raptor_article_title(conv), "abduction");

    let atlas_dir = tmp.path().join("sep-abduction").join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();
    let atoms = AtomsFile::new((0..4).map(|i| summary_atom(i, "sep")).collect());
    for _ in 0..2 {
        IngestAtlas
            .write_atlas_edges(&atlas_dir, &EdgesFile::new(Vec::new()))
            .unwrap();
        IngestAtlas.write_atlas_atoms(&atlas_dir, &atoms).unwrap();
    }
    assert_eq!(read_atlas_atoms(&atlas_dir).unwrap().atoms().len(), 4);
    assert!(atlas_dir.join("atoms.lance").is_dir());

    IngestAtlas.write_population_marker(&atlas_dir).unwrap();
    assert!(population_marker_is_current(&atlas_dir));
    assert!(seed_population(&atlas_dir)
        .kinds
        .contains(&AtomType::Summary));
}
