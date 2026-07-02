// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn govern` — the runnable governance loop over a corpus's
//! event-sourced common law (Governance Atlas / FR-9).
//!
//! Model-free verbs (no daemon) read the [`GovernanceView`] read-model or
//! append to the `GovernanceOplog`:
//!   - `seed`     — `AssertRule` every extracted rule-claim, establishing
//!                  the governed baseline (the rule set is *defined* by
//!                  the oplog, and nothing else populates it today).
//!   - `tensions` — list open tensions, ranked, with both rule texts.
//!   - `resolve`  — supersede one tensioned rule with the other and mark
//!                  the tension resolved.
//!   - `accept`   — record a tension as known-and-tolerated.
//!
//! `ask` is the runtime build: a turn sealed to the corpus, grounded in
//! *current law* (the active-set retrieval filter drops superseded rules'
//! evidence — FR-9 RL-3), cite-or-abstain gated via
//! `GateSurface::Governance` (RL-1/RL-2), with supersession provenance.

use std::path::PathBuf;

use corpus_engine::enrichment::GovernanceView;
use sovereign_cli_shared::help::{self, Help, HelpSection};

pub mod accept;
pub mod ask;
pub mod resolve;
pub mod seed;
pub mod tensions;

const HELP: Help = Help {
    command: "svrn govern",
    summary: "Governance over a corpus's event-sourced common law (FR-9): seed rules, surface tensions, adjudicate, ask current law.",
    sections: &[
        HelpSection::Usage(
            "svrn govern <seed|tensions|resolve|accept|ask> <corpus-id> [args]",
        ),
        HelpSection::SubcommandsTitled(
            "Verbs",
            &[
                ("seed <corpus>", "AssertRule every extracted rule-claim (governed baseline; idempotent)."),
                ("tensions <corpus>", "List open tensions, ranked, with both rule texts."),
                ("resolve <corpus> <tension-id> --keep <rule-id>", "Supersede the other tensioned rule; mark resolved."),
                ("accept <corpus> <tension-id> --rationale <s>", "Record the tension as known-and-tolerated."),
                ("ask <corpus> \"<question>\"", "Answer from current law (active-set filtered, cite-or-abstain)."),
            ],
        ),
        HelpSection::Flags(&[
            ("--keep <rule-id>", "resolve: which tensioned rule wins (the other is superseded)."),
            ("--rationale <s>", "Human rationale recorded on the oplog op."),
        ]),
        HelpSection::Examples(&[
            (
                "svrn govern seed maple-house",
                "Establish the governed rule baseline after enrichment.",
            ),
            (
                "svrn govern ask maple-house \"how many nights can a guest stay?\"",
                "Answer from current law, dropping any superseded rule's evidence.",
            ),
        ]),
    ],
};

pub async fn run_govern(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let (cmd, rest) = args.split_first().unwrap();
    match cmd.as_str() {
        "seed" => seed::cmd_seed(rest),
        "tensions" => tensions::cmd_tensions(rest),
        "resolve" => resolve::cmd_resolve(rest),
        "accept" => accept::cmd_accept(rest),
        "ask" => ask::cmd_ask(rest).await,
        other => {
            eprintln!("error: unknown govern subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

/// `~/.sovereign/indexes/<corpus>/atlas` — where `atoms.json`,
/// `edges.json` and `governance_oplog.jsonl` live. The same root the
/// daemon's `CorpusEngine` reads, so an oplog the CLI appends here is
/// seen by `govern ask`'s active-set retrieval filter.
pub(crate) fn atlas_dir(corpus_id: &str) -> PathBuf {
    crate::enrich_cmd::paths::index_root(corpus_id)
        .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME)
}

/// Load the governance read-model for a corpus, or a friendly error
/// pointing at the missing prerequisite.
pub(crate) fn load_view(corpus_id: &str) -> Result<GovernanceView, String> {
    let dir = atlas_dir(corpus_id);
    if !dir.join("atoms.json").exists() {
        return Err(format!(
            "no enriched atlas for `{corpus_id}` at {} — run `svrn enrich build {corpus_id} --full` first",
            dir.display()
        ));
    }
    GovernanceView::from_atlas_dir(&dir).map_err(|e| format!("reading governance view: {e}"))
}

/// Unix seconds now — the timestamp stamped on appended oplog ops.
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
