// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn awareness` — development glassbox CLI for the
//! relational + strategic awareness pipeline.
//!
//! Every subcommand answers one developer question: what's in the
//! atlas, what does extraction produce, what would the digest look
//! like, what would the model suggest, how does decay reshape memory
//! over time, how does extraction quality compare to a golden set.
//!
//! The CLI is a thin orchestration layer — it reuses the same
//! StateStore, NoteStore, FeatureStore, and atlas writer the
//! production pipeline does. No mock storage; the developer runs
//! against a real (but development) `~/.svrnmesh/` instance.
//!
//! ## Module layout
//!
//! Mirrors the `atos_cmd/` layout:
//!
//! - [`args`] — flag parsing helpers (mirrors `atos_cmd/args.rs`)
//! - [`store_open`] — `.sovereign/` resolver + store openers
//! - [`render`] — shared output formatting (status symbols, `--json`
//!   toggle pattern from `enrich_cmd/errors.rs`)
//! - [`entities`] — `awareness entities` — list extracted entities
//! - [`timeline`] — `awareness timeline <name>` — interaction history
//! - [`reset`] — `awareness reset` — clear entity enrichment data
//!
//! Phase 1 ships the three read-only subcommands plus reset. Phases
//! 2–4 add seed/extract/digest/suggest/trace/decay/eval/scenario.

// ─── Feature gating ──────────────────────────────────────────────────
//
// The gate lives in `main.rs` (`#[cfg(feature = "awareness")] mod
// awareness_cmd;`) and, for the heavy submodules, here. It does NOT
// live as an inner `#![cfg]` on this module: it did until 2026-08-21,
// spelled `dev-tools`, while `main.rs` declared the module under
// `awareness` — so `--features awareness` alone configured the entire
// module out and the build failed with E0433 at the dispatch site. Two
// gates on one module, disagreeing, is how a feature nothing compiles
// stays that way.
//
// `args` is deliberately NOT gated. It is data plus
// `sovereign_cli_shared::args::parse` — none of the knowledge-view
// surface the rest of this module needs. Gated with its readers, its
// tests never ran in ANY build (nc-22b reported three of them
// never-ran, correctly). Ungated, they run in every `sovereign-cli`
// test run.
pub(crate) mod args;

#[cfg(feature = "awareness")]
mod decay;
#[cfg(feature = "awareness")]
mod digest;
#[cfg(feature = "awareness")]
mod entities;
#[cfg(feature = "awareness")]
mod eval;
#[cfg(feature = "awareness")]
mod extract;
#[cfg(feature = "awareness")]
mod filter;
#[cfg(feature = "awareness")]
mod golden;
#[cfg(feature = "awareness")]
mod inference;
#[cfg(feature = "awareness")]
mod render;
#[cfg(feature = "awareness")]
mod reset;
#[cfg(feature = "awareness")]
mod scenario;
#[cfg(feature = "awareness")]
mod seed;
#[cfg(feature = "awareness")]
mod store_open;
#[cfg(feature = "awareness")]
mod suggest;
#[cfg(feature = "awareness")]
mod templates;
#[cfg(feature = "awareness")]
mod timeline;
#[cfg(feature = "awareness")]
mod trace;

#[cfg(feature = "awareness")]
pub async fn run_awareness(args: &[String]) -> i32 {
    let Some(first) = args.first() else {
        print_help();
        return 1;
    };

    if matches!(first.as_str(), "--help" | "-h" | "help") {
        print_help();
        return 0;
    }

    let rest = &args[1..];
    match first.as_str() {
        "entities" => entities::cmd_entities(rest).await,
        "timeline" => timeline::cmd_timeline(rest).await,
        "reset" => reset::cmd_reset(rest).await,
        "seed" => seed::cmd_seed(rest).await,
        "extract" => extract::cmd_extract(rest).await,
        "digest" => digest::cmd_digest(rest).await,
        "suggest" => suggest::cmd_suggest(rest).await,
        "trace" => trace::cmd_trace(rest).await,
        "decay" => decay::cmd_decay(rest).await,
        "eval" => eval::cmd_eval(rest).await,
        "scenario" => scenario::cmd_scenario(rest).await,
        "filter" => filter::cmd_filter(rest).await,
        other => {
            eprintln!("awareness: unknown subcommand '{other}'");
            print_help();
            2
        }
    }
}

#[cfg(feature = "awareness")]
fn print_help() {
    eprintln!(
        "svrn awareness — development glassbox CLI for the\n\
         relational + strategic awareness pipeline.\n\
         \n\
         USAGE\n    sovereign awareness <subcommand> [flags]\n\
         \n\
         SUBCOMMANDS (Phases 1–4)\n\
         \x20   entities                       List extracted entities + provenance\n\
         \x20       [--kind person|organization|initiative|all]\n\
         \x20       [--sort recency|frequency|name]\n\
         \x20       [--json]\n\
         \x20   timeline <entity-name>         Show interaction timeline for an entity\n\
         \x20       [--window 90]\n\
         \x20       [--include-chunks]\n\
         \x20   reset                          Clear entity enrichment data (asks for confirmation)\n\
         \x20       [--entities-only | --full]\n\
         \x20   seed --from-template <name>    Inject synthetic conversation history\n\
         \x20       --from-file <path>         Load TOML scenario from a file\n\
         \x20       [--days N] [--dry-run]\n\
         \x20   extract                        Run entity extraction over the StateStore\n\
         \x20       [--phase entity|all] [--limit N]\n\
         \x20       [--mock | --dry-run | (default: real model)]\n\
         \x20       [--verbose]\n\
         \x20   digest                         Render the relational + strategic digest blocks\n\
         \x20       [--context \"<text>\"] [--budget relational=N,strategic=M]\n\
         \x20   suggest <conversation-id>      Replay turns; show what suggest_note would fire\n\
         \x20       [--all-turns] [--verbose] [--mock | --dry-run]\n\
         \x20   trace <entity-name>            Per-entity decision walk\n\
         \x20   decay                          Simulate uniform vs entity-aware memory decay\n\
         \x20       [--months N] [--rate F] [--threshold F] [--show-entity-linked]\n\
         \x20   eval                           Score current atlas against a golden set\n\
         \x20       [--from-template <name> | --golden <path-to-jsonl>]\n\
         \x20       [--report <out-path>] [--json]\n\
         \x20   scenario <path-to-toml>        Run a scripted end-to-end scenario\n\
         \x20       [--output <dir>]\n\
         \x20   filter                         Second-pass model filter for initiative atlas\n\
         \x20       [--verbose] [--dry-run]\n\
         \n\
         GLOBAL FLAGS\n\
         \x20   --db-path <path>               Override .sovereign/ root (default: ~/.svrnmesh)\n\
         \x20   --help, -h                     Show this message.\n\
         \n\
         This CLI is built only with `cargo build --features dev-tools`.\n\
         It is not user-facing and does not appear in the production binary.\n"
    );
}
