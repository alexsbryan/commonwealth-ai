// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn chat ask "<question>"` — one-shot turn, asked of the daemon.
//!
//! # This is a surface now (TOPOLOGY §10 phase 6)
//!
//! It used to be:
//!   1. Bootstrap a `Runtime` in this process.
//!   2. Call `handle_message_stream(message, conversation_id)`.
//!   3. Drain the chunk stream to stdout.
//!   4. Go back to the store and read the assistant message it just wrote,
//!      to learn what the turn had done.
//!
//! Step 4 is the one that made this a host rather than a surface: "find the
//! row the turn produced" only works from inside the process that owns the
//! store, so the answer to "what did this turn do" could not cross a process
//! boundary. Now the turn runs on the daemon and its result ARRIVES — as
//! `TurnFrame::Complete`, a value that serializes — and this file holds no
//! `Runtime`, no store, no corpus engine.
//!
//! The streaming shape is unchanged and still load-bearing for the same
//! reason: reasoning-heavy models emit thousands of `<think>` tokens before
//! the first visible answer character, and users should see progress
//! immediately rather than after 30 s of apparent silence. `TurnFrame::Token`
//! arrives per delta, so the live echo below is byte-for-byte what it was.
//!
//! # What moved to the daemon with it
//!
//! `--naked` is [`TurnMode::Naked`] on the wire — it had no wire form at all
//! before phase 6, so converting this file was blocked on adding one. The
//! non-streamable-intent fallback that used to live HERE (matching the error
//! string and re-running `handle_turn`) is now inside
//! `sovereign_core::runtime::serve_turn`, decided before the turn starts
//! rather than caught after it fails — see its doc comment for the
//! double-persist bug the two host copies disagreed about.

use std::io::{self, Write};

use serde_json::json;

use sovereign_contracts::types::projection::{Citation, Provenance};
use sovereign_contracts::types::TurnMode;
use sovereign_turn_client::{TurnClient, TurnObserver, TurnOutcome};

use crate::chat_cmd::config::{
    parse_globals_for_chat as parse_globals, print_daemon_served_turn_notes,
};
use crate::chat_cmd::render;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn chat ask",
    summary: "Ask a question once. Streams the answer, then prints provenance.",
    sections: &[
        HelpSection::Usage("svrn chat ask \"<question>\" [flags]"),
        HelpSection::Flags(&[
            (
                "--corpus <id>",
                "Restrict retrieval to this corpus (repeatable: --corpus sep --corpus gutenberg). \
                 The daemon refuses an id it has not installed and lists the ones it has. \
                 Not combinable with --conversation, whose allow-list is already set.",
            ),
            (
                "--conversation <id>",
                "Reuse an existing conversation id (default: the daemon mints one).",
            ),
            (
                "--format text|json",
                "Output format. `json` dumps the full message + metadata.",
            ),
            (
                "--show-reasoning",
                "Render <think> blocks inline instead of a collapsed handle.",
            ),
            (
                "--naked",
                "Raw model: retrieval, router, grounding gate, tools and atlas all bypassed.",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Examples(&[(
            "svrn chat ask --corpus sep \"what is compatibilism?\"",
            "Answer from the `sep` corpus only.",
        )]),
        HelpSection::Notes(
            "The question is taken from the first non-flag positional argument. \
             Wrap it in quotes. Prints a `▶ reasoning ...` line after the answer \
             summarizing how much was hidden (override with `--show-reasoning`).\n\n\
             The turn runs ON THE DAEMON (--daemon, default http://localhost:9741), not in \
             this process: `--data-dir` does not scope it (use --corpus), and \
             SOVEREIGN_GATE_* knobs must be exported where the daemon is launched, not \
             in this shell. Both are reported on stderr when they apply.",
        ),
    ],
};

pub async fn cmd_ask(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }

    let (globals, rest) = match parse_globals(args).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let parsed = match parse_ask_args(&rest) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let Some(question) = parsed.question else {
        eprintln!("error: missing question. Usage: svrn chat ask \"<question>\"");
        return 2;
    };

    // Before anything reaches the daemon: what this shell set that the
    // daemon will not see (gate knobs, --data-dir). Stderr only.
    print_daemon_served_turn_notes(&globals);

    let client = TurnClient::new(&globals.daemon_base);

    // A caller-supplied id is reused as-is; otherwise the DAEMON mints one,
    // because minting it here and hoping the host agrees is how an id and the
    // row it names come into existence separately. `POST /v1/conversations`
    // seeds the row — and the corpus allow-list, validated against what the
    // daemon has installed — and returns the id it seeded.
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
    if !parsed.corpora.is_empty() {
        eprintln!("corpora: {}", parsed.corpora.join(", "));
    }

    let mode = if parsed.naked {
        TurnMode::Naked
    } else {
        TurnMode::Grounded
    };
    run_turn(
        &client,
        &question,
        &conversation_id,
        parsed.format,
        parsed.show_reasoning,
        mode,
    )
    .await
}

/// What `svrn chat ask` was asked to do, after the shared globals were
/// taken out of argv. Separated from `cmd_ask` so every flag has a failing
/// input a test can name without a daemon.
#[derive(Debug, Default, PartialEq, Eq)]
struct AskArgs {
    question: Option<String>,
    conversation_id: Option<String>,
    format: OutputFormat,
    show_reasoning: bool,
    naked: bool,
    /// `--corpus`, repeated, in the order given. Empty means "every
    /// installed corpus" and is NOT sent on the wire.
    corpora: Vec<String>,
}

fn parse_ask_args(rest: &[String]) -> Result<AskArgs, String> {
    let mut parsed = AskArgs::default();
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
            "--format" => {
                i += 1;
                parsed.format = match rest.get(i).map(String::as_str) {
                    Some("text") => OutputFormat::Text,
                    Some("json") => OutputFormat::Json,
                    Some(other) => {
                        return Err(format!("--format expects text|json, got `{other}`"));
                    }
                    None => return Err("--format needs a value".to_string()),
                };
            }
            "--show-reasoning" => {
                parsed.show_reasoning = true;
            }
            // Raw model ("naked") — bypass every Sovereign affordance
            // (retrieval, router, grounding gate, tools, atlas); mirrors
            // the desktop "Raw model" setting via handle_message_stream_naked.
            "--naked" => {
                parsed.naked = true;
            }
            arg if parsed.question.is_none() => {
                parsed.question = Some(arg.to_string());
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

/// One `--corpus` value, shared with `chat session`: present and non-blank,
/// or an error that names the flag.
pub(crate) fn parse_corpus_flag_value(value: Option<&String>) -> Result<String, String> {
    match value.map(|v| v.trim()) {
        Some(v) if !v.is_empty() && !v.starts_with('-') => Ok(v.to_string()),
        _ => Err("--corpus needs a corpus id (repeat the flag for several)".to_string()),
    }
}

/// Refused rather than silently dropping the allow-list: `--conversation`
/// reuses a row whose `enabled_corpora` is already what it is, and a
/// `--corpus` that changed nothing would read as having applied (§18.3).
pub(crate) const CORPUS_WITH_CONVERSATION: &str =
    "--corpus applies when the daemon mints the conversation; \
     --conversation reuses one whose corpus allow-list is already set. Drop one of them.";

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum OutputFormat {
    #[default]
    Text,
    Json,
}

async fn run_turn(
    client: &TurnClient,
    question: &str,
    conversation_id: &str,
    format: OutputFormat,
    show_reasoning: bool,
    mode: TurnMode,
) -> i32 {
    eprintln!();
    eprintln!("{BAR}");
    eprintln!("conversation: {conversation_id}");
    eprintln!("> {question}");
    eprintln!("{BAR}");
    if mode == TurnMode::Naked {
        eprintln!("· raw model — Sovereign affordances bypassed ·");
    }

    // In `text` mode we echo deltas as they arrive. In `json` mode the user
    // wants one structured payload at the end, so we buffer silently and dump
    // on completion. `TurnOutcome` accumulates the full text either way.
    let echo_live = matches!(format, OutputFormat::Text);
    if echo_live {
        eprintln!();
    }

    // Locked per write rather than for the whole turn: `TurnObserver`'s hooks
    // are `Send` so a caller can drive a turn from a spawned task, and a held
    // `StdoutLock` is not. Per-delta locking costs nothing at token cadence.
    let mut echo = |chunk: &str| {
        if echo_live {
            let mut out = io::stdout();
            let _ = out.write_all(chunk.as_bytes());
            let _ = out.flush();
        }
    };
    // Progress the in-process path never showed: the daemon narrates what it
    // is doing while the answer is still being retrieved. Stderr, so it stays
    // out of a piped `--format json` payload.
    let mut narrate =
        |_phase: &sovereign_contracts::types::NarrationPhase, text: &str, elapsed_ms: u64| {
            if echo_live && !text.is_empty() {
                eprintln!("· {text} ({:.1}s)", elapsed_ms as f64 / 1000.0);
            }
        };
    let mut queued = |position: u32, wait_ms: u64| {
        eprintln!("· queued #{position} · ~{:.0}s", wait_ms as f64 / 1000.0);
    };

    let outcome = {
        let mut observer = TurnObserver {
            on_token: Some(&mut echo),
            on_narration: Some(&mut narrate),
            on_queue_position: Some(&mut queued),
        };
        client
            .run_turn(conversation_id, question, mode, None, &mut observer)
            .await
    };

    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            if echo_live {
                println!();
            }
            eprintln!("turn failed: {e}");
            return 1;
        }
    };

    if echo_live {
        println!();
    }

    match format {
        OutputFormat::Text => render_text_typed(&outcome, show_reasoning),
        OutputFormat::Json => render_json_typed(conversation_id, &outcome),
    }

    0
}

/// The provenance block, rendered from the turn's terminal frame.
///
/// One field the in-process renderer showed is not here: the per-segment
/// grounding footer (`render::answer_segments_footer`, NATIVE_GROUNDING §6).
/// It reads an `answer_segments` array that `TurnFrame::Complete` does not
/// carry, and unlike `url` and `provenance_tier` — which were two `Option`
/// fields on an existing type — putting it on the wire means designing a
/// typed per-segment provenance surface, which is bigger than phase 6.
///
/// The capability is not lost, only moved: `svrn chat show` reads the
/// persisted blob and renders it there. Named in both places rather than
/// silently dropped, so nobody has to wonder where their footer went.
fn render_text_typed(outcome: &TurnOutcome, show_reasoning: bool) {
    let (reasoning, _visible) = render::split_reasoning(&outcome.text);
    // The visible portion is already on screen via the live echo — only the
    // summary metadata is appended below the answer.
    eprintln!();
    eprintln!("{BAR}");
    let header = render::provenance_header_typed(outcome.provenance.as_ref());
    if !header.is_empty() {
        eprintln!("{header}");
    }
    let reasoning_out = render::render_reasoning(&reasoning, show_reasoning);
    if !reasoning_out.is_empty() {
        eprintln!("{reasoning_out}");
    }
    let footer = render::citations_footer(&outcome.citations);
    if !footer.is_empty() {
        eprint!("{footer}");
    }
    eprintln!("{BAR}");
}

/// The JSON payload, rendered from the turn's terminal frame.
///
/// `metadata` used to be the raw persisted blob. It is now the typed
/// projection of it — the same facts, in the shape the protocol defines,
/// which is what a consumer on the other side of a socket can actually rely
/// on. `epistemic_state` rides along for the first time here: it was always
/// in the blob and this renderer never surfaced it.
fn render_json_typed(conversation_id: &str, outcome: &TurnOutcome) {
    let (reasoning, visible) = render::split_reasoning(&outcome.text);
    let payload = json!({
        "message_id": outcome.message_id,
        "conversation_id": conversation_id,
        "raw": outcome.text,
        "visible": visible,
        "reasoning": reasoning,
        "provenance": outcome.provenance.as_ref().map(provenance_json),
        "citations": outcome.citations.iter().map(citation_json).collect::<Vec<_>>(),
        "epistemic_state": outcome.epistemic_state,
    });
    // JSON on stdout so it is pipe-friendly; the conversational chrome stays
    // on stderr.
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
}

fn provenance_json(p: &Provenance) -> serde_json::Value {
    json!({
        "inference_backend": p.inference_backend,
        "routing_tier": p.routing_tier,
        "total_ms": p.total_ms,
        "ttft_ms": p.ttft_ms,
        "finish_reason": p.finish_reason,
        "completion_tokens": p.completion_tokens,
        "sources": p.sources.iter().map(|s| json!({
            "origin": s.origin,
            "count": s.count,
            "from_peer": s.from_peer,
        })).collect::<Vec<_>>(),
    })
}

fn citation_json(c: &Citation) -> serde_json::Value {
    json!({
        "corpus_id": c.corpus_id,
        "chunk_id": c.chunk_id,
        "title": c.title,
        "snippet": c.snippet,
        "score": c.score,
        "rank": c.rank,
        "url": c.url,
        "provenance_tier": c.provenance_tier,
    })
}

const BAR: &str = "─────────────────────────────────────────────────────────────";

#[cfg(test)]
mod arg_tests {
    use super::*;

    fn svec(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn corpus_repeats_in_order_and_the_question_survives() {
        let p = parse_ask_args(&svec(&[
            "--corpus",
            "sep",
            "what is compatibilism?",
            "--corpus",
            "gutenberg",
        ]))
        .unwrap();
        assert_eq!(p.corpora, vec!["sep", "gutenberg"]);
        assert_eq!(p.question.as_deref(), Some("what is compatibilism?"));
        assert!(p.conversation_id.is_none());
    }

    #[test]
    fn no_corpus_flag_means_an_empty_list_not_a_default() {
        let p = parse_ask_args(&svec(&["hi"])).unwrap();
        assert!(p.corpora.is_empty());
    }

    /// The failing inputs: a bare flag, and a flag that swallowed the next
    /// flag as its value.
    #[test]
    fn a_corpus_flag_without_an_id_is_refused() {
        let err = parse_ask_args(&svec(&["hi", "--corpus"])).unwrap_err();
        assert!(err.contains("--corpus needs a corpus id"), "{err}");
        let err = parse_ask_args(&svec(&["--corpus", "--naked", "hi"])).unwrap_err();
        assert!(err.contains("--corpus needs a corpus id"), "{err}");
    }

    #[test]
    fn corpus_with_conversation_is_refused_not_ignored() {
        let err =
            parse_ask_args(&svec(&["--conversation", "c1", "--corpus", "sep", "hi"])).unwrap_err();
        assert_eq!(err, CORPUS_WITH_CONVERSATION);
    }

    #[test]
    fn the_other_flags_still_parse() {
        let p = parse_ask_args(&svec(&[
            "--format",
            "json",
            "--show-reasoning",
            "--naked",
            "--conversation",
            "c1",
            "q",
        ]))
        .unwrap();
        assert_eq!(p.format, OutputFormat::Json);
        assert!(p.show_reasoning && p.naked);
        assert_eq!(p.conversation_id.as_deref(), Some("c1"));
        assert_eq!(p.question.as_deref(), Some("q"));
    }
}
