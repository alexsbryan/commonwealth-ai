// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn chat show <conversation_id>` — dump a conversation's turns
//! with their persisted provenance + retrieved-chunks metadata.

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::chat_cmd::render;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn chat show",
    summary: "Dump a conversation — turns + provenance + retrieved chunks.",
    sections: &[
        HelpSection::Usage("svrn chat show <conversation_id> [--show-reasoning]"),
        HelpSection::Flags(&[
            (
                "--show-reasoning",
                "Expand <think> blocks inline for every assistant message.",
            ),
            ("--help, -h", "Show this message."),
        ]),
    ],
};

pub async fn cmd_show(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }

    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let mut conversation_id: Option<String> = None;
    let mut show_reasoning = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--show-reasoning" => show_reasoning = true,
            arg if conversation_id.is_none() => conversation_id = Some(arg.to_string()),
            extra => {
                eprintln!("error: unexpected argument `{extra}`");
                return 2;
            }
        }
        i += 1;
    }

    let Some(cid) = conversation_id else {
        eprintln!("error: missing conversation id. Usage: svrn chat show <id>");
        return 2;
    };

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    let convo = match session.store.get_conversation(&cid).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("get_conversation failed: {e}");
            return 1;
        }
    };

    println!(
        "# conversation {id}\n# title: {title}\n# turns: {turns}",
        id = convo.id,
        title = convo.title.as_deref().unwrap_or("<untitled>"),
        turns = convo.messages.len()
    );

    for m in &convo.messages {
        println!();
        println!("── {} ──", m.role_str());
        match m.role {
            sovereign_core::types::Role::User => {
                println!("{}", m.content);
            }
            sovereign_core::types::Role::Assistant => {
                let (reasoning, visible) = render::split_reasoning(&m.content);
                println!("{visible}");
                let header = render::provenance_header(m.metadata.as_ref());
                if !header.is_empty() {
                    println!();
                    println!("  {header}");
                }
                let reasoning_out = render::render_reasoning(&reasoning, show_reasoning);
                if !reasoning_out.is_empty() {
                    println!("  {reasoning_out}");
                }
                let footer = render::retrieved_chunks_footer(m.metadata.as_ref());
                if !footer.is_empty() {
                    println!();
                    print!("{footer}");
                }
                // Per-segment grounding provenance (NATIVE_GROUNDING.md §6).
                // It lives here rather than on the ask path because
                // `TurnFrame::Complete` does not carry `answer_segments` —
                // putting it on the wire means designing a typed per-segment
                // surface, which is bigger than phase 6. `chat show` reads
                // the persisted blob and can render it today, so the CLI
                // keeps the capability instead of losing it to a conversion.
                // Prints nothing unless the native grounding path ran.
                let segments = render::answer_segments_footer(m.metadata.as_ref());
                if !segments.is_empty() {
                    print!("{segments}");
                }
            }
            sovereign_core::types::Role::System => {
                println!("{}", m.content);
            }
        }
    }
    0
}
