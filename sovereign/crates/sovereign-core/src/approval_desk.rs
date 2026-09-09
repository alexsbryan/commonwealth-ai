// SPDX-License-Identifier: AGPL-3.0-or-later
//! The half every [`ApprovalChannel`](crate::traits::ApprovalChannel) has in
//! common: hold the executor's question until someone answers it.
//!
//! An approval channel does two things. It SHOWS the question — a Tauri
//! event, a broadcast to subscribers, a frame down one socket, a terminal
//! prompt — and that half is genuinely per-host. Then it parks the executor on
//! a `oneshot` and lets the host resolve it by key, and that half had no
//! host-specific content and was written out twice: `sovereign-server`'s
//! channel and the desktop's, each with its own pair of
//! `HashMap<String, oneshot::Sender<_>>` and its own copy of the
//! `"{task_id}:{step_id}"` format, in two crates that cannot see each other.
//! Two spellings of one key is ARCH §10.6's smell exactly.
//!
//! The KEY stays the host's — hence the type parameter. The server and the
//! desktop address a question by a host-stamped task slot, which decouples the
//! id their UI shows from the executor's internal `task.id`; the daemon's
//! per-turn channel needs no conversation in its key at all, because the
//! channel belongs to one socket and there is no shared map to collide in.
//! A generic parameter lets both be true without a second desk.
//!
//! Nothing is ever answered by default: a dropped sender surfaces as
//! [`Error::Cancelled`], never as `false` or `""`. An invented consent is the
//! substitution ARCH §18.3 is about, and the executor cannot tell one from a
//! real answer.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::error::{Error, Result};
use crate::types::StepOutput;

/// What a resolve attempt did. Named rather than collapsed into a bool: an
/// answerer that heard nothing back cannot tell "accepted" from "never
/// arrived" (ARCH §18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The question was found and the executor is running again.
    Resolved,
    /// Nothing is parked under that key — already answered, the turn ended,
    /// or the key names something this answerer does not own.
    NoSuchPending,
    /// A question IS parked there, of the other kind. The entry is left
    /// alone: a wrong-kind answer must not consume the question the right one
    /// is still coming for.
    WrongKind,
}

impl ResolveOutcome {
    /// The `bool` the pre-desk `submit_*` methods returned — "did this reach
    /// something". Kept so a host's own wire contract need not change to sit
    /// on the desk.
    pub fn reached_a_question(self) -> bool {
        self == ResolveOutcome::Resolved
    }
}

/// A question the executor is blocked on, and the channel that unblocks it.
enum Pending {
    Approval(oneshot::Sender<bool>),
    Input(oneshot::Sender<String>),
}

impl Pending {
    /// What is actually waiting there, for the wrong-kind trace.
    fn kind(&self) -> &'static str {
        match self {
            Pending::Approval(_) => "approval",
            Pending::Input(_) => "user reply",
        }
    }
}

/// A question that has been parked and not yet shown.
///
/// Parking BEFORE showing closes the window where a fast answerer replies to a
/// question the desk has not recorded yet. The host parks, shows, then awaits.
#[must_use = "a parked question that is never awaited leaves the executor blocked"]
pub struct Parked<T> {
    rx: oneshot::Receiver<T>,
}

impl<T> Parked<T> {
    /// Wait for the answer. `Err(Cancelled)` when the sender was dropped —
    /// the host went away, the desk cancelled it, or the turn was aborted.
    pub async fn answered(self) -> Result<T> {
        self.rx.await.map_err(|_| Error::Cancelled)
    }
}

/// The park-and-resolve machinery behind an `ApprovalChannel`. `K` is the
/// host's own address for a question — see the module docs.
pub struct ApprovalDesk<K> {
    /// `std::sync::Mutex`, never held across an `await`: every operation is a
    /// map lookup plus a non-blocking `oneshot` send.
    pending: Mutex<HashMap<K, Pending>>,
}

impl<K> Default for ApprovalDesk<K> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash + Clone> ApprovalDesk<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Park a step approval under `key`.
    pub fn park_approval(&self, key: K) -> Parked<bool> {
        let (tx, rx) = oneshot::channel();
        self.insert(key, Pending::Approval(tx));
        Parked { rx }
    }

    /// Park an `ask_user` question under `key`.
    pub fn park_input(&self, key: K) -> Parked<String> {
        let (tx, rx) = oneshot::channel();
        self.insert(key, Pending::Input(tx));
        Parked { rx }
    }

    fn insert(&self, key: K, pending: Pending) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, pending);
    }

    /// Answer a parked approval.
    pub fn resolve_approval(&self, key: &K, approved: bool) -> ResolveOutcome {
        self.resolve(key, |pending| match pending {
            Pending::Approval(tx) => {
                let _ = tx.send(approved);
                Ok(())
            }
            other => Err(other),
        })
    }

    /// Answer a parked `ask_user` question.
    pub fn resolve_input(&self, key: &K, content: String) -> ResolveOutcome {
        self.resolve(key, |pending| match pending {
            Pending::Input(tx) => {
                let _ = tx.send(content);
                Ok(())
            }
            other => Err(other),
        })
    }

    /// Take the entry out and hand it to `answer`, which either answers it or
    /// hands it BACK. One decision about kind, made in the closure that owns
    /// the value, and the lock held across it so a restore cannot race a
    /// cancel.
    fn resolve(
        &self,
        key: &K,
        answer: impl FnOnce(Pending) -> std::result::Result<(), Pending>,
    ) -> ResolveOutcome {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = pending.remove(key) else {
            return ResolveOutcome::NoSuchPending;
        };
        match answer(entry) {
            Ok(()) => ResolveOutcome::Resolved,
            Err(entry) => {
                // Glassbox: a wrong-kind answer means a client aimed a reply
                // at the wrong question, and the outcome it gets back reads
                // the same as a stale one from the host side (ARCH §9.1).
                let parked_kind = entry.kind();
                pending.insert(key.clone(), entry);
                drop(pending);
                tracing::debug!(
                    parked_kind,
                    "approval_desk: answer is the wrong kind for the parked question"
                );
                ResolveOutcome::WrongKind
            }
        }
    }

    /// How many questions are parked.
    ///
    /// Zero is the resting state — every entry is a turn blocked on a human.
    /// A count that only rises is a leak, and this is what makes that
    /// assertable rather than argued.
    pub fn parked(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Drop one parked question, cancelling its waiter. What a host calls when
    /// it could not SHOW the question it just parked.
    pub fn cancel(&self, key: &K) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

/// What became of a step, for `ApprovalChannel::emit_progress`.
///
/// One decider for a match that existed three times — core's
/// `AutoApprovalChannel`, the server's channel and the desktop's — each
/// re-deriving the same three outcomes, and the desktop's spelling the jump
/// differently from the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// The step produced output.
    Done,
    /// The step never ran (an untaken branch arm).
    Skipped,
    /// A `Branch` resolved to a jump; the payload is the step jumped to.
    Jump(usize),
}

impl StepStatus {
    pub fn of(output: &StepOutput) -> Self {
        match output {
            StepOutput::Text(_)
            | StepOutput::Json(_)
            | StepOutput::ReasonWithToolsResult { .. } => StepStatus::Done,
            StepOutput::Jump(target) => StepStatus::Jump(*target),
            StepOutput::Skipped => StepStatus::Skipped,
        }
    }
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepStatus::Done => f.write_str("done"),
            StepStatus::Skipped => f.write_str("skipped"),
            StepStatus::Jump(target) => write!(f, "jump to {target}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_reaches_the_question_parked_under_that_key() {
        let desk = ApprovalDesk::<String>::new();
        let parked = desk.park_approval("t1:3".to_string());
        assert_eq!(desk.parked(), 1);
        assert_eq!(
            desk.resolve_approval(&"t1:3".to_string(), true),
            ResolveOutcome::Resolved
        );
        assert!(parked.answered().await.unwrap());
        assert_eq!(desk.parked(), 0, "a resolved question is not still parked");
    }

    #[tokio::test]
    async fn an_answer_under_another_key_resolves_nothing_and_leaves_the_question() {
        let desk = ApprovalDesk::<String>::new();
        let parked = desk.park_approval("t1:3".to_string());
        assert_eq!(
            desk.resolve_approval(&"t2:3".to_string(), true),
            ResolveOutcome::NoSuchPending
        );
        assert_eq!(desk.parked(), 1, "the real question is still waiting");
        desk.resolve_approval(&"t1:3".to_string(), false);
        assert!(!parked.answered().await.unwrap());
    }

    #[tokio::test]
    async fn a_wrong_kind_answer_is_refused_and_does_not_consume_the_question() {
        let desk = ApprovalDesk::<String>::new();
        let parked = desk.park_input("t1:input".to_string());
        assert_eq!(
            desk.resolve_approval(&"t1:input".to_string(), true),
            ResolveOutcome::WrongKind
        );
        assert_eq!(desk.parked(), 1, "the question survived the wrong answer");
        desk.resolve_input(&"t1:input".to_string(), "main".to_string());
        assert_eq!(parked.answered().await.unwrap(), "main");
    }

    #[tokio::test]
    async fn a_cancelled_question_reports_cancelled_rather_than_a_default() {
        let desk = ApprovalDesk::<String>::new();
        let parked = desk.park_approval("t1:3".to_string());
        desk.cancel(&"t1:3".to_string());
        assert_eq!(desk.parked(), 0);
        // NOT `Ok(false)`: a cancelled consent question was never answered,
        // and a `false` here would be an invented refusal (ARCH §18.3).
        assert!(matches!(parked.answered().await, Err(Error::Cancelled)));
    }

    #[test]
    fn step_status_spells_each_outcome_one_way() {
        assert_eq!(
            StepStatus::of(&StepOutput::Text("x".into())).to_string(),
            "done"
        );
        assert_eq!(StepStatus::of(&StepOutput::Skipped).to_string(), "skipped");
        assert_eq!(
            StepStatus::of(&StepOutput::Jump(4)).to_string(),
            "jump to 4"
        );
    }
}
