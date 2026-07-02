// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus ingest <folder>` — build a corpus by running the shipped
//! `notebook` workflow (`extract → chunk → embed → store`) over a folder of
//! documents, on the Step·Artifact·Runner substrate.
//!
//! This is a *production* ingest path backed by the workflow substrate: a real
//! `corpus` subcommand whose mechanism is the Runner, not a bespoke loop. It runs
//! the same document-capable definition as `workflow run notebook` and the
//! desktop folder-ingest — `tool:extract` handles PDF/Office/HTML/epub/md/txt, so
//! this is no longer plain-text only. The corpus it builds is byte-compatible with
//! the bespoke engine's (`tool:corpus_store` writes via the same
//! `CorpusIndex::insert_batch`). Still bespoke-only: OCR, batched embedding, and
//! enrichment — convergence on those is future work.

use sovereign_workflow::Workflow;
use sovereign_workflow_host::resolve_workflow_source;

const DEFAULT_DAEMON: &str = "http://localhost:9741";

pub async fn cmd_corpus_ingest(args: &[String]) -> i32 {
    let mut folder: Option<String> = None;
    let mut corpus: Option<String> = None;
    // Unset → notebook matches every file and extracts each by type.
    let mut glob: Option<String> = None;
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
                glob = args.get(i).cloned();
            }
            "--concurrency" => {
                i += 1;
                concurrency = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(concurrency);
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
            "Usage: svrn corpus ingest <folder> [--corpus <id>] [--glob '*.pdf,*.md'] \
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

    // Run the shipped, document-capable `notebook` workflow — the single ingest
    // definition shared with `workflow run notebook` and the desktop. A user's
    // customized `notebook` (via `workflow copy`) is honored; the shipped one is
    // the fallback. Params drive the source folder/glob and the corpus name.
    let (toml, origin) = match resolve_workflow_source("notebook") {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("ingest: {e}");
            return 1;
        }
    };
    let wf = match Workflow::parse(&toml) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("ingest: parse notebook workflow: {e}");
            return 1;
        }
    };

    let glob_param = glob.unwrap_or_default();
    let mut params = std::collections::BTreeMap::new();
    params.insert("folder".to_string(), folder.clone());
    params.insert("corpus".to_string(), corpus.clone());
    params.insert("glob".to_string(), glob_param.clone());

    let glob_desc = if glob_param.is_empty() {
        "all files".to_string()
    } else {
        glob_param
    };
    eprintln!(
        "Ingesting `{folder}` ({glob_desc}) → corpus `{corpus}` via the workflow runner ({origin})…"
    );
    let code =
        crate::workflow_cmd::run_assembled(&wf, DEFAULT_DAEMON, concurrency, no_cache, params)
            .await;
    if code == 0 {
        eprintln!("\nDone. Query it:  sovereign corpus search {corpus} \"<your question>\"");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `corpus ingest` runs the shipped `notebook` definition — the
    /// document-capable shape (`extract → chunk → embed → store`), not the old
    /// plain-text `chunk → embed → store`. Asserted against the embedded shipped
    /// TOML so it's hermetic (no `~/.sovereign/workflows` dependency).
    #[test]
    fn ingest_runs_the_document_capable_notebook_shape() {
        let (_, toml) = sovereign_workflow_host::SHIPPED_WORKFLOWS
            .iter()
            .find(|(name, _)| *name == "notebook")
            .expect("the `notebook` starter ships");
        let wf = Workflow::parse(toml).unwrap();
        let order = wf.topo_order().unwrap();
        let ids: Vec<&str> = order.iter().map(|&i| wf.steps[i].id.as_str()).collect();
        assert_eq!(ids, vec!["extract", "chunk", "embed", "store"]);
        // The folder/corpus/glob the command passes are real params of the workflow.
        let params = wf.referenced_params();
        assert!(params.contains("folder"));
        assert!(params.contains("corpus"));
        assert!(params.contains("glob"));
    }
}
