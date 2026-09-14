# STREAMING SEND POLICY LIVES IN StreamSink, AND NOWHERE ELSE (nc-17, 2026-08-21, commit 62bdf2dc).

STREAMING SEND POLICY LIVES IN `StreamSink`, AND NOWHERE ELSE (nc-17, 2026-08-21, commit 62bdf2dc).

Before this rung `sovereign-inference/src/embedded/model_slot.rs` held THREE implementations of "deliver one stream token":
  1. `send_stream_piece` — deadline-bounded via `try_send` + poll, on the legacy `Result<String>` channel;
  2. `StreamSink::send_piece` — a bare `blocking_send`, on the typed `StreamFrame` channel (MTP loop);
  3. two more bare `blocking_send`s inline in `stream_generate_loop` (the single-token loop).

ONLY (1) was hardened — and (1) was the half nothing streamed on any more, because `EmbeddedLlamaCpp::complete_stream` had become a 285-line copy of `complete_stream_with_finish`'s routing and the typed path served every streaming chat completion. So the fix for the half-open SSE client that pins a slot *indefinitely* and takes the node out (MESH_SCALE_100_USERS_1000_CORPORA.md §7.2), together with its RED-FIRST test `a_consumer_that_stops_reading_frees_the_slot_within_the_deadline`, was guarding dead code.

INVARIANT NOW: every decode loop sends through `StreamSink`, which owns the deadline (`StreamSink::new` is the ONE place it is derived, from `inference_deadline_secs()`). `send_piece` returns `StreamSend::{Sent, ReceiverGone, DeadlineExceeded}` and a non-`Sent` outcome means STOP DECODING and DO NOT send a terminal frame — the channel is full, so a `Finish` would park on the same wall the token just hit. `stalled_consumer_abort` is the one spelling of the abort (clear KV, free the slot, return `Err`). The MTP loop carries the same rule via its `consumer_stalled` flag.

DO NOT reintroduce a raw `tx.blocking_send(StreamFrame::Token(..))` in a decode loop. It compiles, it passes, and it re-opens a node-level outage that only shows up under a suspended browser tab.

The liveness tests were retargeted onto `StreamSink` and WATCHED TO FAIL against the pre-fix `blocking_send` (pass 2 fail 2, the stall test times out on the pin) before being taken as green (410/410).
