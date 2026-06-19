// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign bench proxy` — Proxy Voting Corpus Q&A bench (AC-4/AC-5).
//!
//! The chaos two-red-line path over an installed `proxy-cik…` corpus.
//! Because the sealed corpus is in the `proxy-cik` family, the live turns
//! automatically take the `GateSurface::ProxyArgument` cite-or-abstain
//! gate, so the bank's rows measure:
//!   - RL-2 (both sides, cited) — `Present` rows over a shareholder
//!     proposal: a correct answer names BOTH the proponent's and the
//!     board's case (the gold_keywords AND-match carries one distinctive
//!     term per side);
//!   - RL-1 (no confabulated opposition) — `AbsentAdjacent` rows: the
//!     "case against" a management item the filing argues only FOR. The
//!     honest move is to abstain; answering with a manufactured against is
//!     the cardinal sin (hallucination on absent);
//!   - AC-5 (steelman entailed by source) — `ProvenanceTrap` rows: the
//!     answer must be backed by the genuinely-supporting passage, not a
//!     near-miss.
//!
//! Proxy has no Lane-A detector (its enrichment quality is exercised by
//! the corpus-engine tension tests, not a per-corpus truth manifest), so
//! this bench is QA-only. Like governance, it is the *tracked* half; the
//! paired hard `bench gate proxy-qa` re-scores the artifact and fails only
//! on regression vs the committed baseline.

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench proxy",
    summary: "Proxy Voting Corpus cite-or-abstain Q&A bench (AC-4 RL-1/RL-2, AC-5).",
    sections: &[
        HelpSection::Usage(
            "sovereign bench proxy qa <corpus-id> [--bank <t>] [--manifest <t>] [--out <jsonl>] [chaos flags]",
        ),
        HelpSection::Flags(&[
            (
                "--bank <path>",
                "Question bank. Default: sovereign/bench/proxy/exxon/bank.toml (pinned to the fixture filing).",
            ),
            (
                "--manifest <path>",
                "Gate manifest. Default: sovereign/bench/proxy/exxon/manifest.toml.",
            ),
            (
                "--out <path>",
                "Write the ResultRow JSONL consumed by `bench gate proxy-qa`.",
            ),
        ]),
        HelpSection::Examples(&[(
            "sovereign bench proxy qa proxy-cik0000034088 --out target/proxy-qa/results.jsonl",
            "Run the chaos two-red-line bank over Exxon's ballot (GateSurface::ProxyArgument applies).",
        )]),
    ],
};

/// Delegate to the chaos two-red-line runner over a proxy corpus. Pure
/// fixture-loader + pinned-intent + proxy answering discipline; the chaos
/// scorer already computes everything (competence/honesty/hallucination +
/// citation fidelity). No bespoke orchestrator.
async fn qa(args: &[String]) -> i32 {
    let Some((corpus, rest)) = args.split_first() else {
        eprintln!("error: usage: sovereign bench proxy qa <corpus-id> [--bank <t>] [--manifest <t>] [--out <jsonl>] [chaos flags]");
        return 2;
    };
    if corpus.starts_with("--") {
        eprintln!("error: the first argument to `qa` must be the corpus id, not a flag");
        return 2;
    }
    let present = |flag: &str| rest.iter().any(|a| a == flag);
    let mut chaos: Vec<String> = vec!["run".into(), "--corpus".into(), corpus.clone()];
    if !present("--bank") {
        chaos.push("--bank".into());
        chaos.push("sovereign/bench/proxy/exxon/bank.toml".into());
    }
    if !present("--manifest") {
        chaos.push("--manifest".into());
        chaos.push("sovereign/bench/proxy/exxon/manifest.toml".into());
    }
    if !present("--out") {
        chaos.push("--out".into());
        chaos.push("target/proxy-qa/results.jsonl".into());
    }
    // Measure the SAME hardened turn `proxy ask` ships: pin the intent to a
    // factual lookup (proxy Qs never need the router) + carry the proxy
    // answering discipline.
    if !present("--pin-intent") {
        chaos.push("--pin-intent".into());
        chaos.push("knowledge_query".into());
    }
    if !present("--custom-instructions") {
        chaos.push("--custom-instructions".into());
        chaos.push(crate::proxy_cmd::ask::PROXY_ASK_DISCIPLINE.to_string());
    }
    chaos.extend(rest.iter().cloned());
    eprintln!(
        "[proxy qa] → chaos-monkey over `{corpus}` (GateSurface::ProxyArgument applies: proxy-cik corpus family)"
    );
    super::chaos_monkey::cmd_chaos_monkey(&chaos).await
}

pub async fn cmd_proxy_bench(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    match args.first().map(|s| s.as_str()) {
        // `qa` is the only lane; accept `run` as a friendly alias.
        Some("qa") | Some("run") => qa(&args[1..]).await,
        Some(other) => {
            eprintln!("error: unknown proxy bench subcommand `{other}`");
            help::print(&HELP);
            2
        }
        None => {
            help::print(&HELP);
            2
        }
    }
}
