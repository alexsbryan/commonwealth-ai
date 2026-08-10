// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench …` — throughput + correctness benchmarks for
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
//! candidates: edit `~/.svrnmesh/config.toml`, restart
//! `sovereign.service`, run `svrn bench atlas --output run.json`,
//! repeat. Results carry the daemon-reported `model_id` so
//! mislabelled archives can't sneak through.
//!
//! Why not auto-swap models inside the bench? Reloading a 27B GGUF
//! is a daemon-restart concern (slot lifecycle, mesh advertise,
//! systemd unit) and folding it into the bench would couple this
//! tool to those concerns. Keep it dumb: measure what's loaded.

mod ablate;
mod adjudicate;
mod all;
mod atlas;
// Crate-visible: `router fit` records its calibration snapshots under
// the same dated-JSON + `latest.json` convention rather than inventing
// a second on-disk layout for the same job.
pub(crate) mod baselines;
mod book_report;
mod chaos_monkey;
mod desktop_bridge;
mod discover;
mod enron;
mod faithfulness;
mod flywheel;
mod gate;
mod governance;
mod lane_baseline;
mod live_runner;
mod mechanism_fidelity;
mod model_resolve;
mod moral;
mod obsidian;
mod parity_compare;
mod promote;
mod proxy_bench;
mod redteam;
mod render;
mod report;
mod resource_meter;
mod routing_replay;
/// Shared per-criterion rubric apparatus (judge · calibration gate ·
/// Wilson-CI scoring + diff). `moral` is its first tenant; the
/// situatedness lane (SITUATED_FLYWHEEL.md) is the second.
pub mod rubric;
mod scaffold;
mod scaffolding_param;
mod situated;
mod uap;
mod vault_report;
mod verifier;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench",
    summary: "Throughput + correctness benchmarks for enrichment LLM tasks.",
    sections: &[
        HelpSection::Usage("svrn bench <subcommand> [args]"),
        HelpSection::Subcommands(&[
            (
                "all",
                "Discover every enrichment-eval + retrieval-judge bench, score each, diff vs baseline, exit 0/1.",
            ),
            (
                "gate",
                "Baseline-relative CI gate for the absolute-verdict lanes (chaos-monkey / mechanism-fidelity / multiturn): re-score a lane's artifact, diff vs a committed baseline, exit 0/1.",
            ),
            (
                "report",
                "Roll the per-lane baselines up into durable, model-attributed reliability reports (reports/<model>/{reliability.json,REPORT.md} + index.json).",
            ),
            (
                "atlas",
                "Run atlas Phase 1 + short-call tasks against the loaded primary model.",
            ),
            (
                "enron",
                "Phase 5 measurement loop for the architecture-over-Enron substrate.",
            ),
            (
                "mechanism-fidelity",
                "Metamorphic audit of whether an agent's wealth-tax relocation decisions track the causal mechanism (P1 collapse / P2 saturation / I1 invariance) vs the label, with a feature-stripped negative control.",
            ),
            (
                "flywheel",
                "Fidelity-Flywheel read side: generate probes from a corpus (I1), run them through the live chat path, verify groundedness/abstention against the witness, capture failures as regression cases.",
            ),
            (
                "promote",
                "Fidelity-Flywheel write side: propose a retrieval scaffolding change, gate it on a held-out pool via paired baseline/candidate arms, apply it on a pass (atoms-decoupled, in-process).",
            ),
            (
                "book-report",
                "Attach Conrad's The Secret Agent (Gutenberg #974) and time ingest + answer quality across 5 question tiers.",
            ),
            (
                "vault-report",
                "Raw folder -> queryable-enriched corpus, timed end to end: time_to_rag_ready + time_to_enriched, per-phase spans, per-note accounting, token ledger.",
            ),
            (
                "moral",
                "Process-focused moral-reasoning lane (MoReBench-derived): per-criterion rubric judging of HOW a model reasons about dilemmas, per-dimension fulfillment, judge-pinned A/B via --report/--diff, judge calibration via --calibrate.",
            ),
            (
                "obsidian",
                "Score an obsidian-vault corpus against the in-repo fixture golden (correctness, not throughput).",
            ),
            (
                "parity-compare",
                "Enrichment-parity gate: run each (corpus, question) through the bench AND desktop-bridge paths, diff the enrichment legs each surfaces, fail when desktop ⊊ bench.",
            ),
            (
                "scaffold",
                "Draft a golden TOML from an existing resolved atlas — sample atoms per axis, emit reviewable starting point.",
            ),
            (
                "uap",
                "Disposition-classification bench over the uap-blue-book corpus (accuracy / macro-F1 / confusion matrix).",
            ),
            (
                "verifier",
                "Verifier-v0 offline seams (VERIFIER_V0.md): extract-claims runs ONE (question, answer) pair through the production gate's claim-list extraction; Stream B harness verbs land here.",
            ),
            (
                "faithfulness",
                "T1 P0.3: judge every RAPTOR node summary's claims against its member-chunk texts (production extract/support registers) — per-corpus unsupported-claim rate, JSONL rows for the gate twin (`bench gate faithfulness`).",
            ),
            (
                "enrichment-adjudicate",
                "T1 P0.2: sample unmatched (zero-credit) extraction atoms from an eval golden, judge junk vs legitimate, report per-axis junk rate + projected junk volume.",
            ),
            (
                "enrichment-ablate",
                "T1 P0.4: run a bank through the production retrieval pipeline under the declared knob matrix (raptor grounding / conv PPR / doc cluster / atlas), one joined table with per-knob SEPARABLE verdicts.",
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
        // `gate` is synchronous — pure file IO + arithmetic, no daemon calls.
        "gate" => gate::cmd_gate(&args[1..]),
        // `report` is synchronous — rolls the on-disk baselines up into
        // per-model reliability reports; no daemon calls.
        "report" => report::cmd_report(&args[1..]),
        "atlas" => atlas::cmd_atlas(&args[1..]).await,
        "book-report" => book_report::cmd_book_report(&args[1..]).await,
        "vault-report" => vault_report::cmd_vault_report(&args[1..]).await,
        "chaos-monkey" => chaos_monkey::cmd_chaos_monkey(&args[1..]).await,
        "routing-replay" => routing_replay::cmd_routing_replay(&args[1..]).await,
        "enron" => enron::cmd_enron(&args[1..]).await,
        "flywheel" => flywheel::cmd_flywheel(&args[1..]).await,
        "governance" => governance::cmd_governance(&args[1..]).await,
        "proxy" => proxy_bench::cmd_proxy_bench(&args[1..]).await,
        "promote" => promote::cmd_promote(&args[1..]).await,
        "mechanism-fidelity" => mechanism_fidelity::cmd_mechanism_fidelity(&args[1..]).await,
        "moral" => moral::cmd_moral(&args[1..]).await,
        "situated" => situated::cmd_situated(&args[1..]).await,
        "obsidian" => obsidian::cmd_obsidian(&args[1..]).await,
        "parity-compare" => parity_compare::cmd_parity_compare(&args[1..]).await,
        "scaffold" => scaffold::cmd_scaffold(&args[1..]).await,
        "uap" => uap::cmd_uap(&args[1..]).await,
        "faithfulness" => faithfulness::cmd_faithfulness(&args[1..]).await,
        "enrichment-adjudicate" => adjudicate::cmd_adjudicate(&args[1..]).await,
        "enrichment-ablate" => ablate::cmd_ablate(&args[1..]).await,
        "verifier" => verifier::cmd_verifier(&args[1..]).await,
        other => {
            eprintln!("error: unknown bench subcommand `{other}`");
            eprintln!();
            help::print(&HELP);
            2
        }
    }
}
