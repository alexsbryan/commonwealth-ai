// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn-driver census — sv-surface rung 0's bar: no hand-drained turn
//! streams in the desktop host (`quality/campaigns/sv-surface.toml`, ladder
//! row 0).
//!
//! # The state this makes unrepresentable
//!
//! A turn-shaped command that drains its own stream handle. Since R5 the
//! drain is `pump_wire_frames` reading the daemon's turn socket through
//! the client family, and the desktop renders the frames it forwards
//! through the ONE renderer (`render_turn_frames`). A hand-rolled drain
//! loop is how the desktop's `redirect_turn` and `resume_session` shipped
//! WITHOUT `present_answer` envelope stripping and the graceful-guard
//! render the plain path had: a re-derived loop reproduces the gaps of the
//! loop it re-derives — and a drain that owns only PART of the frame
//! stream drops the kinds it does not know about (RB3).
//!
//! # Named, not silently ignored
//!
//! `commands/models.rs` still drains a provider stream (`stream.next().await`
//! at its test-chat): that is the MODEL TEST path against the inference
//! provider, not a conversation turn, and it belongs to rung 2's family
//! (the models one-decider rung), where it is enumerated. This census
//! scopes to turn streams in `commands/chat.rs`.
//!
//! Watched to fail: add a second reader of the turn socket in chat.rs, or
//! drop a wire-drive call site, and this goes red naming the offender.
//! Sabotage-verified at landing (a planted drain, watched red, reverted).

use std::path::Path;

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The census pinned `stream.next().await` — a spelling the WIRE client
/// cannot produce (its reader is `TurnStream::next_frame`). So it stayed
/// green while chat.rs held TWO readers of one socket: `finish_wire_turn`
/// ran a private lead loop to `TurnStarted` and `pump_wire_frames` read
/// everything after. That split is exactly what dropped a `Prompt`
/// preceding `TurnStarted` (review RB3/T4). Pinning the reader the client
/// actually has is what makes a planted second drain go red.
#[test]
fn exactly_one_reader_of_the_turn_socket_in_chat_commands() {
    let src = read("src/commands/chat.rs");
    assert_eq!(
        src.match_indices("next_frame().await").count(),
        1,
        "sv-surface rung 0 / RB3: the turn socket has exactly ONE reader \
         in commands/chat.rs — the loop in `pump_wire_frames`, which \
         raises every card and decides the sync id in the same pass. A \
         second reader splits the frame stream, and whichever half does \
         not own a frame kind silently drops it: that is how a leading \
         `Prompt` reached a renderer that ignores prompts and every \
         agentic ask hung."
    );
    assert_eq!(
        src.match_indices("stream.next().await").count(),
        0,
        "a raw futures drain is back in commands/chat.rs; the client \
         family's `next_frame` is the reader"
    );
    // The one reader is inside the pump, not inside the command that
    // waits for the id: `finish_wire_turn` learns the sync id over a
    // channel, it does not read the socket itself.
    let pump = src
        .split_once("async fn pump_wire_frames(")
        .expect("pump_wire_frames is the reader")
        .1;
    assert_eq!(
        pump.match_indices("next_frame().await").count(),
        1,
        "the ONE reader lives in `pump_wire_frames`"
    );
}

#[test]
fn the_richer_acquires_go_through_the_one_drive() {
    let src = read("src/commands/chat.rs");
    // sv-surface R5: the richer acquires (redirect, resume) cross the WIRE
    // now — one `send_redirect` and one `send_resume`, each through the
    // shared `finish_wire_turn` driver. The in-process `drive_stream_handle`
    // call sites are gone with the local drive; the daemon side of both
    // acquires is pinned by sovereign-mesh's turn_surface tests.
    assert_eq!(
        src.match_indices("send_redirect(").count(),
        1,
        "exactly one redirect, through the wire"
    );
    assert_eq!(
        src.match_indices("send_resume(").count(),
        1,
        "exactly one resume, through the wire"
    );
    assert_eq!(
        src.match_indices("drive_stream_handle(").count(),
        0,
        "the in-process richer-acquire drive is deleted from this surface"
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
            && !src.contains("struct DesktopTurnSink")
            && !src.contains("DesktopTurnSink {"),
        "the in-process drive is deleted from this surface; `cancel_stream`'s \
         session cancel remains in-process until the wire grows a cancel \
         (G2), and it is not a turn driver"
    );
    assert!(
        src.contains("connect_with("),
        "the streaming commands open the turn socket through the client family"
    );
}
