// SPDX-License-Identifier: AGPL-3.0-or-later
//! Transcripts read by turn (OICP v0.6 §2.14), through `CorpusEngine::ingest`
//! as an install drives it: the conformance fixture's three documents
//! (`transcripts.json`: a paragraph transcript, Whisper's JSON, and a text
//! declared a transcript that is neither) in one inline recipe.
//!
//! The stored text of each transcript is its words, never a header line; the
//! locators kept beside it say where each turn starts and who speaks it
//! (Whisper's never a speaker); the document in neither shape is listed
//! `failed: not a transcript` and stores nothing.

use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};
use corpus_index::ingest_port::daemon::DocumentState;
use oicp_types::locator::{PlacedLocators, SpanLocator};

const FIXTURE: &str =
    include_str!("../../../../../cmnwlth/crates/oicp-conformance/fixture/transcripts.json");

fn recipe(documents: &[(String, String)]) -> String {
    let mut out = String::from(
        "[corpus]\nid = \"transcripts\"\nname = \"transcripts\"\ndescription = \"fixture\"\n\
         license = \"CC0-1.0\"\nmesh_sharing = false\n\n[acquire]\ntype = \"inline\"\n\n\
         [extract]\ntype = \"plaintext\"\n\n[chunk]\ntype = \"paragraph\"\n",
    );
    for (name, text) in documents {
        out += &format!(
            "\n[[document]]\nname = \"{name}\"\nformat = \"transcript\"\ntext = {}\n",
            toml::Value::String(text.clone())
        );
    }
    out
}

fn fixture_documents() -> Vec<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    v["shelf"]["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().to_string(),
                d["text"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_declared_transcript_is_stored_as_its_words_with_its_turns_kept_beside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(data_dir.join("_recipes")).unwrap();
    let recipe_path = data_dir.join("_recipes/transcripts.toml");
    std::fs::write(&recipe_path, recipe(&fixture_documents())).unwrap();
    let embed: EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 8]) }));
    let engine = CorpusEngine::new(data_dir.join("_recipes"), data_dir.clone(), embed)
        .with_embedding_model("test-mock");
    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("a document in neither shape does not fail the ingest");

    let state = |name: &str| {
        &result
            .documents
            .iter()
            .find(|d| d.source.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("`{name}` is listed: {:?}", result.documents))
            .state
    };
    assert!(
        matches!(state("minutes"), DocumentState::Failed { reason } if reason == "not a transcript"),
        "minutes: {:?}",
        state("minutes")
    );
    let index = corpus_engine::CorpusIndex::open(&data_dir.join("transcripts"))
        .await
        .unwrap();
    let read = |name: &str| {
        let DocumentState::Ready { text_sha256 } = state(name).clone() else {
            panic!("`{name}` is ready: {:?}", state(name))
        };
        text_sha256.expect("a text name")
    };
    let locator_of = |text: &str, locators: &str, quote: &str| {
        let at = text[..text.find(quote).expect("quote in stored text")]
            .chars()
            .count() as u64;
        serde_json::from_str::<PlacedLocators>(locators)
            .unwrap()
            .at(at)
    };

    let interview = index
        .text(&read("harbour-interview"))
        .await
        .unwrap()
        .unwrap();
    assert!(!interview.text.contains("00:00:04"), "{}", interview.text);
    assert!(
        !interview.text.contains("Interviewer"),
        "{}",
        interview.text
    );
    let locators = interview.locators.as_deref().expect("locators kept");
    for (quote, ms, speaker) in [
        ("The ferry has run late", 4000, Some("HARBOUR MASTER")),
        ("Why the ferry", 11000, Some("Interviewer")),
        ("We sail by tables", 15000, Some("HARBOUR MASTER")),
        ("The recording ends", 3_727_000, None),
    ] {
        assert_eq!(
            locator_of(&interview.text, locators, quote),
            Some(SpanLocator::Timestamp {
                milliseconds: ms,
                speaker: speaker.map(str::to_string)
            }),
            "{quote}"
        );
    }
    assert_eq!(
        locator_of(&interview.text, locators, "Interview at the harbour"),
        None,
        "the opening stands under no turn"
    );

    let quay = index.text(&read("quay-recording")).await.unwrap().unwrap();
    assert!(!quay.text.contains("segments"), "{}", quay.text);
    assert_eq!(
        locator_of(
            &quay.text,
            quay.locators.as_deref().unwrap(),
            "Nobody on the quay"
        ),
        Some(SpanLocator::Timestamp {
            milliseconds: 3500,
            speaker: None
        }),
        "Whisper's segment: its start, never a speaker"
    );
    let (_, rows) = index.documents().await.unwrap().unwrap();
    assert!(
        rows.iter().all(|r| r.source_id != "minutes"),
        "the document in neither shape stores nothing: {rows:?}"
    );
}

#[test]
fn a_format_outside_the_closed_set_is_a_recipe_that_does_not_load() {
    let bad =
        recipe(&[("x".into(), "y".into())]).replace("transcript\"\ntext", "subtitles\"\ntext");
    assert!(corpus_engine::recipe::Recipe::from_toml(&bad).is_err());
}
