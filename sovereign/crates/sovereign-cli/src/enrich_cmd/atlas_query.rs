//! `sovereign enrich atlas-query` — Phase A Step 6 query driver.
//!
//! Reads the resolved atlas from
//! `~/.sovereign/indexes/<corpus>/atlas/`, classifies a natural-
//! language query into a traversal plan, walks the atlas, and
//! prints the rendered brief. Zero LLM calls — the classifier +
//! traversal + brief renderer are all pure Rust.
//!
//! For a deeper query that needs LLM synthesis on top of the
//! brief, pipe `atlas-query`'s JSON output into whatever caller
//! wants it (via `--json`).

use std::path::PathBuf;

use corpus_engine::atlas_traversal::{
    assemble_brief, classify_query, traverse,
    engine::AtlasView,
};
use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, AtomEnvelope, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-query",
    summary: "Classify + traverse a query against the resolved atlas (no LLM).",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich atlas-query <corpus-id> \"<query>\" [--json]",
        ),
        HelpSection::Flags(&[(
            "--json",
            "Emit the TraversalResult as pretty JSON instead of the assembled brief.",
        )]),
        HelpSection::Examples(&[
            (
                "sovereign enrich atlas-query brothers_karamazov \"Who is Alyosha?\"",
                "Entity lookup — prints entity's relations, claims, and trajectory.",
            ),
            (
                "sovereign enrich atlas-query bk \"How does Alyosha change?\"",
                "Trajectory — ordered states + transitions.",
            ),
            (
                "sovereign enrich atlas-query bk \"What configurations does the work enact?\"",
                "Lists Configuration atoms with interpretive notes.",
            ),
        ]),
        HelpSection::Notes(
            "Requires `sovereign enrich atlas-resolve <corpus> --phase all` first. The \
             classifier matches the query against the atlas's entity vocabulary; a query \
             it can't classify prints a one-line miss brief. `--json` gives callers the \
             full TraversalResult for programmatic consumption.",
        ),
    ],
};

pub async fn cmd_atlas_query(args: &[String]) -> i32 {
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
            eprintln!("error: loading enrichment config: {e}");
            return 1;
        }
    };

    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: reading {}/atoms.json: {e}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };
    let edges_file = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(err) => {
            eprintln!(
                "error: reading {}/edges.json: {err}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };

    // Partition atoms into typed vectors for the traversal engine.
    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = Vec::new();
    let mut questions = Vec::new();
    let mut configurations = Vec::new();
    for a in atoms_file.atoms {
        match a {
            AtomEnvelope::Entity(x) => entities.push(x),
            AtomEnvelope::Event(x) => events.push(x),
            AtomEnvelope::State(x) => states.push(x),
            AtomEnvelope::Relation(x) => relations.push(x),
            AtomEnvelope::Claim(x) => claims.push(x),
            AtomEnvelope::Question(x) => questions.push(x),
            AtomEnvelope::Configuration(x) => configurations.push(x),
        }
    }

    let view = AtlasView {
        entities: &entities,
        events: &events,
        states: &states,
        relations: &relations,
        claims: &claims,
        questions: &questions,
        configurations: &configurations,
        edges: &edges_file.edges,
    };

    let plan = classify_query(&parsed.query, &entities);
    let result = traverse(&plan, view);

    if parsed.as_json {
        match serde_json::to_string_pretty(&result) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(e) => {
                eprintln!("error: serialising traversal result: {e}");
                1
            }
        }
    } else {
        let brief = assemble_brief(&result);
        println!("{}", brief.to_text());
        if result.hit {
            0
        } else {
            // Non-fatal miss — caller may want to treat differently.
            0
        }
    }
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

#[derive(Debug)]
struct ParsedQuery {
    corpus_id: String,
    query: String,
    as_json: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedQuery, String> {
    let mut corpus_id: Option<String> = None;
    let mut query: Option<String> = None;
    let mut as_json = false;
    let mut positional_count = 0;
    for arg in args {
        match arg.as_str() {
            "--json" => as_json = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                match positional_count {
                    0 => corpus_id = Some(other.to_string()),
                    1 => query = Some(other.to_string()),
                    _ => return Err(format!("unexpected positional argument: {other}")),
                }
                positional_count += 1;
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let query = query.ok_or_else(|| "missing <query>".to_string())?;
    if query.trim().is_empty() {
        return Err("query must be non-empty".to_string());
    }
    Ok(ParsedQuery {
        corpus_id,
        query,
        as_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_corpus_id_and_query() {
        let p = parse_args(&["bk".into(), "Who is Alyosha?".into()]).unwrap();
        assert_eq!(p.corpus_id, "bk");
        assert_eq!(p.query, "Who is Alyosha?");
        assert!(!p.as_json);
    }

    #[test]
    fn parse_args_accepts_json_flag() {
        let p = parse_args(&[
            "bk".into(),
            "Who is Alyosha?".into(),
            "--json".into(),
        ])
        .unwrap();
        assert!(p.as_json);
    }

    #[test]
    fn parse_args_rejects_missing_query() {
        let err = parse_args(&["bk".into()]).unwrap_err();
        assert!(err.contains("query"));
    }

    #[test]
    fn parse_args_rejects_empty_query() {
        let err = parse_args(&["bk".into(), "   ".into()]).unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "q".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }
}
