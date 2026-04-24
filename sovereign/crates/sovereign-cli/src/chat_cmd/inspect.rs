//! `sovereign chat inspect "<question>"` — retrieval without the LLM.
//!
//! Re-runs the embedding + per-corpus search loop `Runtime::search_corpus_indexes`
//! runs on every chat turn, but stops before any generation happens.
//! Prints:
//!   • Which daemon was probed, which models were resolved.
//!   • The embedding dimensions produced for the query.
//!   • Every installed corpus, its dimensions, and whether it was
//!     eligible for hybrid search.
//!   • Per-corpus top-N hits with scores, title, and a snippet.
//!
//! Use this when the model is quoting sources that don't seem to
//! match the question: if `inspect` shows the retrieval found the
//! same wrong sources, the bug is in retrieval (embeddings, recipe
//! wiring, index freshness). If `inspect` shows the *right* sources
//! and the model still flails, the bug is in prompt assembly or the
//! model itself.

use corpus_engine::ScoredChunk;
use serde_json::json;

use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign chat inspect",
    summary: "Run the retrieval stage without the LLM; print every chunk considered.",
    sections: &[
        HelpSection::Usage("sovereign chat inspect \"<question>\" [flags]"),
        HelpSection::Flags(&[
            ("--limit <N>",    "Top-N chunks per corpus to display (default: 5)."),
            ("--corpus <id>",  "Restrict the search to a single corpus_id (default: every installed)."),
            ("--snippet <N>",  "Max chars of chunk content to show inline (default: 200)."),
            ("--format text|json", "Output format (default: text)."),
            ("--help, -h",     "Show this message."),
        ]),
        HelpSection::Notes(
            "Does NOT invoke /v1/chat/completions — only /v1/embeddings. \
             Safe to run against a loaded daemon without consuming the chat slot.",
        ),
    ],
};

pub async fn cmd_inspect(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }

    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let mut question: Option<String> = None;
    let mut limit: usize = 5;
    let mut corpus_filter: Option<String> = None;
    let mut snippet_len: usize = 200;
    let mut format = OutputFormat::Text;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--limit" => {
                i += 1;
                limit = rest
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(limit);
            }
            "--corpus" => {
                i += 1;
                corpus_filter = rest.get(i).cloned();
            }
            "--snippet" => {
                i += 1;
                snippet_len = rest
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(snippet_len);
            }
            "--format" => {
                i += 1;
                match rest.get(i).map(String::as_str) {
                    Some("text") => format = OutputFormat::Text,
                    Some("json") => format = OutputFormat::Json,
                    Some(other) => {
                        eprintln!("error: --format expects text|json, got `{other}`");
                        return 2;
                    }
                    None => {
                        eprintln!("error: --format needs a value");
                        return 2;
                    }
                }
            }
            arg if question.is_none() => {
                question = Some(arg.to_string());
            }
            extra => {
                eprintln!("error: unexpected argument `{extra}`");
                return 2;
            }
        }
        i += 1;
    }

    let Some(question) = question else {
        eprintln!("error: missing question. Usage: sovereign chat inspect \"<question>\"");
        return 2;
    };

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    run_inspect(
        &session,
        &question,
        limit,
        corpus_filter.as_deref(),
        snippet_len,
        format,
    )
    .await
}

#[derive(Copy, Clone, Debug)]
enum OutputFormat {
    Text,
    Json,
}

async fn run_inspect(
    session: &ChatSession,
    question: &str,
    limit: usize,
    corpus_filter: Option<&str>,
    snippet_len: usize,
    format: OutputFormat,
) -> i32 {
    eprintln!("{BAR}");
    eprintln!("query: {question}");
    eprintln!("{BAR}");

    // 1. Embed the query through the split provider. On success we
    //    also learn the embedding's dimensionality, which is what
    //    `search_corpus_indexes` filters corpora on.
    let embedding = match session.inference.embed_query(question).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("embed failed: {e}");
            eprintln!(
                "hint: the daemon may be serving the chat model on /embeddings. \
                 Check `[models] embed` in ~/.config/sovereign/config.toml."
            );
            return 1;
        }
    };
    eprintln!("embedding dims: {}", embedding.len());

    // 2. Enumerate installed indexes, showing ineligibility reasons
    //    up-front so the user doesn't wonder why a corpus got no
    //    hits.
    let indexes = match session.corpus_engine.installed_indexes().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("installed_indexes() failed: {e}");
            return 1;
        }
    };

    if indexes.is_empty() {
        eprintln!("no corpora installed under {}", session_indexes_dir(session));
        eprintln!("install one via `sovereign corpus install <id>` or the desktop folder-drop flow.");
        return 0;
    }

    let mut per_corpus_hits: Vec<CorpusHits> = Vec::new();

    for info in indexes
        .iter()
        .filter(|i| corpus_filter.map_or(true, |f| i.corpus_id == f))
    {
        let dim_match = info.embedding_dimensions == embedding.len();
        let is_code = matches!(info.kind, corpus_engine::CorpusKind::Code);
        eprintln!(
            "corpus {} — {} chunks, kind={:?}, dims {}, model `{}` {}{}",
            info.corpus_id,
            info.chunk_count,
            info.kind,
            info.embedding_dimensions,
            info.embedding_model,
            if dim_match {
                "→ hybrid-search eligible"
            } else {
                "→ FTS-only (dim mismatch with query)"
            },
            if is_code {
                "  [omitted from chat by default — served by code-intelligence MCP tools]"
            } else {
                ""
            },
        );

        // Respect the dimension filter the same way the runtime
        // does: if dims don't match, send an empty embedding so
        // the index uses its Tantivy BM25 path only.
        let query_vec: &[f32] = if dim_match { &embedding } else { &[] };
        let idx = match session.corpus_engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("  open_index failed: {e}");
                continue;
            }
        };
        let hits = match idx.search(query_vec, question, limit).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("  search failed: {e}");
                continue;
            }
        };
        eprintln!("  {} hits (top {limit})", hits.len());
        per_corpus_hits.push(CorpusHits {
            corpus_id: info.corpus_id.clone(),
            chunks: hits,
        });
    }

    match format {
        OutputFormat::Text => print_text(&per_corpus_hits, snippet_len),
        OutputFormat::Json => print_json(&per_corpus_hits, question),
    }

    0
}

struct CorpusHits {
    corpus_id: String,
    chunks: Vec<ScoredChunk>,
}

fn print_text(per_corpus: &[CorpusHits], snippet_len: usize) {
    for bucket in per_corpus {
        if bucket.chunks.is_empty() {
            continue;
        }
        eprintln!();
        eprintln!("═══ {} ═══", bucket.corpus_id);
        for (i, c) in bucket.chunks.iter().enumerate() {
            let title = c
                .title
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("<untitled>");
            let snippet = truncate(&c.content.replace('\n', " "), snippet_len);
            eprintln!(
                "  [{rank:>2}] score={score:.3}  {title}",
                rank = i + 1,
                score = c.score,
                title = title,
            );
            if let Some(url) = c.url.as_deref() {
                eprintln!("       {url}");
            }
            if !c.metadata.is_empty() {
                let mut kvs: Vec<_> = c.metadata.iter().collect();
                kvs.sort_by_key(|(k, _)| k.as_str());
                let joined = kvs
                    .iter()
                    .take(6)
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !joined.is_empty() {
                    eprintln!("       {joined}");
                }
            }
            eprintln!("       {snippet}");
        }
    }
}

fn print_json(per_corpus: &[CorpusHits], question: &str) {
    let payload = json!({
        "query": question,
        "corpora": per_corpus.iter().map(|b| json!({
            "corpus_id": b.corpus_id,
            "chunks": b.chunks.iter().map(|c| json!({
                "score": c.score,
                "title": c.title,
                "url": c.url,
                "content": c.content,
                "metadata": c.metadata,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn session_indexes_dir(session: &ChatSession) -> String {
    // We don't re-store the path on ChatSession to avoid cloning the
    // whole PathBuf. Operationally the user knows their own data
    // dir; print the daemon URL so the "is this the right daemon?"
    // question is obvious.
    format!("configured data dir (daemon={})", session.daemon_base)
}

const BAR: &str = "─────────────────────────────────────────────────────────────";
