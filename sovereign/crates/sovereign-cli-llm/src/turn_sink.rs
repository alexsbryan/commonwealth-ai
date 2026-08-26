// SPDX-License-Identifier: AGPL-3.0-or-later
//! A [`TurnSink`] that renders a turn to the terminal as it runs.
//!
//! # Why this exists
//!
//! `svrn govern ask`, `portfolio ask` and `proxy ask` each drove a turn by
//! hand: call `handle_message_stream_as`, drain the chunk stream, print each
//! delta, and catch a refusal to run a different handler. Three copies of a
//! loop `sovereign_core::runtime::serve_turn` already implements — and all
//! three copies carried the SAME bug, which is what makes them worth
//! converting rather than tidying.
//!
//! Each caught `Error::NotImplemented(_)` and fell back to
//! `Runtime::handle_message`. The streaming path persists the user message
//! BEFORE it refuses a document-attached turn, and `handle_message` persists
//! the user message and THEN runs the turn chain — so every one of them wrote
//! the user's question to the conversation twice on that path. `serve_turn`
//! decides that case up front instead, so the fallback cannot be reached with
//! a message already written (ARCH §10.6, §18.3).
//!
//! These hosts keep their in-process `Runtime` — they need a corpus engine to
//! render their sources footer, which is not a turn concern and does not
//! travel on the turn wire. What they stop doing is re-deriving how a turn is
//! driven. That is the bar: ONE implementation of the drive, reached through
//! a sink by an in-process host and through `sovereign-turn-client` by an
//! out-of-process one.

use std::io::Write;
use std::sync::Mutex;

use sovereign_contracts::types::TurnFrame;
use sovereign_core::runtime::TurnSink;

/// Prints token deltas to stdout as they arrive and keeps the full text.
#[derive(Default)]
pub struct StdoutTurnSink {
    text: Mutex<String>,
    error: Mutex<Option<String>>,
}

impl StdoutTurnSink {
    /// The answer accumulated so far.
    pub fn text(&self) -> String {
        self.text.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The turn's failure, if it reported one.
    ///
    /// Returned rather than printed so the caller decides the exit code — a
    /// report tool that prints a half-answer and exits 0 has told the
    /// operator the run succeeded (ARCH §18.3).
    pub fn failure(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl TurnSink for StdoutTurnSink {
    fn emit(&self, frame: TurnFrame) {
        match frame {
            TurnFrame::Token { chunk, .. } => {
                print!("{chunk}");
                let _ = std::io::stdout().flush();
                self.text
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_str(&chunk);
            }
            TurnFrame::StreamError { message, .. } => {
                *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
            }
            // These tools print a sources footer of their own; the terminal
            // frame's projected metadata is not what they render.
            TurnFrame::Complete { .. }
            | TurnFrame::Narration { .. }
            | TurnFrame::QueuePosition { .. } => {}
        }
    }
}
