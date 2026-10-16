// SPDX-License-Identifier: AGPL-3.0-or-later
//! Each document's outcome in a real ingest (OICP v0.6 §2.9): a library of
//! three plain-text files, one of them bytes no text extractor can read,
//! ingested through `CorpusEngine::ingest`. The two readable ones come back
//! ready under their paths relative to the acquired root, each with the name
//! of the text it stored; the third comes back failed, named, with why.
//! Until this, the failure was one `docs_skipped` count and a log line.

use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};
use corpus_index::ingest_port::daemon::{DocumentOutcome, DocumentState};

const ALPHA: &str = "The harbour master kept the tide tables in a drawer.\n";
const GAMMA: &str = "The ferry ran late on Tuesday and early on Wednesday.\n";

#[tokio::test]
async fn a_library_with_one_unreadable_file_lists_it_failed_and_the_others_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let data_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(data_dir.join("_recipes")).unwrap();
    std::fs::write(root.join("alpha.txt"), ALPHA).unwrap();
    std::fs::write(root.join("broken.txt"), [0xff_u8, 0xfe, 0x00, 0x9f, 0x92]).unwrap();
    std::fs::write(root.join("sub/gamma.txt"), GAMMA).unwrap();

    let recipe = format!(
        r#"[corpus]
id = "outcomes"
name = "outcomes"
description = "fixture"
license = "private"
mesh_sharing = false
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"

[index]
fts = true
vector = false
"#,
        root.display()
    );
    let recipe_path = data_dir.join("_recipes/outcomes.toml");
    std::fs::write(&recipe_path, recipe).unwrap();
    let embed: EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 8]) }));
    let engine = CorpusEngine::new(data_dir.join("_recipes"), data_dir.clone(), embed)
        .with_embedding_model("test-mock");
    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("one unreadable file does not fail the ingest");

    let by = |name: &str| -> &DocumentOutcome {
        result
            .documents
            .iter()
            .find(|d| d.source.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("`{name}` is listed: {:?}", result.documents))
    };
    assert_eq!(result.documents.len(), 3, "{:?}", result.documents);
    assert_eq!(result.documents_unlisted, 0);
    let index = corpus_engine::CorpusIndex::open(&data_dir.join("outcomes"))
        .await
        .expect("the corpus opens");
    for (name, text) in [("alpha.txt", ALPHA), ("sub/gamma.txt", GAMMA)] {
        let DocumentState::Ready { text_sha256 } = &by(name).state else {
            panic!("`{name}` is ready: {:?}", by(name))
        };
        let stored = text_sha256.unwrap_or_else(|| panic!("`{name}` names its text"));
        let back = index
            .text(&stored)
            .await
            .unwrap()
            .unwrap_or_else(|a| panic!("`{name}`'s text reads back by its name: {a:?}"));
        assert!(back.text.contains(text.trim()), "{name}: {:?}", back.text);
    }
    let DocumentState::Failed { reason } = &by("broken.txt").state else {
        panic!("`broken.txt` failed: {:?}", by("broken.txt"))
    };
    assert!(reason.contains("UTF-8"), "the reason says why: {reason}");
    assert_eq!(result.docs_skipped, 1);
}
