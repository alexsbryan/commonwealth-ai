// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn backlog` — file work into the seat's ranked backlog.
//!
//! The backlog is a pull queue with heap SEMANTICS and no heap: an item
//! is a notes-store todo (`related_entity=backlog`) with a header block,
//! and ordering is derived at every read by `scripts/co-backlog.py`.
//! Insert is therefore O(1) with nothing to re-index, out-of-band edits
//! (an operator changing a Value, a reviewer clearing a stamp) are just
//! fields at read time, and editing the ruler re-scores everything for
//! free. A materialized heap was rejected at this scale; the escalation
//! if reads ever go hot is `ORDER BY` in the store's own SQLite, with
//! this verb unchanged.
//!
//! What this verb adds over "write a note by hand" is that the SCORE
//! comes from the local model against the versioned ruler, so a producer
//! that is not a person — a CI lane, a watcher, a worker session — can
//! file something ranked. And because a machine drafted it, it lands
//! UNVETTED: `Scored-by:` is a stamp the renderer treats as
//! disqualifying, and a person clearing it is the review.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub mod add;
pub mod item;
pub mod ruler;
pub mod score;

const HELP: Help = Help {
    command: "svrn backlog",
    summary: "File work into the seat's ranked backlog, scored by the local model against the versioned value ruler",
    sections: &[
        HelpSection::Usage("svrn backlog add \"<text>\" [--objective <anchor>] [--key <id>] [--no-score]"),
        HelpSection::Subcommands(&[(
            "add \"<text>\"",
            "Score one item on the resident model and file it, unvetted.",
        )]),
        HelpSection::Flags(&[
            ("--objective <anchor>", "The standing objective, initiative or order id it serves."),
            ("--key <id>", "Producer identity. A repeat filing under the same key UPDATES that item instead of filing a duplicate."),
            ("--producer <name>", "What filed it. Defaults to `svrn backlog add`."),
            ("--no-score", "File it unscored for later triage. No model call, no daemon needed."),
            ("--db <path>", "The notes store. Defaults to $CO_BACKLOG_NOTES_DB, else $SOVEREIGN_DATA_DIR/notes.db, else ~/.sovereign/notes.db — never discovered from the working directory."),
            ("--ruler <path>", "The value ruler. Defaults to $CO_BACKLOG_RULER, else quality/backlog-ruler.toml from the repo."),
            ("--create", "Create the store if it does not exist. Off by default: a fresh store at a wrong path looks exactly like a working one."),
            ("--daemon <url>", "The daemon to score against. Defaults to the configured client port."),
            ("--json", "Print the result as JSON."),
        ]),
        HelpSection::Examples(&[
            (
                "svrn backlog add \"decline p50 is 11s on cold cache\" --objective \"one sweep\"",
                "Score one item on the resident model and file it.",
            ),
            (
                "svrn backlog add \"$(cat failure.txt)\" --key bench-lane:retrieval-prod",
                "File from an automated producer; a repeat failure updates the same item.",
            ),
        ]),
        HelpSection::Notes(
            "Machine-scored items ALWAYS land unvetted and cannot be pulled: the \
             score is a draft, and vetting is a human act. The item carries \
             `Scored-by: <model>`, which scripts/co-backlog.py treats as \
             disqualifying however complete the rest of the header looks — a \
             reviewer clears that line, and the clearing is the review.\n\n\
             If the daemon is down or no chat model is resident, `add` REFUSES \
             and says which. It never files an unscored item as a scored one. \
             Use --no-score to file deliberately unscored.\n\n\
             The ruler is quality/backlog-ruler.toml — the same file \
             scripts/co-backlog.py ranks with. Edit it and everything re-scores \
             on the next render.",
        ),
    ],
};

pub async fn run_backlog(args: &[String]) -> i32 {
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
        "add" => add::cmd_add(rest).await,
        other => {
            eprintln!("error: unknown backlog subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}
