// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus search <id> <query> [--limit N] [--json]` — embed the query via the
//! daemon's embed slot and search a corpus index, closing the ingest→query loop
//! for a workflow-built corpus (or any installed one). Vector + FTS hybrid.

use corpus_index::index::CorpusIndex;
use oicp_client::RemoteApiProvider;
use sovereign_contracts::traits::InferenceProvider;

const DEFAULT_DAEMON: &str = "http://localhost:9741";

pub async fn cmd_corpus_search(args: &[String]) -> i32 {
    let mut id: Option<String> = None;
    let mut limit = 5usize;
    let mut json = false;
    let mut terms: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(limit);
            }
            "--json" => json = true,
            s if id.is_none() => id = Some(s.to_string()),
            s => terms.push(s.to_string()),
        }
        i += 1;
    }
    let query = terms.join(" ");
    let Some(id) = id else {
        eprintln!("Usage: svrn corpus search <id> <query> [--limit N] [--json]");
        return 1;
    };
    if query.is_empty() {
        eprintln!("Usage: svrn corpus search <id> <query> [--limit N] [--json]");
        return 1;
    }

    let hits = match search_corpus(&id, &query, limit).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    // `--json`: the hits as a JSON array of `corpus_index::types::ScoredChunk`
    // on stdout, for a caller that asserts on them (`svrn quality lane
    // chat-ask`) rather than reading the table.
    if json {
        return match serde_json::to_string(&hits) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(e) => {
                eprintln!("serialise hits: {e}");
                1
            }
        };
    }
    if hits.is_empty() {
        println!("No results in `{id}` for: {query}");
        return 0;
    }
    println!(
        "\n{} result(s) in `{id}` for \u{201c}{query}\u{201d}:\n",
        hits.len()
    );
    for (n, h) in hits.iter().enumerate() {
        let title = h.title.as_deref().unwrap_or("(untitled)");
        let preview: String = h.content.chars().take(180).collect();
        println!("{}. [{:.3}] {}", n + 1, h.score, title);
        println!("   {}\u{2026}\n", preview.replace('\n', " ").trim());
    }
    0
}

/// Search one corpus and hand back the hits.
///
/// Extracted from [`cmd_corpus_search`] on 2026-09-04 so
/// `svrn quality lane chat-ask` can assert "the fixture is findable" through
/// the SAME embed-model resolution, the same index path and the same hybrid
/// search the operator's `svrn corpus search` runs. A lane that opened the
/// index its own way would be checking a different question than the one the
/// operator would type (ARCH §10.6). The lane now runs this verb itself,
/// `--json`, through the dispatcher (pb-cli-llm-bench-move).
///
/// Errors are strings a human can act on — they are what the command prints.
pub(crate) async fn search_corpus(
    id: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<corpus_index::types::ScoredChunk>, String> {
    // Embed the query via the daemon's embed slot, resolved through the ONE
    // decider (`sovereign_workflow_host::daemon_models`): configured stem →
    // advertised id, proved by a `/v1/embeddings` probe. The refusal names
    // what was probed; "advertises no embedding model" is no longer a
    // verdict anything here reaches from an id substring.
    let v1 = format!("{DEFAULT_DAEMON}/v1");
    let embed_model = sovereign_workflow_host::resolve_embed_model(&v1, None)
        .await
        .map(|r| r.id)
        .map_err(|e| {
            format!(
                "Cannot embed the query via the daemon at {DEFAULT_DAEMON}: {e}\n\
                 Start it with `svrn daemon` if it is not running."
            )
        })?;
    let provider = RemoteApiProvider::new(&v1, None, &embed_model, 8192);
    let embedding = provider
        .embed(query)
        .await
        .map_err(|e| format!("embed query failed: {e}"))?;

    // Open the corpus by id under the canonical index dir.
    let index_dir = sovereign_contracts::setup_config::SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("indexes");
    let path = index_dir.join(id);
    let index = CorpusIndex::open(&path)
        .await
        .map_err(|e| format!("open corpus `{id}` at {}: {e}", path.display()))?;
    index
        .search(&embedding, query, limit)
        .await
        .map_err(|e| format!("search `{id}`: {e}"))
}
