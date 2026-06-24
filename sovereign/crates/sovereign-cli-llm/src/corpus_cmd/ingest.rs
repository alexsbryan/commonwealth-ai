// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign corpus ingest <folder>` — build a corpus by running the
//! `chunk → embed → store` workflow over a folder of documents, on the
//! Step·Artifact·Runner substrate.
//!
//! This is the first *production* ingest path backed by the workflow substrate:
//! a real `corpus` subcommand whose mechanism is the Runner, not a bespoke loop.
//! The engine's own `ingest()` is untouched — this covers the plain-text case
//! (no extraction/OCR/sharding), proven identical by the chunk→embed→store diff
//! and queryable via `corpus search`. Convergence with the rich engine path
//! (batched embed, resume, enrichment) is future work.

use sovereign_workflow::Workflow;

const DEFAULT_DAEMON: &str = "http://localhost:9741";

pub async fn cmd_corpus_ingest(args: &[String]) -> i32 {
    let mut folder: Option<String> = None;
    let mut corpus: Option<String> = None;
    let mut glob = "*.txt".to_string();
    let mut concurrency = 4usize;
    let mut no_cache = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-cache" => no_cache = true,
            "--corpus" => {
                i += 1;
                corpus = args.get(i).cloned();
            }
            "--glob" => {
                i += 1;
                if let Some(g) = args.get(i) {
                    glob = g.clone();
                }
            }
            "--concurrency" => {
                i += 1;
                concurrency = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(concurrency);
            }
            s if !s.starts_with('-') && folder.is_none() => folder = Some(s.to_string()),
            other => {
                eprintln!("Unknown argument: {other}");
                return 1;
            }
        }
        i += 1;
    }

    let Some(folder) = folder else {
        eprintln!(
            "Usage: sovereign corpus ingest <folder> [--corpus <id>] [--glob '*.txt'] \
             [--concurrency N] [--no-cache]"
        );
        return 1;
    };
    // Default corpus id = the folder's basename.
    let corpus = corpus.unwrap_or_else(|| {
        std::path::Path::new(&folder)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("corpus")
            .to_string()
    });

    let wf = match build_ingest_workflow(&folder, &corpus, &glob) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("ingest: {e}");
            return 1;
        }
    };

    eprintln!("Ingesting `{folder}` (glob {glob}) → corpus `{corpus}` via the workflow runner…");
    let code = crate::workflow_cmd::run_assembled(
        &wf,
        DEFAULT_DAEMON,
        concurrency,
        no_cache,
        std::collections::BTreeMap::new(),
    )
    .await;
    if code == 0 {
        eprintln!("\nDone. Query it:  sovereign corpus search {corpus} \"<your question>\"");
    }
    code
}

/// Build the `chunk → embed → store` ingest workflow for a folder. The shape
/// mirrors `examples/ingest.toml`; built as TOML so it reuses the parser's
/// edge-derivation + validation rather than hand-constructing the graph.
fn build_ingest_workflow(
    folder: &str,
    corpus: &str,
    glob: &str,
) -> std::result::Result<Workflow, String> {
    let toml = format!(
        r#"
[workflow]
name = "ingest-{corpus_id}"

[source]
type = "folder"
path = "{folder}"
glob = "{glob}"

[[step]]
id = "chunk"
uses = "tool:chunk"
params = {{ path = "{{item.path}}" }}

[[step]]
id = "embed"
uses = "embed:default"
for_each = "chunk"
input = "{{element.text}}"

[[step]]
id = "store"
uses = "tool:corpus_store"
params = {{ corpus = "{corpus_id}", chunks = "{{chunk.output}}", embeddings = "{{embed.output}}", title = "{{item.stem}}", source_doc_id = "{{item.path}}" }}
"#,
        corpus_id = toml_escape(corpus),
        folder = toml_escape(folder),
        glob = toml_escape(glob),
    );
    Workflow::parse(&toml).map_err(|e| e.to_string())
}

/// Escape a value for a TOML basic (double-quoted) string — backslash + quote
/// (enough for filesystem paths and corpus ids; control chars don't occur here).
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_workflow_builds_chunk_embed_store_in_order() {
        let wf = build_ingest_workflow("/data/docs", "mybook", "*.txt").unwrap();
        let order = wf.topo_order().unwrap();
        let ids: Vec<&str> = order.iter().map(|&i| wf.steps[i].id.as_str()).collect();
        // chunk → embed (maps over chunk) → store (consumes both collections).
        assert_eq!(ids, vec!["chunk", "embed", "store"]);
        let embed = wf.steps.iter().find(|s| s.id == "embed").unwrap();
        assert_eq!(embed.for_each.as_deref(), Some("chunk"));
    }

    #[test]
    fn ingest_workflow_escapes_paths() {
        // A path with a quote must not break the generated TOML.
        let wf = build_ingest_workflow("/data/\"odd\" docs", "c", "*.txt").unwrap();
        assert_eq!(wf.steps.len(), 3);
    }
}
