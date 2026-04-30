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
//! By default the runner is retrieval-only — it does NOT call
//! `/v1/chat/completions`. That keeps the cheap baseline cheap and
//! isolates retrieval-tuning experiments from chat-model variance.
//! Pass `--synth` to drive each question through the full
//! `Runtime::handle_message_stream` path the desktop chat surface uses
//! (intent classifier → router → search tools → prompt assembly →
//! chat completion); facts are then matched against the synthesised
//! answer rather than the retrieved chunks. Synth mode exercises the
//! routing and aggregation layers, which are tunable in their own
//! right.

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
            "sovereign eval run --bank <path> [--synth] [--limit N] [--inspect] [--format text|json] [--output <path>]",
        ),
        HelpSection::Flags(&[
            ("--bank <path>",  "Path to the bank TOML (e.g. sovereign-recipes/wikipedia/eval/wikipedia_questions.toml)."),
            ("--synth",        "Drive each question through the full chat pipeline (routing → retrieval → synthesis). Slower, but exercises the model + routing layers."),
            ("--routing-only", "Call the classifier per question and score the routing decision against `expected_intent` (or category default). Skips retrieval and synthesis — fast iteration loop for tuning the classifier prompt."),
            ("--limit <N>",    "Top-N chunks to retrieve per question (retrieval mode only; default: 10)."),
            ("--inspect",      "Print missing facts/sources + top retrieved chunks per question."),
            ("--format text|json", "Stdout format (default: text)."),
            ("--output <path>", "Also write the full run as pretty JSON to this path."),
            ("--help, -h",     "Show this message."),
        ]),
        HelpSection::Notes(
            "All `chat` global flags also apply: --daemon, --data-dir, --chat-model, \
             --embed-model, --temperature, --max-tokens. The bank's `corpus` field \
             MUST match an installed corpus_id; install it first via \
             `sovereign corpus install <id>`. Under --synth, --chat-model selects the \
             model that will do synthesis; --max-tokens lets you sweep the \
             latency/coverage tradeoff (lower = faster wall, terser answer) without \
             touching the operator's product config.",
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
    synth: bool,
    routing_only: bool,
    format: OutputFormat,
    output: Option<PathBuf>,
}

impl Default for RunArgs {
    fn default() -> Self {
        Self {
            bank: PathBuf::new(),
            limit: 10,
            inspect: false,
            synth: false,
            routing_only: false,
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

    let (mut globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    // Eval is a rule-following measurement, not a free-form chat —
    // sampling variance from a non-zero temperature shows up as
    // run-to-run noise on the bank metrics, swamping the signal we
    // care about (did the routing/retrieval/synthesis change actually
    // move the score?). Default to temperature 0 unless the operator
    // passed `--temperature` explicitly. Same logic applies to
    // retrieval-only mode (route classifier + Fast-slot calls in any
    // path the runtime takes) so we set it on `globals` regardless of
    // mode rather than only under `--synth`.
    if globals.temperature.is_none() {
        globals.temperature = Some(0.0);
    }

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
            "--synth" => {
                a.synth = true;
            }
            "--routing-only" => {
                a.routing_only = true;
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

    if a.routing_only {
        eprintln!(
            "routing-only mode — calling the classifier per question, scoring against \
             expected_intent (or category default). No retrieval, no synthesis. \
             Useful for tuning the classifier prompt against a specific fast-slot model."
        );
        let run = match runner::run_bank_routing(&session, &bank).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        report::print_routing(&run);
        if let Some(path) = a.output.as_deref() {
            if let Err(e) = report::write_routing_json_file(&run, path) {
                eprintln!("error: write output: {e}");
                return 1;
            }
            eprintln!("wrote routing JSON to {}", path.display());
        }
        return 0;
    }

    let run = if a.synth {
        eprintln!(
            "synth mode — driving full chat pipeline. This will take ~one chat-completion \
             per question; sit tight."
        );
        match runner::run_bank_synth(&session, &bank).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        match runner::run_bank(&session, &bank, a.limit).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    };

    match a.format {
        OutputFormat::Text => report::print_text(&run, a.inspect, Some(&bank)),
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
