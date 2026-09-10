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
use sovereign_contracts::types::{
    InformationRequest, LessonProposedPayload, MessageRefinedPayload, TurnAnswer, TurnFrame,
    TurnNotice, TurnPrompt,
};
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
    /// What `Notice::StepDone` reports as its task. A DISPLAY label, not a
    /// correlation key (the desk's ids are); stamped from the conversation
    /// id at construction, which is the same thing both other hosts' UIs
    /// show — `sovereign-server` stamps `set_task_id(&conversation_id)`,
    /// the desktop's Tauri channel does the same.
    task_label: String,
    desk: ApprovalDesk<String>,
    /// The TURN this channel is currently serving — a nonce minted where the
    /// turn is spawned, plus whether that turn's answerer has gone.
    ///
    /// The channel is per SOCKET and a socket runs turns sequentially, so
    /// without the nonce the ids are pure step counters: `step:0` on the
    /// second turn is spelled exactly like `step:0` on the first, and a late
    /// answer to the first turn's question resolves the SECOND turn's
    /// (sv-surface C2). `std::sync::Mutex`, never held across an await.
    turn: std::sync::Mutex<TurnEpoch>,
}

/// One turn's identity on a socket that serves many, one after another.
struct TurnEpoch {
    /// What every prompt id this turn mints begins with. Empty before the
    /// first turn — the ids are then unprefixed, which is what a reply
    /// arriving before any turn started should fail to resolve.
    nonce: String,
    /// The answerer is gone for this turn: it cancelled, or the socket hung
    /// up. Every further question the turn asks is refused AT THE DESK
    /// rather than parked, so a replan that re-issues the same step cannot
    /// re-park it on a client that is not coming back (sv-surface RB2).
    abandoned: bool,
}

impl SocketApprovalChannel {
    pub fn new(frames: mpsc::UnboundedSender<TurnFrame>, task_label: &str) -> Self {
        Self {
            frames,
            task_label: task_label.to_string(),
            desk: ApprovalDesk::new(),
            turn: std::sync::Mutex::new(TurnEpoch {
                nonce: String::new(),
                abandoned: false,
            }),
        }
    }

    /// Begin a turn under `nonce` — called where the turn is SPAWNED, which
    /// is the one place that knows a new turn is starting.
    ///
    /// Clears the previous turn's abandonment and re-bases the ids, so an
    /// answer aimed at a question from a turn that has ended resolves
    /// nothing (`NoSuchPending`) instead of landing on the same-numbered
    /// step of the turn now running (sv-surface C2).
    pub fn begin_turn(&self, nonce: &str) {
        let mut turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        turn.nonce = nonce.to_string();
        turn.abandoned = false;
    }

    /// Nobody is going to answer this turn's questions: resolve every parked
    /// one by the per-KIND hangup policy (E2 — approval and user reply are
    /// CANCELLED, information reads as the user's SKIP) and refuse the ones
    /// that have not been asked yet.
    ///
    /// This is what a `Cancel` and a hangup both need. Without it a turn
    /// parked on a consent card is blocked on a `oneshot` nobody holds, so
    /// the executor never returns and NO terminal frame is ever emitted —
    /// the client's stop button does nothing and the socket goes quiet
    /// forever (sv-surface RB2, C13).
    ///
    /// Returns how many parked questions it resolved.
    pub fn abandon(&self) -> usize {
        self.turn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .abandoned = true;
        let resolved = self.desk.abandon_all();
        if resolved > 0 {
            tracing::info!(
                resolved,
                "turn_approval: the turn's parked questions were abandoned — the executor \
                 returns and its turn can terminate"
            );
        }
        resolved
    }

    /// The id a question asked RIGHT NOW would be parked under, or `None`
    /// when this turn has been abandoned and must not park anything more.
    fn prompt_id(&self, suffix: &str) -> Option<String> {
        let turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        if turn.abandoned {
            return None;
        }
        Some(if turn.nonce.is_empty() {
            suffix.to_string()
        } else {
            format!("{}:{}", turn.nonce, suffix)
        })
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

/// The refusal owed to an executor that asks a question after its turn was
/// abandoned. `Cancelled` rather than a default answer: nobody is there, and
/// an invented consent is the substitution this file exists to remove.
fn abandoned(kind: &str) -> Error {
    tracing::debug!(
        kind,
        "turn_approval: refused a question on an abandoned turn — the client cancelled or hung up"
    );
    Error::Cancelled
}

#[async_trait]
impl ApprovalChannel for SocketApprovalChannel {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool> {
        let Some(id) = self.prompt_id(&format!("step:{}", step.id)) else {
            return Err(abandoned("approval"));
        };
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
        // One at a time — the executor is sequential within a task, so the
        // turn's nonce plus the word is already unambiguous.
        let Some(id) = self.prompt_id("input") else {
            return Err(abandoned("user reply"));
        };
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

    /// G3 — the structured information request. The one prompt kind whose
    /// closed-socket policy is SKIP rather than cancel: `None` is a real
    /// answer (the user's skip), so a vanished card and a pressed skip lead
    /// to the same place — corpus-only synthesis — and the executor cannot
    /// tell them apart, which is the point.
    async fn request_information(&self, request: &InformationRequest) -> Option<String> {
        // Abandoned reads as the SKIP, not as a cancel — E2's per-kind
        // policy, and the same `None` a vanished card produces.
        let id = self.prompt_id(&format!("info:{}", request.step_id))?;
        let parked = self.desk.park_information(id.clone());
        let frame = TurnFrame::Prompt {
            id: id.clone(),
            prompt: TurnPrompt::Information {
                request: request.clone(),
            },
        };
        if self.frames.send(frame).is_err() {
            self.desk.cancel(&id);
            tracing::warn!(
                id = %id,
                "turn_approval: information request skipped — the socket closed before it reached the user"
            );
            return None;
        }
        parked.answered_or_skipped().await
    }

    /// G6 — step progress, as the typed Notice. The daemon TRACED AND
    /// DROPPED this until R3; the desktop rendered a Tauri `step-done`
    /// event and the server an `ExecutorEvent`, three spellings of the
    /// same three fields.
    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        self.notice(TurnNotice::StepDone {
            task_id: self.task_label.clone(),
            step_id: step.id,
            description: step.description.clone(),
            status: StepStatus::of(output),
        });
    }

    /// G4 — the post-stream refined answer. CORRECTNESS-LOAD-BEARING: the
    /// UI sticks on "Refining your answer" forever without it
    /// (collaboration.rs records the repro). Fires AFTER the terminal
    /// `Complete`, on a socket the connection loop keeps open.
    fn emit_message_refined(&self, payload: MessageRefinedPayload) {
        self.notice(TurnNotice::MessageRefined(payload));
    }

    /// G5 — a drafted lesson, fire-and-forget: the surface either passes
    /// the payload to its lesson-save command later or does nothing.
    fn emit_lesson_proposed(&self, payload: LessonProposedPayload) {
        self.notice(TurnNotice::LessonProposed(payload));
    }
}

impl SocketApprovalChannel {
    /// Say a Notice. Owed no answer, so a closed socket costs nothing but
    /// a trace — the turn it belonged to is over or ending, and there is
    /// nobody left to substitute for.
    fn notice(&self, notice: TurnNotice) {
        if self.frames.send(TurnFrame::Notice { notice }).is_err() {
            tracing::debug!("turn_approval: notice dropped — the socket is closed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::StepKind;

    pub(super) fn step(id: usize) -> Step {
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

    pub(super) fn preview() -> ActionPreview {
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
        (Arc::new(SocketApprovalChannel::new(tx, "conv-a")), rx)
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

#[cfg(test)]
mod r3_producer_tests {
    use super::*;
    // The consent fixtures live one module up; one spelling of a step and
    // its preview, not two (ARCH §10.6).
    use super::tests::{preview, step};
    use sovereign_contracts::types::InformationRequest;
    use sovereign_core::types::StepOutput;

    fn channel() -> (
        Arc<SocketApprovalChannel>,
        mpsc::UnboundedReceiver<TurnFrame>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(SocketApprovalChannel::new(tx, "conv-a")), rx)
    }

    fn info_request(step_id: usize) -> InformationRequest {
        InformationRequest {
            current_understanding: "The answer depends on a 2024 figure".into(),
            gap: "The adoption rate for region Y".into(),
            relevance: "It decides whether the trend reversed".into(),
            satisfying_source: "A statistics agency press release".into(),
            search_hints: Vec::new(),
            task_id: String::new(),
            step_id,
            kind: Default::default(),
            task_title: String::new(),
            routes: Vec::new(),
        }
    }

    /// G3: the request parks and prompts with its full struct; a pasted
    /// answer resolves it; the SKIP is `None`, a real answer.
    #[tokio::test]
    async fn an_information_request_prompts_and_resolves() {
        let (chan, mut rx) = channel();
        let asking = {
            let chan = Arc::clone(&chan);
            let req = info_request(3);
            tokio::spawn(async move { chan.request_information(&req).await })
        };

        let frame = rx.recv().await.expect("the socket is asked");
        assert_eq!(
            frame,
            TurnFrame::Prompt {
                id: "info:3".into(),
                prompt: TurnPrompt::Information {
                    request: info_request(3),
                },
            },
            "the whole InformationRequest crosses, gap and routes intact"
        );
        assert_eq!(chan.parked(), 1);

        assert_eq!(
            chan.submit(
                "info:3",
                &TurnAnswer::Information {
                    content: Some("12.4% (Eurostat, 2024)".into()),
                    sources: Vec::new(),
                }
            ),
            ResolveOutcome::Resolved
        );
        assert_eq!(
            asking.await.unwrap(),
            Some("12.4% (Eurostat, 2024)".to_string()),
            "the pasted content is the answer"
        );
    }

    /// G3's one semantic difference from approvals: a CLOSED socket reads
    /// as the SKIP, not as a cancellation — the executor falls through to
    /// corpus-only synthesis either way.
    #[tokio::test]
    async fn a_closed_socket_skips_the_information_request_rather_than_cancelling() {
        let (chan, rx) = channel();
        drop(rx);

        let outcome = chan.request_information(&info_request(1)).await;
        assert_eq!(outcome, None, "NOT Err(Cancelled): the skip is the answer");
        assert_eq!(chan.parked(), 0, "nothing is left parked");
    }

    /// G6: step progress crosses as the TYPED notice — `Jump(4)` stays a
    /// number, not the prose "jump to 4".
    #[tokio::test]
    async fn step_progress_becomes_a_typed_notice() {
        let (chan, mut rx) = channel();
        let step = Step {
            id: 5,
            description: "Pick a branch".into(),
            kind: sovereign_contracts::types::StepKind::Branch {
                condition: "always".into(),
                if_true: 6,
                if_false: 7,
            },
            requires_approval: false,
            inputs: Vec::new(),
            sampling: None,
            evaluation: None,
        };
        chan.emit_progress(&step, &StepOutput::Jump(4));

        assert_eq!(
            rx.try_recv().expect("the notice was sent"),
            TurnFrame::Notice {
                notice: TurnNotice::StepDone {
                    task_id: "conv-a".into(),
                    step_id: 5,
                    description: "Pick a branch".into(),
                    status: StepStatus::Jump(4),
                },
            },
            "the task label stamps the notice; the status stays typed"
        );
    }

    /// G4/G5: the two post-terminal emits reach the socket as notices —
    /// the frames that keep the UI off "Refining your answer" forever.
    #[tokio::test]
    async fn the_post_terminal_emits_reach_the_socket_as_notices() {
        let (chan, mut rx) = channel();
        chan.emit_message_refined(MessageRefinedPayload {
            conversation_id: "conv-a".into(),
            message_id: "m1".into(),
            new_content: "the revised answer.".into(),
        });
        chan.emit_lesson_proposed(LessonProposedPayload {
            id: "l1".into(),
            conversation_id: "conv-a".into(),
            message_id: "m1".into(),
            display: "Prefer terse answers".into(),
            prompt_form: "answer tersely".into(),
            enforcement: "prompt".into(),
            params: serde_json::json!({}),
            taught_from: "\"too long\"".into(),
        });

        assert!(matches!(
            rx.try_recv().unwrap(),
            TurnFrame::Notice {
                notice: TurnNotice::MessageRefined(_)
            }
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            TurnFrame::Notice {
                notice: TurnNotice::LessonProposed(_)
            }
        ));
    }

    /// E2's hangup policy, applied per prompt KIND — the half sv-surface
    /// RB2/C13 found unreachable. A consent question is CANCELLED (an
    /// invented yes on behalf of somebody who left is the §18.3
    /// substitution) and an information request reads as the user's SKIP,
    /// which is a real answer the executor cannot tell from a pressed one.
    ///
    /// Named failing input (§18.1): delete `abandon`'s `desk.abandon_all()`
    /// and both awaits below hang forever — which is exactly what a turn
    /// parked on a consent card did when its client pressed stop.
    #[tokio::test]
    async fn abandoning_a_turn_cancels_a_consent_and_skips_an_information_request() {
        let (chan, mut rx) = channel();
        let consent = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.request_approval(&step(1), &preview()).await })
        };
        rx.recv().await.expect("the consent question is asked");
        let asked = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.request_information(&info_request(2)).await })
        };
        rx.recv().await.expect("the information request is asked");
        assert_eq!(chan.parked(), 2);

        assert_eq!(chan.abandon(), 2, "both parked questions were resolved");
        assert_eq!(chan.parked(), 0, "and none is still waiting");

        assert!(
            matches!(consent.await.unwrap(), Err(Error::Cancelled)),
            "a consent question nobody is left to answer is CANCELLED, never granted"
        );
        assert_eq!(
            asked.await.unwrap(),
            None,
            "an information request reads as the skip — the one kind whose absent \
             answer is an answer"
        );
    }

    /// And the turn stays abandoned: a step the executor re-issues after the
    /// cancel (a replan re-asking the same question) is refused at the desk
    /// rather than parked on a client that is not coming back — otherwise the
    /// unpark buys one loop of the executor and then blocks again.
    #[tokio::test]
    async fn a_question_asked_after_the_abandon_is_refused_rather_than_parked() {
        let (chan, mut rx) = channel();
        chan.abandon();

        assert!(matches!(
            chan.request_approval(&step(1), &preview()).await,
            Err(Error::Cancelled)
        ));
        assert_eq!(
            chan.request_information(&info_request(2)).await,
            None,
            "same per-kind policy for a question that never got asked"
        );
        assert_eq!(chan.parked(), 0, "nothing was parked");
        assert!(rx.try_recv().is_err(), "and nothing was shown to a client");
    }

    /// sv-surface C2: the desk is per SOCKET and a socket runs turns one
    /// after another, so an id that is only a step counter is spelled the
    /// same on every turn. A late answer to the first turn's question then
    /// resolves the SECOND turn's — a user's stale click granting a consent
    /// they were never shown.
    #[tokio::test]
    async fn a_late_answer_cannot_reach_the_next_turns_question() {
        let (chan, mut rx) = channel();
        chan.begin_turn("turn-one");
        let first = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.request_approval(&step(0), &preview()).await })
        };
        let TurnFrame::Prompt { id: first_id, .. } = rx.recv().await.unwrap() else {
            panic!("the first turn asks");
        };
        assert_eq!(first_id, "turn-one:step:0");
        chan.abandon();
        assert!(matches!(first.await.unwrap(), Err(Error::Cancelled)));

        chan.begin_turn("turn-two");
        let second = {
            let chan = Arc::clone(&chan);
            tokio::spawn(async move { chan.request_approval(&step(0), &preview()).await })
        };
        let TurnFrame::Prompt { id: second_id, .. } = rx.recv().await.unwrap() else {
            panic!("the second turn asks");
        };
        assert_ne!(
            second_id, first_id,
            "the same step number on a new turn is a DIFFERENT question"
        );

        assert_eq!(
            chan.submit(&first_id, &TurnAnswer::Approved(true)),
            ResolveOutcome::NoSuchPending,
            "the stale click resolves nothing"
        );
        assert_eq!(
            chan.submit(&second_id, &TurnAnswer::Approved(false)),
            ResolveOutcome::Resolved
        );
        assert!(
            !second.await.unwrap().unwrap(),
            "the live question got the answer that was aimed at it"
        );
    }
}
