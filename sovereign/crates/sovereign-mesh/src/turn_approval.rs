// SPDX-License-Identifier: AGPL-3.0-or-later
//! One socket's approval channel — the daemon-side end of `TurnFrame::
//! ApprovalRequest` (sv-surface rung 6 commit C1, TOPOLOGY hazard 12).
//!
//! # The hazard
//!
//! A daemon serving several sockets holds ONE `Runtime`, so its process-wide
//! `approval` member could not tell whose turn was asking. The shipped answer
//! was `AutoApprovalChannel`: every write-effectful step granted, silently,
//! including under an interactive client that had attached and was showing the
//! user a chat window. Silent is the load-bearing word — auto-approval on a
//! headless run is a posture somebody chose; auto-approval under a UI that
//! would have raised a consent card is a substitution nobody named (ARCH
//! §18.3).
//!
//! # Why this file is small
//!
//! Because the channel is PER TURN. `sovereign_core::runtime::capabilities`
//! installs it around the turn the socket started, so the turn holds its own
//! channel and there is no shared map to address. A process-wide router would
//! have needed a conversation→socket registry, an RAII owner guard to survive
//! a hangup, and a conversation in every key — all of it machinery to work out
//! something the turn already knows.
//!
//! **One socket cannot answer another's question, structurally.** Socket B's
//! reply is handed to B's own channel, whose desk holds only B's questions.
//! There is no key B could construct that reaches A's, because B never touches
//! A's map (ARCH §7).
//!
//! # Claimed, never assumed
//!
//! A socket becomes the answerer by ASKING — `?approvals=true` on the stream
//! upgrade. `svrn chat`, the bench harness and every other reader that streams
//! tokens installs no approval handler; registering them implicitly would turn
//! today's auto-grant into a HANG, since the daemon would emit a request, park
//! the step, and wait on a client with nothing to answer with. A socket that
//! claims nothing runs the turn on the daemon's own commissioned channel,
//! exactly as before.

use std::sync::Arc;

use async_trait::async_trait;
use sovereign_contracts::types::TurnFrame;
use sovereign_core::approval_desk::{ApprovalDesk, ResolveOutcome, StepStatus};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::ApprovalChannel;
use sovereign_core::types::{ActionPreview, Step, StepOutput};
use tokio::sync::mpsc;

/// Which question is waiting. No conversation and no socket in the key: the
/// desk belongs to ONE socket running ONE turn, so the step addresses it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Question {
    /// A step's `request_approval`, addressed by the step's own id.
    Step(usize),
    /// The turn's `ask_user`. One at a time — the executor is sequential
    /// within a task.
    Input,
}

/// The approval channel of ONE turn socket: show the question as a frame,
/// park the executor, resolve when the reply comes back up the same socket.
pub struct SocketApprovalChannel {
    /// The socket's per-turn frame channel — the same one the turn's tokens
    /// go down, so a consent question arrives in order with the answer it
    /// interrupts.
    frames: mpsc::UnboundedSender<TurnFrame>,
    /// What goes out as the frame's `task_id`, and what a client echoes back.
    ///
    /// The conversation, because that is what `task_id` already means on this
    /// seam: `sovereign-server`'s handler stamps its channel with
    /// `set_task_id(&conversation_id)` and keys on it. Same field, same
    /// meaning, both hosts. It is not what ADDRESSES the question here — the
    /// socket does that — so a reply naming a different one still resolves,
    /// and it cannot reach another socket's desk to begin with.
    conversation_id: String,
    desk: ApprovalDesk<Question>,
}

impl SocketApprovalChannel {
    pub fn new(frames: mpsc::UnboundedSender<TurnFrame>, conversation_id: &str) -> Self {
        Self {
            frames,
            conversation_id: conversation_id.to_string(),
            desk: ApprovalDesk::new(),
        }
    }

    /// Answer a parked step approval.
    pub fn submit_approval(&self, step_id: usize, approved: bool) -> ResolveOutcome {
        self.desk
            .resolve_approval(&Question::Step(step_id), approved)
    }

    /// Answer a parked `ask_user` question.
    pub fn submit_user_reply(&self, content: String) -> ResolveOutcome {
        self.desk.resolve_input(&Question::Input, content)
    }

    /// How many of this socket's questions are still waiting. Zero at rest;
    /// a count that survives the turn is a leak.
    pub fn parked(&self) -> usize {
        self.desk.parked()
    }

    /// Park, then show. A frame that cannot be sent means the socket is gone,
    /// and granting on a vanished user's behalf is the substitution this file
    /// removes — so the step is cancelled, which is also what the hangup abort
    /// is about to do to the turn.
    async fn ask<T>(
        &self,
        question: Question,
        parked: sovereign_core::approval_desk::Parked<T>,
        frame: TurnFrame,
    ) -> Result<T> {
        if self.frames.send(frame).is_err() {
            self.desk.cancel(&question);
            tracing::warn!(
                ?question,
                "turn_approval: cancelled — the socket closed before the question reached it"
            );
            return Err(Error::Cancelled);
        }
        parked.answered().await
    }
}

#[async_trait]
impl ApprovalChannel for SocketApprovalChannel {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool> {
        let question = Question::Step(step.id);
        let parked = self.desk.park_approval(question.clone());
        self.ask(
            question,
            parked,
            TurnFrame::ApprovalRequest {
                task_id: self.conversation_id.clone(),
                step_id: step.id,
                preview: preview.clone(),
            },
        )
        .await
    }

    async fn ask_user(&self, question_text: &str) -> Result<String> {
        let question = Question::Input;
        let parked = self.desk.park_input(question.clone());
        self.ask(
            question,
            parked,
            TurnFrame::UserInputRequest {
                task_id: self.conversation_id.clone(),
                question: question_text.to_string(),
            },
        )
        .await
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        // No progress frame in the turn protocol yet — `TurnFrame` carries
        // narration, and the daemon installs no narration broadcast (see
        // `turn_http`'s module docs). Traced rather than dropped, so a
        // daemon run's step ledger is readable at `info`.
        tracing::info!(
            step_id = step.id,
            description = %step.description,
            status = %StepStatus::of(output),
            "turn_approval: step progress"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::StepKind;

    fn step(id: usize) -> Step {
        Step {
            id,
            description: "run the migration".into(),
            kind: StepKind::Tool {
                tool_id: "shell".into(),
                params: serde_json::json!({ "cmd": "migrate" }),
            },
            requires_approval: true,
            inputs: Vec::new(),
            sampling: None,
            evaluation: None,
        }
    }

    fn preview() -> ActionPreview {
        ActionPreview {
            tool_id: "shell".into(),
            description: "Run the migration".into(),
            params: serde_json::json!({ "cmd": "migrate" }),
        }
    }

    fn channel(
        conv: &str,
    ) -> (
        Arc<SocketApprovalChannel>,
        mpsc::UnboundedReceiver<TurnFrame>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(SocketApprovalChannel::new(tx, conv)), rx)
    }

    /// The whole point: the socket is ASKED, and its answer runs the step.
    /// Delete the `ask` send and this hangs; grant without asking (the shipped
    /// `AutoApprovalChannel` behaviour C1 replaces) and no frame arrives.
    #[tokio::test]
    async fn the_socket_is_asked_and_its_answer_resolves_the_step() {
        let (chan, mut rx) = channel("conv-a");
        let asking = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.request_approval(&step(7), &preview()).await })
        };

        let frame = rx.recv().await.expect("the socket is asked");
        assert_eq!(
            frame,
            TurnFrame::ApprovalRequest {
                task_id: "conv-a".into(),
                step_id: 7,
                preview: preview(),
            },
            "the question reaches the socket as the wire frame, preview intact"
        );

        // Still waiting — a channel that answered itself would have finished.
        assert_eq!(chan.parked(), 1);
        assert_eq!(chan.submit_approval(7, true), ResolveOutcome::Resolved);
        assert!(
            asking.await.unwrap().unwrap(),
            "the user's yes is the answer"
        );
        assert_eq!(chan.parked(), 0, "answering unparks the question");
    }

    /// The structural claim, and the hazard C1 closes: TWO sockets on one
    /// daemon. B cannot answer A's question — not because a check refuses it,
    /// but because B's desk does not contain it and B can reach no other.
    #[tokio::test]
    async fn a_second_socket_cannot_answer_the_first_sockets_question() {
        let (a, mut a_rx) = channel("conv-a");
        let (b, mut b_rx) = channel("conv-b");

        let asking = {
            let a = Arc::clone(&a);
            tokio::spawn(async move { a.request_approval(&step(3), &preview()).await })
        };
        a_rx.recv().await.expect("A is asked");

        // B never saw it. `try_recv` and not a timeout: a frame delivered to
        // the wrong socket is a leak, and a leak that only shows up as a slow
        // test is one nobody watches.
        assert!(
            b_rx.try_recv().is_err(),
            "the question went to A's socket only"
        );
        assert_eq!(
            b.submit_approval(3, true),
            ResolveOutcome::NoSuchPending,
            "B answering A's step reaches nothing"
        );
        assert_eq!(a.parked(), 1, "A's question is still waiting after B tried");

        assert_eq!(a.submit_approval(3, false), ResolveOutcome::Resolved);
        assert!(
            !asking.await.unwrap().unwrap(),
            "A's own answer is the one that lands, and it was a NO"
        );
    }

    /// A socket that goes away mid-question cancels it rather than granting
    /// on a vanished user's behalf, and leaves nothing parked (ARCH §18.3).
    #[tokio::test]
    async fn a_closed_socket_cancels_the_question_rather_than_granting_it() {
        let (chan, rx) = channel("conv-a");
        drop(rx);

        let outcome = chan.request_approval(&step(1), &preview()).await;
        assert!(
            matches!(outcome, Err(Error::Cancelled)),
            "NOT Ok(true): nobody was there to consent; got {outcome:?}"
        );
        assert_eq!(
            chan.parked(),
            0,
            "the unshowable question is not left parked"
        );
    }

    /// `ask_user` rides the same seam, and an approval aimed at a pending
    /// question resolves nothing and leaves it waiting.
    ///
    /// It reads as `NoSuchPending` rather than `WrongKind` because
    /// [`Question`] carries the kind in the key, so the two cannot collide
    /// here at all — the desk's own restore-on-mismatch is a guard for key
    /// policies that do not, not a branch this host reaches.
    #[tokio::test]
    async fn a_user_reply_answers_a_question_and_an_approval_does_not() {
        let (chan, mut rx) = channel("conv-a");
        let asking = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.ask_user("Which branch?").await })
        };
        assert_eq!(
            rx.recv().await.expect("the socket is asked"),
            TurnFrame::UserInputRequest {
                task_id: "conv-a".into(),
                question: "Which branch?".into(),
            }
        );

        assert_eq!(
            chan.submit_approval(0, true),
            ResolveOutcome::NoSuchPending,
            "an approval aimed at a question reaches nothing"
        );
        assert_eq!(chan.parked(), 1, "the question survived the wrong answer");

        assert_eq!(
            chan.submit_user_reply("main".into()),
            ResolveOutcome::Resolved
        );
        assert_eq!(asking.await.unwrap().unwrap(), "main");
    }
}
