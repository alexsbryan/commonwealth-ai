// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn search <query> [--corpus <id>]... [--document <name>]... [--limit N]
//! [--json]` — search your corpora, or only some texts in them.
//!
//! A client of the daemon's knowledge search as the manifest advertises it
//! (`search_endpoint`), with v0.6 §2.8's two features: `--document` scopes
//! the hits to texts by name (`knowledge:scoped`), and each hit is printed
//! with its range of its stored text, or why it has none (`knowledge:range`),
//! and where that range stands when the host names it (`knowledge:locator`,
//! §2.13: the headings above it, a transcript turn's time and speaker).
//! A host that does not advertise `knowledge:scoped` is told so when
//! `--document` is asked, never searched unscoped. Every hit prints its text's
//! name, so a search is how a user finds the name to scope the next one by.
//! It is the platform's own second user of scoped search (CITE_V1 §1.3).
//!
//! `--sentences` asks for sentences by meaning (v0.6 §2.11,
//! `knowledge:sentences`): each hit a sentence with its range, and the reply's
//! scorer, its route (`passages` while the library is still being indexed,
//! with the count pending) and any ceiling the scan stopped at are printed.
//! A host that does not advertise the feature is told so, never asked.
//!
//! A hit whose document carries a record (v0.6 §2.16, `ingest:lookups`) or
//! declared metadata prints its catalogue entry under it (`catalogue`).

#[path = "search_cmd/catalogue.rs"]
mod catalogue;

use sovereign_contracts::oicp::{
    features, KnowledgeResult, KnowledgeSearchResponse, ProviderManifest, SentenceRoute,
    SpanLocator,
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
    /// Sentences by meaning, not passages (`knowledge:sentences`).
    #[arg(long)]
    sentences: bool,
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
    let endpoint =
        match search_endpoint(&http, &base, !opts.documents.is_empty(), opts.sentences).await {
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
        "granularity": if opts.sentences { "sentence" } else { "passage" },
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
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        match body.get("scorer").and_then(|s| s.as_str()) {
            Some(scorer) => eprintln!(
                "search: {status}: the daemon cannot run its sentence scorer `{scorer}` ({})",
                body.get("detail")
                    .and_then(|d| d.as_str())
                    .unwrap_or("no detail")
            ),
            None => eprintln!("search: {status}{why}"),
        }
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
    if let (Some(scorer), Some(route)) = (&reply.scorer, reply.route) {
        println!("\nscored by {} ({})", scorer.name.as_str(), scorer.model);
        match (route, reply.sentences_pending) {
            (SentenceRoute::Passages, Some(n)) => println!(
                "still indexing: {n} sentence(s) pending; these are the sentences of the best passages"
            ),
            _ => println!("from the sentence index"),
        }
    }
    if let Some(t) = reply.truncated {
        println!(
            "scanned {} of {} sentences in scope (the host's ceiling)",
            t.scanned, t.in_scope
        );
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
    sentences: bool,
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
    if sentences && !m.has_feature(features::KNOWLEDGE_SENTENCES) {
        return Err(
            "the daemon does not advertise `knowledge:sentences`, so --sentences cannot be honoured"
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
        (Some(d), _) => {
            for line in catalogue::lines(d) {
                println!("     {line}");
            }
            println!("     text {}", d.text_sha256)
        }
        (None, Some(why)) => println!("     no text: {why}"),
        (None, None) => {}
    }
    match (&hit.span, &hit.span_absent) {
        (Some(s), _) => println!("     at [{}, {})", s.start, s.end),
        (None, Some(why)) => println!("     no range: {why}"),
        (None, None) => {}
    }
    if let Some(l) = hit.span.as_ref().and_then(|s| s.locator.as_ref()) {
        println!("     {}", stands(l));
    }
}

/// Where a span stands (`knowledge:locator`, v0.6 §2.13), as a reader cites
/// it. A span with no locator prints no line: absence is "nowhere named".
fn stands(l: &SpanLocator) -> String {
    match l {
        SpanLocator::HeadingPath { headings } => format!("under {}", headings.join(" › ")),
        SpanLocator::Timestamp {
            milliseconds,
            speaker,
        } => {
            let s = milliseconds / 1000;
            let at = format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60);
            match speaker {
                Some(who) => format!("at {at}, {who}"),
                None => format!("at {at}"),
            }
        }
        SpanLocator::Page { page } => format!("page {page}"),
        SpanLocator::Row { row } => format!("row {row}"),
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
             svrn search <query> --sentences              Sentences by meaning\n\
             svrn search <query> --limit 20 --json        More hits, as JSON",
        ),
        crate::util::help::HelpSection::Notes(
            "Each hit prints its text's name (`text <64 hex>`): pass it to --document to \
             search within that text. A hit's range `at [start, end)` is in code points of \
             that text; a hit with none says why (`no range: not an exact slice`). A hit \
             whose document has a catalogue record prints it first: author, year, title, \
             where it appeared, how many fields a resolver supplied, and any identifier \
             that did not resolve. Where the host names where a range stands, a line says \
             so (`under BOOK I. › CHAPTER I.`, `at 0:01:04, HARBOUR MASTER`).",
        ),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locator_reads_as_a_reader_cites_it() {
        let path = SpanLocator::HeadingPath {
            headings: vec![
                "BOOK I.".into(),
                "CHAPTER I. OF THE DIVISION OF LABOUR.".into(),
            ],
        };
        assert_eq!(
            stands(&path),
            "under BOOK I. › CHAPTER I. OF THE DIVISION OF LABOUR."
        );
        let turn = SpanLocator::Timestamp {
            milliseconds: 3_727_000,
            speaker: Some("HARBOUR MASTER".into()),
        };
        assert_eq!(stands(&turn), "at 1:02:07, HARBOUR MASTER");
        let whisper = SpanLocator::Timestamp {
            milliseconds: 3500,
            speaker: None,
        };
        assert_eq!(stands(&whisper), "at 0:00:03");
    }
}
