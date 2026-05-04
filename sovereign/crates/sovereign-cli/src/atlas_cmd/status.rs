//! `sovereign atlas status` — per-corpus atlas readiness display.
//!
//! Phase D1 — surfaces what `/internal/atlas/status` returns: atlas
//! atom + tier-2 counts, embed-cache presence, Tier-2 progress
//! (chapters done/total when a workspace exists), and token spend.
//! Reads on-disk state directly so it works without a running
//! daemon — useful when the daemon is restarting or the user is
//! diagnosing a stuck install.

use std::path::PathBuf;

use sovereign_tools::atlas_status::{compute_atlas_status, status_for_corpus};

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign atlas status",
    summary: "Show atlas readiness for every installed corpus (or just one).",
    sections: &[
        HelpSection::Usage(
            "sovereign atlas status [<corpus_id>] [--data-dir <path>] [--json]",
        ),
        HelpSection::Flags(&[
            (
                "--data-dir <path>",
                "Override the default ~/.sovereign data directory.",
            ),
            (
                "--json",
                "Emit the full payload as JSON (same shape as /internal/atlas/status).",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign atlas status",
                "Table of every installed corpus with atlas + Tier-2 + tokens columns.",
            ),
            (
                "sovereign atlas status wikipedia --json",
                "JSON for one corpus — useful for scripting / desktop polling.",
            ),
        ]),
        HelpSection::Notes(
            "Atlas readiness has four moving parts: structural atlas built (atoms.json), \
             Tier-2 enrichment (entities at depth=extracted), embed cache present \
             (atoms.embeddings.bin), and any in-flight `<corpus>-tier2` workspace. \
             A corpus needs the first three for atlas grounding to fire on chat turns.",
        ),
    ],
};

pub async fn run(args: &[String]) -> i32 {
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

    let data_dir = parsed.data_dir.unwrap_or_else(default_data_dir);
    let indexes_dir = data_dir.join("indexes");
    let enrichment_dir = data_dir.join("enrichment");

    let rows = match parsed.corpus_id.as_deref() {
        Some(cid) => match status_for_corpus(&indexes_dir, &enrichment_dir, cid) {
            Some(row) => vec![row],
            None => {
                eprintln!("no corpus '{cid}' under {}", indexes_dir.display());
                return 1;
            }
        },
        None => compute_atlas_status(&indexes_dir, &enrichment_dir),
    };

    if parsed.json {
        let body = serde_json::json!({"corpora": rows});
        println!("{}", serde_json::to_string_pretty(&body).unwrap());
        return 0;
    }

    if rows.is_empty() {
        println!("(no corpora installed at {})", indexes_dir.display());
        return 0;
    }

    println!(
        "{:<32} {:>10} {:>10} {:>11} {:>14} {:>12}",
        "corpus", "atlas", "tier-2", "embed-cache", "tier-2 prog", "tokens"
    );
    println!("{}", "─".repeat(95));
    for r in &rows {
        let atlas = r
            .atlas
            .as_ref()
            .map(|s| format_count(s.atom_count))
            .unwrap_or_else(|| "—".into());
        let tier2 = r
            .atlas
            .as_ref()
            .map(|s| format_count(s.tier2_count))
            .unwrap_or_else(|| "—".into());
        let cache = if r.embed_cache_present { "✓" } else { "—" };
        let prog = match &r.tier2_progress {
            Some(p) if p.chapters_total > 0 => {
                let pct = (p.chapters_done as f64) * 100.0 / (p.chapters_total as f64);
                format!("{}/{} ({:.0}%)", p.chapters_done, p.chapters_total, pct)
            }
            _ => "—".into(),
        };
        let tokens = r
            .tier2_tokens
            .as_ref()
            .map(|t| format_count(t.total_tokens))
            .unwrap_or_else(|| "—".into());
        println!(
            "{:<32} {:>10} {:>10} {:>11} {:>14} {:>12}",
            r.corpus_id, atlas, tier2, cache, prog, tokens
        );
    }
    0
}

fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".sovereign")
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[derive(Debug)]
struct Parsed {
    corpus_id: Option<String>,
    data_dir: Option<PathBuf>,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut corpus_id: Option<String> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--data-dir requires a path".to_string())?;
                data_dir = Some(PathBuf::from(v));
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    Ok(Parsed {
        corpus_id,
        data_dir,
        json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_args() {
        let p = parse_args(&[]).unwrap();
        assert!(p.corpus_id.is_none());
        assert!(!p.json);
    }

    #[test]
    fn parse_with_corpus_and_json() {
        let p = parse_args(&["wikipedia".into(), "--json".into()]).unwrap();
        assert_eq!(p.corpus_id.as_deref(), Some("wikipedia"));
        assert!(p.json);
    }

    #[test]
    fn parse_unknown_flag() {
        assert!(parse_args(&["--bogus".into()]).is_err());
    }
}
