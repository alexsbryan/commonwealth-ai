// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn chat list` — enumerate recent conversations.

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn chat list",
    summary: "List recent conversations from the state store.",
    sections: &[
        HelpSection::Usage("svrn chat list [--limit N] [--offset N]"),
        HelpSection::Flags(&[
            ("--limit <N>", "Max conversations to show (default: 20)."),
            ("--offset <N>", "Skip the first N (default: 0)."),
            ("--help, -h", "Show this message."),
        ]),
    ],
};

pub async fn cmd_list(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&HELP);
        return 0;
    }

    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let mut limit: usize = 20;
    let mut offset: usize = 0;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--limit" => {
                i += 1;
                limit = rest.get(i).and_then(|s| s.parse().ok()).unwrap_or(limit);
            }
            "--offset" => {
                i += 1;
                offset = rest.get(i).and_then(|s| s.parse().ok()).unwrap_or(offset);
            }
            extra => {
                eprintln!("error: unexpected argument `{extra}`");
                return 2;
            }
        }
        i += 1;
    }

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    let convos = match session.store.list_conversations(limit, offset).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("list_conversations failed: {e}");
            return 1;
        }
    };
    if convos.is_empty() {
        eprintln!("(no conversations)");
        return 0;
    }
    for c in convos {
        let title = c.title.as_deref().unwrap_or("<untitled>");
        let updated = format_ts(c.updated_at);
        let turns = c.messages.len();
        println!("{id}  {updated}  {turns:>3} turn(s)  {title}", id = c.id);
    }
    0
}

fn format_ts(epoch_secs: i64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let dt = UNIX_EPOCH + Duration::from_secs(epoch_secs.max(0) as u64);
    chrono::DateTime::<chrono::Utc>::from(dt)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}
