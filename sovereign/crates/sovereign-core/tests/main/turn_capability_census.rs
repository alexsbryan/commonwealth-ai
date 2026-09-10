// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sv-surface G7/C6 falsifier: **no site under `src/runtime/` reads a
//! per-turn capability off the `Runtime` member.**
//!
//! `runtime/capabilities.rs` installs two task-local capabilities around a
//! turn — the approval channel and the routing-event sink — and publishes one
//! accessor for each: `Runtime::turn_approval()` and
//! `Runtime::turn_routing_events()`. Both fall back to the commissioned member
//! when no host installed a scope, so an in-process host reads exactly what it
//! read before. That fallback is why the defect is SILENT: reading
//! `self.routing_events` compiles, passes every in-process test, and works in
//! the desktop — and on the daemon resolves to `NoOpRoutingEventSink`
//! (`runtime.rs` ~558), where the clarification card and the narration chips
//! are dropped on the floor. The user sees an empty answer with no card while
//! the identical in-process turn raises one.
//!
//! Commit 7e12f7006 claimed "all 21 read sites in core now read the
//! capability". Twelve did not (`ask_move` ×4, `attached_doc` ×4,
//! `document_op` ×2, `knowledge_query` ×2), and nothing went red. Hence this
//! file: the claim is now machine-checked (ARCH §11.1, §7 — make it
//! structural, not remembered).
//!
//! ## Named failing input (ARCH §18.1)
//!
//! Write `self.routing_events.emit_turn_narration(..)` or
//! `self.approval.as_ref()` anywhere under `src/runtime/` outside
//! `capabilities.rs` and this test fails, naming the file and line. That is
//! the edit a reflex copy-paste from a neighbouring handler makes, which is
//! the whole reason the bar is scanned rather than written down.

use super::runtime_source_scan::{assert_corpus_is_whole, render, scan};

/// The two per-turn capabilities. Reading one of these off `self` is the
/// defect; calling `self.turn_approval()` / `self.turn_routing_events()` is
/// the fix.
const CAPABILITY_FIELDS: &[&str] = &["approval", "routing_events"];

/// The ONE file allowed to name them: the decider that chooses between the
/// turn's capability and the commissioned member.
const DECIDER: &str = "capabilities.rs";

/// ARCH §18.4 — the size term. A scan that read nothing publishes the same
/// zero a real green does, and zero is exactly this file's claim.
#[test]
fn the_scanner_reads_the_whole_runtime_tree() {
    assert_corpus_is_whole();
}

/// The second half of the instrument check: prove the scan finds reads it is
/// NOT looking for. `self.inference` and `self.store` are core members read
/// all over the runtime and no capability work touches them, so a scan that
/// cannot see THEM cannot be trusted to see a capability read either.
#[test]
fn the_scanner_finds_core_reads_it_is_not_looking_for() {
    let control = scan(&["inference", "store"], &[]);
    assert!(
        control.len() > 10,
        "instrument check failed: only {} `self.inference` / `self.store` \
         read(s) found under src/runtime/, which cannot be right — the \
         scanner is not matching, so its zero below would be meaningless",
        control.len()
    );
}

/// The bar itself: 12 → 0.
#[test]
fn no_runtime_site_reads_a_per_turn_capability_off_the_runtime_member() {
    let hits = scan(CAPABILITY_FIELDS, &[DECIDER]);
    assert!(
        hits.is_empty(),
        "sv-surface G7/C6: {} process-wide capability read(s) are back. On the \
         daemon these resolve to the no-op sink / the auto-granting host \
         channel, so the clarification card, the narration chips and the \
         MessageRefined event are silently dropped on the wire while the \
         in-process turn works. Call `self.turn_routing_events()` / \
         `self.turn_approval()` instead — read in the turn's own task, BEFORE \
         any `tokio::spawn` that carries the result:\n{}",
        hits.len(),
        render(&hits)
    );
}

/// The decider must actually BE one. If someone deletes the
/// `unwrap_or_else(|| Arc::clone(&self.<member>))` fallback, every host that
/// installs no scope — the desktop in-process, the server, `svrn chat`, and
/// every test in this crate — loses its channel, and the census above would
/// still read a clean zero. Two reads, one per capability, in `capabilities.rs`
/// and nowhere else.
#[test]
fn the_decider_still_falls_back_to_the_commissioned_member() {
    let hits = scan(CAPABILITY_FIELDS, &[]);
    assert_eq!(
        hits.len(),
        CAPABILITY_FIELDS.len(),
        "expected exactly one commissioned-member read per capability, all in \
         {DECIDER}; the fallback in `turn_approval` / `turn_routing_events` is \
         what makes every unscoped host behaviour-preserving. Found:\n{}",
        render(&hits)
    );
    for (file, line, text) in &hits {
        assert!(
            file.ends_with(DECIDER),
            "{file}:{line} reads a capability member outside the decider: {text}"
        );
    }
}
