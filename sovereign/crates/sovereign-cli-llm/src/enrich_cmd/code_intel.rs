// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich code-intel <corpus>` — generate per-symbol intent summaries
//! for a SCIP-indexed CODE corpus and index them as searchable chunks.
//!
//! This is the conceptual->code retrieval bridge (see
//! `sovereign/docs/specs/CODE_INTEL_CHAT.md`): for every function in the
//! corpus's SCIP graph, the daemon's chat model writes a plain-English summary
//! plus the questions it answers (user-vocabulary, never code jargon); each is
//! embedded + upserted into `chunks.lance`, content-hash-gated so only changed
//! bodies cost a model call. Mirrors `enrich extract`'s provider construction.

use corpus_engine::enrichment::code_intel::pass::run_code_intel_for_corpus;

use super::config::EnrichConfig;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich code-intel",
    summary:
        "Summarize every function in a CODE corpus and index the summaries as searchable chunks.",
    sections: &[
        HelpSection::Usage("svrn enrich code-intel <corpus-id>"),
        HelpSection::Flags(&[
            (
                "<corpus-id>",
                "The installed code corpus (id, display name, or unique substring). Must have a \
                 SCIP graph (scip_graph.db) and a `source_path` in its _corpus_meta.json.",
            ),
            (
                "--files=<a,b,...>",
                "Optional scope: enrich only symbols whose file path contains one of these \
                 comma-separated substrings (e.g. --files=streaming.rs,engine.rs). Empty = whole \
                 corpus.",
            ),
        ]),
    ],
};

/// Parse `--files=a,b` / `--files a,b` into the substring scope (empty = whole corpus).
fn parse_files_flag(args: &[String]) -> Vec<String> {
    let split = |v: &str| -> Vec<String> {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--files=") {
            return split(v);
        }
        if a == "--files" {
            if let Some(v) = args.get(i + 1) {
                return split(v);
            }
        }
    }
    Vec::new()
}

pub async fn cmd_code_intel(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    // First non-flag arg is the corpus query (id / name / unique substring).
    let Some(query) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&HELP);
        return 2;
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let indexes_dir = data_dir.join("indexes");

    let corpus_id = match crate::corpus_resolve::resolve_corpus_id(&indexes_dir, query) {
        Ok(id) => id,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };

    // Build the daemon-backed embed + chat providers (same path as `enrich extract`).
    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon is not responding at {} — start it first",
            cfg.base_url
        );
        return 2;
    }
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, chat) = client.into_closures();

    let corpus_dir = indexes_dir.join(&corpus_id);
    let file_filter = parse_files_flag(args);
    if file_filter.is_empty() {
        println!(
            "code-intel: enriching corpus '{corpus_id}' ({})",
            corpus_dir.display()
        );
    } else {
        println!(
            "code-intel: enriching corpus '{corpus_id}' scoped to files matching {:?} ({})",
            file_filter,
            corpus_dir.display()
        );
    }

    match run_code_intel_for_corpus(&corpus_dir, &corpus_id, &chat, &embed, &file_filter).await {
        Ok(report) => {
            println!(
                "code-intel: {} symbols | summarized {} (reused {}, failed {}) | indexed {} (skipped {}, failed {})",
                report.symbols,
                report.enrich.regenerated,
                report.enrich.reused,
                report.enrich.failed,
                report.index.upserted,
                report.index.skipped,
                report.index.failed,
            );
            0
        }
        Err(e) => {
            eprintln!("error: code-intel pass failed: {e}");
            1
        }
    }
}
