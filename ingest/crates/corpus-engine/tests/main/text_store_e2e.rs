// SPDX-License-Identifier: AGPL-3.0-or-later
//! ADDRESSED_TEXT §5.1's failing input: a source ingested twice, once by the
//! main path and once by the watched-folder delta, stores ONE text under ONE
//! name, and every chunk of it carries that name. Both paths go through the
//! engine as svrn drives it (`LocalCorpusPort`), over a folder staged the way
//! svrn's extract stage writes it, stated hash and extractor included.

use std::path::Path;
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_index::corpus::Corpus;
use corpus_index::index::DocSource;
use corpus_index::ingest_port::{DocFetchFn, FetchedDoc, LocalCorpusPort, WatchedUpdate};
use corpus_index::types::EmbedFn;
use kernel_types::Sha256Hash;

const ID: &str = "texts-fixture";
const DOC: &str = "notes/alpha.md";
const TEXT: &str = "Alpha keeps its exact words.\n\nA second paragraph follows the first one.";
const STAGE: &str = "local-stage:md@test";

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

/// The file's bytes, the source sha256 svrn states for them.
fn source_bytes() -> Vec<u8> {
    format!("---\ntitle: alpha\n---\n{TEXT}").into_bytes()
}

fn stage(dir: &Path) -> std::path::PathBuf {
    let staged = dir.join("staging.jsonl");
    let line = serde_json::json!({
        "id": DOC, "title": DOC, "content": TEXT, "source_path": DOC,
        "source_sha256": Sha256Hash::of(&source_bytes()).to_hex(),
        "extractor": STAGE,
    });
    std::fs::write(&staged, format!("{line}\n")).unwrap();
    let recipe = format!(
        "[corpus]\nid = \"{ID}\"\nname = \"Texts\"\ndescription = \"d\"\nlicense = \"local\"\n\
         mesh_sharing = false\nschema_version = 1\n\n[acquire]\ntype = \"local_file\"\n\
         path = \"{}\"\n\n[extract]\ntype = \"jsonl\"\ncontent_field = \"content\"\n\
         title_field = \"title\"\n\n[chunk]\ntype = \"paragraph\"\nmax_chars = 40\n\
         overlap_chars = 0\n\n[index]\nfts = true\nvector = true\n",
        staged.display()
    );
    let path = dir.join("recipes").join(format!("{ID}.toml"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, recipe).unwrap();
    path
}

async fn redeliver_by_delta(engine: &Arc<CorpusEngine>) {
    let update = WatchedUpdate {
        corpus_id: ID.into(),
        version: "sweep".into(),
        entries: [(DOC.to_string(), "edited-mtime".to_string())].into(),
        new_documents: vec![],
        updated_documents: vec![DOC.into()],
        deleted_documents: vec![],
    };
    let fetch: DocFetchFn = Arc::new(|_doc: &str| {
        Box::pin(async {
            Ok(FetchedDoc {
                content: TEXT.into(),
                source: DocSource::Hashed {
                    sha256: Sha256Hash::of(&source_bytes()),
                    extractor: STAGE.into(),
                },
            })
        })
    });
    Arc::clone(engine)
        .apply_watched_update(&update, fetch, Box::new(|_, _, _| {}))
        .await
        .expect("delta applies");
}

/// One text file, one record naming it, and every chunk of the source naming
/// it — the store's state after whichever path wrote last.
async fn assert_one_text_one_name(engine: &CorpusEngine, when: &str) {
    let name = Sha256Hash::of_str(TEXT);
    let corpus = Corpus::named(engine.index_dir(), ID).unwrap();
    let files: Vec<String> = std::fs::read_dir(corpus.texts_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        files,
        vec![name.to_hex()],
        "{when}: one text, named by its sha256"
    );

    let index = corpus.open().await.unwrap();
    let stored = index.text(&name).await.unwrap().expect("held");
    assert_eq!(stored.text, TEXT, "{when}");
    assert_eq!(
        stored.documents.len(),
        1,
        "{when}: one record: {:?}",
        stored.documents
    );
    let record = &stored.documents[0];
    assert_eq!(record.source_id, DOC, "{when}");
    assert_eq!(
        record.source_sha256,
        Some(Sha256Hash::of(&source_bytes())),
        "{when}"
    );
    assert_eq!(record.extractor, STAGE, "{when}: the stated extractor");

    let rows = index
        .chunks_by_source_doc_ids(&[DOC.to_string()])
        .await
        .unwrap();
    assert!(rows.len() >= 2, "{when}: the fixture cuts several chunks");
    let ids: Vec<u64> = rows.iter().map(|r| r.id).collect();
    let names = index.chunk_text_sha256s(&ids).await.unwrap();
    for id in ids {
        assert_eq!(
            names.get(&id),
            Some(&name),
            "{when}: chunk {id} names its text"
        );
    }
}

#[tokio::test]
async fn a_source_through_the_main_path_and_the_delta_stores_one_text_with_one_name() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_at(dir.path());
    let recipe = stage(dir.path());
    let port: &dyn LocalCorpusPort = engine.as_ref();
    port.ingest_recipe_path(&recipe, None)
        .await
        .expect("ingest");
    assert_one_text_one_name(&engine, "after the main path").await;

    redeliver_by_delta(&engine).await;
    assert_one_text_one_name(&engine, "after the delta").await;

    // Removal takes the texts with the corpus directory.
    engine.remove_corpus_everything(ID).unwrap();
    assert!(!Corpus::named(engine.index_dir(), ID)
        .unwrap()
        .texts_dir()
        .exists());
}
