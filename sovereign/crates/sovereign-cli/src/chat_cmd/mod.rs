//! `sovereign chat ...` — CLI mirror of the desktop chat surface.
//!
//! Why this exists
//! ---------------
//! The desktop chat flow is a load-bearing, multi-stage pipeline:
//!   user turn → intent classification → multi-source retrieval
//!     (conversation-history + folder-* corpora + sep + web) →
//!     prompt assembly → daemon `/v1/chat/completions` stream →
//!     conversation persistence → provenance metadata.
//!
//! When it misbehaves — say, retrieving the wrong sources for a
//! question about a book the corpus doesn't contain — the desktop
//! surface hides too much: the reasoning block is collapsed, the
//! retrieved chunks are tucked behind a disclosure triangle, and the
//! search plan is never surfaced at all. Debugging it means
//! screenshot-copy-pasting out of a GUI.
//!
//! `sovereign chat` runs the same `Runtime::handle_message_stream`
//! path from a terminal, streams the tokens to stdout, and prints
//! the provenance + retrieved chunks + reasoning inline. It shares
//! the daemon-backed bootstrap with `sovereign enrich` (HTTP to
//! `localhost:9741`) — no embedded llama.cpp, no Tauri, just the
//! runtime and the same daemon your desktop app already talks to.
//!
//! The `inspect` subcommand drops the LLM altogether and prints what
//! the retrieval stage would have returned. Use it when the model is
//! flailing and you need to know whether the sources it's quoting
//! are actually what the retrieval picked, or whether the model is
//! hallucinating them.

pub mod ask;
pub mod bootstrap;
pub mod config;
pub mod inspect;
pub mod list;
pub mod render;
pub mod session;
pub mod show;

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign chat",
    summary: "CLI mirror of the desktop chat flow — same Runtime, same retrieval, stdout-friendly.",
    sections: &[
        HelpSection::Usage("sovereign chat <subcommand> [args]"),
        HelpSection::SubcommandsTitled(
            "Primary",
            &[
                ("ask",     "One-shot: ask a question, stream the answer, print provenance."),
                ("session", "Interactive REPL over a single persistent conversation."),
            ],
        ),
        HelpSection::SubcommandsTitled(
            "Diagnostics",
            &[
                (
                    "inspect",
                    "Run the retrieval stage WITHOUT the LLM. Prints every corpus searched, \
                     every chunk returned with score + source, and the system prompt the \
                     LLM would have received. This is the tool for 'why did it quote the \
                     wrong book?'",
                ),
            ],
        ),
        HelpSection::SubcommandsTitled(
            "Browse",
            &[
                ("list", "List recent conversations from the state store."),
                ("show", "Dump a conversation's turns + provenance metadata."),
            ],
        ),
        HelpSection::Flags(&[
            ("--daemon <url>",    "Override the daemon base URL (default http://localhost:9741)."),
            ("--data-dir <path>", "State-store root (default: SetupConfig.data.dir, else ~/.sovereign)."),
            ("--chat-model <id>", "Force a specific chat model ID (default: SetupConfig.models.primary stem; fallback to first non-embed /v1/models entry)."),
            ("--embed-model <id>","Force a specific embedding model ID (default: SetupConfig.models.embed stem; fallback to first embedding-like /v1/models entry)."),
            ("--help, -h",        "Show this message."),
        ]),
        HelpSection::Notes(
            "Requires `sovereign daemon` at the configured client port (default 9741). \
             Bootstrap probes /v1/models before any subcommand runs — if the probe fails \
             the command exits 2 with a remediation hint.",
        ),
    ],
};

pub async fn run_chat(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    let first = args[0].as_str();
    if first == "--help" || first == "-h" || first == "help" {
        help::print(&HELP);
        return 0;
    }
    let (cmd, rest) = args.split_first().unwrap();
    match cmd.as_str() {
        "ask" => ask::cmd_ask(rest).await,
        "session" => session::cmd_session(rest).await,
        "inspect" => inspect::cmd_inspect(rest).await,
        "list" => list::cmd_list(rest).await,
        "show" => show::cmd_show(rest).await,
        other => {
            eprintln!("error: unknown subcommand '{other}'");
            eprintln!();
            help::print(&HELP);
            2
        }
    }
}
