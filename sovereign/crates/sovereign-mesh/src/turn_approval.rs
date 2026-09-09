// SPDX-License-Identifier: AGPL-3.0-or-later
//! One socket's approval channel — the daemon-side end of `TurnFrame::
//! Prompt` (sv-surface rung 6 commit C1, TOPOLOGY hazard 12; R1 gave it
//! the two-shape protocol).
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
use sovereign_contracts::types::{TurnAnswer, TurnFrame, TurnPrompt};
use sovereign_core::approval_desk::{ApprovalDesk, ResolveOutcome, StepStatus};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::ApprovalChannel;
use sovereign_core::types::{ActionPreview, Step, StepOutput};
use tokio::sync::mpsc;

/// The approval channel of ONE turn socket: show the question as a frame,
/// park the executor, resolve when the reply comes back up the same socket.
///
/// The desk is keyed by the SAME id the [`TurnFrame::Prompt`] carries —
/// one host-minted id, which is the whole correlation story. There is no
/// conversation and no socket in the key because the desk belongs to ONE
/// socket running ONE turn, so the id is already unambiguous.
pub struct SocketApprovalChannel {
    /// The socket's per-turn frame channel — the same one the turn's tokens
    /// go down, so a consent question arrives in order with the answer it
    /// interrupts.
    frames: mpsc::UnboundedSender<TurnFrame>,
    desk: ApprovalDesk<String>,
}

impl SocketApprovalChannel {
    pub fn new(frames: mpsc::UnboundedSender<TurnFrame>) -> Self {
        Self {
            frames,
            desk: ApprovalDesk::new(),
        }
    }

    /// Answer a parked question by the id it was asked under.
    ///
    /// The ONE mapping from the wire's answer shape to the parked kind
    /// lives on the desk (`resolve_answer`); an id nothing is parked
    /// under is `NoSuchPending`, a wrong-kind answer is `WrongKind` and
    /// the question survives.
    pub fn submit(&self, id: &str, answer: &TurnAnswer) -> ResolveOutcome {
        self.desk.resolve_answer(&id.to_string(), answer)
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
        id: String,
        parked: sovereign_core::approval_desk::Parked<T>,
        prompt: TurnPrompt,
    ) -> Result<T> {
        // The id travels in the frame; a clone stays behind for the
        // cancel path, which only runs when nobody took the frame.
        let frame = TurnFrame::Prompt {
            id: id.clone(),
            prompt,
        };
        if self.frames.send(frame).is_err() {
            self.desk.cancel(&id);
            tracing::warn!(
                id = %id,
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
        let id = format!("step:{}", step.id);
        let parked = self.desk.park_approval(id.clone());
        self.ask(
            id,
            parked,
            TurnPrompt::Approval {
                preview: preview.clone(),
            },
        )
        .await
    }

    async fn ask_user(&self, question_text: &str) -> Result<String> {
        // One at a time — the executor is sequential within a task.
        let id = "input".to_string();
        let parked = self.desk.park_input(id.clone());
        self.ask(
            id,
            parked,
            TurnPrompt::UserInput {
                question: question_text.to_string(),
            },
        )
        .await
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        // No progress frame in the turn protocol yet — `TurnFrame` carries
        // narration, and the daemon installs no narration broadcast (see
        // `turn_http`'s module docs). `TurnNotice::StepDone` is the
        // sv-surface G6 row, landing with its producer. Traced rather than
        // dropped, so a daemon run's step ledger is readable at `info`.
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

    fn channel() -> (
        Arc<SocketApprovalChannel>,
        mpsc::UnboundedReceiver<TurnFrame>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(SocketApprovalChannel::new(tx)), rx)
    }

    /// The whole point: the socket is ASKED, and its answer runs the step.
    /// Delete the `ask` send and this hangs; grant without asking (the shipped
    /// `AutoApprovalChannel` behaviour C1 replaces) and no frame arrives.
    #[tokio::test]
    async fn the_socket_is_asked_and_its_answer_resolves_the_step() {
        let (chan, mut rx) = channel();
        let asking = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.request_approval(&step(7), &preview()).await })
        };

        let frame = rx.recv().await.expect("the socket is asked");
        assert_eq!(
            frame,
            TurnFrame::Prompt {
                id: "step:7".into(),
                prompt: TurnPrompt::Approval { preview: preview() },
            },
            "the question reaches the socket as the wire frame, preview intact"
        );

        // Still waiting — a channel that answered itself would have finished.
        assert_eq!(chan.parked(), 1);
        assert_eq!(
            chan.submit("step:7", &TurnAnswer::Approved(true)),
            ResolveOutcome::Resolved
        );
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
        let (a, mut a_rx) = channel();
        let (b, mut b_rx) = channel();

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
            b.submit("step:3", &TurnAnswer::Approved(true)),
            ResolveOutcome::NoSuchPending,
            "B answering A's step reaches nothing"
        );
        assert_eq!(a.parked(), 1, "A's question is still waiting after B tried");

        assert_eq!(
            a.submit("step:3", &TurnAnswer::Approved(false)),
            ResolveOutcome::Resolved
        );
        assert!(
            !asking.await.unwrap().unwrap(),
            "A's own answer is the one that lands, and it was a NO"
        );
    }

    /// A socket that goes away mid-question cancels it rather than granting
    /// on a vanished user's behalf, and leaves nothing parked (ARCH §18.3).
    #[tokio::test]
    async fn a_closed_socket_cancels_the_question_rather_than_granting_it() {
        let (chan, rx) = channel();
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

    /// `ask_user` rides the same seam, and a wrong-KIND answer aimed at a
    /// pending question is refused and does not consume it — the flat id
    /// namespace the R1 fold bought: before it, the kind lived in the desk
    /// KEY, so a mismatched reply could not even find the entry. Now it can,
    /// and the desk's restore-on-mismatch is the guard that fires.
    #[tokio::test]
    async fn a_user_reply_answers_a_question_and_an_approval_does_not() {
        let (chan, mut rx) = channel();
        let asking = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.ask_user("Which branch?").await })
        };
        assert_eq!(
            rx.recv().await.expect("the socket is asked"),
            TurnFrame::Prompt {
                id: "input".into(),
                prompt: TurnPrompt::UserInput {
                    question: "Which branch?".into(),
                },
            }
        );

        assert_eq!(
            chan.submit("input", &TurnAnswer::Approved(true)),
            ResolveOutcome::WrongKind,
            "an approval aimed at a question is refused by name"
        );
        assert_eq!(chan.parked(), 1, "the question survived the wrong answer");

        assert_eq!(
            chan.submit("input", &TurnAnswer::Text("main".into())),
            ResolveOutcome::Resolved
        );
        assert_eq!(asking.await.unwrap().unwrap(), "main");
    }
}
