// SPDX-License-Identifier: AGPL-3.0-or-later
//! Does an atlas evidence `chunk_id` (a SECTION slug like `sec_0011`) resolve
//! to a real chunk through `CorpusIndex::resolve_sections_to_chunks`?
//!
//! The walk's evidence resolver searches the atom's passage preview instead of
//! using this exact path. This probe answers whether the exact path is
//! actually available on a real corpus before anything is rewired.
//!
//! ```text
//! cargo run -p corpus-engine --features treesitter \
//!   --example section_resolve_probe -- chaos-secret-agent sec_0011 sec_0001
//! ```

use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: section_resolve_probe <corpus_id> <section_id>...");
        std::process::exit(2);
    }
    let corpus_id = args[0].clone();
    let sections: Vec<String> = args[1..].to_vec();

    let root = std::path::PathBuf::from(std::env::var("HOME").expect("HOME")).join(".svrnmesh");
    let embed: corpus_engine::types::EmbedFn = Arc::new(|_s: &str| {
        Box::pin(async { Ok(Vec::new()) })
            as std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = corpus_engine::error::Result<Vec<f32>>> + Send,
                >,
            >
    });
    let engine =
        corpus_engine::CorpusEngine::new(root.join("recipes"), root.join("indexes"), embed);
    let index = match engine.open_index_for_corpus(&corpus_id).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("open_index_for_corpus({corpus_id}) failed: {e}");
            std::process::exit(1);
        }
    };
    // What keys does chunk metadata actually carry? A resolver that returns an
    // empty map and a corpus that stamps no `section_id` look identical from
    // the caller's side (ARCH §18.3), so print the real keys.
    if std::env::var("PROBE_DUMP_META").is_ok() {
        match index.chunks_by_ids(&[0, 1, 2, 100, 200]).await {
            Ok(rows) => {
                for r in rows.iter().take(5) {
                    println!("  chunk {} meta={:?}", r.id, r.metadata_raw);
                }
            }
            Err(e) => println!("  chunks_by_ids failed: {e}"),
        }
    }
    let t = std::time::Instant::now();
    let map = match index.resolve_sections_to_chunks(&sections).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("resolve_sections_to_chunks failed: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "corpus={corpus_id} asked={} resolved={} in {}ms",
        sections.len(),
        map.len(),
        t.elapsed().as_millis()
    );
    for s in &sections {
        match map.get(s) {
            Some(id) => println!("  {s:12} -> chunk {id}"),
            None => println!("  {s:12} -> UNRESOLVED"),
        }
    }
    // Which chunk of a section carries a phrase? `PROBE_FIND="..."` scans the
    // section's whole chunk list, which is what tells "the section reached the
    // window" apart from "the RIGHT chunk of it did".
    if let Ok(needle) = std::env::var("PROBE_FIND") {
        let manifest_path = root.join("indexes").join(&corpus_id).join("chapters.json");
        let manifest =
            corpus_engine::enrichment::pipeline::chapter_manifest::ChapterManifest::load(
                &manifest_path,
            )
            .ok()
            .flatten();
        for sec in &sections {
            let Some(entry) = manifest.as_ref().and_then(|m| m.get(sec)) else {
                continue;
            };
            println!("\n  section {sec}: {} chunk(s)", entry.chunk_ids.len());
            if let Ok(rows) = index.chunks_by_ids(&entry.chunk_ids).await {
                for r in rows {
                    if r.content.to_lowercase().contains(&needle.to_lowercase()) {
                        println!(
                            "    chunk {:5} MATCH :: {}",
                            r.id,
                            r.content
                                .replace('\n', " ")
                                .chars()
                                .take(160)
                                .collect::<String>()
                        );
                    }
                }
            }
        }
    }

    // Show the resolved chunk's text so "resolved" can be checked against what
    // the atom actually cited, not just counted.
    for s in &sections {
        if let Some(id) = map.get(s) {
            if let Ok(rows) = index.chunks_by_ids(&[*id]).await {
                if let Some(r) = rows.first() {
                    let body = r.content.replace('\n', " ");
                    println!(
                        "\n  [{s} -> {id}] knife={} carving={} :: {}",
                        body.to_lowercase().contains("knife"),
                        body.to_lowercase().contains("carving"),
                        body.chars().take(200).collect::<String>()
                    );
                }
            }
        }
    }
}
