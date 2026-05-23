//! `sovereign bench …` — throughput + correctness benchmarks for
//! enrichment-pipeline LLM tasks. Used to pick a primary model
//! before committing to a long batch (the SEP 1800-article ingest
//! is the motivating workload).
//!
//! Subcommands:
//!   - `atlas`    — atlas Phase 1 + short-call throughput against
//!                  the running daemon's currently-loaded primary
//!   - `obsidian` — atlas correctness score for an obsidian-vault
//!                  corpus against the in-repo fixture golden (or a
//!                  user-supplied vault + golden via `--corpus`/`--golden`)
//!
//! The bench hits the live daemon at `--base-url` (default
//! localhost:9741) so the model under test is whichever
//! `[models].primary` the daemon was started with. To compare
//! candidates: edit `~/.sovereign/config.toml`, restart
//! `sovereign.service`, run `sovereign bench atlas --output run.json`,
//! repeat. Results carry the daemon-reported `model_id` so
//! mislabelled archives can't sneak through.
//!
//! Why not auto-swap models inside the bench? Reloading a 27B GGUF
//! is a daemon-restart concern (slot lifecycle, mesh advertise,
//! systemd unit) and folding it into the bench would couple this
//! tool to those concerns. Keep it dumb: measure what's loaded.

mod all;
mod atlas;
mod baselines;
mod book_report;
mod discover;
mod obsidian;
mod render;
mod scaffold;

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench",
    summary: "Throughput + correctness benchmarks for enrichment LLM tasks.",
    sections: &[
        HelpSection::Usage("sovereign bench <subcommand> [args]"),
        HelpSection::Subcommands(&[
            (
                "all",
                "Discover every enrichment-eval + retrieval-judge bench, score each, diff vs baseline, exit 0/1.",
            ),
            (
                "atlas",
                "Run atlas Phase 1 + short-call tasks against the loaded primary model.",
            ),
            (
                "book-report",
                "Attach Conrad's The Secret Agent (Gutenberg #974) and time ingest + answer quality across 5 question tiers.",
            ),
            (
                "obsidian",
                "Score an obsidian-vault corpus against the in-repo fixture golden (correctness, not throughput).",
            ),
            (
                "scaffold",
                "Draft a golden TOML from an existing resolved atlas — sample atoms per axis, emit reviewable starting point.",
            ),
        ]),
        HelpSection::Notes(
            "Operates against the running daemon at localhost:9741. The model under \
             test is whichever `[models].primary` the daemon was started with — \
             change models by editing config.toml + restarting the service.",
        ),
    ],
};

pub async fn run_bench(args: &[String]) -> i32 {
    // Top-level help fires when no args or the first arg itself is
    // a help token. Subcommand-level `--help` (e.g. `bench atlas
    // --help`) passes through to the subcommand so each verb can
    // document its own flags. Mirrors `enrich_cmd::run_enrich`.
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
        "all" => all::cmd_all(&args[1..]).await,
        "atlas" => atlas::cmd_atlas(&args[1..]).await,
        "book-report" => book_report::cmd_book_report(&args[1..]).await,
        "obsidian" => obsidian::cmd_obsidian(&args[1..]).await,
        "scaffold" => scaffold::cmd_scaffold(&args[1..]).await,
        other => {
            eprintln!("error: unknown bench subcommand `{other}`");
            eprintln!();
            help::print(&HELP);
            2
        }
    }
}



