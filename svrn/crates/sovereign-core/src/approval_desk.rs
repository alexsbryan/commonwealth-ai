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

// Owned by sovereign-contracts since sv-surface R1 (2026-09-09):
// `TurnNotice::{StepDone,ResolveAck}` carries these across the wire, and
// a wire type may not mirror a core type (ARCH §10.6). Re-exported here
// so every historical `sovereign_core::approval_desk::` importer is
// unaffected.
pub use sovereign_contracts::types::approval::{ResolveOutcome, StepStatus};

/// A question the executor is blocked on, and the channel that unblocks it.
enum Pending {
    Approval(oneshot::Sender<bool>),
    Input(oneshot::Sender<String>),
    /// A structured information request. `None` is the user's SKIP — a real
    /// answer, which is why this one is not `Input`.
    Information(oneshot::Sender<Option<String>>),
}

impl Pending {
    /// What is actually waiting there, for the wrong-kind trace.
    fn kind(&self) -> &'static str {
        match self {
            Pending::Approval(_) => "approval",
            Pending::Input(_) => "user reply",
            Pending::Information(_) => "information",
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

impl Parked<Option<String>> {
    /// [`Self::answered`] for the information request, whose contract is
    /// `Option` already: a cancelled wait reads as the SKIP the user could
    /// have pressed, and the executor falls through to corpus-only synthesis
    /// either way.
    pub async fn answered_or_skipped(self) -> Option<String> {
        self.rx.await.unwrap_or(None)
    }
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

    /// Park a structured information request under `key`.
    pub fn park_information(&self, key: K) -> Parked<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.insert(key, Pending::Information(tx));
        Parked { rx }
    }

    /// Park under `key`, and SAY SO when that key already held a question.
    ///
    /// The overwrite is kept — the newer question is the live one, and the
    /// displaced waiter learns of it as `Cancelled` when its sender drops,
    /// which is the honest report of what happened to it (§18.3). What was
    /// missing was the trace: a host whose ids collide (a step counter
    /// reused across turns) silently loses a question, and the executor it
    /// belonged to blocks with no record of why (sv-surface S3).
    fn insert(&self, key: K, pending: Pending) {
        let displaced = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, pending);
        if let Some(displaced) = displaced {
            tracing::warn!(
                displaced_kind = displaced.kind(),
                "approval_desk: a new question reused a parked id — the displaced \
                 question's waiter is cancelled"
            );
        }
    }

    /// Answer a parked approval.
    pub fn resolve_approval(&self, key: &K, approved: bool) -> ResolveOutcome {
        self.resolve(key, |pending| match pending {
            Pending::Approval(tx) => Ok(tx.send(approved).is_ok()),
            other => Err(other),
        })
    }

    /// Answer a parked `ask_user` question.
    pub fn resolve_input(&self, key: &K, content: String) -> ResolveOutcome {
        self.resolve(key, |pending| match pending {
            Pending::Input(tx) => Ok(tx.send(content).is_ok()),
            other => Err(other),
        })
    }

    /// Take the entry out and hand it to `answer`, which either answers it or
    /// hands it BACK. One decision about kind, made in the closure that owns
    /// the value, and the lock held across it so a restore cannot race a
    /// cancel.
    ///
    /// `answer` reports `Ok(true)` when the parked waiter actually received
    /// the value and `Ok(false)` when its receiver had already gone. Both
    /// took the entry; only the first restarted an executor, and collapsing
    /// them into one success was the §18.3 defect sv-surface RB2 names.
    fn resolve(
        &self,
        key: &K,
        answer: impl FnOnce(Pending) -> std::result::Result<bool, Pending>,
    ) -> ResolveOutcome {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = pending.remove(key) else {
            return ResolveOutcome::NoSuchPending;
        };
        match answer(entry) {
            Ok(true) => ResolveOutcome::Resolved,
            Ok(false) => {
                drop(pending);
                tracing::debug!(
                    "approval_desk: the answer took the question but its waiter was already gone"
                );
                ResolveOutcome::WaiterGone
            }
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

    /// Answer a parked information request. `None` is the skip.
    pub fn resolve_information(&self, key: &K, content: Option<String>) -> ResolveOutcome {
        self.resolve(key, |pending| match pending {
            Pending::Information(tx) => Ok(tx.send(content).is_ok()),
            other => Err(other),
        })
    }

    /// Answer a parked question with the WIRE's answer shape — the one
    /// mapping from [`TurnAnswer`] to the parked kind (sv-surface R1).
    ///
    /// Every host that faces `TurnRequest::Answer` sits on this rather
    /// than re-deriving the match: two channels writing it twice would
    /// be the second spelling of one dispatch (ARCH §10.6). The
    /// kind-specific `resolve_*` wrappers stay for the hosts whose own
    /// surfaces still speak their local shapes — the desktop's Tauri
    /// channel folds in C2.
    pub fn resolve_answer(
        &self,
        key: &K,
        answer: &sovereign_contracts::types::TurnAnswer,
    ) -> ResolveOutcome {
        match answer {
            sovereign_contracts::types::TurnAnswer::Approved(approved) => {
                self.resolve_approval(key, *approved)
            }
            sovereign_contracts::types::TurnAnswer::Text(content) => {
                self.resolve_input(key, content.clone())
            }
            sovereign_contracts::types::TurnAnswer::Information { content, .. } => {
                self.resolve_information(key, content.clone())
            }
        }
    }

    /// Whether a question is parked under `key` — the honest form of what a
    /// UI asks before spending work on a submission that may be stale.
    pub fn is_parked(&self, key: &K) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(key)
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

    /// Resolve EVERY parked question the way a vanished answerer resolves
    /// it — the hangup policy, applied per prompt KIND (sv-surface E2).
    ///
    /// An approval and an `ask_user` are CANCELLED: their sender drops and
    /// the waiter reads `Err(Cancelled)`, because inventing a consent or a
    /// reply on behalf of somebody who is not there is the §18.3
    /// substitution this whole file exists to remove. An information
    /// request reads as the user's SKIP (`None`) — a real answer, and
    /// `Pending::Information`'s own doc already said so: the executor falls
    /// through to corpus-only synthesis whether the card was skipped or
    /// never seen.
    ///
    /// Called when the answerer is gone for good: the client cancelled the
    /// turn, or the socket hung up. Without it a turn parked on a consent
    /// card never returns and no terminal frame is ever emitted — the
    /// executor is blocked on a `oneshot` nobody holds the other end of
    /// (sv-surface RB2).
    ///
    /// Returns how many questions were resolved, for the trace.
    pub fn abandon_all(&self) -> usize {
        let taken: Vec<(K, Pending)> = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.drain().collect()
        };
        for (_key, entry) in &taken {
            tracing::debug!(
                kind = entry.kind(),
                "approval_desk: abandoning a parked question — nobody is left to answer it"
            );
        }
        let count = taken.len();
        for (_key, entry) in taken {
            match entry {
                // Dropped, not answered: `Parked::answered` reads the drop as
                // `Err(Cancelled)`, which is what actually happened.
                Pending::Approval(_) | Pending::Input(_) => {}
                // The one kind whose absent answer IS an answer.
                Pending::Information(tx) => {
                    let _ = tx.send(None);
                }
            }
        }
        count
    }
}

// `StepStatus` (incl. `of` and `Display`) moved to
// sovereign-contracts::types::approval with `ResolveOutcome` — see the
// re-export above.

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

    /// sv-surface RB2 (review finding C1): an answer that reached the desk
    /// and found nobody waiting is NOT "the executor is running again".
    ///
    /// The three resolve arms ended `let _ = tx.send(v); Ok(())`, so a send
    /// into a dropped receiver — the turn was aborted, the task cancelled —
    /// reported `Resolved`, the success-shaped `Err` ARCH §18.3 names. It is
    /// the one outcome an answerer most needs to tell from a real resolve.
    #[tokio::test]
    async fn an_answer_whose_waiter_is_gone_is_not_reported_as_resolved() {
        let desk = ApprovalDesk::<String>::new();
        let parked = desk.park_approval("t1:3".to_string());
        drop(parked); // the executor went away
        assert_eq!(
            desk.resolve_approval(&"t1:3".to_string(), true),
            ResolveOutcome::WaiterGone
        );
        assert_eq!(desk.parked(), 0, "the answer still consumed the question");
    }

    /// The hangup policy, per prompt KIND (E2): a consent question and an
    /// `ask_user` are CANCELLED, an information request reads as the SKIP.
    #[tokio::test]
    async fn abandon_all_cancels_consents_and_skips_information_requests() {
        let desk = ApprovalDesk::<String>::new();
        let consent = desk.park_approval("t1:0".to_string());
        let asked = desk.park_input("t1:input".to_string());
        let info = desk.park_information("t1:info:0".to_string());

        assert_eq!(desk.abandon_all(), 3);
        assert_eq!(desk.parked(), 0);

        assert!(matches!(consent.answered().await, Err(Error::Cancelled)));
        assert!(matches!(asked.answered().await, Err(Error::Cancelled)));
        assert_eq!(
            info.answered_or_skipped().await,
            None,
            "the one kind whose absent answer IS an answer"
        );
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
