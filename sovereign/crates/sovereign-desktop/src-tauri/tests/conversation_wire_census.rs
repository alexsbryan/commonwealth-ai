// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface rung 3's conversation family census: the conversation wire
//! surface belongs to the routes (`sovereign_mesh::turn_http`) and the
//! family client (`sovereign-turn-client`) — the desktop consumes, it does
//! not re-derive.
//!
//! # The state this makes unrepresentable
//!
//! Two retreats, both needled:
//!
//! 1. A hand-rolled conversation HTTP client — any `/v1/conversations` URL
//!    literal in the desktop's Rust is someone re-minting what
//!    `sovereign_turn_client::TurnClient` already speaks. When rung 6
//!    converts the desktop to a pure client, it imports the family client;
//!    a local URL const is the private-client twin the sv-one-client bar
//!    exists to kill.
//! 2. A local mirror of the routes' wire envelopes. The rung deleted the
//!    two field-identical mirrors (`ConversationEntry`,
//!    `CreateConversationResponse` — mod.rs now re-exports the wire types,
//!    the rung-4 reading pattern); a re-defined
//!    `ConversationListEntry`/`ConversationListResponse`/
//!    `ConversationResponse` in the façade would be the mirror coming back.
//!
//! NOT needled, deliberately: `ConversationDetail`, the local
//! `MessageEntry`, `MessageResponse` and `TaskSummary` in `mod.rs` are the
//! desktop's in-process IPC contract (the frontend renders `metadata` raw)
//! and are sanctioned until rung 6 converts the renderer onto the wire
//! projections — their own comment says so. Needling them would fire on a
//! legitimate state.
//!
//! Sabotage-verified at landing: a planted mirror struct, watched red
//! naming the rule, reverted; then a planted `/v1/conversations` URL const,
//! watched red, reverted.

use std::path::{Path, PathBuf};

fn manifest_src(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(path: &PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A struct/enum *definition* starts a line (after optional whitespace and a
/// visibility prefix). English prose — "construct and serialize",
/// "reconstruction" — cannot trip this; only a real definition does. (The
/// rung-4 reading census' detector, verbatim: its first draft counted prose
/// and false-fired on the word "construct".)
fn is_definition(line: &str) -> bool {
    let mut rest = line.trim_start();
    for vis in ["pub(crate) ", "pub(super) ", "pub "] {
        if let Some(stripped) = rest.strip_prefix(vis) {
            rest = stripped;
            break;
        }
    }
    rest.starts_with("struct ") || rest.starts_with("enum ")
}

/// The wire-envelope names the routes own. A definition of any of these in
/// the desktop's commands façade is a mirror of a schema that already has a
/// home.
const WIRE_ENVELOPES: &[&str] = &[
    "ConversationListResponse",
    "ConversationListEntry",
    "ConversationResponse",
];

#[test]
fn the_conversation_surface_defines_no_wire_envelope() {
    for file in ["src/commands/mod.rs", "src/commands/conversation.rs"] {
        let path = manifest_src(file);
        let src = read(&path);
        let banned: Vec<&str> = src
            .lines()
            .filter(|l| is_definition(l))
            .filter(|l| {
                WIRE_ENVELOPES
                    .iter()
                    .any(|name| l.contains(&format!("struct {name}")))
            })
            .collect();
        assert!(
            banned.is_empty(),
            "sv-surface rung 3: a wire envelope is defined in {file} again \
             ({banned:?}). The conversation list/create shapes ARE the \
             served schema — `sovereign_contracts::daemon_wire`'s types, \
             which `turn_http` emits and `mod.rs` re-exports; a \
             local definition is a second spelling of a served schema, and \
             hand-kept compatibility is not compatibility (the rung-4 \
             lesson)."
        );
    }

    // The positive half: the façade actually re-exports the wire types, so
    // the rule above cannot pass by accident of the shapes moving somewhere
    // less watched.
    let src = read(&manifest_src("src/commands/mod.rs"));
    assert!(
        src.contains("sovereign_contracts::daemon_wire::ConversationListEntry"),
        "the conversation list entry must be the wire type, re-exported. It \
         moved out of `sovereign_mesh::turn_http` at sv-surface svt-3 — same \
         definition, same bytes, one layer down — so this pin follows it \
         rather than the desktop growing a twin."
    );
}

#[test]
fn the_desktop_holds_no_conversation_route_urls() {
    /// Every `.rs` under `src/`, recursively — the URL needle must not be
    /// escapable by moving to a new file.
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    rust_files(&manifest_src("src"), &mut files);

    // The route families sovereign-turn-client speaks (rung 6 widened this
    // from `/v1/conversations` alone: the memory/insight/notes repoints
    // arrived, and a planted hand-rolled `/v1/memories/` URL sailed through
    // the old needle GREEN — §18.1, the check now names the input that beat
    // it). reading.rs's `/internal/corpus/*` daemon reads predate the client
    // family and stay out of scope here; their own module is the recorded
    // exception, not a silent one.
    const NEEDLES: &[&str] = &[
        "/v1/conversations",
        "/v1/memories",
        "/v1/insights",
        "/v1/notes",
    ];

    let mut offenders: Vec<String> = Vec::new();
    for path in files {
        let src = read(&path);
        for (lineno, line) in src.lines().enumerate() {
            // Prose lines are skipped: a doc comment that MENTIONS the
            // route is documentation, not a client. Everything else uses
            // the substring-anywhere calibration — a `"/v1/…"`-quoted
            // needle let a planted `http://…:9741/v1/…` const sail
            // through GREEN at rung 2 (§18.1), and the substring form
            // still fires on trailing comments after code.
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in NEEDLES {
                if line.contains(needle) {
                    offenders.push(format!(
                        "{}:{}: {}",
                        path.strip_prefix(manifest_src(""))
                            .unwrap_or(&path)
                            .display(),
                        lineno + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "sv-surface: the desktop carries hand-rolled daemon route URLs — \
         {offenders:#?}. The wire surface is consumed through \
         sovereign-turn-client (the family client); a local URL is a private \
         client minting itself."
    );
}
