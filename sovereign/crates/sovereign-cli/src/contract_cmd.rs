// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn contract` — what this CLI promises, how much of it is proven, and when
//! that was last checked against a running system.
//!
//! WHY A VERB. The CLI contract has grown five layers — a manifest of commands,
//! an experience axis over it, three static ratchet test files, a live journey
//! runner, and a nightly lane that drives both — and until 2026-07-30 every one
//! of them was reachable only by knowing it existed. The map lived inside a
//! cargo test (`cargo test … print_the_experience_map -- --nocapture`), the
//! lanes were two scripts under `scripts/`, and the nightly's verdict was a
//! `latest.json` under `~/.sovereign/`. A developer asking the obvious question
//! — "is the CLI I just changed covered by anything?" — had nowhere to look.
//!
//! This repo has a graveyard of quality tools that were built, documented, and
//! never contacted reality again: the harness this one replaced was called by
//! nothing at all, and its env-var switch appears nowhere but inside the script
//! that read it. A quality surface that cannot be found is on the same path. So
//! the surface has one front door, it prints the honest numbers rather than a
//! reassuring summary, and it ends by naming the exact commands that re-derive
//! everything it just said.
//!
//! Rendering lives in `sovereign_cli_shared::cli_contract_report`, shared with
//! `cli_contract_journeys` — one census, one renderer, so the number the gate
//! enforces is the number this prints.

use sovereign_cli_shared::cli_contract::Contract;
use sovereign_cli_shared::cli_contract_report as report;

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let view = args.first().map(String::as_str).unwrap_or("");
    // The manifest is a dev-checkout artifact. Say so precisely rather than
    // printing an empty report: an installed binary has no `docs/` to read.
    let contract = match Contract::load_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("contract: {e}");
            eprintln!("  `svrn contract` reads sovereign/docs/cli-contract.toml, which exists");
            eprintln!("  only in a source checkout. Run it from one.");
            return 2;
        }
    };
    // Payload to stdout, every view — this is a report, not narration
    // (payload-vs-narration seam, note f5acdf59).
    match view {
        "" | "all" => print!("{}", report::render_report(&contract, report::nightly_posture().as_ref())),
        "map" => print!("{}", report::render_experience_map(&contract)),
        "census" => print!("{}", report::render_census(&contract)),
        "nightly" => print!("{}", report::render_nightly(report::nightly_posture().as_ref())),
        other => {
            eprintln!("contract: unknown view `{other}`");
            crate::util::help::print(&HELP);
            return 2;
        }
    }
    0
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn contract",
    summary: "What the CLI promises, how much of it is proven, and when that was last checked.",
    sections: &[
        crate::util::help::HelpSection::Usage("svrn contract [map|census|nightly]"),
        crate::util::help::HelpSection::Subcommands(&[
            ("(bare)", "everything below, plus how to run each lane yourself"),
            ("map", "experiences (the promises) and the journeys serving each"),
            ("census", "how many steps can actually fail — live vs never-run"),
            ("nightly", "the last journey-lane verdict on this host, and its age"),
        ]),
        crate::util::help::HelpSection::Examples(&[
            ("svrn contract", "the whole report"),
            ("svrn contract census", "just the honest coverage numbers"),
            ("svrn contract nightly", "did anything run here, and when"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Reads sovereign/docs/cli-contract.toml, so it needs a source checkout. \
             The census is the same one cli_contract_journeys enforces as a gate — \
             one definition, so this report cannot flatter the manifest.",
        ),
    ],
};
