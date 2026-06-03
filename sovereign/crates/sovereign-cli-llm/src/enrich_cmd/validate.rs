//! `sovereign enrich validate <corpus> --questions <path>` — runs a
//! QueryBattery against the corpus's atlas and prints a score table.
//! Does NOT ask the model to generate answers; this validates the
//! atlas + traversal only.

use std::path::PathBuf;

use corpus_engine::enrichment::pipeline::{run_battery, Atlas, QueryBattery};

use super::config::EnrichConfig;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich validate",
    summary: "Run a QueryBattery against the corpus atlas and print a score table.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich validate <corpus-id> --questions <path> [--threshold <f>] [--pass <f>]",
        ),
        HelpSection::Flags(&[
            ("--questions <path>", "JSON file (bare array of strings, or `{\"questions\":[…]}`)."),
            ("--threshold <f>", "Cosine threshold for LOCATE inclusion (default 0.5)."),
            ("--pass <f>", "Pass threshold for the top-match score (default 0.7). Rows below count as misses."),
        ]),
    ],
};

pub async fn cmd_validate(args: &[String]) -> i32 {
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
        eprintln!(
            "error: daemon is not responding at {} — start it first",
            cfg.base_url
        );
        return 2;
    }

    let battery = match QueryBattery::load(&parsed.questions_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: loading battery: {e}");
            return 1;
        }
    };
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

    let res = match run_battery(&battery, &atlas, &embed, parsed.threshold).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: running battery: {e}");
            return 1;
        }
    };

    // Print the table.
    println!(
        "{:>4} | {:<60} | {:>6} | {:>3} | {:>4} | {:>3}",
        "#", "Question", "Top", "Pos", "Tens", "Psg"
    );
    println!("{}", "-".repeat(100));
    for (i, row) in res.rows.iter().enumerate() {
        let q: String = row.question.chars().take(58).collect();
        let pass_marker = if row.top_match_similarity >= parsed.pass {
            " "
        } else {
            "!"
        };
        println!(
            "{:>3}{} | {:<60} | {:>6.2} | {:>3} | {:>4} | {:>3}",
            i + 1,
            pass_marker,
            q,
            row.top_match_similarity,
            row.positions,
            row.tensions,
            row.grounding_passages
        );
    }
    let rate = res.pass_rate(parsed.pass);
    let passed = (rate * res.rows.len() as f32).round() as usize;
    println!();
    println!(
        "  Passed: {}/{} ({:.0}%) at threshold {:.2}",
        passed,
        res.rows.len(),
        rate * 100.0,
        parsed.pass
    );
    if passed < res.rows.len() {
        return 1;
    }
    0
}

#[derive(Debug)]
struct ParsedValidate {
    corpus_id: String,
    questions_path: PathBuf,
    threshold: f32,
    pass: f32,
}

fn parse_args(args: &[String]) -> Result<ParsedValidate, String> {
    let mut corpus_id: Option<String> = None;
    let mut questions_path: Option<PathBuf> = None;
    let mut threshold = 0.5f32;
    let mut pass = 0.7f32;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--questions" => {
                questions_path = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or("--questions requires a path".to_string())?,
                ));
                i += 2;
            }
            "--threshold" => {
                threshold = args
                    .get(i + 1)
                    .ok_or("--threshold requires a value".to_string())?
                    .parse()
                    .map_err(|e| format!("--threshold: {e}"))?;
                i += 2;
            }
            "--pass" => {
                pass = args
                    .get(i + 1)
                    .ok_or("--pass requires a value".to_string())?
                    .parse()
                    .map_err(|e| format!("--pass: {e}"))?;
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional: {other}"));
                }
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let questions_path = questions_path.ok_or_else(|| "missing --questions <path>".to_string())?;
    Ok(ParsedValidate {
        corpus_id,
        questions_path,
        threshold,
        pass,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_validate_minimum() {
        let args = vec!["ak".into(), "--questions".into(), "/tmp/q.json".into()];
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "ak");
        assert_eq!(p.questions_path, PathBuf::from("/tmp/q.json"));
        assert!((p.threshold - 0.5).abs() < 0.001);
        assert!((p.pass - 0.7).abs() < 0.001);
    }

    #[test]
    fn parse_validate_requires_questions() {
        let err = parse_args(&["ak".into()]).unwrap_err();
        assert!(err.contains("--questions"));
    }

    #[test]
    fn parse_validate_custom_thresholds() {
        let args = [
            "ak",
            "--questions",
            "/x.json",
            "--threshold",
            "0.4",
            "--pass",
            "0.85",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
        let p = parse_args(&args).unwrap();
        assert!((p.threshold - 0.4).abs() < 0.001);
        assert!((p.pass - 0.85).abs() < 0.001);
    }
}
