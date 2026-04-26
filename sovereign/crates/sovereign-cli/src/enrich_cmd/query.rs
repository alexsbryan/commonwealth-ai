//! `sovereign enrich query <corpus> "<text>" [--show-traversal]` —
//! one-off atlas traversal. Loads the atlas from the phase cache,
//! embeds the query via the daemon, prints LOCATE / TRAVERSE /
//! grounding sections.

use corpus_engine::enrichment::pipeline::{traverse_atlas, Atlas};

use super::config::EnrichConfig;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich query",
    summary: "Run one query against the assembled atlas and print the traversal.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich query <corpus-id> \"<text>\" [--show-traversal] [--threshold <f>]",
        ),
        HelpSection::Flags(&[
            ("--show-traversal", "Print the full LOCATE/TRAVERSE/GROUNDING breakdown (default on)."),
            ("--threshold <f>", "Cosine similarity threshold for LOCATE inclusion (default 0.5)."),
        ]),
        HelpSection::Notes(
            "Requires phase 3 (concerns) cache. Phase 5/6/7 caches are optional; missing caches \
             degrade to empty sections rather than errors.",
        ),
    ],
};

pub async fn cmd_query(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };
    let cfg = match EnrichConfig::require(&parsed.corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!("error: daemon is not responding at {} — start it first", cfg.base_url);
        return 2;
    }

    let atlas = match Atlas::from_cache_dir(&paths::cache_dir(&cfg.corpus_id)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, _chat) = client.into_closures();

    let traversal =
        match traverse_atlas(&atlas, &parsed.query, &embed, parsed.threshold).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: traversal failed: {e}");
                return 1;
            }
        };

    println!("Query: {}", parsed.query);
    println!();
    if parsed.show {
        println!("LOCATE:");
        if traversal.locate.is_empty() {
            println!("  (no canonical concerns matched)");
        } else {
            for m in &traversal.locate {
                println!(
                    "  {} (similarity {:.2}): {}",
                    m.concern_id, m.similarity, m.concern_text
                );
            }
        }
        println!();
        println!("TRAVERSE:");
        if traversal.positions.is_empty() {
            println!("  (no positions under matched concerns)");
        } else {
            println!("  Positions:");
            for p in &traversal.positions {
                let snip: String = p.text.chars().take(200).collect();
                println!("    {} (concern {}) — {snip}…", p.position_id, p.concern_id);
            }
        }
        if !traversal.tensions.is_empty() {
            println!();
            println!("  Tensions:");
            for t in &traversal.tensions {
                println!(
                    "    {}: {} × {} — {}",
                    t.tension_id, t.position_a_id, t.position_b_id, t.description
                );
            }
        }
        println!();
        println!("GROUNDING:");
        if traversal.grounding_chunk_ids.is_empty() {
            println!("  (no grounding passages)");
        } else {
            println!(
                "  {} passage(s): {}",
                traversal.grounding_chunk_ids.len(),
                traversal
                    .grounding_chunk_ids
                    .iter()
                    .take(10)
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    } else {
        let top = traversal
            .locate
            .first()
            .map(|m| format!("{} ({:.2})", m.concern_id, m.similarity))
            .unwrap_or_else(|| "none".into());
        println!(
            "  top match: {top} · {} positions · {} tensions · {} passages",
            traversal.positions.len(),
            traversal.tensions.len(),
            traversal.grounding_chunk_ids.len()
        );
    }
    0
}

#[derive(Debug)]
struct ParsedQuery {
    corpus_id: String,
    query: String,
    show: bool,
    threshold: f32,
}

fn parse_args(args: &[String]) -> Result<ParsedQuery, String> {
    let mut corpus_id: Option<String> = None;
    let mut query: Option<String> = None;
    // Default on — the whole point of this command is to show traversal.
    let mut show = true;
    let mut threshold = 0.5f32;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--show-traversal" => {
                show = true;
                i += 1;
            }
            "--no-traversal" => {
                show = false;
                i += 1;
            }
            "--threshold" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--threshold requires a numeric value".to_string())?;
                threshold = v
                    .parse::<f32>()
                    .map_err(|e| format!("--threshold: {e}"))?;
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if query.is_none() {
                    query = Some(other.to_string());
                } else {
                    return Err(format!("unexpected positional: {other}"));
                }
                i += 1;
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let query = query.ok_or_else(|| "missing query text".to_string())?;
    Ok(ParsedQuery { corpus_id, query, show, threshold })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_default_threshold() {
        let args = vec!["ak".into(), "what about x?".into()];
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "ak");
        assert_eq!(p.query, "what about x?");
        assert!(p.show);
        assert!((p.threshold - 0.5).abs() < 0.001);
    }

    #[test]
    fn parse_query_with_threshold() {
        let args = vec![
            "ak".into(),
            "q".into(),
            "--threshold".into(),
            "0.7".into(),
        ];
        let p = parse_args(&args).unwrap();
        assert!((p.threshold - 0.7).abs() < 0.001);
    }

    #[test]
    fn parse_query_rejects_missing_text() {
        let err = parse_args(&["ak".into()]).unwrap_err();
        assert!(err.contains("query text"));
    }

    #[test]
    fn parse_query_rejects_bad_threshold() {
        let err = parse_args(&[
            "ak".into(),
            "q".into(),
            "--threshold".into(),
            "notanumber".into(),
        ])
        .unwrap_err();
        assert!(err.contains("threshold"));
    }
}
