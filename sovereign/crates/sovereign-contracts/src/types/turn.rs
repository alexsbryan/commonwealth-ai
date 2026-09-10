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
//!
//! # The two shapes (sv-surface R1, 2026-09-09)
//!
//! Nine host→client capabilities were on their way to becoming nine
//! frame/reply pairs. Measured, they are TWO: everything the executor
//! parks on a human either MUST be answered — show a thing, park,
//! resolve by id — or is owed nothing. So the asking half is one frame
//! carrying the closed [`TurnPrompt`] enum, the telling half is one
//! frame carrying the closed [`TurnNotice`] enum, and the reply is one
//! request carrying the closed [`TurnAnswer`] enum (quality/campaigns/
//! sv-surface.toml, "THE PROTOCOL SHAPE"). The names are `Turn*`-
//! qualified rather than the campaign sketch's bare `Prompt`/`Notice`/
//! `Answer` because `Prompt` and `Answer` are already type nouns in
//! other workspace crates (commonwealth-api, kernel-types) and
//! concept-gate holds that line.
//!
//! The old `ApprovalRequest`/`UserInputRequest` frames folded into
//! [`TurnFrame::Prompt`], and `Approve`/`UserReply` into
//! [`TurnRequest::Answer`], while nothing rendered them — the census
//! was three ignore arms, a deliberate error, and these byte pins. That
//! window closes the moment a surface wires the Prompt path, which is
//! why the fold landed first.
//!
//! Some [`TurnNotice`] variants have no producer on the daemon yet.
//! They are not speculative: each is a row of the campaign's gap ledger
//! (G4-G8, G10) whose daemon-side producer is scheduled work, and
//! landing them with the shape is what stops each producer from
//! becoming its own protocol edit. The "variants are added when a
//! corresponding emit site exists" rule below is superseded FOR THESE
//! NAMED ROWS by that ledger — an unlisted variant still needs an emit
//! site. Forward compatibility holds: a client that cannot render a
//! variant still parses the frame (sovereign-mobile's mirror carries
//! `#[serde(other)]` for exactly this).

use serde::{Deserialize, Serialize};

use crate::types::approval::{ResolveOutcome, StepStatus};
use crate::types::epistemic::EpistemicState;
use crate::types::narration::{ClarificationRequest, InterpretationProposed, NarrationPhase};
use crate::types::projection::{Citation, Provenance, TaskSummary, TurnMetadata};
use crate::types::ui::ActionPreview;
use crate::types::{
    InformationRequest, LessonProposedPayload, MessageRefinedPayload, SearchedSourceEntry,
};

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
        /// The background task this turn spawned, when it ran the agentic
        /// path. Added in phase 6: the streaming door produced one and threw
        /// it away, so a streaming client could not learn a task existed
        /// while a REST client could. Optional + `skip_serializing_if`, so a
        /// plain chat turn's frame is byte-identical to before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskSummary>,
        /// How the turn was SERVED — routed intent, grounding-gate outcome,
        /// stage ledger. Added 2026-09-04: `svrn chat ask --format json` is
        /// a surface now (phase 6) and these three facts had no way across
        /// the process boundary, so the only reader left was SQL. Absent
        /// when the turn reported none of them; never `null`, never `{}`
        /// (ARCH §18.3). A plain chat turn's frame is byte-identical to
        /// before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<TurnMetadata>,
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
    /// The turn has stopped on a question only this client can answer:
    /// the host shows [`TurnPrompt`], parks the executor, and resumes it
    /// when [`TurnRequest::Answer`] arrives carrying the same `id`.
    ///
    /// Emitted only to the socket that OWNS this conversation's approvals
    /// (`?approvals=true` on the stream upgrade). A host with no such
    /// owner answers the question itself and emits nothing — the shipped
    /// non-interactive behaviour — so a client that never opts in cannot
    /// receive a frame it would have to hang on.
    ///
    /// `id` is the host-minted address of the parked question and the
    /// ONLY correlation key. Three hosts once stamped three key formats
    /// (`{task}:{step}`, `:input`, `:info:{step}`) and the wire carried
    /// all three; one id opaque to the client replaces them. A closed
    /// socket CANCELS an approval (granting on a vanished user's behalf
    /// is the §18.3 substitution) but reads as SKIP for an information
    /// request — the hangup policy is per prompt kind, and
    /// [`TurnPrompt`]'s variant docs say which is which.
    Prompt {
        /// The parked question's id — echo it in [`TurnRequest::Answer`].
        id: String,
        /// What is being asked.
        prompt: TurnPrompt,
    },
    /// The host tells the client something no answer is owed: step
    /// progress, a re-synthesised answer, a proposed lesson, the
    /// acknowledgement of an answer that resolved something, the message
    /// id a turn just acquired.
    ///
    /// NOT bounded by the turn: `MessageRefined` and `LessonProposed`
    /// fire AFTER the terminal `Complete` (post-stream refinement, a
    /// detached capture spawn), so the contract is "on this socket,
    /// until it closes" — never "during the turn". A drain that stops
    /// at `Complete` will not see those two (sv-surface G13 names that
    /// gap for the client family).
    Notice {
        /// What the host is saying.
        notice: TurnNotice,
    },
}

/// What a parked executor is waiting to be told — the PROMPT half of the
/// two-shape protocol. Closed on purpose (ARCH §2): a new KIND of
/// question is a protocol change, never a string discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnPrompt {
    /// A write-effectful step needs the user's consent before it runs.
    /// A socket that closes mid-question CANCELS it — nobody was there
    /// to consent, and an invented yes is the §18.3 substitution.
    Approval {
        /// What the step will do, as the approval decider was given it.
        /// The whole [`ActionPreview`] rather than a projection of it:
        /// the executor, the desktop card and this frame render one
        /// consent question and there is one value that states it
        /// (ARCH §10.6).
        preview: ActionPreview,
    },
    /// A free-form question to the user (`ApprovalChannel::ask_user` —
    /// a `UserInput` step). A closed socket cancels it, same as
    /// [`TurnPrompt::Approval`]: an invented reply is not an answer.
    UserInput {
        /// The question, in the turn's own words.
        question: String,
    },
    /// A structured information request (`ApprovalChannel::
    /// request_information`). The one prompt kind whose closed-socket
    /// policy is SKIP, not cancel: `None` is a real answer — the user's
    /// skip — and the executor falls through to corpus-only synthesis
    /// either way, so a vanished card and a pressed skip lead to the
    /// same place (sv-surface G3's one semantic difference).
    Information {
        /// The gap, spelled out.
        request: InformationRequest,
    },
}

/// The client's reply to a [`TurnFrame::Prompt`] — the ANSWER half of
/// the two-shape protocol.
///
/// A wrong-kind answer (an `Approved` aimed at a parked `Information`)
/// is refused and the question SURVIVES for the right one: the desk's
/// restore-on-mismatch behaviour, which a typed enum does not remove —
/// a client can still aim any answer at any id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnAnswer {
    /// Reply to [`TurnPrompt::Approval`]: whether the step may proceed.
    Approved(bool),
    /// Reply to [`TurnPrompt::UserInput`]: the user's words.
    Text(String),
    /// Reply to [`TurnPrompt::Information`]. `content: None` IS the skip —
    /// a real answer, not an absence: the executor resumes with
    /// corpus-only synthesis rather than waiting on a card nobody is
    /// looking at.
    ///
    /// `sources` carries the web-search registry rows when the content was
    /// produced by the CLIENT's own search (sv-surface G3b): the daemon
    /// folds them into the conversation's cumulative `searched_sources`
    /// as part of resolving this answer, so one user action — picking a
    /// search result — is one atomic effect on the host. Empty for paste
    /// and skip, and omitted from the bytes when empty (a plain answer is
    /// byte-identical to the pre-G3b shape, modulo the content key the
    /// struct variant introduced while nothing shipped answered this).
    Information {
        /// What the user pasted, or `None` when they pressed skip.
        content: Option<String>,
        /// Registry rows the content was built from — resolved AND
        /// persisted by the host in one effect.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sources: Vec<SearchedSourceEntry>,
    },
}

/// What the host says that no answer is owed — the NOTICE half of the
/// two-shape protocol. See [`TurnFrame::Notice`] for the lifetime
/// contract ("on this socket, until it closes").
///
/// [`TurnFrame::Narration`] deliberately stays a frame of its own
/// (sv-surface E3): it is the one variant with real readers today, and
/// folding it would be a wire break for no gain. The desktop's OTHER
/// two routing events joined as variants with their producer (sv-surface
/// G7): the payloads already lived in this crate — only the daemon-side
/// per-socket sink was missing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnNotice {
    /// The turn acquired its assistant message id — emitted at stream
    /// handle acquisition, before the first token (sv-surface G10).
    ///
    /// This is the placeholder signal. All three turn commands return
    /// the message id SYNCHRONOUSLY in-process so the UI can paint a
    /// placeholder over the cold-turn retrieval wait; over the wire the
    /// id otherwise first arrives on the first `Token`, which is most of
    /// that wait later. Landing the variant pre-registers the choice
    /// the G10 row demanded BEFORE any surface converts, so the
    /// conversion discovers a frame, not a dilemma.
    TurnStarted {
        /// The assistant message this turn is about to stream.
        message_id: String,
    },
    /// A step of the in-flight task finished (sv-surface G6). The
    /// daemon TRACES AND DROPS this today; the desktop renders it as a
    /// `step-done` Tauri event and the server as an `ExecutorEvent`.
    /// One frame replaces all three spellings.
    StepDone {
        /// The background task the step belongs to.
        task_id: String,
        /// The step that finished.
        step_id: usize,
        /// What the step was, in its own words.
        description: String,
        /// How it ended — typed, not pre-rendered prose.
        status: StepStatus,
    },
    /// An already-streamed assistant message was re-synthesised with
    /// user-supplied content (sv-surface G4). CORRECTNESS-LOAD-BEARING:
    /// without this emit the UI sticks on "Refining your answer"
    /// forever (collaboration.rs records the repro). Fires AFTER the
    /// terminal `Complete`.
    MessageRefined(MessageRefinedPayload),
    /// A draft lesson was proposed from a coaching turn (sv-surface
    /// G5). Fire-and-forget: the surface either passes the payload to
    /// its lesson-save command later or does nothing. Fires AFTER the
    /// terminal `Complete`.
    LessonProposed(LessonProposedPayload),
    /// Whether an answer resolved anything (sv-surface G8). `submit_*`
    /// returned a bool that could not tell "accepted" from "never
    /// arrived"; [`ResolveOutcome`] already carried the distinction and
    /// now crosses the wire instead of the bool.
    ResolveAck {
        /// The id the answer was aimed at.
        id: String,
        /// What the answer found there.
        outcome: ResolveOutcome,
    },
    /// The router read the input a moderate-confidence way: the banner
    /// with its interpretation and redirect chips (sv-surface G7). The
    /// session the chips resume against is retained ~30s, the same
    /// window `TurnRequest::Resume` names.
    InterpretationProposed(InterpretationProposed),
    /// The router could not decide: synthesis is suppressed and the card
    /// asks the user to pick or type (sv-surface G7). Answered with
    /// [`TurnRequest::Resume`] against the session the options carry.
    ClarificationRequest(ClarificationRequest),
}

/// How much of the Sovereign pipeline a turn runs through.
///
/// `Naked` is the desktop's "Raw model" setting and `svrn chat ask --naked`:
/// retrieval, the router, the grounding gate, tools and the atlas are all
/// bypassed, and the user talks to the model directly. Both hosts had it and
/// it had **no wire representation** — so a surface that stopped assembling
/// its own `Runtime` and started connecting to the daemon (TOPOLOGY phase 6)
/// would have silently lost the flag: the answer comes back grounded, the
/// client cannot tell, and "raw model" quietly stops meaning anything. A
/// capability that exists in-process and not on the wire is what blocks a
/// host from becoming a surface, which is why this is here rather than in a
/// host's flag parser (ARCH §18.3).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnMode {
    /// The full pipeline: retrieval, routing, grounding gate, tools, atlas.
    #[default]
    Grounded,
    /// Raw model — every Sovereign affordance bypassed.
    Naked,
}

impl TurnMode {
    /// `skip_serializing_if` hook: the default mode is not written to the
    /// wire, so a grounded turn's bytes are unchanged from before the field
    /// existed.
    pub fn is_default(&self) -> bool {
        matches!(self, TurnMode::Grounded)
    }
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
        /// Skip the router and classify this turn as the named intent.
        ///
        /// `svrn govern ask`, `portfolio ask` and `proxy ask` all pin
        /// `KnowledgeQuery` — they are asking a corpus a question and the
        /// router's opinion is not wanted. That was reachable only in-process
        /// (`Runtime::handle_message_stream_as`), which is what kept those
        /// three hosts running their own turns: not that they were doing
        /// something un-turn-like, but that one turn PARAMETER had no wire
        /// form. `None` routes normally.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<crate::types::routing::Intent>,
        /// How much of the pipeline to run. Absent on the wire means
        /// [`TurnMode::Grounded`], and `Grounded` is OMITTED when writing —
        /// so the bytes are byte-for-byte what they were before this field
        /// existed, in both directions. `turn_request_wire_form` asserts
        /// round-trip symmetry, so a plain `#[serde(default)]` here would
        /// have re-serialised every existing client's message with a key it
        /// never sent.
        #[serde(default, skip_serializing_if = "TurnMode::is_default")]
        mode: TurnMode,
    },
    /// Answer a parked [`TurnFrame::Prompt`] — the reply carrying the
    /// same `id` the question arrived with.
    ///
    /// Resolved against the SENDING socket's own conversation: the host
    /// looks the id up in the desk that socket's questions live on, so
    /// one client cannot answer another client's question by guessing
    /// an id — there is no map it could reach through. A wrong-KIND
    /// answer is refused by name and the question survives; so is an
    /// id nothing is parked under (already answered, turn ended).
    Answer {
        /// The id from the [`TurnFrame::Prompt`] being answered.
        id: String,
        /// The answer, in the shape the question's kind expects.
        answer: TurnAnswer,
    },
    /// Continue an earlier turn under the intent the user picked — the reply
    /// to a ClarificationCard option or a NextStepOffer button.
    ///
    /// In-process this is `Runtime::resume_session_stream`, and like
    /// [`TurnRequest::Message`]'s `intent` it was a turn PARAMETER with no
    /// wire form. A surface that stopped assembling its own `Runtime` lost
    /// the ability to answer its own clarification cards, so the card kept a
    /// host behind it — which is what a pure-client attach cannot have
    /// (sv-surface rung 6).
    Resume {
        /// The follow-up text, in the option's own words. [`TurnRequest::Redirect`]
        /// is the variant that re-uses the original message instead.
        content: String,
        /// The `QuerySession` this continues, retained ~30s past its turn.
        ///
        /// A host refuses an id belonging to a DIFFERENT conversation: the
        /// socket's conversation is pinned by its URL, so a session from
        /// another one is not this client's to name. An id the host no
        /// longer holds is NOT refused — the 30s GC and a daemon restart are
        /// both ordinary, and here the id is provenance rather than a key
        /// (the resume path reads nothing out of the session).
        session_id: String,
        /// Wire-form `Intent` to classify the continuation as, skipping the
        /// router. An unparseable hint degrades to `SimpleQuery` rather than
        /// failing the turn — `parse_intent_hint`'s shipped contract, so a
        /// typo costs routing quality and not the answer.
        intent_hint: String,
    },
    /// Cancel the in-flight turn and re-answer the SAME user message under a
    /// different intent — the reply to a routing "did you mean?" card.
    ///
    /// Carries no `content` on purpose: `Runtime::redirect_turn_stream` reads
    /// the original message off the session it names, so a client that
    /// re-sent the text could disagree with what was actually asked. The
    /// session holds the one copy.
    Redirect {
        /// The `QuerySession` to redirect. Required to be live AND this
        /// socket's, both refused by name: the host resolves the turn's
        /// message and its conversation FROM this session, so an id from
        /// another conversation would run a turn there and stream it here,
        /// and an id the host has dropped has no message to re-answer.
        session_id: String,
        /// Wire-form `Intent` to re-answer under, with the same degradation
        /// `Resume`'s hint has.
        intent_hint: String,
    },
}
