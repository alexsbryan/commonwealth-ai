// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn-driver census — sv-surface rung 0's bar: no hand-drained turn
//! streams in the desktop host (`quality/campaigns/sv-surface.toml`, ladder
//! row 0).
//!
//! # The state this makes unrepresentable
//!
//! A turn-shaped command that drains its own stream handle. The one driver
//! owns the drain — `serve_turn` for a plain turn, `drive_stream_handle`
//! for a caller that acquires through a richer path (redirect/resume) —
//! and the desktop renders the resulting frames through the ONE renderer
//! (`render_turn_frames`). A hand-rolled drain loop is how the desktop's
//! `redirect_turn` and `resume_session` shipped WITHOUT `present_answer`
//! envelope stripping and the graceful-guard render the plain path had:
//! a re-derived loop reproduces the gaps of the loop it re-derives.
//!
//! # Named, not silently ignored
//!
//! `commands/models.rs` still drains a provider stream (`stream.next().await`
//! at its test-chat): that is the MODEL TEST path against the inference
//! provider, not a conversation turn, and it belongs to rung 2's family
//! (the models one-decider rung), where it is enumerated. This census
//! scopes to turn streams in `commands/chat.rs`.
//!
//! Watched to fail: reintroduce a `stream.next().await` drain in chat.rs, or
//! drop a `drive_stream_handle` call site, and this goes red naming the
//! offender. Sabotage-verified at landing (a planted drain, watched red,
//! reverted).

use std::path::Path;

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn no_hand_drained_turn_streams_in_chat_commands() {
    let src = read("src/commands/chat.rs");
    assert_eq!(
        src.match_indices("stream.next().await").count(),
        0,
        "sv-surface rung 0: a hand-drained turn stream is back in \
         commands/chat.rs. The one driver owns the drain — `serve_turn` for a \
         plain turn, `drive_stream_handle` for a richer acquire — and this \
         surface renders through `render_turn_frames`. A private drain loop \
         re-derives the driver AND its gaps."
    );
}

#[test]
fn the_richer_acquires_go_through_the_one_drive() {
    let src = read("src/commands/chat.rs");
    assert_eq!(
        src.match_indices("drive_stream_handle(").count(),
        2,
        "sv-surface rung 0: expected exactly the redirect + resume call \
         sites driving through `drive_stream_handle`. More means a new \
         richer-acquire flow landed without the campaign recording it; fewer \
         means one of the two fell back to a private drain."
    );
}

#[test]
fn every_turn_shaped_command_renders_through_the_one_renderer() {
    let src = read("src/commands/chat.rs");
    // sv-surface R5: ALL FOUR turn-shaped commands (send_message_stream,
    // send_message, redirect_turn, resume_session) drive the WIRE through
    // `finish_wire_turn` — the daemon's turn socket via the client family,
    // in BOTH boot modes ("Local" means the daemon happens to be
    // in-process). The renderer spawns exactly once, inside the shared
    // driver: a command spawning its own `render_turn_frames` is a second
    // driver wearing a command's name.
    assert_eq!(
        src.match_indices("spawn(render_turn_frames(").count(),
        1,
        "the ONE renderer spawn lives in finish_wire_turn; per-command \
         spawns are the rung-0 regression"
    );
    assert_eq!(
        src.match_indices("finish_wire_turn(").count(),
        4,
        "the definition plus three streaming callers (send_message_stream, \
         redirect_turn, resume_session) — the one-shot send_message rides \
         the REST turn and needs no stream"
    );
    assert!(
        !src.contains("sovereign_core::runtime::serve_turn(")
            && !src.contains("drive_stream_handle(")
            && !src.contains("DesktopTurnSink"),
        "the in-process drive is deleted from this surface; `cancel_stream`'s \
         session cancel remains in-process until the wire grows a cancel \
         (G2), and it is not a turn driver"
    );
    assert!(
        src.contains("connect_with("),
        "the streaming commands open the turn socket through the client family"
    );
}
