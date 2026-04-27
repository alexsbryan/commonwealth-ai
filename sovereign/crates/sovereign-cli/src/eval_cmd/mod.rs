//! `sovereign eval ...` — measure retrieval quality against a question
//! bank.
//!
//! The eval command is the measurement substrate for everything we
//! tune in the corpus pipeline: filter scope, chunker config,
//! embedding model, retrieval limits, eventually atlas-seeded
//! re-ranking. Run it once to baseline, change one knob, run it again,
//! diff. Without this loop, "did the change help?" is a vibes call
//! against whatever question the developer typed last.
//!
//! The runner reuses `chat_cmd::bootstrap::ChatSession` so it talks to
//! the same daemon your desktop app uses. That keeps eval honest:
//! whatever model is loaded for chat is what the bank measures.
//!
//! Subcommands:
//!   - `run`  — execute a bank, print results, optionally write JSON
//!   - `diff` — compare two run-output JSON files (planned; not in v1)
//!
//! v1 is retrieval-only — it does NOT call `/v1/chat/completions`. The
//! question is "did the right chunks come back?" not "did the model
//! say the right thing about them." Synthesis judgment can layer on
//! later behind `--synth`; today it would just couple eval cost to
//! whatever 27B model the daemon happens to have loaded.

pub mod bank;
pub mod report;
pub mod runner;
pub mod score;

use std::path::PathBuf;

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign eval",
    summary: "Run a question bank against a corpus; measure retrieval quality.",
    sections: &[
        HelpSection::Usage("sovereign eval <subcommand> [args]"),
        HelpSection::Subcommands(&[
            ("run", "Execute a bank and print per-question + rollup scores."),
        ]),
        HelpSection::Notes(
            "Operates against the running daemon at localhost:9741 (override with \
             --daemon). Retrieval-only — does not call the chat model. The bank \
             format lives at sovereign-recipes/<corpus>/eval/*.toml.",
        ),
    ],
};

const RUN_HELP: Help = Help {
    command: "sovereign eval run",
    summary: "Run a question bank, print per-question results + category rollup.",
    sections: &[
        HelpSection::Usage(
            "sovereign eval run --bank <path> [--limit N] [--inspect] [--format text|json] [--output <path>]",
        ),
        HelpSection::Flags(&[
            ("--bank <path>",  "Path to the bank TOML (e.g. sovereign-recipes/wikipedia/eval/wikipedia_questions.toml)."),
            ("--limit <N>",    "Top-N chunks to retrieve per question (default: 10)."),
            ("--inspect",      "Print missing facts/sources + top retrieved chunks per question."),
            ("--format text|json", "Stdout format (default: text)."),
            ("--output <path>", "Also write the full run as pretty JSON to this path."),
            ("--help, -h",     "Show this message."),
        ]),
        HelpSection::Notes(
            "All `chat` global flags also apply: --daemon, --data-dir, --chat-model, \
             --embed-model. The bank's `corpus` field MUST match an installed \
             corpus_id; install it first via `sovereign corpus install <id>`.",
        ),
    ],
};

pub async fn run_eval(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    let first = args[0].as_str();
    if first == "--help" || first == "-h" || first == "help" {
        help::print(&HELP);
        return 0;
    }
    match first {
        "run" => cmd_run(&args[1..]).await,
        other => {
            eprintln!("error: unknown subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

#[derive(Debug, Clone)]
enum OutputFormat {
    Text,
    Json,
}

struct RunArgs {
    bank: PathBuf,
    limit: usize,
    inspect: bool,
    format: OutputFormat,
    output: Option<PathBuf>,
}

impl Default for RunArgs {
    fn default() -> Self {
        Self {
            bank: PathBuf::new(),
            limit: 10,
            inspect: false,
            format: OutputFormat::Text,
            output: None,
        }
    }
}

async fn cmd_run(args: &[String]) -> i32 {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        help::print(&RUN_HELP);
        return 0;
    }

    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let mut a = RunArgs::default();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--bank" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --bank needs a value");
                    return 2;
                };
                a.bank = PathBuf::from(v);
            }
            "--limit" => {
                i += 1;
                a.limit = rest
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(a.limit);
            }
            "--inspect" => {
                a.inspect = true;
            }
            "--format" => {
                i += 1;
                match rest.get(i).map(String::as_str) {
                    Some("text") => a.format = OutputFormat::Text,
                    Some("json") => a.format = OutputFormat::Json,
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
            "--output" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("error: --output needs a value");
                    return 2;
                };
                a.output = Some(PathBuf::from(v));
            }
            extra => {
                eprintln!("error: unexpected argument `{extra}`");
                return 2;
            }
        }
        i += 1;
    }

    if a.bank.as_os_str().is_empty() {
        eprintln!("error: --bank is required");
        eprintln!("hint: try sovereign-recipes/wikipedia/eval/wikipedia_questions.toml");
        return 2;
    }

    let bank = match bank::load_bank(&a.bank) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: load bank: {e}");
            return 1;
        }
    };
    eprintln!(
        "loaded bank `{}` — {} questions, target corpus `{}`",
        bank.bank.name,
        bank.questions.len(),
        bank.bank.corpus,
    );

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    let run = match runner::run_bank(&session, &bank, a.limit).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    match a.format {
        OutputFormat::Text => report::print_text(&run, a.inspect),
        OutputFormat::Json => {
            if let Err(e) = report::print_json(&run) {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }

    if let Some(path) = a.output.as_deref() {
        if let Err(e) = report::write_json_file(&run, path) {
            eprintln!("error: write output: {e}");
            return 1;
        }
        eprintln!("wrote run JSON to {}", path.display());
    }

    0
}
