// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus search <id> <query> [--limit N]` — embed the query via the
//! daemon's embed slot and search a corpus index, closing the ingest→query loop
//! for a workflow-built corpus (or any installed one). Vector + FTS hybrid.

use corpus_engine::CorpusIndex;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;

const DEFAULT_DAEMON: &str = "http://localhost:9741";

pub async fn cmd_corpus_search(args: &[String]) -> i32 {
    let mut id: Option<String> = None;
    let mut limit = 5usize;
    let mut terms: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(limit);
            }
            s if id.is_none() => id = Some(s.to_string()),
            s => terms.push(s.to_string()),
        }
        i += 1;
    }
    let query = terms.join(" ");
    let Some(id) = id else {
        eprintln!("Usage: svrn corpus search <id> <query> [--limit N]");
        return 1;
    };
    if query.is_empty() {
        eprintln!("Usage: svrn corpus search <id> <query> [--limit N]");
        return 1;
    }

    // Embed the query via the daemon's embed slot — reuses the workflow command's
    // model discovery so the `embed` model convention lives in one place.
    let v1 = format!("{DEFAULT_DAEMON}/v1");
    let models = match sovereign_workflow_host::discover_models(&v1).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "Daemon not reachable at {DEFAULT_DAEMON} ({e}). Start it with `svrn daemon`."
            );
            return 1;
        }
    };
    let Some(embed_model) = models.embed else {
        eprintln!("The daemon advertises no embedding model — cannot embed the query.");
        return 1;
    };
    let provider = RemoteApiProvider::new(&v1, None, &embed_model, 8192);
    let embedding = match provider.embed(&query).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("embed query failed: {e}");
            return 1;
        }
    };

    // Open the corpus by id under the canonical index dir.
    let index_dir = sovereign_core::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("indexes");
    let path = index_dir.join(&id);
    let index = match CorpusIndex::open(&path).await {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("open corpus `{id}` at {}: {e}", path.display());
            return 1;
        }
    };

    let hits = match index.search(&embedding, &query, limit).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("search `{id}`: {e}");
            return 1;
        }
    };
    if hits.is_empty() {
        println!("No results in `{id}` for: {query}");
        return 0;
    }
    println!("\n{} result(s) in `{id}` for \u{201c}{query}\u{201d}:\n", hits.len());
    for (n, h) in hits.iter().enumerate() {
        let title = h.title.as_deref().unwrap_or("(untitled)");
        let preview: String = h.content.chars().take(180).collect();
        println!("{}. [{:.3}] {}", n + 1, h.score, title);
        println!("   {}\u{2026}\n", preview.replace('\n', " ").trim());
    }
    0
}
