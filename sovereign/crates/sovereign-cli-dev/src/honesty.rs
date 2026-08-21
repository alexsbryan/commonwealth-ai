// SPDX-License-Identifier: AGPL-3.0-or-later
//! Honest-uncertainty protocol — the discipline the system follows
//! when it lacks information it needs.
//!
//! ## The rules (from requirements)
//!
//! When the system doesn't know something:
//! 1. **Name the gap specifically.** Not "I don't know"; "I don't
//!    have documentation for Polygon's WebSocket reconnection
//!    behavior."
//! 2. **State the best guess** at where the answer lives:
//!    `Best guess: polygon.io/docs/options/ws_connecting`.
//! 3. **Ask once.** `Should I fetch it?`
//! 4. **If yes:** fetch, index, and the resource joins the project
//!    corpus — we never ask about it again.
//! 5. **If no:** note the gap and continue. If the gap becomes
//!    blocking later, the protocol asks again at that moment —
//!    but it does NOT re-ask the same gap speculatively.
//!
//! The system does not:
//! - Hallucinate an answer and state it confidently.
//! - Ask for the same information twice.
//! - Build infrastructure to avoid asking.
//! - Present the gap as a failure.
//!
//! ## Structure
//!
//! [`Gap`] names what's missing. [`Guess`] is the best-guess source.
//! [`HonestyProtocol`] is the driver that runs the script. It
//! delegates all I/O to three small traits:
//!
//! - [`Interlocutor`] — how we render the question and read the
//!   answer. Production: stdin. Tests: scripted.
//! - [`Fetcher`] — how we resolve an accepted source into bytes
//!   indexed into the project corpus. Production: reqwest +
//!   `ProjectDocsStore`. Tests: in-memory mock.
//! - [`GapMemory`] — how we remember we asked. Production:
//!   `.sovereign/project.toml` under `[gaps]`. Tests: `BTreeMap`.
//!
//! The protocol is the policy. The traits are the seams.
//!
//! ## Testing demand surface
//!
//! The protocol is a cross-cutting primitive — M6.1/M6.3/M6.4/M6.6
//! each exercise it in a different shape. The tests in this module
//! document the invariants the protocol must uphold regardless of
//! the caller:
//!
//! - A gap with a best guess + user-accept resolves via the fetcher.
//! - A gap the user declines records a `Deferred` resolution.
//! - A previously-confronted gap returns its prior resolution
//!   instead of re-prompting (the "ask once" invariant).
//! - A custom source overrides the best guess but still flows
//!   through the fetcher.
//! - A failed fetch records a `Deferred` resolution — fetch-error
//!   is not a silent success.
//!
//! Callers (init, found, amend) are expected to add their own tests
//! that exercise the protocol AT the integration layer too — this
//! module only proves the policy, not any specific caller's shape.

// The first caller (M6.1) lands in a follow-up commit; until then
// the types and traits are reachable only from tests. Scoping the
// allowance to the module keeps us honest — the moment init uses
// any of these, its usage will pin them.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

// ─── Data types ──────────────────────────────────────────────────────────────

/// A named piece of information the system lacks.
///
/// The `id` is the dedup key — stable across runs. Pick ids that
/// name the *resource*, not the *moment*: `polygon.ws.reconnect`,
/// not `found.session.q3`. The memory layer stores resolutions
/// keyed on this id.
///
/// NOT `sovereign_contracts::types::epistemic::Gap`, which indexes into
/// `EpistemicState::demands` and carries acquisition routes; this is a
/// missing SOURCE with a best-guess location. nc-12 adjudicated the same
/// split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    pub id: String,
    /// Human-readable one-liner. Shown in the prompt.
    pub description: String,
    /// Best guess at where the answer lives. `None` means "we
    /// genuinely have no idea where to look" — rarer than you'd
    /// think; prefer an honest guess with low confidence over `None`.
    pub best_guess: Option<Guess>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guess {
    Url(String),
    Path(PathBuf),
    /// Natural-language hint ("probably in the team wiki under
    /// /docs/ingest"). Not actionable without user input; used when
    /// we know *about* a source but don't have a URL/path.
    Text(String),
}

/// Outcome of confronting a gap. Persisted via [`GapMemory`] so
/// repeats are idempotent.
///
/// NOT `sovereign_tools::sec_facts_render::Resolution` (a resolved SEC fact
/// or a refusal); this is what happened when a `Gap` was put to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The source was fetched and indexed. `source` is the effective
    /// URL/path that the fetcher consumed (may differ from the
    /// best guess if the user supplied a custom one).
    Fetched {
        source: String,
        bytes_indexed: usize,
    },
    /// The user declined, OR the fetcher failed. The gap is
    /// documented but not resolved. The protocol will NOT re-ask;
    /// the caller is expected to re-prompt only when the gap
    /// becomes *newly* blocking (a different moment, not the same
    /// moment).
    Deferred,
    /// Best guess was a [`Guess::Text`] with no actionable target,
    /// or the user accepted but there was nothing to fetch. Distinct
    /// from `Deferred` so callers can tell "nothing to do" from
    /// "user said no."
    Skipped,
}

/// What the user said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterlocutorAnswer {
    /// Use the gap's best guess verbatim.
    AcceptGuess,
    /// User supplied their own URL/path.
    Custom(String),
    /// User declined.
    Decline,
}

/// What a fetch attempt produced.
#[derive(Debug, Clone)]
pub enum FetchOutcome {
    Ok { bytes_indexed: usize },
    Err(String),
}

// ─── Seams (traits) ──────────────────────────────────────────────────────────

/// Renders the question and reads the answer. Split from the
/// protocol so tests can script answers and so non-CLI hosts (IDE
/// integrations, future TUI) can plug in their own prompting.
pub trait Interlocutor {
    fn ask(&mut self, gap: &Gap) -> InterlocutorAnswer;
}

/// Resolves an accepted source into bytes-in-the-corpus. In
/// production this owns both the HTTP client and the
/// `ProjectDocsStore` handle; tests substitute an in-memory mock.
///
/// Contract: on `Ok`, the fetcher has ALREADY indexed the content.
/// `Resolution::Fetched` therefore guarantees "the corpus has this
/// now" — no two-phase dance at the protocol layer.
pub trait Fetcher {
    fn fetch_and_index(&self, source: &str) -> FetchOutcome;
}

/// Durable record of confronted gaps. Keyed on [`Gap::id`].
pub trait GapMemory {
    fn previously_asked(&self, gap_id: &str) -> Option<Resolution>;
    fn record(&mut self, gap_id: &str, resolution: &Resolution);
}

// ─── Protocol driver ─────────────────────────────────────────────────────────

/// Composes the three seams into the protocol. Calling code uses
/// only [`HonestyProtocol::confront`]; the protocol handles the
/// "ask once" rule and the accept-vs-fetch-vs-fail dispatch.
pub struct HonestyProtocol<I, F, M> {
    interlocutor: I,
    fetcher: F,
    memory: M,
}

impl<I: Interlocutor, F: Fetcher, M: GapMemory> HonestyProtocol<I, F, M> {
    pub fn new(interlocutor: I, fetcher: F, memory: M) -> Self {
        Self {
            interlocutor,
            fetcher,
            memory,
        }
    }

    /// Apply the protocol to a gap. Returns the resolution. Always
    /// persists to memory before returning — repeat calls with the
    /// same `gap.id` short-circuit via `previously_asked`.
    pub fn confront(&mut self, gap: &Gap) -> Resolution {
        if let Some(prior) = self.memory.previously_asked(&gap.id) {
            return prior;
        }
        let answer = self.interlocutor.ask(gap);
        let resolution = match answer {
            InterlocutorAnswer::Decline => Resolution::Deferred,
            InterlocutorAnswer::AcceptGuess => match gap.best_guess.as_ref() {
                Some(Guess::Url(u)) => self.try_fetch(u),
                Some(Guess::Path(p)) => self.try_fetch(&p.display().to_string()),
                // Text guess isn't directly fetchable — the user
                // would have needed to supply a Custom source. If
                // they accepted a text guess, there's nothing
                // machine-actionable to do; we record Skipped so
                // the next session doesn't re-ask.
                Some(Guess::Text(_)) | None => Resolution::Skipped,
            },
            InterlocutorAnswer::Custom(src) => self.try_fetch(&src),
        };
        self.memory.record(&gap.id, &resolution);
        resolution
    }

    fn try_fetch(&self, source: &str) -> Resolution {
        match self.fetcher.fetch_and_index(source) {
            FetchOutcome::Ok { bytes_indexed } => Resolution::Fetched {
                source: source.to_string(),
                bytes_indexed,
            },
            FetchOutcome::Err(_) => Resolution::Deferred,
        }
    }
}

// ─── Production impls ────────────────────────────────────────────────────────

/// Reads answers from stdin. The prompt format IS the protocol's
/// user-facing expression of honesty; change it carefully.
///
/// Layout:
/// ```text
/// ? I don't have documentation for Polygon's WebSocket reconnection behavior.
///   Best guess: https://polygon.io/docs/options/ws_connecting
///   Fetch it? [Y/n, or paste a different URL]
/// ```
///
/// Empty input → `AcceptGuess` (the default is always "yes, go get
/// it" — the protocol is meant to *reduce* friction, not inflate
/// it with defensive defaults).
pub struct StdinInterlocutor {
    writer: Box<dyn Write + Send>,
    reader: Box<dyn BufRead + Send>,
}

impl StdinInterlocutor {
    pub fn new() -> Self {
        Self {
            writer: Box::new(io::stderr()),
            reader: Box::new(io::BufReader::new(io::stdin())),
        }
    }
}

impl Default for StdinInterlocutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Interlocutor for StdinInterlocutor {
    fn ask(&mut self, gap: &Gap) -> InterlocutorAnswer {
        let _ = writeln!(self.writer, "? {}", gap.description);
        if let Some(g) = gap.best_guess.as_ref() {
            let _ = writeln!(self.writer, "  Best guess: {}", render_guess(g));
            let _ = write!(self.writer, "  Fetch it? [Y/n, or paste a different URL] ");
        } else {
            let _ = writeln!(self.writer, "  No guess — can you point me somewhere?");
            let _ = write!(self.writer, "  [paste a URL, or leave blank to skip] ");
        }
        let _ = self.writer.flush();

        let mut line = String::new();
        if self.reader.read_line(&mut line).is_err() {
            return InterlocutorAnswer::Decline;
        }
        parse_answer(&line, gap.best_guess.is_some())
    }
}

fn render_guess(g: &Guess) -> String {
    match g {
        Guess::Url(u) => u.clone(),
        Guess::Path(p) => p.display().to_string(),
        Guess::Text(t) => t.clone(),
    }
}

/// Parse a stdin response. Extracted so tests cover the input
/// grammar without needing a full `StdinInterlocutor`.
pub fn parse_answer(line: &str, has_guess: bool) -> InterlocutorAnswer {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return if has_guess {
            InterlocutorAnswer::AcceptGuess
        } else {
            InterlocutorAnswer::Decline
        };
    }
    let lower = trimmed.to_lowercase();
    match lower.as_str() {
        "y" | "yes" => InterlocutorAnswer::AcceptGuess,
        "n" | "no" | "skip" => InterlocutorAnswer::Decline,
        _ => {
            // Heuristic: anything that looks like a URL or path is
            // a Custom source. We don't try to be clever; if the
            // user typed "maybe later" that's going to hit fetch
            // and fail, recorded as Deferred — which is the right
            // outcome anyway.
            if trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with('/')
                || trimmed.starts_with('.')
            {
                InterlocutorAnswer::Custom(trimmed.to_string())
            } else {
                // Unparseable → treat as decline. Better to defer
                // than to fetch nonsense.
                InterlocutorAnswer::Decline
            }
        }
    }
}

/// In-memory memory backed by a `BTreeMap`. Used by tests and by
/// callers that want a transient protocol (e.g., a one-shot init
/// pass where durability is handled elsewhere).
#[derive(Default)]
pub struct InMemoryGapMemory {
    inner: BTreeMap<String, Resolution>,
}

impl InMemoryGapMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> BTreeMap<String, Resolution> {
        self.inner.clone()
    }
}

impl GapMemory for InMemoryGapMemory {
    fn previously_asked(&self, gap_id: &str) -> Option<Resolution> {
        self.inner.get(gap_id).cloned()
    }
    fn record(&mut self, gap_id: &str, resolution: &Resolution) {
        self.inner.insert(gap_id.to_string(), resolution.clone());
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Scripted interlocutor — hands out answers from a queue.
    struct ScriptedInterlocutor {
        answers: Vec<InterlocutorAnswer>,
        asked: RefCell<Vec<String>>,
    }
    impl ScriptedInterlocutor {
        fn new(answers: Vec<InterlocutorAnswer>) -> Self {
            Self {
                answers,
                asked: RefCell::new(Vec::new()),
            }
        }
        fn asked_ids(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }
    impl Interlocutor for ScriptedInterlocutor {
        fn ask(&mut self, gap: &Gap) -> InterlocutorAnswer {
            self.asked.borrow_mut().push(gap.id.clone());
            self.answers.remove(0)
        }
    }

    struct RecordingFetcher {
        result: FetchOutcome,
        calls: RefCell<Vec<String>>,
    }
    impl RecordingFetcher {
        fn ok(bytes: usize) -> Self {
            Self {
                result: FetchOutcome::Ok {
                    bytes_indexed: bytes,
                },
                calls: RefCell::new(Vec::new()),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                result: FetchOutcome::Err(msg.into()),
                calls: RefCell::new(Vec::new()),
            }
        }
        fn fetched(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }
    impl Fetcher for RecordingFetcher {
        fn fetch_and_index(&self, source: &str) -> FetchOutcome {
            self.calls.borrow_mut().push(source.to_string());
            self.result.clone()
        }
    }

    fn url_gap() -> Gap {
        Gap {
            id: "polygon.ws.reconnect".into(),
            description: "documentation for Polygon's WebSocket reconnect behavior".into(),
            best_guess: Some(Guess::Url("https://polygon.io/docs/ws".into())),
        }
    }

    #[test]
    fn accept_guess_fetches_and_records_resolution() {
        let interloc = ScriptedInterlocutor::new(vec![InterlocutorAnswer::AcceptGuess]);
        let fetcher = RecordingFetcher::ok(4321);
        let mut protocol = HonestyProtocol::new(interloc, fetcher, InMemoryGapMemory::new());
        let gap = url_gap();
        let res = protocol.confront(&gap);
        assert_eq!(
            res,
            Resolution::Fetched {
                source: "https://polygon.io/docs/ws".into(),
                bytes_indexed: 4321,
            }
        );
        // Fetcher saw the source.
        assert_eq!(
            protocol.fetcher.fetched(),
            vec!["https://polygon.io/docs/ws"]
        );
        // Memory remembers.
        assert!(protocol.memory.previously_asked(&gap.id).is_some());
    }

    #[test]
    fn decline_records_deferred_without_fetching() {
        let interloc = ScriptedInterlocutor::new(vec![InterlocutorAnswer::Decline]);
        let fetcher = RecordingFetcher::ok(999);
        let mut protocol = HonestyProtocol::new(interloc, fetcher, InMemoryGapMemory::new());
        let res = protocol.confront(&url_gap());
        assert_eq!(res, Resolution::Deferred);
        assert!(
            protocol.fetcher.fetched().is_empty(),
            "declined gap must not fetch"
        );
    }

    #[test]
    fn previously_asked_short_circuits_ask_once_invariant() {
        // Pre-populate memory with a prior Fetched outcome; protocol
        // must NOT call the interlocutor again.
        let interloc = ScriptedInterlocutor::new(vec![]); // would panic on pop
        let fetcher = RecordingFetcher::ok(0);
        let mut memory = InMemoryGapMemory::new();
        memory.record(
            "polygon.ws.reconnect",
            &Resolution::Fetched {
                source: "https://polygon.io/docs/ws".into(),
                bytes_indexed: 4321,
            },
        );
        let mut protocol = HonestyProtocol::new(interloc, fetcher, memory);
        let res = protocol.confront(&url_gap());
        match res {
            Resolution::Fetched { bytes_indexed, .. } => assert_eq!(bytes_indexed, 4321),
            other => panic!("expected cached Fetched, got {other:?}"),
        }
        assert!(
            protocol.interlocutor.asked_ids().is_empty(),
            "ask-once invariant: cached gap must not re-prompt"
        );
        assert!(
            protocol.fetcher.fetched().is_empty(),
            "cached gap must not re-fetch"
        );
    }

    #[test]
    fn custom_source_overrides_best_guess_through_fetcher() {
        let interloc = ScriptedInterlocutor::new(vec![InterlocutorAnswer::Custom(
            "https://internal.wiki/polygon-runbook".into(),
        )]);
        let fetcher = RecordingFetcher::ok(512);
        let mut protocol = HonestyProtocol::new(interloc, fetcher, InMemoryGapMemory::new());
        let res = protocol.confront(&url_gap());
        assert_eq!(
            res,
            Resolution::Fetched {
                source: "https://internal.wiki/polygon-runbook".into(),
                bytes_indexed: 512,
            }
        );
        assert_eq!(
            protocol.fetcher.fetched(),
            vec!["https://internal.wiki/polygon-runbook"],
            "custom source reaches the fetcher verbatim"
        );
    }

    #[test]
    fn fetch_failure_records_deferred_not_a_silent_success() {
        let interloc = ScriptedInterlocutor::new(vec![InterlocutorAnswer::AcceptGuess]);
        let fetcher = RecordingFetcher::err("HTTP 504 from polygon.io");
        let mut protocol = HonestyProtocol::new(interloc, fetcher, InMemoryGapMemory::new());
        let res = protocol.confront(&url_gap());
        assert_eq!(res, Resolution::Deferred);
        // And memory recorded Deferred so we don't re-ask next
        // session even though the fetch itself failed — that's
        // the "ask once" promise. The caller decides whether the
        // gap re-emerges at a later blocking moment.
        assert_eq!(
            protocol.memory.previously_asked("polygon.ws.reconnect"),
            Some(Resolution::Deferred),
        );
    }

    #[test]
    fn text_guess_skips_when_accepted_with_no_actionable_target() {
        let gap = Gap {
            id: "team.wiki.ingest".into(),
            description: "we need the team's notes on ingest throughput".into(),
            best_guess: Some(Guess::Text("probably in the team wiki".into())),
        };
        let interloc = ScriptedInterlocutor::new(vec![InterlocutorAnswer::AcceptGuess]);
        let fetcher = RecordingFetcher::ok(0);
        let mut protocol = HonestyProtocol::new(interloc, fetcher, InMemoryGapMemory::new());
        let res = protocol.confront(&gap);
        assert_eq!(res, Resolution::Skipped);
        assert!(
            protocol.fetcher.fetched().is_empty(),
            "text guesses are not fetchable — skip, don't invent a URL"
        );
    }

    #[test]
    fn parse_answer_input_grammar() {
        // Empty + has_guess → AcceptGuess
        assert_eq!(parse_answer("\n", true), InterlocutorAnswer::AcceptGuess);
        assert_eq!(parse_answer("", true), InterlocutorAnswer::AcceptGuess);
        // Empty + no guess → Decline
        assert_eq!(parse_answer("\n", false), InterlocutorAnswer::Decline);
        // Explicit yes/no
        assert_eq!(parse_answer("y\n", true), InterlocutorAnswer::AcceptGuess);
        assert_eq!(parse_answer("YES\n", true), InterlocutorAnswer::AcceptGuess);
        assert_eq!(parse_answer("n\n", true), InterlocutorAnswer::Decline);
        assert_eq!(parse_answer("no\n", true), InterlocutorAnswer::Decline);
        assert_eq!(parse_answer("skip\n", true), InterlocutorAnswer::Decline);
        // URL → Custom
        match parse_answer("https://foo.dev/x\n", true) {
            InterlocutorAnswer::Custom(s) => assert_eq!(s, "https://foo.dev/x"),
            other => panic!("expected Custom, got {other:?}"),
        }
        // Absolute path → Custom
        match parse_answer("/home/yara/docs.md\n", true) {
            InterlocutorAnswer::Custom(s) => assert_eq!(s, "/home/yara/docs.md"),
            other => panic!("expected Custom, got {other:?}"),
        }
        // Junk → Decline (safer than fetching gibberish)
        assert_eq!(
            parse_answer("maybe later\n", true),
            InterlocutorAnswer::Decline
        );
    }
}
