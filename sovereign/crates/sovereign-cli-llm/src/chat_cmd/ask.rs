//! `sovereign chat ask "<question>"` — one-shot streaming turn.
//!
//! Same shape as the desktop's `sendMessageStream` flow:
//!   1. Bootstrap the Runtime.
//!   2. Call `handle_message_stream(message, conversation_id)`.
//!   3. Drain the chunk stream straight to stdout.
//!   4. Read the persisted assistant message back out of the store
//!      so we can render provenance + retrieved chunks + reasoning.
//!
//! The stream-and-read pattern (rather than buffering the whole
//! answer in RAM) matters for the `<think>...</think>` traces:
//! reasoning-heavy models stream thousands of tokens before the
//! first visible answer character. Users should see progress
//! immediately, not after 30 s of apparent silence.

use std::io::{self, Write};

use futures::StreamExt;
use serde_json::json;

use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use crate::chat_cmd::render;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign chat ask",
    summary: "Ask a question once. Streams the answer, then prints provenance.",
    sections: &[
        HelpSection::Usage("sovereign chat ask \"<question>\" [flags]"),
        HelpSection::Flags(&[
            (
                "--conversation <id>",
                "Reuse an existing conversation id (default: fresh uuid).",
            ),
            (
                "--format text|json",
                "Output format. `json` dumps the full message + metadata.",
            ),
            (
                "--show-reasoning",
                "Render <think> blocks inline instead of a collapsed handle.",
            ),
            ("--help, -h", "Show this message."),
        ]),
        HelpSection::Notes(
            "The question is taken from the first non-flag positional argument. \
             Wrap it in quotes. Prints a `▶ reasoning ...` line after the answer \
             summarizing how much was hidden (override with `--show-reasoning`).",
        ),
    ],
};

pub async fn cmd_ask(args: &[String]) -> i32 {
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

    let mut question: Option<String> = None;
    let mut conversation_id: Option<String> = None;
    let mut format = OutputFormat::Text;
    let mut show_reasoning = false;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--conversation" => {
                i += 1;
                conversation_id = rest.get(i).cloned();
            }
            "--format" => {
                i += 1;
                match rest.get(i).map(String::as_str) {
                    Some("text") => format = OutputFormat::Text,
                    Some("json") => format = OutputFormat::Json,
                    Some(other) => {
                        eprintln!("error: --format expects text|json, got `{other}`");
                        return 2;
                    }
                    None => {
                        eprintln!("error: --format needs a value");
                        return 2;
                    }
                }
            }
            "--show-reasoning" => {
                show_reasoning = true;
            }
            arg if question.is_none() => {
                question = Some(arg.to_string());
            }
            extra => {
                eprintln!("error: unexpected argument `{extra}`");
                return 2;
            }
        }
        i += 1;
    }

    let Some(question) = question else {
        eprintln!("error: missing question. Usage: sovereign chat ask \"<question>\"");
        return 2;
    };

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    let conversation_id = conversation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let exit = run_turn(
        &session,
        &question,
        &conversation_id,
        format,
        show_reasoning,
    )
    .await;
    exit
}

#[derive(Copy, Clone, Debug)]
enum OutputFormat {
    Text,
    Json,
}

async fn run_turn(
    session: &ChatSession,
    question: &str,
    conversation_id: &str,
    format: OutputFormat,
    show_reasoning: bool,
) -> i32 {
    eprintln!();
    eprintln!("{BAR}");
    eprintln!("conversation: {conversation_id}");
    eprintln!("> {question}");
    eprintln!("{BAR}");

    let handle = match session
        .runtime
        .handle_message_stream(question, conversation_id)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("stream start failed: {e}");
            return 1;
        }
    };

    let message_id = handle.message_id.clone();
    let mut stream = handle.stream;
    let mut raw = String::new();
    let mut stdout = io::stdout().lock();

    // In `text` mode we stream chunks as they arrive. In `json` mode
    // the user wants one structured payload at the end, so we buffer
    // silently and dump on stream close.
    let echo_live = matches!(format, OutputFormat::Text);
    if echo_live {
        let _ = writeln!(stdout);
    }

    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                raw.push_str(&chunk);
                if echo_live {
                    let _ = stdout.write_all(chunk.as_bytes());
                    let _ = stdout.flush();
                }
            }
            Err(e) => {
                if echo_live {
                    let _ = writeln!(stdout);
                }
                eprintln!("stream error: {e}");
                return 1;
            }
        }
    }
    if echo_live {
        let _ = writeln!(stdout);
    }

    // Metadata is persisted by `handle_message_stream` after the
    // stream closes — fetch the saved row so we can render the full
    // provenance block exactly like the desktop does.
    let metadata = session
        .store
        .get_conversation(conversation_id)
        .await
        .ok()
        .and_then(|c| {
            c.messages
                .iter()
                .find(|m| m.id == message_id)
                .and_then(|m| m.metadata.clone())
        });

    match format {
        OutputFormat::Text => render_text(&raw, metadata.as_ref(), show_reasoning),
        OutputFormat::Json => render_json(&message_id, conversation_id, &raw, metadata.as_ref()),
    }

    0
}

fn render_text(raw: &str, metadata: Option<&serde_json::Value>, show_reasoning: bool) {
    let (reasoning, _visible) = render::split_reasoning(raw);
    // The `visible` portion is already on screen via the live-echo
    // path — no need to re-print it. We only append the summary
    // metadata below the answer.
    eprintln!();
    eprintln!("{BAR}");
    let header = render::provenance_header(metadata);
    if !header.is_empty() {
        eprintln!("{header}");
    }
    let reasoning_out = render::render_reasoning(&reasoning, show_reasoning);
    if !reasoning_out.is_empty() {
        eprintln!("{reasoning_out}");
    }
    let footer = render::retrieved_chunks_footer(metadata);
    if !footer.is_empty() {
        eprint!("{footer}");
    }
    eprintln!("{BAR}");
}

fn render_json(
    message_id: &str,
    conversation_id: &str,
    raw: &str,
    metadata: Option<&serde_json::Value>,
) {
    let (reasoning, visible) = render::split_reasoning(raw);
    let payload = json!({
        "message_id": message_id,
        "conversation_id": conversation_id,
        "raw": raw,
        "visible": visible,
        "reasoning": reasoning,
        "metadata": metadata,
    });
    // Print JSON on stdout so it's pipe-friendly; the conversational
    // chrome stays on stderr.
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
}

const BAR: &str = "─────────────────────────────────────────────────────────────";
