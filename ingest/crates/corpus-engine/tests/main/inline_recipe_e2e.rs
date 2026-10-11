// SPDX-License-Identifier: AGPL-3.0-or-later
//! The conformance fixture library (oicp-v0.5.md §6.1), ingested through the
//! engine as an install drives it, in both of its forms:
//!
//! - **inline**, the committed `library.recipe.toml`, unmodified: each
//!   document's record says source id `name`, source sha256 the sha256 of its
//!   `text`, and the metadata the recipe declared, byte for byte;
//! - **files**, the `--fixture-dir` form: the same texts written as files and
//!   a `local_file` recipe declaring the same metadata by `source`.
//!
//! Both forms hold the same library, so their digests are equal.

use std::path::Path;
use std::sync::Arc;

use corpus_engine::recipe::Recipe;
use corpus_engine::recipe_documents::DeclaredDocument;
use corpus_engine::CorpusEngine;
use corpus_index::corpus::Corpus;
use corpus_index::index::DocumentRecord;
use corpus_index::ingest_port::LocalCorpusPort;
use corpus_index::types::EmbedFn;
use kernel_types::Sha256Hash;

const FIXTURE: &str =
    include_str!("../../../../../cmnwlth/crates/oicp-conformance/fixture/library.recipe.toml");
const ID: &str = "oicp-conformance-fixture";

fn engine_at(dir: &Path) -> Arc<CorpusEngine> {
    let embed: EmbedFn = Arc::new(|text: &str| {
        let mut v = vec![0f32; 16];
        for b in text.as_bytes() {
            v[(*b as usize) % 16] += 1.0;
        }
        Box::pin(async move { Ok(v) })
    });
    let engine = CorpusEngine::new(dir.join("recipes"), dir.join("indexes"), embed);
    Arc::new(engine.with_embedding_model("test-mock"))
}

/// `(name, text, declared metadata)` per fixture document, as the committed
/// recipe declares them.
fn documents() -> Vec<(String, String, Option<String>)> {
    Recipe::from_toml(FIXTURE)
        .expect("the fixture loads")
        .documents
        .into_iter()
        .map(|d| match d {
            DeclaredDocument::Inline {
                name,
                text,
                metadata,
            } => (name, text, metadata),
            DeclaredDocument::File { .. } => panic!("the fixture is inline"),
        })
        .collect()
}

async fn ingest(dir: &Path, recipe: &str) -> Vec<DocumentRecord> {
    let engine = engine_at(dir);
    let path = dir.join("recipes").join(format!("{ID}.toml"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, recipe).unwrap();
    let port: &dyn LocalCorpusPort = engine.as_ref();
    port.ingest_recipe_path(&path, None).await.expect("ingest");
    let index = Corpus::named(engine.index_dir(), ID)
        .unwrap()
        .open()
        .await
        .unwrap();
    let (_, mut rows) = index.documents().await.unwrap().expect("texts stored");
    rows.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    // Every record's text is stored and reads back under its name.
    for r in &rows {
        let stored = index.text(&r.text_sha256).await.unwrap().expect("held");
        assert_eq!(Sha256Hash::of_str(&stored.text), r.text_sha256);
    }
    rows
}

fn assert_records_are_the_fixture(rows: &[DocumentRecord], form: &str) {
    let docs = documents();
    assert_eq!(rows.len(), docs.len(), "{form}: one record per document");
    for (name, text, metadata) in &docs {
        let r = rows
            .iter()
            .find(|r| &r.source_id == name)
            .unwrap_or_else(|| panic!("{form}: no record with source id `{name}`: {rows:?}"));
        assert_eq!(
            r.source_sha256,
            Some(Sha256Hash::of_str(text)),
            "{form}: `{name}`'s source bytes are its text"
        );
        assert_eq!(
            &r.metadata, metadata,
            "{form}: `{name}`'s declared metadata, byte for byte"
        );
        assert!(
            r.extractor.starts_with("plaintext@"),
            "{form}: {}",
            r.extractor
        );
    }
}

/// The `--fixture-dir` form, as oicp-conformance renders it
/// (`Library::dir_recipe`): the inline recipe's header with a `local_file`
/// acquire, and each document's metadata declared by `source`.
fn dir_recipe(dir: &Path) -> String {
    let header = FIXTURE.split("\n[[document]]").next().unwrap().replace(
        "[acquire]\ntype = \"inline\"",
        &format!(
            "[acquire]\ntype = \"local_file\"\npath = '{}'",
            dir.display()
        ),
    );
    let mut out = header;
    for (name, text, metadata) in documents() {
        std::fs::write(dir.join(format!("{name}.txt")), text.as_bytes()).unwrap();
        out += &format!("\n[[document]]\nsource = \"{name}.txt\"\n");
        if let Some(m) = metadata {
            out += &format!("metadata = '''{m}'''\n");
        }
    }
    out
}

#[test]
fn the_conformance_fixture_recipe_loads_unmodified() {
    let r = Recipe::from_toml(FIXTURE).expect("the normative example loads");
    assert!(matches!(
        r.acquire,
        corpus_engine::recipe::AcquirerConfig::Inline
    ));
    let docs = documents();
    let names: Vec<&str> = docs.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, ["okafor2019", "lindqvist2021", "harbour-notes"]);
    assert!(docs[0].1.starts_with("Rivers that cross three borders"));
    assert!(
        docs[0]
            .2
            .as_deref()
            .is_some_and(|m| m.starts_with(r#"{"id":"okafor2019","type":"article-journal""#)),
        "metadata is kept as the recipe wrote it: {:?}",
        docs[0].2
    );
    assert_eq!(docs[2].2, None, "harbour-notes declares none");
}

#[tokio::test]
async fn the_inline_fixture_and_its_file_form_store_the_same_library() {
    let inline_dir = tempfile::tempdir().unwrap();
    let inline = ingest(inline_dir.path(), FIXTURE).await;
    assert_records_are_the_fixture(&inline, "inline");

    let files_dir = tempfile::tempdir().unwrap();
    let source_root = files_dir.path().join("fixture");
    std::fs::create_dir_all(&source_root).unwrap();
    let files = ingest(files_dir.path(), &dir_recipe(&source_root)).await;
    assert_records_are_the_fixture(&files, "files");

    let digest = |rows: &[DocumentRecord]| {
        let hex: Vec<(String, Option<String>, String)> = rows
            .iter()
            .map(|r| {
                (
                    r.text_sha256.to_hex(),
                    r.source_sha256.map(|s| s.to_hex()),
                    r.extractor.clone(),
                )
            })
            .collect();
        oicp_types::evidence::texts_digest_preimage(
            hex.iter()
                .map(|(t, s, e)| (t.as_str(), s.as_deref(), e.as_str())),
        )
    };
    assert_eq!(
        digest(&inline),
        digest(&files),
        "the two forms of one library hold the same texts, sources and extractors"
    );
}
