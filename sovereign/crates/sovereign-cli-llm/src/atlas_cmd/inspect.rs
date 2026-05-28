//! `sovereign atlas list-corpora | list-atoms | show-atom` — read-only
//! atlas inspection from the CLI. Same code path as the desktop's
//! atlas inspector (sovereign-tools::atlas_view::FileAtlasReader);
//! different transport.
//!
//! The CLI is the sanity-check loop for triage:
//!
//! ```text
//! sovereign atlas list-corpora                         # what atlases exist?
//! sovereign atlas list-atoms wikipedia --type=Claim    # what got extracted?
//! sovereign atlas show-atom wikipedia entity-0042      # full record
//! ```
//!
//! Output is human-formatted by default. Pass `--format=json` to get
//! the same DTOs the desktop receives — useful for piping into `jq`.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::atoms::AtomType;
use sovereign_tools::atlas_view::{AtomFilter, FileAtlasReader, PageCursor};

use crate::util::help::{self, Help, HelpSection};

const LIST_CORPORA_HELP: Help = Help {
    command: "sovereign atlas list-corpora",
    summary: "List every installed corpus that has an atlas on disk.",
    sections: &[
        HelpSection::Usage("sovereign atlas list-corpora [--format=text|json]"),
        HelpSection::Notes(
            "Reads from `<data-dir>/indexes/<corpus>/atlas/`. Uses the cached \
             `_summary.json` sidecar so this is fast even at wiki scale.",
        ),
    ],
};

const LIST_ATOMS_HELP: Help = Help {
    command: "sovereign atlas list-atoms",
    summary: "Browse atoms within a corpus — filterable by type and substring.",
    sections: &[
        HelpSection::Usage(
            "sovereign atlas list-atoms <corpus_id> [--type=TYPE] [--query=Q] \
             [--limit=N] [--offset=N] [--format=text|json]",
        ),
        HelpSection::Notes(
            "TYPE is one of: Entity, Event, State, Relation, Claim, Question, \
             Configuration, ArgumentReconstruction. Default limit is 50; pass \
             --limit=0 to dump everything matching.",
        ),
    ],
};

const SHOW_ATOM_HELP: Help = Help {
    command: "sovereign atlas show-atom",
    summary: "Show full inspector record for one atom — type-specific body, \
              evidence excerpts, related atoms, cross-corpus links.",
    sections: &[
        HelpSection::Usage(
            "sovereign atlas show-atom <corpus_id> <atom_id> [--format=text|json]",
        ),
    ],
};

// ─── list-corpora ────────────────────────────────────────────

pub async fn run_list_corpora(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&LIST_CORPORA_HELP);
        return 0;
    }
    let format = parse_format(args);

    let reader = match build_reader() {
        Ok(r) => r,
        Err(code) => return code,
    };
    let summaries = match reader.list_corpora().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: list_corpora failed: {e}");
            return 1;
        }
    };

    if format == Format::Json {
        match serde_json::to_string_pretty(&summaries) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("error: serialise JSON: {e}");
                1
            }
        }
    } else {
        if summaries.is_empty() {
            println!("No atlases on disk yet.");
            return 0;
        }
        println!("{:<32} {:>10}  per-type counts", "CORPUS", "ATOMS");
        for s in &summaries {
            let breakdown: Vec<String> = s
                .atom_counts
                .iter()
                .map(|(t, n)| format!("{}={n}", atom_type_short(t)))
                .collect();
            println!(
                "{:<32} {:>10}  {}",
                truncate(&s.corpus_id, 32),
                s.total_atoms,
                breakdown.join(" ")
            );
        }
        0
    }
}

// ─── list-atoms ──────────────────────────────────────────────

pub async fn run_list_atoms(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&LIST_ATOMS_HELP);
        return 0;
    }
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let Some(corpus_id) = positional.first().map(|s| s.as_str()) else {
        eprintln!("error: missing <corpus_id>");
        help::print(&LIST_ATOMS_HELP);
        return 2;
    };

    let atom_type = match parse_flag(args, "--type") {
        Some(t) => match parse_atom_type(&t) {
            Some(parsed) => Some(parsed),
            None => {
                eprintln!(
                    "error: unknown --type `{t}`. Expected: Entity, Event, State, \
                     Relation, Claim, Question, Configuration, ArgumentReconstruction.",
                );
                return 2;
            }
        },
        None => None,
    };
    let name_query = parse_flag(args, "--query");
    let limit: usize = parse_flag(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let offset: usize = parse_flag(args, "--offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let format = parse_format(args);

    let reader = match build_reader() {
        Ok(r) => r,
        Err(code) => return code,
    };
    let effective_limit = if limit == 0 { usize::MAX } else { limit };
    let page = match reader
        .list_atoms(
            corpus_id,
            AtomFilter {
                atom_type,
                name_query: name_query.clone(),
                min_salience: None,
            },
            PageCursor {
                offset,
                limit: effective_limit,
            },
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: list_atoms: {e}");
            return 1;
        }
    };

    if format == Format::Json {
        match serde_json::to_string_pretty(&page) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("error: serialise JSON: {e}");
                1
            }
        }
    } else {
        println!(
            "Showing {} of {} matching atoms in `{corpus_id}`.",
            page.items.len(),
            page.total_matching
        );
        if !page.items.is_empty() {
            println!();
            println!(
                "{:<24} {:<22} {:<8} {:>4}  NAME",
                "ATOM_ID", "TYPE", "SALIENCE", "EVID",
            );
        }
        for item in &page.items {
            let salience = item
                .salience
                .map(|s| format!("{s:.2}"))
                .unwrap_or_else(|| "—".into());
            println!(
                "{:<24} {:<22} {:<8} {:>4}  {}",
                item.atom_id.as_str(),
                atom_type_short(&item.atom_type),
                salience,
                item.evidence_chunk_count,
                truncate(&item.display_name, 60),
            );
        }
        if let Some(next) = page.next_offset {
            println!();
            println!("(more — re-run with --offset={next})");
        }
        0
    }
}

// ─── show-atom ───────────────────────────────────────────────

pub async fn run_show_atom(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&SHOW_ATOM_HELP);
        return 0;
    }
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let (Some(corpus_id), Some(atom_id)) = (
        positional.first().map(|s| s.as_str()),
        positional.get(1).map(|s| s.as_str()),
    ) else {
        eprintln!("error: missing <corpus_id> <atom_id>");
        help::print(&SHOW_ATOM_HELP);
        return 2;
    };
    let format = parse_format(args);

    let reader = match build_reader() {
        Ok(r) => r,
        Err(code) => return code,
    };
    let detail = match reader.get_atom_detail(corpus_id, atom_id).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            eprintln!("error: atom `{atom_id}` not found in corpus `{corpus_id}`");
            return 1;
        }
        Err(e) => {
            eprintln!("error: get_atom_detail: {e}");
            return 1;
        }
    };

    if format == Format::Json {
        match serde_json::to_string_pretty(&detail) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("error: serialise JSON: {e}");
                1
            }
        }
    } else {
        println!("{} — {}", detail.atom_id.as_str(), detail.display_name);
        println!("  type:        {}", atom_type_short(&detail.atom_type));
        if let Some(s) = detail.salience {
            println!("  salience:    {s:.2}");
        }
        println!("  stable_key:  {}", detail.stable_key.as_str());
        println!("  corpus:      {}", detail.corpus_id);
        println!();
        if !detail.evidence_excerpts.is_empty() {
            println!("EVIDENCE ({} excerpt(s)):", detail.evidence_excerpts.len());
            for e in &detail.evidence_excerpts {
                println!("  • {}", e.section_id);
                if let Some(p) = &e.passage_preview {
                    println!("      {}", truncate(p, 100));
                }
            }
            println!();
        }
        if !detail.related.is_empty() {
            println!("RELATED ({}):", detail.related.len());
            for r in &detail.related {
                let arrow = if r.role == "source" { "←" } else { "→" };
                println!(
                    "  {} [{:?}] {} ({}, conf {:.2})",
                    arrow,
                    r.edge_type,
                    r.display_name,
                    atom_type_short(&r.atom_type),
                    r.confidence,
                );
            }
            println!();
        }
        if !detail.cross_corpus.is_empty() {
            println!("CROSS-CORPUS ({}):", detail.cross_corpus.len());
            for c in &detail.cross_corpus {
                println!(
                    "  → [{:?}] {}: {} (signal {}, conf {:.2})",
                    c.edge_type,
                    c.peer_corpus_id,
                    c.peer_canonical_name,
                    c.signal,
                    c.confidence,
                );
            }
        }
        0
    }
}

// ─── Shared helpers ──────────────────────────────────────────

fn build_reader() -> Result<FileAtlasReader, i32> {
    let indexes_dir = match resolve_indexes_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(1);
        }
    };
    Ok(FileAtlasReader::new(indexes_dir))
}

/// Resolve the indexes directory the same way other CLI commands do
/// (atlas-status, corpus-status). Honours `SOVEREIGN_DATA_DIR`, then
/// falls back to `~/.sovereign/indexes`.
fn resolve_indexes_dir() -> Result<PathBuf, String> {
    if let Ok(env) = std::env::var("SOVEREIGN_DATA_DIR") {
        return Ok(PathBuf::from(env).join("indexes"));
    }
    let home = std::env::var("HOME").map_err(|_| "$HOME unset".to_string())?;
    Ok(PathBuf::from(home).join(".sovereign").join("indexes"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

fn parse_format(args: &[String]) -> Format {
    match parse_flag(args, "--format").as_deref() {
        Some("json") => Format::Json,
        _ => Format::Text,
    }
}

fn parse_flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    args.iter().find_map(|a| {
        if let Some(stripped) = a.strip_prefix(&prefix) {
            Some(stripped.to_string())
        } else if a == name {
            // Support `--flag value` form too.
            let idx = args.iter().position(|x| x == name)?;
            args.get(idx + 1).cloned()
        } else {
            None
        }
    })
}

fn parse_atom_type(s: &str) -> Option<AtomType> {
    match s {
        "Entity" | "entity" => Some(AtomType::Entity),
        "Event" | "event" => Some(AtomType::Event),
        "State" | "state" => Some(AtomType::State),
        "Relation" | "relation" => Some(AtomType::Relation),
        "Claim" | "claim" => Some(AtomType::Claim),
        "Question" | "question" => Some(AtomType::Question),
        "Configuration" | "configuration" | "Config" | "config" => {
            Some(AtomType::Configuration)
        }
        "ArgumentReconstruction"
        | "argumentreconstruction"
        | "Argument"
        | "argument" => Some(AtomType::ArgumentReconstruction),
        _ => None,
    }
}

fn atom_type_short(t: &AtomType) -> &'static str {
    match t {
        AtomType::Entity => "Entity",
        AtomType::Event => "Event",
        AtomType::State => "State",
        AtomType::Relation => "Relation",
        AtomType::Claim => "Claim",
        AtomType::Question => "Question",
        AtomType::Configuration => "Configuration",
        AtomType::ArgumentReconstruction => "ArgumentRecon",
        AtomType::Position => "Position",
        AtomType::Opposition => "Opposition",
        AtomType::Asset => "Asset",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
