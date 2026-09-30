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

use sovereign_workflow_host::resolve_workflow_source;

/// The daemon base via the ONE decider — `sovereign_contracts::setup_config::
/// client_daemon_base()` (env `SOVEREIGN_DAEMON_URL`, then `[daemon]
/// client_port`, then the compiled default), the same resolution
/// `workflow_cmd::default_daemon` applies.
fn default_daemon_base() -> String {
    sovereign_contracts::setup_config::client_daemon_base()
}

pub async fn cmd_corpus_ingest(args: &[String]) -> i32 {
    let mut folder: Option<String> = None;
    let mut corpus: Option<String> = None;
    // Unset → notebook matches every file and extracts each by type.
    let mut glob: Option<String> = None;
    let mut concurrency = 4usize;
    let mut no_cache = false;
    let mut share = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-cache" => no_cache = true,
            "--share" => share = true,
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
             [--concurrency N] [--no-cache] [--share]"
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
    // The daemon this CLI talks to, via the ONE decider (env, then config
    // client_port, then the compiled default). Was a hardcoded
    // `http://localhost:9741` — the same §10.6 defect rung 2 fixed in
    // model_cmd: a CLI pointed at a second daemon silently ingested through
    // the first.
    let daemon = default_daemon_base();
    let code =
        crate::workflow_cmd::run_assembled(&toml, &daemon, concurrency, no_cache, params).await;
    if code != 0 {
        return code;
    }
    eprintln!("\nDone. Query it:  svrn corpus search {corpus} \"<your question>\"");
    if share {
        return report_share(
            &corpus,
            share_corpus(&sovereign_cli_base::dirs::configured_indexes_dir(), &corpus),
        );
    }
    0
}

/// `svrn corpus share <id>` — let this mesh's members search an installed
/// corpus. It sets `query_sharing` in the corpus's meta, the one key
/// `build_hosted_corpora` (sovereign-mesh/src/capabilities.rs) filters the
/// fan-out on. The meta's mtime invalidates the engine's info cache
/// (`installed_indexes`), so the next gossip tick advertises it; no restart.
pub async fn cmd_corpus_share(args: &[String]) -> i32 {
    let [id] = args else {
        eprintln!("Usage: svrn corpus share <id>");
        eprintln!();
        eprintln!("Let the members of this mesh search an installed corpus. Its text stays");
        eprintln!("here; members get cited passages back. `svrn corpus list` names the ids.");
        return if args.is_empty() { 1 } else { 0 };
    };
    report_share(
        id,
        share_corpus(&sovereign_cli_base::dirs::configured_indexes_dir(), id),
    )
}

fn report_share(id: &str, result: Result<std::path::PathBuf, String>) -> i32 {
    match result {
        Ok(meta) => {
            tracing::info!(corpus = id, meta = %meta.display(), "corpus shared: query_sharing set");
            println!(
                "Shared `{id}` with this mesh's members. ({})",
                meta.display()
            );
            0
        }
        Err(e) => {
            tracing::warn!(corpus = id, error = %e, "corpus share refused");
            eprintln!("corpus share: {e}");
            1
        }
    }
}

/// Set `query_sharing = true` in `<indexes_dir>/<id>`'s meta, every other
/// field kept as written. Returns the meta's path.
pub(super) fn share_corpus(
    indexes_dir: &std::path::Path,
    id: &str,
) -> Result<std::path::PathBuf, String> {
    let corpus = corpus_index::corpus::Corpus::named(indexes_dir, id)
        .ok_or("corpus id must not be empty")?;
    if !corpus.is_installed() {
        return Err(format!(
            "no installed corpus `{id}` under {} — `svrn corpus list`",
            indexes_dir.display()
        ));
    }
    let path = corpus.meta_path();
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut meta: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let obj = meta
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
    obj.insert("query_sharing".into(), serde_json::Value::Bool(true));
    let out = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    std::fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `corpus share` sets the key, keeps every other field, and refuses a
    /// corpus that is not installed. That the fan-out then lists it on the
    /// same engine is sovereign-mesh's half
    /// (`a_meta_flip_to_query_sharing_is_listed_by_the_fanout`, moved there
    /// with pb-cli-llm-ingest-move, beside the filter). The failing input is
    /// a local-only corpus (what `corpus_store` creates).
    #[tokio::test]
    async fn share_sets_query_sharing_and_keeps_the_meta() {
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let root = indexes.join("larkspur");
        let idx = corpus_index::index::CorpusIndex::create_with_sharing(
            &root,
            "larkspur",
            "larkspur",
            "test-embed",
            8,
            false,
            Some(false),
            "private",
        )
        .await
        .unwrap();
        idx.mark_ingestion_complete().unwrap();
        drop(idx);

        let meta = share_corpus(&indexes, "larkspur").unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&meta).unwrap()).unwrap();
        assert_eq!(written["query_sharing"], serde_json::Value::Bool(true));
        assert_eq!(written["corpus_id"], "larkspur", "other fields kept");

        assert!(
            share_corpus(&indexes, "absent").is_err(),
            "refused, not created"
        );
    }

    /// `corpus ingest` runs the shipped `notebook` definition — the
    /// document-capable shape (`extract → chunk → embed → store`), not the old
    /// plain-text `chunk → embed → store`. Asserted against the embedded shipped
    /// TOML so it's hermetic (no `~/.svrnmesh/workflows` dependency).
    #[test]
    fn ingest_runs_the_document_capable_notebook_shape() {
        let (_, toml) = sovereign_workflow_host::SHIPPED_WORKFLOWS
            .iter()
            .find(|(name, _)| *name == "notebook")
            .expect("the `notebook` starter ships");
        let wf = sovereign_workflow::Workflow::parse(toml).unwrap();
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
