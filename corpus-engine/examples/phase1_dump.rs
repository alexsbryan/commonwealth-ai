//! Dump the composed Phase-1 `ChatPrompt` (system, user, response schema) for
//! one chapter of a declared-ontology corpus, as JSON on stdout — the input
//! to a 15-second daemon probe, where a rebuild is six minutes.
//!
//!     cargo run -p corpus-engine --features treesitter --example phase1_dump -- //!         ~/.svrnmesh/enrichment/<corpus>/config.json chapter.json
//!
//! `chapter.json` is `{"id": "sec_00014", "title": "...", "text": "..."}`. The
//! chapter is shaped the way the runner shapes it (title line first, ordinal
//! metadata), so the user message is byte-identical to what `enrich extract`
//! sends — compare against `RUST_LOG=sovereign_enrichment_build=debug`'s
//! `inference_client: request body` line before trusting a probe. Property
//! order in the schema is the model's generation order (note 5c06bc92), so
//! this binary depends on corpus-engine's `serde_json/preserve_order`.
use corpus_engine::enrichment::pipeline::pipelines::configurable_atlas::{
    CustomAtlasSpec, CustomOntology,
};
use corpus_engine::enrichment::pipeline::pipelines::genre::AtlasGenre;
use corpus_engine::enrichment::pipeline::types::ChapterInput;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let spec: CustomAtlasSpec = serde_json::from_value(cfg["ontology"].clone()).unwrap();
    let genre = CustomOntology::from_policies(&spec.name, &spec.policies());
    let ch: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[2]).unwrap()).unwrap();
    // Mirror the runner's ChapterInput: the chunker keeps the heading line
    // as the first paragraph, and the manifest's ordinal renders as
    // "**Position:** chapter N".
    let title = ch["title"].as_str().unwrap().to_string();
    let text = format!("{title}\n\n{}", ch["text"].as_str().unwrap());
    let id = ch["id"].as_str().unwrap().to_string();
    let ordinal = id
        .trim_start_matches("sec_")
        .trim_start_matches('0')
        .to_string();
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("ordinal".to_string(), ordinal);
    let chapter = ChapterInput {
        chapter_id: id,
        title,
        approx_tokens: text.len() / 4,
        text,
        metadata,
    };
    let prompt = genre
        .compose_phase1(&chapter, &[], None)
        .expect("declared ontology");
    println!("{}", serde_json::to_string(&prompt).unwrap());
}
