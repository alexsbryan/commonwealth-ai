//! `sovereign atlas budget` — show or set the per-corpus Tier-2
//! triage budget.
//!
//! The budget caps how many top-priority articles the post-install
//! triage step picks for the deep-enrichment queue. Default is
//! [`DEFAULT_TIER2_BUDGET`] (1,000 articles). Operators bump it
//! upward when they have disk + token budget for a deeper Tier-2
//! pass (e.g. 5,000 on a 200 GB partition); downward to keep
//! enrichment lean on storage-constrained nodes.
//!
//! Persistence: writes `<corpus>/atlas/triage-config.json` so the
//! budget travels with the atlas — peers pulling the atlas via mesh
//! transfer inherit the same cap, and the daemon's resume scan
//! honours it on next boot.

use std::path::PathBuf;

use sovereign_tools::atlas_postinstall::{
    read_triage_budget, write_triage_budget, DEFAULT_TIER2_BUDGET,
};

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign atlas budget",
    summary:
        "Show or set the Tier-2 enrichment budget (top-N articles) for a corpus's atlas.",
    sections: &[
        HelpSection::Usage("sovereign atlas budget <corpus_id> [<count>] [--data-dir <path>]"),
        HelpSection::Flags(&[
            (
                "--data-dir <path>",
                "Override the default ~/.sovereign data directory.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign atlas budget wikipedia",
                "Print the current budget (or default if no override is set).",
            ),
            (
                "sovereign atlas budget wikipedia 5000",
                "Set the Tier-2 budget for `wikipedia` to 5,000 articles. Takes \
                 effect on the next post-install triage rebuild.",
            ),
        ]),
        HelpSection::Notes(
            "The override lives at `<data-dir>/indexes/<corpus_id>/atlas/triage-config.json`. \
             Delete the file (or pass `--unset`) to revert to the default. The default is \
             1,000 articles — calibrated to fit Vital Articles L1+L2+L3 with headroom on \
             a wikipedia-scale atlas.",
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

    let data_dir = parsed
        .data_dir
        .unwrap_or_else(|| default_data_dir());
    let atlas_dir = data_dir
        .join("indexes")
        .join(&parsed.corpus_id)
        .join("atlas");

    if parsed.unset {
        let path = atlas_dir.join(sovereign_tools::atlas_postinstall::TRIAGE_CONFIG_FILE);
        match std::fs::remove_file(&path) {
            Ok(_) => {
                println!(
                    "Removed override at {} — budget reverts to default ({} articles).",
                    path.display(),
                    DEFAULT_TIER2_BUDGET
                );
                0
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!(
                    "No override set for `{}` — budget already at default ({} articles).",
                    parsed.corpus_id, DEFAULT_TIER2_BUDGET
                );
                0
            }
            Err(e) => {
                eprintln!("error: removing {}: {e}", path.display());
                1
            }
        }
    } else if let Some(n) = parsed.set_to {
        if !atlas_dir.exists() {
            // Allow setting before atlas exists — the post-install
            // hook will create the atlas dir and pick this up.
            if let Err(e) = std::fs::create_dir_all(&atlas_dir) {
                eprintln!(
                    "error: creating {} (atlas not yet built): {e}",
                    atlas_dir.display()
                );
                return 1;
            }
        }
        match write_triage_budget(&atlas_dir, n) {
            Ok(()) => {
                println!(
                    "Set Tier-2 budget for `{}` to {} articles.",
                    parsed.corpus_id, n
                );
                println!(
                    "Takes effect on the next post-install triage rebuild \
                     (`sovereign corpus install {}` or daemon resume).",
                    parsed.corpus_id
                );
                0
            }
            Err(e) => {
                eprintln!("error: writing triage-config.json: {e}");
                1
            }
        }
    } else {
        // Just show current.
        let current = read_triage_budget(&atlas_dir);
        match current {
            Some(n) => println!(
                "{}: Tier-2 budget = {} articles (override active).",
                parsed.corpus_id, n
            ),
            None => println!(
                "{}: Tier-2 budget = {} articles (default — no override).",
                parsed.corpus_id, DEFAULT_TIER2_BUDGET
            ),
        }
        0
    }
}

fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".sovereign")
}

#[derive(Debug)]
struct Parsed {
    corpus_id: String,
    set_to: Option<usize>,
    unset: bool,
    data_dir: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut corpus_id: Option<String> = None;
    let mut set_to: Option<usize> = None;
    let mut unset = false;
    let mut data_dir: Option<PathBuf> = None;

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
            "--unset" => {
                unset = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if set_to.is_none() {
                    let n: usize = other
                        .parse()
                        .map_err(|e| format!("count must be a positive integer: {e}"))?;
                    if n == 0 {
                        return Err("count must be > 0".into());
                    }
                    set_to = Some(n);
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                i += 1;
            }
        }
    }

    let corpus_id = corpus_id.ok_or("missing <corpus_id>".to_string())?;
    if unset && set_to.is_some() {
        return Err("--unset and <count> are mutually exclusive".into());
    }
    Ok(Parsed {
        corpus_id,
        set_to,
        unset,
        data_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_show_only() {
        let p = parse_args(&["wikipedia".into()]).unwrap();
        assert_eq!(p.corpus_id, "wikipedia");
        assert!(p.set_to.is_none());
        assert!(!p.unset);
    }

    #[test]
    fn parse_set_value() {
        let p = parse_args(&["wikipedia".into(), "5000".into()]).unwrap();
        assert_eq!(p.set_to, Some(5000));
    }

    #[test]
    fn parse_unset_flag() {
        let p = parse_args(&["wikipedia".into(), "--unset".into()]).unwrap();
        assert!(p.unset);
    }

    #[test]
    fn parse_rejects_zero() {
        let err = parse_args(&["wikipedia".into(), "0".into()]).unwrap_err();
        assert!(err.contains("> 0"));
    }

    #[test]
    fn parse_rejects_unset_with_value() {
        let err = parse_args(&["wikipedia".into(), "100".into(), "--unset".into()]).unwrap_err();
        assert!(err.contains("mutually exclusive"));
    }
}
