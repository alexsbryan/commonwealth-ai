//! Opt-in real-data smoke for the ChatGPT export extractor.
//!
//! Faithful unit fixtures (in `extractors/chatgpt_export.rs`) cover the
//! parse/render logic, but only a real export proves the serde model
//! survives a whole file. Most machines don't have one, so this test is
//! gated on `SOVEREIGN_CHATGPT_EXPORT` pointing at a real
//! `conversations.json` — unset → it no-ops with a skip note.
//!
//! Run it against a real export with:
//! ```bash
//! SOVEREIGN_CHATGPT_EXPORT=~/.sovereign/conversations-chatgpt/conversations.json \
//!   ./scripts/sovereign-test.sh --human --filter chatgpt_real_export
//! ```

use corpus_engine::extractors::chatgpt_export::ChatgptExportExtractor;
use corpus_engine::extractors::Extractor;

#[test]
fn real_chatgpt_export_parses_and_renders_cleanly() {
    let Ok(path) = std::env::var("SOVEREIGN_CHATGPT_EXPORT") else {
        eprintln!("SOVEREIGN_CHATGPT_EXPORT unset — skipping real-data smoke");
        return;
    };

    let docs: Vec<_> = ChatgptExportExtractor::new()
        .extract(std::path::Path::new(&path))
        .expect("real export must parse")
        .map(|r| r.expect("each conversation must convert"))
        .collect();

    assert!(!docs.is_empty(), "real export yielded zero conversations");

    for doc in &docs {
        // Every rendered doc must carry the threaded-turns header
        // contract so the shared chunker can consume it.
        assert!(
            doc.content.contains("### [") && doc.content.contains("] "),
            "doc {:?} missing turn-block headers",
            doc.source_id
        );
        let has_turn = doc.content.contains("] user\n") || doc.content.contains("] assistant\n");
        assert!(has_turn, "doc {:?} has no user/assistant turns", doc.source_id);

        // No Private-Use-Area marker control chars may leak into chunk
        // text / embeddings — they must all be cleaned to readable text.
        assert!(
            !doc.content.chars().any(|c| ('\u{E200}'..='\u{E20F}').contains(&c)),
            "doc {:?} leaked PUA marker chars into content",
            doc.source_id
        );

        // Metadata invariants the desktop / atlas surfaces rely on.
        let meta = doc.metadata.as_ref().expect("doc must carry metadata");
        assert_eq!(meta["source"], "chatgpt");
        assert!(doc.title.is_some(), "doc {:?} has no title", doc.source_id);
    }

    eprintln!(
        "real-data smoke OK: {} conversation(s) rendered cleanly from {}",
        docs.len(),
        path
    );
}
