// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn protocol — what a client and a serving host say to each other
//! while one turn runs.
//!
//! # Why this is not in the server
//!
//! Until 2026-08-25 this vocabulary was `ServerEvent`, a private module of
//! the `sovereign-server` **binary** — a crate nothing can depend on. That
//! made it the only turn protocol in the tree and simultaneously
//! unspeakable by any other process, which is why `TOPOLOGY.md §3.5` draws
//! the server as a *surface* speaking a protocol to the daemon while the
//! surface in fact owned it. Phase 5b moved it here, to the DTO crate that
//! sits below all three hosts (note `d91de4b1`).
//!
//! # The split, and the state it removes
//!
//! `ServerEvent` was one enum with two transports, and the rule keeping
//! them apart was a doc comment:
//!
//! > `StepDone` / `ApprovalReq` / `UserInput` are genuinely fan-out
//! > (broadcast across connections). The streaming variants `Token` /
//! > `Complete` / `StreamError` are NOT broadcast — `ws.rs` sends them
//! > down the single requesting socket, because tokens are per-turn and
//! > per-tenant and must never fan to another client's connection.
//!
//! One type on both channels means `broadcast_tx.send(ServerEvent::Token
//! { .. })` compiles — a tenant's answer delivered to every other
//! connected client, prevented only by everyone remembering. [`TurnFrame`]
//! is the per-turn half and has no fan-out constructor to reach: the
//! executor's fan-out events keep their own type in the host that owns an
//! executor, and the two channels no longer have a type in common. Per
//! ARCH §7 the invariant is structural rather than remembered.
//!
//! # Wire form
//!
//! Externally tagged as `{"type": "<variant>", "data": {...}}` with
//! snake_case variant names — byte-identical to what `ServerEvent`
//! emitted, because the mobile client already speaks it. `tests/
//! turn_wire_form.rs` pins each variant's bytes; changing them is a
//! client-visible protocol change, not a refactor.

use serde::{Deserialize, Serialize};

use crate::types::epistemic::EpistemicState;
use crate::types::narration::NarrationPhase;
use crate::types::projection::{Citation, Provenance};

/// Host → client, for ONE turn, down the ONE connection that asked for it.
///
/// Never fan-out. Every variant carries something scoped to a single
/// tenant's in-flight turn — its tokens, its queue position, its terminal
/// metadata — so a frame delivered to a second connection is a leak, not a
/// duplicate. See the module docs for why that is now a type error rather
/// than a convention.
///
/// Variants are added when a corresponding emit site exists. Don't add
/// speculative variants — they break exhaustiveness for downstream
/// consumers without ever firing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum TurnFrame {
    /// One streamed token delta for an assistant message. Emitted once
    /// per chunk as the host synthesizes the response.
    Token {
        /// The assistant message this delta belongs to.
        message_id: String,
        /// The delta itself — append it to what came before.
        chunk: String,
    },
    /// Terminal frame, sent after the stream is exhausted and the host has
    /// persisted the assistant message. Carries the projected provenance +
    /// corpus-grounded citations for the completed message (see
    /// [`crate::types::projection`]).
    Complete {
        /// The assistant message that just finished.
        message_id: String,
        /// How the answer was produced — model, routing tier, latency.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<Provenance>,
        /// Corpus-grounded citations, in retrieval rank order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
        /// The typed epistemic ledger (EPISTEMIC_STATE.md), when the turn
        /// stamped one. `None` on old messages / kill switch off. I2-C
        /// closes the wire gap; mobile rendering stays deferred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epistemic_state: Option<EpistemicState>,
    },
    /// A streaming turn failed, or the host was busy. `retry_after_secs`
    /// is set on the busy case so the client mirrors REST `503` behaviour
    /// (the "host busy" connectivity state) rather than a generic error.
    StreamError {
        /// What went wrong, in the host's words.
        message: String,
        /// Seconds to wait before retrying — the shed case only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_secs: Option<u64>,
    },
    /// A glassbox progress signal for the in-flight turn: a phase the
    /// runtime entered or completed (retrieval, synthesis, gap check, tool
    /// call), forwarded from the runtime's narration channel. Lets the
    /// client show what the host is actually doing before and while the
    /// answer streams — the desktop-parity "process handles".
    Narration {
        /// The assistant message this turn is producing. Empty for
        /// narration emitted before the stream handle is acquired.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message_id: String,
        /// Which phase boundary this marks. Unit variants serialize as a
        /// string (`"retrieval_start"`), struct variants as a single-key
        /// object (`{ "retrieval_complete": { … } }`). The client reads
        /// the key for an icon and falls back gracefully on unknowns.
        ///
        /// Typed as [`NarrationPhase`] rather than a `serde_json::Value`:
        /// the server carried the `Value` because `NarrationPhase` was a
        /// crate it could name but this enum was not. Both live here now,
        /// and the wire form is unchanged — the same derive produces it.
        phase: NarrationPhase,
        /// Human-readable narration text from the runtime (e.g. "Read 12
        /// chunks across sep, wikipedia").
        text: String,
        /// Wall-clock milliseconds since the turn began.
        elapsed_ms: u64,
    },
    /// The host is at capacity and this turn is queued behind others.
    /// Emitted before the turn starts streaming, and again each time it
    /// moves up the line, so the client can render "#k · ~Ns". The turn
    /// still runs to completion once a slot frees — this is *not* a
    /// terminal frame (unlike [`TurnFrame::StreamError`], which is the
    /// shed outcome).
    QueuePosition {
        /// 1-based place in line (1 = next to be served).
        position: u32,
        /// Rough wait estimate (ms), accounting for the parallel decode
        /// slots.
        estimated_wait_ms: u64,
    },
}

/// Client → host, for one turn.
///
/// The inbound half has no fan-out counterpart to be confused with — one
/// connection, one reader — so unlike [`TurnFrame`] it is a single type
/// covering everything a client can say mid-turn, including the two
/// replies that resolve an executor's pending approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum TurnRequest {
    /// Start a turn on this conversation with the given user text.
    Message {
        /// The user's message.
        content: String,
    },
    /// Answer a pending [`crate::types::ui::ActionPreview`] approval.
    Approve {
        /// Task the step belongs to.
        task_id: String,
        /// Step awaiting the decision.
        step_id: usize,
        /// Whether the step may proceed.
        approved: bool,
    },
    /// Answer a pending `ask_user` question.
    UserReply {
        /// Task that asked.
        task_id: String,
        /// The user's answer.
        content: String,
    },
}
