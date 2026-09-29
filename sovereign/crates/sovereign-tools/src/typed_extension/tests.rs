// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for the typed-extension pass.
//!
//! Drives `run_typed_extension` end-to-end with:
//! - an in-memory `SqliteStateStore` seeded with one Ready conv, two
//!   RAPTOR leaves, and two vault themes;
//! - a `CannedInferenceProvider` returning a fixed argumentative
//!   typed-extension JSON envelope.
//!
//! Pins the load-bearing contracts:
//! 1. Pass A + Pass B both fire (one LLM call per leaf, one per theme).
//! 2. `atoms.json` lands with the expected shape — content-hash ids,
//!    populated per-axis counts.
//! 3. Re-running the pass with no upstream changes returns
//!    `ExtractionStatus::SkippedManifestMatch` without LLM traffic.
//! 4. Editing a leaf summary invalidates the manifest and forces a
//!    re-run.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use corpus_engine_atlas_reader::citation::SourceCitation;
use corpus_engine_atlas_reader::fixtures;
use corpus_engine_atlas_reader::ports::double::AtlasPortDouble;
use corpus_engine_atlas_reader::ports::ArgumentativeResponse;
use futures::Stream;
use sovereign_core::conv_tiered::{ConvRaptorNodeRow, ConvSkeletonRow, VaultThemeRow};
use sovereign_core::error::{Error, Result as SovResult};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};
use sovereign_store::sqlite::SqliteStateStore;

use super::{run_typed_extension, ExtractionStatus, MANIFEST_FILENAME};

/// LLM stub. Returns a fixed argumentative typed-extension JSON
/// envelope on every `complete` call and counts how many calls fired.
/// The counter lets tests assert "the second run made no LLM calls".
struct CannedInferenceProvider {
    body: String,
    calls: Arc<AtomicUsize>,
}

impl CannedInferenceProvider {
    fn new(body: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                body: body.to_string(),
                calls: Arc::clone(&calls),
            }),
            calls,
        )
    }
}

#[async_trait]
impl InferenceProvider for CannedInferenceProvider {
    async fn complete(&self, _request: &CompletionRequest) -> SovResult<CompletionResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            text: self.body.clone(),
            tokens_used: 64,
            prompt_tokens: 32,
            model_id: "canned-test".into(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        Err(Error::NotImplemented(
            "CannedInferenceProvider has no streaming surface".into(),
        ))
    }

    /// A deterministic 4-d embedding, not a refusal.
    ///
    /// The stub HAD to grow one when ei-3-index made `atoms_ann.lance` part of
    /// the atlas write: a provider that cannot embed now fails the write, which
    /// is the point. Giving it a real (if trivial) embed surface means these
    /// four tests exercise the whole daemon path -- atoms.json, the v2 store
    /// AND the seed table -- instead of the two-thirds of it that used to run.
    /// `embed_query` defaults to this, and the query-side closure is what the
    /// loader is handed (`inference_to_embed_query_fn`), so the table and the
    /// queries stay in one vector space.
    async fn embed(&self, text: &str) -> SovResult<Vec<f32>> {
        let n = text.len() as f32;
        Ok(vec![n, 1.0, 0.0, 0.0])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

/// JSON envelope returned by the canned provider — one of each kind so
/// every axis count comes out non-zero. The leaf's fixture, so the
/// implementor-side test writes the same responses.
const CANNED_BODY: &str = fixtures::ARGUMENTATIVE_ENVELOPE;

/// What the pass handed the port's write: every response and the citation
/// map it keyed on section id.
#[derive(Default)]
struct Written {
    responses: Vec<ArgumentativeResponse>,
    citations: HashMap<String, SourceCitation>,
}

/// The per-kind counts the double's write answers with.
fn canned_counts() -> HashMap<String, u32> {
    [
        "mechanism",
        "named_position",
        "evidence",
        "opposition",
        "concession",
    ]
    .into_iter()
    .map(|k| (k.to_string(), 1))
    .collect()
}

/// The atlas port as the pass asks it. A response counts one atom when it is
/// JSON; the write records its inputs in `written` and answers
/// [`canned_counts`]. What ingest does with those inputs (resolve,
/// content-hash, the four artifacts, the citations) is proven on
/// `IngestAtlas` (corpus-engine's atlas_port_parity
/// `typed_extension_write_lands_every_artifact_with_content_hash_ids` and
/// `typed_extension_write_carries_primary_source_citations`).
fn port(written: Arc<Mutex<Written>>) -> AtlasPortDouble {
    AtlasPortDouble::new()
        .on_argumentative_system("argumentative system")
        .on_argumentative_schema(|| serde_json::json!({"type": "object"}))
        .on_render_source_recovery_block(|ex| ex.join("\n"))
        .on_argumentative_atom_count(|text, _| {
            serde_json::from_str::<serde_json::Value>(text)
                .map(|_| 1)
                .map_err(|e| e.to_string())
        })
        .on_write_typed_extension(move |_, _, responses, _, citations| {
            let mut w = written.lock().unwrap();
            w.responses.extend_from_slice(responses);
            w.citations.extend(citations.clone());
            Ok(canned_counts())
        })
}

async fn seed_store_with_two_leaves_and_two_themes(corpus_id: &str) -> Arc<SqliteStateStore> {
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());

    // One Ready conv so list_ready_source_doc_ids_for_corpus returns it.
    let conv_uuid = "conv-test-1";
    store
        .save_conv_skeleton(&ConvSkeletonRow {
            corpus_id: corpus_id.into(),
            conv_uuid: conv_uuid.into(),
            state: "Ready".into(),
            skeleton_json: None,
            overview: None,
            segments_json: None,
            chunk_count: 4,
            updated_at: 1_700_000_000,
        })
        .await
        .unwrap();

    // Two level-0 leaves, both above the tiny-stub threshold.
    let leaves = vec![
        make_leaf(corpus_id, conv_uuid, "n-leaf-1", "PBM spread pricing extracts opaque rents that cost payers and patients while delivering minimal intermediation value."),
        make_leaf(corpus_id, conv_uuid, "n-leaf-2", "Markets-versus-regulation framings in healthcare repeatedly understate concentration risks introduced by middleman consolidation."),
    ];
    store
        .save_conv_raptor_nodes(corpus_id, conv_uuid, &leaves)
        .await
        .unwrap();

    // Two vault themes.
    let themes = vec![
        VaultThemeRow {
            corpus_id: corpus_id.into(),
            theme_id: "theme-1".into(),
            summary:
                "Cross-note theme: markets versus regulation as competing allocation mechanisms."
                    .into(),
            summary_embedding: Vec::new(),
            member_source_doc_ids_json: "[]".into(),
            cluster_coherence: 0.9,
            created_at: 1_700_000_000,
        },
        VaultThemeRow {
            corpus_id: corpus_id.into(),
            theme_id: "theme-2".into(),
            summary:
                "Cross-note theme: PBM intermediation extracts more rent than it produces value."
                    .into(),
            summary_embedding: Vec::new(),
            member_source_doc_ids_json: "[]".into(),
            cluster_coherence: 0.85,
            created_at: 1_700_000_000,
        },
    ];
    store.save_vault_themes(corpus_id, &themes).await.unwrap();

    store
}

fn make_leaf(corpus_id: &str, conv_uuid: &str, node_id: &str, summary: &str) -> ConvRaptorNodeRow {
    ConvRaptorNodeRow {
        node_id: node_id.into(),
        corpus_id: corpus_id.into(),
        conv_uuid: conv_uuid.into(),
        level: 0,
        summary: summary.into(),
        summary_embedding: Vec::new(),
        centroid_embedding: Vec::new(),
        children_node_ids_json: "[]".into(),
        direct_member_chunk_ids_json: None,
        evidence_chunk_ids_json: "[]".into(),
        quote_spans_json: "[]".into(),
        primary_entities_json: r#"["PBM","Spread Pricing"]"#.into(),
        cluster_coherence: 0.9,
        created_at: 1_700_000_000,
        prompt_version: String::new(),
        summarizer_model: String::new(),
    }
}

#[tokio::test]
async fn end_to_end_writes_atoms_and_manifest() {
    let corpus_id = "test-corpus-e2e";
    let store = seed_store_with_two_leaves_and_two_themes(corpus_id).await;
    let (inference_arc, call_counter) = CannedInferenceProvider::new(CANNED_BODY);
    let inference: Arc<dyn InferenceProvider> = inference_arc;
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let written: Arc<Mutex<Written>> = Arc::default();
    let atlas = port(Arc::clone(&written));

    let report = run_typed_extension(
        &atlas,
        corpus_id,
        &store,
        &inference,
        &atlas_dir,
    )
    .await
    .expect("typed extension should succeed end-to-end");
    assert_eq!(report.status, ExtractionStatus::Wrote);
    assert_eq!(report.pass_a_calls, 2, "two leaves → two Pass A calls");
    assert_eq!(report.pass_b_calls, 2, "two themes → two Pass B calls");
    assert_eq!(
        call_counter.load(Ordering::SeqCst),
        4,
        "exactly four LLM calls total"
    );

    // The write gets every accepted response: Pass A's two leaves whole,
    // Pass B's two themes cross-leaf only, each as the model wrote it. The
    // four artifacts, the per-axis counts and the content-hash ids are what
    // ingest makes of these, proven on `IngestAtlas`.
    {
        let w = written.lock().unwrap();
        assert_eq!(w.responses.len(), 4);
        assert_eq!(w.responses.iter().filter(|r| !r.cross_leaf_only).count(), 2);
        assert_eq!(w.responses.iter().filter(|r| r.cross_leaf_only).count(), 2);
        assert!(w.responses.iter().all(|r| r.response_text == CANNED_BODY));
        for r in &w.responses {
            assert!(
                w.citations.contains_key(&r.section_id),
                "every response's section id keys a citation: {}",
                r.section_id
            );
        }
    }

    // The report and the manifest carry the counts the write answered.
    assert_eq!(report.atoms_per_kind, canned_counts());
    assert!(atlas_dir.join(MANIFEST_FILENAME).exists());
}

#[tokio::test]
async fn rerun_with_no_changes_skips_via_manifest() {
    let corpus_id = "test-corpus-skip";
    let store = seed_store_with_two_leaves_and_two_themes(corpus_id).await;
    let (inference_arc, call_counter) = CannedInferenceProvider::new(CANNED_BODY);
    let inference: Arc<dyn InferenceProvider> = inference_arc;
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let written: Arc<Mutex<Written>> = Arc::default();
    let atlas = port(Arc::clone(&written));

    let first = run_typed_extension(
        &atlas,
        corpus_id,
        &store,
        &inference,
        &atlas_dir,
    )
    .await
    .unwrap();
    assert_eq!(first.status, ExtractionStatus::Wrote);
    let calls_after_first = call_counter.load(Ordering::SeqCst);
    assert_eq!(calls_after_first, 4);

    let second = run_typed_extension(
        &atlas,
        corpus_id,
        &store,
        &inference,
        &atlas_dir,
    )
    .await
    .unwrap();
    assert_eq!(
        second.status,
        ExtractionStatus::SkippedManifestMatch,
        "manifest must short-circuit a no-op rerun"
    );
    assert_eq!(
        call_counter.load(Ordering::SeqCst),
        calls_after_first,
        "skipped rerun must fire zero new LLM calls"
    );
}

#[tokio::test]
async fn editing_a_leaf_invalidates_manifest_and_forces_rerun() {
    let corpus_id = "test-corpus-edit";
    let store = seed_store_with_two_leaves_and_two_themes(corpus_id).await;
    let (inference_arc, call_counter) = CannedInferenceProvider::new(CANNED_BODY);
    let inference: Arc<dyn InferenceProvider> = inference_arc;
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let written: Arc<Mutex<Written>> = Arc::default();
    let atlas = port(Arc::clone(&written));

    run_typed_extension(
        &atlas,
        corpus_id,
        &store,
        &inference,
        &atlas_dir,
    )
    .await
    .unwrap();
    let calls_after_first = call_counter.load(Ordering::SeqCst);
    assert_eq!(calls_after_first, 4);

    // Rewrite the conv's RAPTOR set with an edited summary.
    let new_leaves = vec![make_leaf(
        corpus_id,
        "conv-test-1",
        "n-leaf-1",
        "ENTIRELY DIFFERENT summary that should bust the manifest hash and force another extraction pass for this leaf.",
    )];
    store
        .save_conv_raptor_nodes(corpus_id, "conv-test-1", &new_leaves)
        .await
        .unwrap();

    let second = run_typed_extension(
        &atlas,
        corpus_id,
        &store,
        &inference,
        &atlas_dir,
    )
    .await
    .unwrap();
    assert_eq!(
        second.status,
        ExtractionStatus::Wrote,
        "summary edit must trigger re-extraction"
    );
    // One leaf in the new set, two themes — three LLM calls on the rerun.
    assert_eq!(
        call_counter.load(Ordering::SeqCst),
        calls_after_first + 3,
        "rerun must fire one Pass A call (per new leaf) plus two Pass B calls"
    );
}

#[tokio::test]
async fn empty_inputs_short_circuit_without_writes() {
    let corpus_id = "test-corpus-empty";
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let (inference_arc, call_counter) = CannedInferenceProvider::new(CANNED_BODY);
    let inference: Arc<dyn InferenceProvider> = inference_arc;
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let written: Arc<Mutex<Written>> = Arc::default();
    let atlas = port(Arc::clone(&written));

    let report = run_typed_extension(
        &atlas,
        corpus_id,
        &store,
        &inference,
        &atlas_dir,
    )
    .await
    .unwrap();
    assert_eq!(report.status, ExtractionStatus::SkippedNoInputs);
    assert_eq!(
        call_counter.load(Ordering::SeqCst),
        0,
        "no inputs → no LLM traffic"
    );
    assert!(atlas.calls().is_empty(), "no inputs → the port is never asked");
    assert!(!atlas_dir.join(MANIFEST_FILENAME).exists());
}

#[tokio::test]
async fn atoms_carry_primary_source_citations_when_quote_spans_present() {
    // Seed a corpus whose leaves carry verbatim quote_spans —
    // RAPTOR-style. Drive run_typed_extension end-to-end and assert
    // every produced atom's first_appearance / evidence ChunkRef
    // points at the `chunk:<id>` form (not the leaf's `raptor:<node>`
    // fallback) AND carries the verbatim sentence as passage_preview.
    let corpus_id = "test-corpus-citations";
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let conv_uuid = "conv-cite-1";
    store
        .save_conv_skeleton(&ConvSkeletonRow {
            corpus_id: corpus_id.into(),
            conv_uuid: conv_uuid.into(),
            state: "Ready".into(),
            skeleton_json: None,
            overview: None,
            segments_json: None,
            chunk_count: 4,
            updated_at: 1_700_000_000,
        })
        .await
        .unwrap();

    let primary_quote =
        "Spread pricing lets PBMs charge payers more than they reimburse pharmacies.";
    let primary_chunk_id: u32 = 7777;
    let leaf = ConvRaptorNodeRow {
        node_id: "n-cite-1".into(),
        corpus_id: corpus_id.into(),
        conv_uuid: conv_uuid.into(),
        level: 0,
        summary: "PBMs extract opaque rents through opaque spread-pricing schemes that cost payers and patients while delivering minimal intermediation value.".into(),
        summary_embedding: Vec::new(),
        centroid_embedding: Vec::new(),
        children_node_ids_json: "[]".into(),
        direct_member_chunk_ids_json: None,
        evidence_chunk_ids_json: "[]".into(),
        quote_spans_json: serde_json::json!([
            {"chunk_id": primary_chunk_id, "char_start": 0, "char_end": primary_quote.len(), "text": primary_quote},
            {"chunk_id": 8888, "char_start": 0, "char_end": 16, "text": "secondary quote"}
        ])
        .to_string(),
        primary_entities_json: r#"["PBM","Spread Pricing"]"#.into(),
        cluster_coherence: 0.9,
        created_at: 1_700_000_000,
        prompt_version: String::new(),
        summarizer_model: String::new(),
    };
    store
        .save_conv_raptor_nodes(corpus_id, conv_uuid, &[leaf])
        .await
        .unwrap();

    let (inference_arc, _calls) = CannedInferenceProvider::new(CANNED_BODY);
    let inference: Arc<dyn InferenceProvider> = inference_arc;
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let written: Arc<Mutex<Written>> = Arc::default();
    let atlas = port(Arc::clone(&written));

    let report = run_typed_extension(
        &atlas,
        corpus_id,
        &store,
        &inference,
        &atlas_dir,
    )
    .await
    .expect("typed extension should succeed end-to-end");
    assert_eq!(report.status, ExtractionStatus::Wrote);

    // The response is keyed on the primary quote_span's source chunk, and
    // its citation carries the verbatim sentence: what ingest writes onto
    // every atom's first_appearance / evidence ChunkRef (proven on
    // `IngestAtlas`).
    let expected_chunk_id = format!("chunk:{primary_chunk_id}");
    let w = written.lock().unwrap();
    assert!(!w.responses.is_empty());
    for r in &w.responses {
        assert_eq!(
            r.section_id, expected_chunk_id,
            "atom citations must point at the primary quote_span's source chunk"
        );
    }
    let citation = w
        .citations
        .get(&expected_chunk_id)
        .expect("the primary chunk keys a citation");
    assert_eq!(
        citation.passage_preview.as_deref(),
        Some(primary_quote),
        "passage_preview must carry the verbatim source sentence"
    );
}

#[test]
fn person_seeds_subsume_surnames_and_drop_noise() {
    use sovereign_core::conv_tiered::ChunkEntityRow;
    fn row(text: &str, label: &str, score: f64, chunk_id: u64) -> ChunkEntityRow {
        ChunkEntityRow {
            corpus_id: "c".into(),
            chunk_id,
            text: text.into(),
            label: label.into(),
            char_start: 0,
            char_end: text.len() as i64,
            score,
            conv_uuid: Some("note.md".into()),
            extracted_at: 0,
        }
    }
    let rows = vec![
        row("Elinor Ostrom", "Person", 0.9, 10),
        row("Ostrom", "Person", 0.9, 11),
        row("Ostrom", "Person", 0.9, 12),
        row("Garrett Hardin", "Person", 0.9, 20),
        // Noise that must NOT seed:
        row("user", "Person", 0.9, 30),       // single token, no host
        row("Margaret", "Person", 0.9, 31),   // single token, no host
        row("2024-01-15", "Person", 0.9, 32), // the wikilink/date trap
        row("FTC", "Organization", 0.9, 33),  // wrong label
        row("Weak Name", "Person", 0.3, 34),  // below score floor
    ];
    let seeds = super::harvest::build_person_seed_entities(&rows);
    let names: Vec<&str> = seeds.iter().map(|e| e.canonical_name.as_str()).collect();
    assert!(names.contains(&"Elinor Ostrom"), "names = {names:?}");
    assert!(names.contains(&"Garrett Hardin"));
    assert_eq!(seeds.len(), 2, "noise must be gated out: {names:?}");
    let ostrom = seeds
        .iter()
        .find(|e| e.canonical_name == "Elinor Ostrom")
        .unwrap();
    assert!(
        ostrom.aliases.iter().any(|a| a == "Ostrom"),
        "surname must subsume as alias: {:?}",
        ostrom.aliases
    );
    // Subsumed counts rank Ostrom (1+2 mentions) above Hardin (1).
    assert_eq!(seeds[0].canonical_name, "Elinor Ostrom");
}

#[test]
fn figure_sentences_pick_digit_bearing_text_and_respect_caps() {
    let text = "The first plain sentence has no numbers in it at all. \
                The agency documented $224.8 million in spread income that year. \
                Another plain sentence follows without figures. \
                Margins reached 58% in the most recent quarter. \
                A third figure: 12,000 units shipped in 2019. \
                Yet another numeric line: 7 of 9 axes regressed.";
    let got = super::harvest::figure_sentences_from(text, 3);
    assert_eq!(got.len(), 3, "per-call cap must hold: {got:?}");
    assert!(got[0].contains("$224.8 million"));
    assert!(got[1].contains("58%"));
    // Plain sentences never surface.
    assert!(got.iter().all(|s| s.chars().any(|c| c.is_ascii_digit())));

    // Sub-20-char and digit-free inputs yield nothing.
    assert!(super::harvest::figure_sentences_from("No digits here at all, ever.", 5).is_empty());
    assert!(super::harvest::figure_sentences_from("a 1.", 5).is_empty());
}
