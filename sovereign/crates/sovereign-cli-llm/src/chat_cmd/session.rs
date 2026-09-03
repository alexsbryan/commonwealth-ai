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

use crate::chat_cmd::ask::{parse_corpus_flag_value, CORPUS_WITH_CONVERSATION};
use crate::chat_cmd::config::{
    parse_globals_for_chat as parse_globals, print_daemon_served_turn_notes,
};
use crate::chat_cmd::render;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn chat session",
    summary: "Interactive REPL over a single conversation.",
    sections: &[
        HelpSection::Usage("svrn chat session [flags]"),
        HelpSection::Flags(&[
            (
                "--corpus <id>",
                "Restrict every turn's retrieval to this corpus (repeatable). The daemon refuses \
                 an id it has not installed and lists the ones it has. Not combinable with \
                 --conversation.",
            ),
            (
                "--conversation <id>",
                "Resume an existing conversation id (default: the daemon mints one).",
            ),
            (
                "--show-reasoning",
                "Render <think> blocks inline after every turn.",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Notes(
            "Type `quit` or `exit` to end. Ctrl-D also works. Blank lines are ignored.\n\n\
             Every turn runs ON THE DAEMON (--daemon, default http://localhost:9741): \
             `--data-dir` does not scope it (use --corpus), and SOVEREIGN_GATE_* knobs must be \
             exported where the daemon is launched, not in this shell.",
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
    let parsed = match parse_session_args(&rest) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let show_reasoning = parsed.show_reasoning;

    // What this shell set that the daemon will not see — see `chat_cmd::ask`.
    print_daemon_served_turn_notes(&globals);

    let client = TurnClient::new(&globals.daemon_base);

    // The daemon mints the id and seeds the row (and the corpus allow-list)
    // in the same call — see `chat_cmd::ask` for why the client no longer
    // invents one.
    let conversation_id = match parsed.conversation_id {
        Some(id) => id,
        None => {
            let allow = (!parsed.corpora.is_empty()).then_some(parsed.corpora.as_slice());
            match client.create_conversation(None, allow).await {
                Ok(c) => c.id,
                Err(e) => {
                    eprintln!("could not start a conversation on the daemon: {e}");
                    if parsed.corpora.is_empty() {
                        eprintln!("hint: is the daemon running? `svrn daemon start`");
                    }
                    return 1;
                }
            }
        }
    };
    eprintln!();
    eprintln!("conversation: {conversation_id}");
    if !parsed.corpora.is_empty() {
        eprintln!("corpora: {}", parsed.corpora.join(", "));
    }
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

/// `svrn chat session`'s own flags, after the shared globals are taken out.
#[derive(Debug, Default, PartialEq, Eq)]
struct SessionArgs {
    conversation_id: Option<String>,
    show_reasoning: bool,
    /// `--corpus`, repeated. Empty means "every installed corpus".
    corpora: Vec<String>,
}

fn parse_session_args(rest: &[String]) -> Result<SessionArgs, String> {
    let mut parsed = SessionArgs::default();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--conversation" => {
                i += 1;
                parsed.conversation_id = Some(
                    rest.get(i)
                        .cloned()
                        .ok_or_else(|| "--conversation needs a value".to_string())?,
                );
            }
            "--corpus" => {
                i += 1;
                parsed.corpora.push(parse_corpus_flag_value(rest.get(i))?);
            }
            "--show-reasoning" => {
                parsed.show_reasoning = true;
            }
            extra => return Err(format!("unexpected argument `{extra}`")),
        }
        i += 1;
    }
    if !parsed.corpora.is_empty() && parsed.conversation_id.is_some() {
        return Err(CORPUS_WITH_CONVERSATION.to_string());
    }
    Ok(parsed)
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

#[cfg(test)]
mod arg_tests {
    use super::*;

    fn svec(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn session_corpus_repeats() {
        let p = parse_session_args(&svec(&["--corpus", "sep", "--corpus", "crs_reports"])).unwrap();
        assert_eq!(p.corpora, vec!["sep", "crs_reports"]);
    }

    #[test]
    fn session_corpus_without_an_id_is_refused() {
        let err = parse_session_args(&svec(&["--corpus"])).unwrap_err();
        assert!(err.contains("--corpus needs a corpus id"), "{err}");
    }

    #[test]
    fn session_corpus_with_conversation_is_refused() {
        let err =
            parse_session_args(&svec(&["--corpus", "sep", "--conversation", "c1"])).unwrap_err();
        assert_eq!(err, CORPUS_WITH_CONVERSATION);
    }
}
