// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn chat session` — multi-turn interactive REPL.
//!
//! Every iteration:
//!   1. Read a line from stdin.
//!   2. Stream the answer to stdout via `handle_message_stream`.
//!   3. Render provenance footer.
//!   4. Loop.
//!
//! Holds one conversation id for the whole session so follow-up
//! turns get the prior context the desktop relies on. `quit` / `exit`
//! / Ctrl-D end the session; the Runtime's end-of-conversation hook
//! fires on `quit` so any memory-extraction side effects match the
//! desktop's "close the tab" behaviour.

use sovereign_core::runtime::message_metadata;
use std::io::{self, BufRead, Write};

use futures::StreamExt;

use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use crate::chat_cmd::render;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn chat session",
    summary: "Interactive REPL over a single conversation.",
    sections: &[
        HelpSection::Usage("svrn chat session [flags]"),
        HelpSection::Flags(&[
            (
                "--conversation <id>",
                "Resume an existing conversation id (default: fresh uuid).",
            ),
            (
                "--show-reasoning",
                "Render <think> blocks inline after every turn.",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Notes(
            "Type `quit` or `exit` to end. Ctrl-D also works. Blank lines are ignored.",
        ),
    ],
};

pub async fn cmd_session(args: &[String]) -> i32 {
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

    let mut conversation_id: Option<String> = None;
    let mut show_reasoning = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--conversation" => {
                i += 1;
                conversation_id = rest.get(i).cloned();
            }
            "--show-reasoning" => {
                show_reasoning = true;
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

    let conversation_id = conversation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    eprintln!();
    eprintln!("conversation: {conversation_id}");
    eprintln!("Type `quit` to exit.");
    eprintln!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut input = String::new();

    loop {
        input.clear();
        eprint!("> ");
        let _ = io::stderr().flush();
        let n = stdin.lock().read_line(&mut input).unwrap_or(0);
        if n == 0 {
            // EOF — graceful close.
            break;
        }
        let line = input.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "quit" | "exit") {
            let _ = session.runtime.end_conversation(&conversation_id).await;
            break;
        }

        if let Err(code) = run_one(&session, line, &conversation_id, show_reasoning).await {
            return code;
        }
        let _ = stdout.flush();
    }

    0
}

/// One streaming turn. Returns `Err(code)` for hard failures that
/// should abort the REPL; soft errors (model parses failure, etc.)
/// are printed to stderr and we keep looping.
async fn run_one(
    session: &ChatSession,
    question: &str,
    conversation_id: &str,
    show_reasoning: bool,
) -> std::result::Result<(), i32> {
    let handle = match session
        .runtime
        .handle_message_stream(question, conversation_id)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[turn failed] {e}");
            return Ok(());
        }
    };

    let message_id = handle.message_id.clone();
    let mut stream = handle.stream;
    let mut raw = String::new();
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout);

    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                raw.push_str(&chunk);
                let _ = stdout.write_all(chunk.as_bytes());
                let _ = stdout.flush();
            }
            Err(e) => {
                let _ = writeln!(stdout);
                eprintln!("[stream error] {e}");
                return Ok(());
            }
        }
    }
    let _ = writeln!(stdout);
    let _ = writeln!(stdout);

    // THE lookup (ARCH §10.6). This was hand-rolled here, in `session.rs`,
    // and three times over in the desktop — five copies of "find the message
    // the turn just wrote and read its metadata".
    let metadata = message_metadata(session.store.as_ref(), conversation_id, &message_id).await;

    let header = render::provenance_header(metadata.as_ref());
    if !header.is_empty() {
        eprintln!("  {header}");
    }
    let (reasoning, _) = render::split_reasoning(&raw);
    let reasoning_out = render::render_reasoning(&reasoning, show_reasoning);
    if !reasoning_out.is_empty() {
        eprintln!("  {reasoning_out}");
    }
    let footer = render::retrieved_chunks_footer(metadata.as_ref());
    if !footer.is_empty() {
        eprintln!();
        eprint!("{footer}");
    }
    eprintln!();
    Ok(())
}
