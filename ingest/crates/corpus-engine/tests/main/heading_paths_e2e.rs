// SPDX-License-Identifier: AGPL-3.0-or-later
//! A plain text's heading paths (OICP v0.6 §2.13), through
//! `CorpusEngine::ingest` as an install drives it: the conformance fixture's
//! headings shelf (`headings.json`: a headed text with a contents line and a
//! preface, and a text with no headings) in one inline recipe.
//!
//! The places kept beside each stored text answer every quotation the
//! fixture lists with the path it states (data, so an engine that agrees
//! with `HeadingOutline` and not with the spec still fails), and a text
//! with no headings keeps no places at all.

use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};
use corpus_index::ingest_port::daemon::DocumentState;
use oicp_types::locator::{PlacedLocators, SpanLocator};

const FIXTURE: &str =
    include_str!("../../../../../cmnwlth/crates/oicp-conformance/fixture/headings.json");

fn recipe(fixture: &serde_json::Value) -> String {
    let mut out = String::from(
        "[corpus]\nid = \"headings\"\nname = \"headings\"\ndescription = \"fixture\"\n\
         license = \"CC0-1.0\"\nmesh_sharing = false\n\n[acquire]\ntype = \"inline\"\n\n\
         [extract]\ntype = \"plaintext\"\n\n[chunk]\ntype = \"paragraph\"\n",
    );
    for d in fixture["shelf"]["documents"].as_array().unwrap() {
        out += &format!(
            "\n[[document]]\nname = \"{}\"\ntext = {}\n",
            d["name"].as_str().unwrap(),
            toml::Value::String(d["text"].as_str().unwrap().to_string())
        );
    }
    out
}

#[tokio::test]
async fn a_plain_text_keeps_the_heading_path_each_span_stands_under_and_none_without_headings() {
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(data_dir.join("_recipes")).unwrap();
    let recipe_path = data_dir.join("_recipes/headings.toml");
    std::fs::write(&recipe_path, recipe(&fixture)).unwrap();
    let embed: EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 8]) }));
    let engine = CorpusEngine::new(data_dir.join("_recipes"), data_dir.clone(), embed)
        .with_embedding_model("test-mock");
    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .unwrap();
    let index = corpus_engine::CorpusIndex::open(&data_dir.join("headings"))
        .await
        .unwrap();

    let mut named = 0;
    for l in fixture["located"].as_array().unwrap() {
        let (document, quote) = (
            l["document"].as_str().unwrap(),
            l["quote"].as_str().unwrap(),
        );
        let want: Option<SpanLocator> = serde_json::from_value(l["locator"].clone()).unwrap();
        let Some(DocumentState::Ready {
            text_sha256: Some(name),
        }) = result
            .documents
            .iter()
            .find(|d| d.source.as_deref() == Some(document))
            .map(|d| d.state.clone())
        else {
            panic!("`{document}` is stored: {:?}", result.documents)
        };
        let stored = index.text(&name).await.unwrap().unwrap();
        let at = stored.text[..stored.text.find(quote).expect("quote in stored text")]
            .chars()
            .count() as u64;
        let got = stored
            .locators
            .as_deref()
            .map(|j| serde_json::from_str::<PlacedLocators>(j).unwrap())
            .and_then(|p| p.at(at));
        assert_eq!(got, want, "`{quote}` ({document})");
        if document == "logbook" {
            assert_eq!(
                stored.locators, None,
                "a text with no headings keeps no places"
            );
        }
        named += usize::from(want.is_some());
    }
    assert_eq!(named, 3, "the fixture names three heading paths");
}
