// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn chat session` — multi-turn interactive REPL, asked of the daemon.
//!
//! Every iteration:
//!   1. Read a line from stdin.
//!   2. Send it as a `TurnRequest` and stream the `TurnFrame::Token`s out.
//!   3. Render the provenance footer from `TurnFrame::Complete`.
//!   4. Loop.
//!
//! Holds one conversation id for the whole session so follow-up turns get the
//! prior context the desktop relies on. `quit` / `exit` / Ctrl-D end the
//! session; the conversation-end memory pass fires on `quit` so the
//! side effects match the desktop's "close the tab" behaviour — it is a call
//! to the daemon now (`POST /v1/conversations/{id}/end`) rather than a method
//! on a `Runtime` this process owns.
//!
//! Phase 6 (TOPOLOGY §10): this file holds no `Runtime`, no store and no
//! corpus engine. See `chat_cmd::ask` for the full argument.

use std::io::{self, BufRead, Write};

use sovereign_contracts::types::TurnMode;
use sovereign_turn_client::{TurnClient, TurnObserver};

use crate::chat_cmd::config::parse_globals_for_chat as parse_globals;
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

    let (globals, rest) = match parse_globals(args).await {
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

    let client = TurnClient::new(&globals.daemon_base);

    // The daemon mints the id and seeds the row in the same call — see
    // `chat_cmd::ask` for why the client no longer invents one.
    let conversation_id = match conversation_id {
        Some(id) => id,
        None => match client.create_conversation(None).await {
            Ok(c) => c.id,
            Err(e) => {
                eprintln!("could not start a conversation on the daemon: {e}");
                eprintln!("hint: is the daemon running? `svrn daemon start`");
                return 1;
            }
        },
    };
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
            // The conversation-end memory-extraction pass, now a lifecycle
            // call on the daemon rather than a method on a Runtime this
            // process owns. Still best-effort — quitting a REPL should not
            // fail because a memory pass did — but a failure is now VISIBLE
            // rather than dropped on the floor by a bare `let _ =`.
            if let Err(e) = client.end_conversation(&conversation_id).await {
                eprintln!("[note] conversation-end memory pass did not run: {e}");
            }
            break;
        }

        if let Err(code) = run_one(&client, line, &conversation_id, show_reasoning).await {
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
    client: &TurnClient,
    question: &str,
    conversation_id: &str,
    show_reasoning: bool,
) -> std::result::Result<(), i32> {
    println!();
    // Locked per write, not for the turn — see `chat_cmd::ask`.
    let mut echo = |chunk: &str| {
        let mut out = io::stdout();
        let _ = out.write_all(chunk.as_bytes());
        let _ = out.flush();
    };
    // The daemon narrates what it is doing while the answer is still being
    // retrieved. The in-process REPL had no way to show this — the narration
    // channel existed but nothing in this file subscribed to it.
    let mut narrate =
        |_p: &sovereign_contracts::types::NarrationPhase, text: &str, elapsed_ms: u64| {
            if !text.is_empty() {
                eprintln!("· {text} ({:.1}s)", elapsed_ms as f64 / 1000.0);
            }
        };

    let outcome = {
        let mut observer = TurnObserver {
            on_token: Some(&mut echo),
            on_narration: Some(&mut narrate),
            on_queue_position: None,
        };
        client
            .run_turn(
                conversation_id,
                question,
                TurnMode::Grounded,
                None,
                &mut observer,
            )
            .await
    };

    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            println!();
            eprintln!("[turn failed] {e}");
            // Soft failure: the REPL keeps looping, as it always did.
            return Ok(());
        }
    };

    println!();
    println!();

    let header = render::provenance_header_typed(outcome.provenance.as_ref());
    if !header.is_empty() {
        eprintln!("  {header}");
    }
    let (reasoning, _) = render::split_reasoning(&outcome.text);
    let reasoning_out = render::render_reasoning(&reasoning, show_reasoning);
    if !reasoning_out.is_empty() {
        eprintln!("  {reasoning_out}");
    }
    let footer = render::citations_footer(&outcome.citations);
    if !footer.is_empty() {
        eprintln!();
        eprint!("{footer}");
    }
    eprintln!();
    Ok(())
}
