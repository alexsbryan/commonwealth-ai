// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn search <query> [--corpus <id>]... [--document <name>]... [--limit N]
//! [--json]` — search your corpora, or only some texts in them.
//!
//! A client of the daemon's knowledge search as the manifest advertises it
//! (`search_endpoint`), with v0.6 §2.8's two features: `--document` scopes
//! the hits to texts by name (`knowledge:scoped`), and each hit is printed
//! with its range of its stored text, or why it has none (`knowledge:range`).
//! A host that does not advertise `knowledge:scoped` is told so when
//! `--document` is asked, never searched unscoped. Every hit prints its text's
//! name, so a search is how a user finds the name to scope the next one by.
//! It is the platform's own second user of scoped search (CITE_V1 §1.3).

use sovereign_contracts::oicp::{
    features, KnowledgeResult, KnowledgeSearchResponse, ProviderManifest,
};

#[derive(clap::Parser, Debug)]
#[command(name = "svrn search", no_binary_name = true, disable_help_flag = true)]
struct Opts {
    /// What to look for.
    #[arg(required = true, num_args = 1..)]
    query: Vec<String>,
    /// A corpus to search; repeatable. None means every corpus you may read.
    #[arg(long = "corpus")]
    corpora: Vec<String>,
    /// A text, by its name, the hits must come from; repeatable.
    #[arg(long = "document")]
    documents: Vec<String>,
    /// How many hits.
    #[arg(long, default_value_t = 10)]
    limit: u32,
    /// Print the reply as JSON.
    #[arg(long)]
    json: bool,
}

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let opts = match sovereign_cli_shared::flag_surface::parse::<Opts>(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("search: {e}");
            return 2;
        }
    };
    let Some(base) = sovereign_cli_shared::urls::daemon_base_or_refuse(None) else {
        return 2;
    };
    let http = reqwest::Client::new();
    let endpoint = match search_endpoint(&http, &base, !opts.documents.is_empty()).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("search: {e}");
            return 1;
        }
    };
    let body = serde_json::json!({
        "query": opts.query.join(" "),
        "corpora": (!opts.corpora.is_empty()).then_some(&opts.corpora),
        "documents": opts.documents,
        "limit": opts.limit,
    });
    let resp = match http
        .post(format!("{base}{endpoint}"))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("search: the daemon at {base} did not answer: {e}");
            return 1;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let why = if status == reqwest::StatusCode::BAD_REQUEST && !opts.documents.is_empty() {
            " (is each --document a text name, 64 hex characters as `svrn search` prints them?)"
        } else {
            ""
        };
        eprintln!("search: {status}{why}");
        return 1;
    }
    let reply: KnowledgeSearchResponse = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("search: the reply does not decode: {e}");
            return 1;
        }
    };
    if opts.json {
        match serde_json::to_string_pretty(&reply) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("search: the reply does not encode: {e}");
                return 1;
            }
        }
        return 0;
    }
    for (i, hit) in reply.results.iter().enumerate() {
        print_hit(i + 1, hit);
    }
    for c in &reply.corpora_unavailable {
        println!("corpus {c} not searched: unavailable");
    }
    println!(
        "\n{} hit(s) from {} corpus(es){}",
        reply.results.len(),
        reply.corpora_searched.len(),
        if opts.documents.is_empty() {
            String::new()
        } else {
            format!(", scoped to {} text(s)", opts.documents.len())
        }
    );
    0
}

/// The manifest's search endpoint, if the host offers what was asked.
async fn search_endpoint(
    http: &reqwest::Client,
    base: &str,
    scoped: bool,
) -> Result<String, String> {
    let m: ProviderManifest = http
        .get(format!("{base}/oicp/v1/capabilities"))
        .send()
        .await
        .map_err(|e| format!("the daemon at {base} did not answer: {e}"))?
        .json()
        .await
        .map_err(|e| format!("the manifest does not decode: {e}"))?;
    let k = m
        .knowledge
        .as_ref()
        .ok_or("the daemon advertises no knowledge search")?;
    if scoped && !m.has_feature(features::KNOWLEDGE_SCOPED) {
        return Err(
            "the daemon does not advertise `knowledge:scoped`, so --document cannot be honoured"
                .into(),
        );
    }
    Ok(k.search_endpoint.clone())
}

fn print_hit(rank: usize, hit: &KnowledgeResult) {
    let source = hit.document.as_ref().map_or_else(
        || hit.source_doc_id.clone().unwrap_or_default(),
        |d| d.source.id.clone(),
    );
    println!("{rank:>3}. {:.3}  {}  {source}", hit.score, hit.corpus_id);
    let mut head: String = hit
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(100)
        .collect();
    if hit.content.chars().count() > 100 {
        head.push('…');
    }
    println!("     {head}");
    match (&hit.document, &hit.document_absent) {
        (Some(d), _) => println!("     text {}", d.text_sha256),
        (None, Some(why)) => println!("     no text: {why}"),
        (None, None) => {}
    }
    match (&hit.span, &hit.span_absent) {
        (Some(s), _) => println!("     at [{}, {})", s.start, s.end),
        (None, Some(why)) => println!("     no range: {why}"),
        (None, None) => {}
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn search",
    summary: "Search your corpora, or only some texts in them (OICP knowledge search).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn search <query>                          Every corpus you may read\n\
             svrn search <query> --corpus <id> ...        Only these corpora\n\
             svrn search <query> --document <name> ...    Only these texts\n\
             svrn search <query> --limit 20 --json        More hits, as JSON",
        ),
        crate::util::help::HelpSection::Notes(
            "Each hit prints its text's name (`text <64 hex>`): pass it to --document to \
             search within that text. A hit's range `at [start, end)` is in code points of \
             that text; a hit with none says why (`no range: not an exact slice`).",
        ),
    ],
};
