// SPDX-License-Identifier: AGPL-3.0-or-later
//! The six contracts of campaign ontology-layer (ONTOLOGY_METHOD §Invariants),
//! each a test on fixtures of the three shapes (`tests/fixtures/layer/`: mail,
//! an issue tracker, news), run through the default path's own code: the
//! manifest `enrich init` builds from the rows, Phase 1 through the
//! `PhaseRunner` (the passes reader), and `atlas-resolve`'s whole layer
//! (`resolve_with_inputs`: 3a, 3b, stamps, RESOLVE, derived folds), with the
//! model answered by a scripted oracle through the Asker.
//!
//! The oracle reads only what a question shows and never a type or attribute
//! name: Locate takes the first claim kind for a line of six words or more and
//! "none" otherwise; Choose takes the first value; RESOLVE's choice takes the
//! first candidate when one is shown. So a contract that fails is the code's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use corpus_engine::enrichment::asker::{answering, Asker};
use corpus_engine::enrichment::atlas::SectionDocuments;
use corpus_engine::enrichment::ontology::OntologyPolicies;
use corpus_engine::enrichment::pipeline::{
    ChapterSelection, ChatPrompt, CustomAtlasSpec, PhaseCache, PhaseRunner, RunOutputWriter,
};
use corpus_engine::index::EnrichmentChunkRow;
use corpus_engine::types::EmbedFn;
use corpus_engine::{InferenceFn, Recipe};
use serde_json::{json, Map, Value};

use super::atlas_resolve::{collect_section_extractions, resolve_with_inputs, ResolvePhase};
use super::atlas_resolve_documents::{from_documents, DECISIONS_FILE, DERIVED_FILE};
use super::config::{EnrichConfig, CONFIG_SCHEMA_VERSION};
use super::corpus_io::{build_manifest_from_corpus_rows, hydrate_corpus_chapters_from_rows};

#[path = "layer_contract_tests/contracts.rs"]
mod contracts;
#[path = "layer_contract_tests/reader.rs"]
mod reader;

const SHAPES: [&str; 3] = ["mail", "issues", "news"];

fn fixture_dir(shape: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/layer/{shape}"))
}

/// A fixture: its recipe's text and its documents, one JSON object each.
#[derive(Clone)]
struct Fixture {
    recipe: String,
    documents: Vec<Map<String, Value>>,
}

impl Fixture {
    fn load(shape: &str) -> Self {
        let dir = fixture_dir(shape);
        let recipe = std::fs::read_to_string(dir.join("recipe.toml")).unwrap();
        let documents = std::fs::read_to_string(dir.join("documents.jsonl"))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Self { recipe, documents }
    }

    fn spec(&self) -> CustomAtlasSpec {
        Recipe::from_toml(&self.recipe)
            .unwrap()
            .custom_atlas_spec()
            .expect("every layer fixture declares an ontology")
    }

    fn policies(&self) -> OntologyPolicies {
        self.spec().policies()
    }

    /// The rows the index holds for these documents, as the jsonl extractor
    /// writes them: the content and title fields consumed, every other field
    /// metadata, `id` the source document id. One chunk per document (each
    /// body is under the chunker's cap); ids in document order.
    fn rows(&self) -> Vec<EnrichmentChunkRow> {
        let (content, title) = self.fields();
        self.documents
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let mut metadata = d.clone();
                let body = metadata.remove(&content).unwrap();
                let heading = metadata.remove(&title);
                EnrichmentChunkRow {
                    id: i as u64 + 1,
                    content: body.as_str().unwrap().to_string(),
                    title: heading.and_then(|t| t.as_str().map(str::to_string)),
                    url: None,
                    source_doc_id: d.get("id").and_then(Value::as_str).map(str::to_string),
                    metadata_raw: Some(Value::Object(metadata).to_string()),
                }
            })
            .collect()
    }

    /// The `[extract]` block's content and title fields.
    fn fields(&self) -> (String, String) {
        let field = |k: &str| {
            let line = self
                .recipe
                .lines()
                .find(|l| l.trim_start().starts_with(k))
                .unwrap_or_else(|| panic!("the fixture's [extract] names `{k}`"));
            line.split('"').nth(1).unwrap().to_string()
        };
        (field("content_field"), field("title_field"))
    }
}

/// Everything one run asked, and what it wrote.
struct Run {
    prompts: Vec<ChatPrompt>,
    /// atoms.json, edges.json, the RESOLVE decisions and the derived values.
    files: BTreeMap<&'static str, String>,
}

impl Run {
    fn atoms(&self) -> Vec<Value> {
        serde_json::from_str::<Value>(&self.files["atoms.json"]).unwrap()["atoms"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn lines(&self, file: &str) -> Vec<Value> {
        self.files[file]
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// How the oracle answers a Choose: the first value, or (`Spread`) a value
/// picked from the question's own bytes, so statements of different
/// documents get different values while the oracle still reads no name.
#[derive(Clone, Copy, PartialEq)]
enum Choose {
    First,
    Spread,
}

/// The value a `Spread` Choose picks among `labels` (the last is "none").
fn spread_pick(prompt: &ChatPrompt, labels: usize) -> usize {
    let sum: usize = prompt.user.bytes().map(usize::from).sum();
    sum % (labels - 1).max(1)
}

/// The scripted oracle (module doc), recording every question it is asked.
fn oracle(asked: Arc<Mutex<Vec<ChatPrompt>>>, choose: Choose) -> InferenceFn {
    Arc::new(move |prompt: &ChatPrompt, _| {
        asked.lock().unwrap().push(prompt.clone());
        let labels: Vec<String> = prompt
            .response_schema
            .as_ref()
            .and_then(|s| s["enum"].as_array())
            .map(|a| a.iter().map(|l| l.as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        let phase = prompt.phase_id.clone().unwrap_or_default();
        let answer = if labels.is_empty() {
            Err(corpus_engine::Error::Extraction(format!(
                "the oracle answers forced choices only; `{phase}` is not one"
            )))
        } else {
            let pick = match phase.as_str() {
                "document_passes_locate" => {
                    let line = prompt
                        .user
                        .rsplit_once("\n\nLine ")
                        .and_then(|(_, rest)| rest.split_once('"'))
                        .and_then(|(_, rest)| rest.split_once("\"\n"))
                        .map(|(text, _)| text)
                        .unwrap_or("");
                    if line.split_whitespace().count() >= 6 {
                        0
                    } else {
                        labels.len() - 1
                    }
                }
                "document_passes_choose" if choose == Choose::Spread => {
                    spread_pick(prompt, labels.len())
                }
                _ => 0,
            };
            let rest = 0.1 / labels.len() as f64;
            let dist: Map<String, Value> = labels
                .iter()
                .enumerate()
                .map(|(i, l)| (l.clone(), json!(if i == pick { 0.9 } else { rest })))
                .collect();
            Ok(Value::Object(dist).to_string())
        };
        Box::pin(async move { answer })
    })
}

/// A deterministic embedding of the text alone.
fn embedder() -> EmbedFn {
    Arc::new(|text: &str| {
        let mut v = vec![0f32; 8];
        for (i, b) in text.bytes().enumerate() {
            v[i % 8] += f32::from(b) / 255.0;
        }
        Box::pin(async move { Ok(v) })
    })
}

fn config(corpus_id: &str, spec: CustomAtlasSpec) -> EnrichConfig {
    EnrichConfig {
        schema_version: CONFIG_SCHEMA_VERSION,
        corpus_id: corpus_id.into(),
        pipeline_id: "custom_atlas".into(),
        source_path: format!("corpus:{corpus_id}").into(),
        chapter_regex: String::new(),
        chat_model: "oracle".into(),
        chat_models: None,
        embed_model: "oracle".into(),
        base_url: "http://127.0.0.1:9".into(),
        embed_base_url: None,
        min_section_body_words: 0,
        toc_markers: None,
        max_output_tokens: 4096,
        phase1b_max_output_tokens: None,
        phase_overrides: None,
        ontology: Some(spec),
        created_at: "2026-10-09T00:00:00Z".into(),
    }
}

/// The default path over `fixture`'s documents in their order, answered by
/// `asker` (the oracle under `daemon`, the store under `replay`), its store in
/// `store`.
async fn run_with(fixture: &Fixture, asker: Asker, store: &Path) -> Run {
    run_choosing(fixture, asker, store, Choose::First).await
}

async fn run_choosing(fixture: &Fixture, asker: Asker, store: &Path, choose: Choose) -> Run {
    let spec = fixture.spec();
    let corpus_id = "layer-fixture";
    let cfg = config(corpus_id, spec.clone());
    let rows = fixture.rows();
    let manifest =
        build_manifest_from_corpus_rows(corpus_id, rows.clone(), None, None, 1, None).unwrap();
    let chapters = hydrate_corpus_chapters_from_rows(&manifest, &rows);
    let documents = SectionDocuments::from_chunk_rows(
        manifest
            .chapters
            .iter()
            .map(|c| (c.id.as_str(), c.chunk_ids.as_slice())),
        &rows,
    );

    let asked = Arc::new(Mutex::new(Vec::new()));
    let (embed, chat) =
        answering(asker, store, embedder(), oracle(Arc::clone(&asked), choose)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let pipeline = super::pipeline_resolve::resolve_pipeline(&cfg).unwrap();
    let runner = PhaseRunner::new(
        pipeline,
        embed.clone(),
        chat.clone(),
        PhaseCache::new(work.path().join("cache")),
        RunOutputWriter::new(work.path().join("runs")),
        work.path().join("exemplars"),
    )
    .with_min_body_words(0);
    let phase1 = runner
        .phase_1_extract_questions(&chapters, &ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    let sections = collect_section_extractions(&phase1.output.questions_by_chapter);

    let policies = spec.policies();
    let inputs = from_documents(|| Ok(documents), &policies, corpus_id, true).unwrap();
    let atlas = work.path().join("atlas");
    std::fs::create_dir_all(&atlas).unwrap();
    resolve_with_inputs(
        &cfg,
        &sections,
        inputs,
        &embed,
        &chat,
        &atlas,
        ResolvePhase::All,
    )
    .await
    .unwrap();
    let mut files = BTreeMap::new();
    for name in ["atoms.json", "edges.json", DECISIONS_FILE, DERIVED_FILE] {
        files.insert(
            name,
            std::fs::read_to_string(atlas.join(name)).unwrap_or_default(),
        );
    }
    let prompts = asked.lock().unwrap().clone();
    Run { prompts, files }
}

async fn run(fixture: &Fixture) -> Run {
    let store = tempfile::tempdir().unwrap();
    run_with(fixture, Asker::Daemon, store.path()).await
}

/// The declared names of a fixture's types and attributes.
fn declared_names(policies: &OntologyPolicies) -> BTreeSet<String> {
    policies
        .shape
        .types
        .iter()
        .flat_map(|t| {
            std::iter::once(t.name.clone()).chain(t.attributes.iter().map(|a| a.name.clone()))
        })
        .collect()
}

#[tokio::test]
async fn every_fixture_runs_the_default_path_and_resolve_makes_records() {
    for shape in SHAPES {
        let f = Fixture::load(shape);
        let run = run(&f).await;
        let decided: Vec<String> = f
            .policies()
            .shape
            .types
            .iter()
            .filter(|t| t.identity_criterion.is_some() && t.source.is_none())
            .map(|t| t.name.clone())
            .collect();
        let records = run
            .atoms()
            .into_iter()
            .filter(|a| {
                let ty = a["data"]["entity_type"]
                    .as_str()
                    .or(a["data"]["event_type"].as_str());
                ty.is_some_and(|t| decided.iter().any(|d| d == t))
            })
            .count();
        assert!(records > 0, "{shape}: RESOLVE made no record");
        assert!(
            !run.lines(DECISIONS_FILE).is_empty(),
            "{shape}: no decision"
        );
    }
}
