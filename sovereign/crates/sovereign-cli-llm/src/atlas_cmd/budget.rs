// SPDX-License-Identifier: AGPL-3.0-or-later
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
    read_triage_budget, read_triage_config, write_triage_budget, write_triage_config,
    DEFAULT_EXPANSION_FRACTION, DEFAULT_EXPANSION_HOPS, DEFAULT_TIER2_BUDGET,
};

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign atlas budget",
    summary: "Show or set the Tier-2 enrichment budget + expansion knobs for a corpus's atlas.",
    sections: &[
        HelpSection::Usage(
            "sovereign atlas budget <corpus_id> [<count>] [--expansion-fraction <0..0.9>] \
             [--expansion-hops <1|2>] [--data-dir <path>] [--unset]",
        ),
        HelpSection::Flags(&[
            (
                "--expansion-fraction <0..0.9>",
                "Share of the budget reserved for seed-expansion picks (1-hop \
                 outbound wikilinks from the seed set). Default 0.3. Set to 0 to \
                 disable expansion and revert to the pre-expansion seed-only behaviour.",
            ),
            (
                "--expansion-hops <1|2>",
                "How many wikilink hops to walk outward from each seed. Default 1. \
                 2-hop captures grandchildren at half weight; rarely worth it on \
                 Wikipedia-scale graphs.",
            ),
            (
                "--data-dir <path>",
                "Override the default ~/.sovereign data directory.",
            ),
            (
                "--unset",
                "Remove the entire override file — budget AND expansion knobs revert \
                 to defaults.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign atlas budget wikipedia",
                "Print the current budget + expansion config (or defaults if no override is set).",
            ),
            (
                "sovereign atlas budget wikipedia 5000",
                "Set the Tier-2 budget for `wikipedia` to 5,000 articles. \
                 Existing expansion knobs are preserved.",
            ),
            (
                "sovereign atlas budget wikipedia 1000 --expansion-fraction 0.4",
                "1000 total picks, 40% (400) reserved for seed-expansion candidates.",
            ),
            (
                "sovereign atlas budget wikipedia 1000 --expansion-fraction 0",
                "Disable expansion entirely — full 1000 budget goes to vital + centrality seeds.",
            ),
        ]),
        HelpSection::Notes(
            "The override lives at `<data-dir>/indexes/<corpus_id>/atlas/triage-config.json`. \
             Two-phase triage: pick seeds by (Vital Articles tier × centrality × bumps) up \
             to (1 - expansion_fraction) * budget, then expand 1-hop outbound through \
             wikilinks and rank candidates by hits-from-seeds + tier + centrality. The \
             default 30% expansion fraction captures connective-tissue articles (Einstein \
             → Bohr, photoelectric effect) without crowding out the vital roster.",
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
    let atlas_dir = data_dir
        .join("indexes")
        .join(&parsed.corpus_id)
        .join("atlas");

    if parsed.unset {
        let path = atlas_dir.join(sovereign_tools::atlas_postinstall::TRIAGE_CONFIG_FILE);
        match std::fs::remove_file(&path) {
            Ok(_) => {
                println!(
                    "Removed override at {} — all knobs revert to defaults \
                     (budget={}, expansion_fraction={}, expansion_hops={}).",
                    path.display(),
                    DEFAULT_TIER2_BUDGET,
                    DEFAULT_EXPANSION_FRACTION,
                    DEFAULT_EXPANSION_HOPS,
                );
                0
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!(
                    "No override set for `{}` — already at defaults.",
                    parsed.corpus_id
                );
                0
            }
            Err(e) => {
                eprintln!("error: removing {}: {e}", path.display());
                1
            }
        }
    } else if parsed.set_to.is_some()
        || parsed.expansion_fraction.is_some()
        || parsed.expansion_hops.is_some()
    {
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
        // Use write_triage_config when expansion knobs are touched
        // so all three fields land in a single atomic write.
        let result = if parsed.expansion_fraction.is_some() || parsed.expansion_hops.is_some() {
            write_triage_config(
                &atlas_dir,
                parsed.set_to,
                parsed.expansion_fraction,
                parsed.expansion_hops,
            )
        } else {
            // Budget-only path keeps the legacy entry point for
            // back-compat — preserves any expansion knobs already
            // on disk.
            write_triage_budget(&atlas_dir, parsed.set_to.unwrap())
        };
        match result {
            Ok(()) => {
                let resolved = read_triage_config(&atlas_dir);
                println!(
                    "Set Tier-2 config for `{}`: budget={}, expansion_fraction={}, expansion_hops={}.",
                    parsed.corpus_id,
                    resolved.budget_articles,
                    resolved.expansion_fraction,
                    resolved.expansion_hops,
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
        let resolved = read_triage_config(&atlas_dir);
        let has_override = read_triage_budget(&atlas_dir).is_some();
        let suffix = if has_override {
            " (override active)"
        } else {
            " (defaults — no override)"
        };
        println!(
            "{}: budget={}, expansion_fraction={}, expansion_hops={}{}",
            parsed.corpus_id,
            resolved.budget_articles,
            resolved.expansion_fraction,
            resolved.expansion_hops,
            suffix,
        );
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
    expansion_fraction: Option<f64>,
    expansion_hops: Option<u32>,
    unset: bool,
    data_dir: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut corpus_id: Option<String> = None;
    let mut set_to: Option<usize> = None;
    let mut expansion_fraction: Option<f64> = None;
    let mut expansion_hops: Option<u32> = None;
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
            "--expansion-fraction" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--expansion-fraction requires a value".to_string())?;
                let f: f64 = v
                    .parse()
                    .map_err(|e| format!("--expansion-fraction must be a float: {e}"))?;
                if !(0.0..=0.9).contains(&f) {
                    return Err(format!(
                        "--expansion-fraction must be in [0.0, 0.9]; got {f}"
                    ));
                }
                expansion_fraction = Some(f);
                i += 2;
            }
            "--expansion-hops" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--expansion-hops requires a value".to_string())?;
                let h: u32 = v
                    .parse()
                    .map_err(|e| format!("--expansion-hops must be an integer: {e}"))?;
                if !(1..=2).contains(&h) {
                    return Err(format!("--expansion-hops must be 1 or 2; got {h}"));
                }
                expansion_hops = Some(h);
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
    if unset && (set_to.is_some() || expansion_fraction.is_some() || expansion_hops.is_some()) {
        return Err("--unset is mutually exclusive with set values".into());
    }
    Ok(Parsed {
        corpus_id,
        set_to,
        expansion_fraction,
        expansion_hops,
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

    #[test]
    fn parse_expansion_fraction_and_hops() {
        let p = parse_args(&[
            "wikipedia".into(),
            "1000".into(),
            "--expansion-fraction".into(),
            "0.4".into(),
            "--expansion-hops".into(),
            "2".into(),
        ])
        .unwrap();
        assert_eq!(p.set_to, Some(1000));
        assert_eq!(p.expansion_fraction, Some(0.4));
        assert_eq!(p.expansion_hops, Some(2));
    }

    #[test]
    fn parse_rejects_out_of_range_fraction() {
        let err = parse_args(&[
            "wikipedia".into(),
            "--expansion-fraction".into(),
            "1.5".into(),
        ])
        .unwrap_err();
        assert!(err.contains("[0.0, 0.9]"));
    }

    #[test]
    fn parse_rejects_invalid_hops() {
        let err =
            parse_args(&["wikipedia".into(), "--expansion-hops".into(), "5".into()]).unwrap_err();
        assert!(err.contains("1 or 2"));
    }

    #[test]
    fn parse_unset_alone_is_fine() {
        let p = parse_args(&["wikipedia".into(), "--unset".into()]).unwrap();
        assert!(p.unset);
        assert!(p.expansion_fraction.is_none());
    }
}
