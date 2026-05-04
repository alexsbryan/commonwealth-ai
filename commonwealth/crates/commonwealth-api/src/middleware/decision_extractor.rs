//! `DecisionExtractor` — Phase 7.2 per-turn decision extraction.
//!
//! Inserted in the pipeline AFTER `artifact_surface`. The flow is:
//!
//! - **`post_process` (turn N)**: scan the assistant response with
//!   [`sovereign_tools::notes::response_mine`]. If a decision-like
//!   sentence fires, store it on `session.pending_decision`.
//!
//! - **`process` (turn N+1)**: inspect the stored candidate.
//!   - If the user's latest message contains a correction phrase
//!     ("actually, that's not a decision" / "scratch that" / etc.),
//!     drop the candidate without persisting.
//!   - Otherwise persist as a `source='extracted'` note and inject
//!     `[Noted: "<snippet>". Auto-recording unless corrected.]`
//!     into the system prompt. The agent sees the audit trail and
//!     can correct the next time around if anything's wrong.
//!
//! Either way `session.pending_decision` is cleared so the same
//! candidate doesn't fire twice.
//!
//! ## Why "post" then "next-turn process"
//!
//! The plan describes a "two-turn lookahead." The mechanic above
//! achieves it: the agent's response is mined in `post_process`,
//! the user's reaction is observed in the NEXT `process`. If the
//! user keeps going, the candidate becomes a note. If they push
//! back, it doesn't.
//!
//! This avoids surprising the user mid-turn (the agent's response
//! has already gone out by `post_process`) while still recording
//! decisions automatically — the user can always correct in the
//! same conversational style they'd already be using.
//!
//! ## Note kind
//!
//! Always `decision`. The audit's source-priority sort
//! (`agent > committed > extracted > inferred > observed`) puts
//! these below explicit `note(...)` calls but above `source='inferred'`
//! response-mining rows, which is exactly the trust ordering we
//! want — the agent gave us a sentence AND the user didn't push
//! back, so it's higher confidence than a blind regex pass over
//! the transcript.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use corpus_engine::{NoteScope, NoteSource, NoteStore};
use sovereign_tools::notes::response_mine;

use super::{Middleware, MiddlewareError, MiddlewareSession, PipelineContext, ResponseView};
use crate::openai_types::{ChatCompletionRequest, ChatMessage};

/// User-message substrings (case-insensitive) that drop a pending
/// candidate. Conservatively short — the goal is to recognise the
/// common correction shapes, not every conceivable phrasing.
///
/// Substring match is fine here: the user is replying *to* the
/// candidate, so any of these phrases anywhere in the message
/// signals "ignore that mining."
const CORRECTION_PHRASES: &[&str] = &[
    "scratch that",
    "ignore that",
    "not a decision",
    "no, that's not",
    "no that's not",
    "undo that",
    "drop that",
    "disregard",
    "rephrase",
    "rethinking",
    "actually no",
];

/// Maximum length of the snippet we keep on the session and surface
/// to the user. ResponseMiner already caps to ~320 chars per match;
/// this is a defense-in-depth ceiling.
const MAX_PENDING_SNIPPET_LEN: usize = 320;

/// The middleware itself. Stateless — all state lives on the
/// session. The pipeline executor instantiates one per request,
/// so the cost is just an Arc-clone.
pub struct DecisionExtractor;

impl DecisionExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DecisionExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for DecisionExtractor {
    fn id(&self) -> &'static str {
        "decision_extractor"
    }

    /// Pre-path: consume any pending candidate. If the user pushed
    /// back, drop it; otherwise persist as a `source='extracted'`
    /// note and surface the audit-trail line so the agent knows
    /// what was recorded.
    async fn process(
        &self,
        request: &mut ChatCompletionRequest,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        let Some(snippet) = session.pending_decision.take() else {
            return Ok(());
        };
        let user_msg = last_user_text(request).unwrap_or("").to_lowercase();
        if user_msg
            .lines()
            .any(|line| CORRECTION_PHRASES.iter().any(|p| line.contains(p)))
        {
            tracing::debug!(
                snippet = %truncate(&snippet, 80),
                "decision_extractor: user corrected; dropping pending candidate"
            );
            return Ok(());
        }

        // Persist. Errors are logged at warn — the audit's
        // `extracted` source is best-effort; other extraction
        // streams hold the floor (response_mine in 7.3 audit
        // assembly, observed patterns, commit harvester, agent).
        let session_id = ctx.session_id.as_deref().unwrap_or("decision-extractor");
        let notes_db = notes_db_path(&ctx.repo_root);
        match NoteStore::open(&notes_db) {
            Ok(store) => {
                let scope = if session.feature_id.is_some() {
                    NoteScope::Feature
                } else {
                    NoteScope::Global
                };
                if let Err(e) = store
                    .write_note_with_source(
                        "decision",
                        &snippet,
                        Vec::new(),
                        Vec::new(),
                        session_id,
                        scope,
                        session.feature_id.as_deref(),
                        None,
                        NoteSource::Extracted,
                        None,
                    )
                    .await
                {
                    tracing::warn!(
                        snippet = %truncate(&snippet, 80),
                        error = %e,
                        "decision_extractor: failed to persist extracted note"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    notes_db = %notes_db.display(),
                    error = %e,
                    "decision_extractor: notes DB unavailable; skipping persistence"
                );
            }
        }

        // Inject the audit-trail line so the agent sees what got
        // recorded. We attach to (or prepend) the system prompt
        // because the user prompt already exists. Format matches
        // the spec: `[Noted: "<decision>". Auto-recording unless
        // corrected.]` — single line so it doesn't change the
        // model's reasoning shape.
        inject_audit_trail(request, &snippet);

        Ok(())
    }

    /// Post-path: mine the assistant's response for decision-shaped
    /// sentences. Stash the first match on the session for the
    /// next turn to consume.
    async fn post_process(
        &self,
        response: &ResponseView<'_>,
        session: &mut MiddlewareSession,
        _ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        if response.content.is_empty() {
            return Ok(());
        }
        let matches = response_mine::mine(response.content);
        if let Some(first) = matches.into_iter().next() {
            let snippet = truncate(&first.sentence, MAX_PENDING_SNIPPET_LEN);
            session.pending_decision = Some(snippet);
        }
        Ok(())
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Mirror `approval_gate::notes_db_path`. Same convention so both
/// middleware pick up the same file.
fn notes_db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("notes.db")
}

/// Last user-role message body, if any. Used to detect a
/// correction phrase on the next turn.
fn last_user_text(request: &ChatCompletionRequest) -> Option<&str> {
    request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
}

/// Find or create a system message and inject the audit-trail
/// line as a separate paragraph. We APPEND rather than replace so
/// other middleware's preamble work is preserved.
fn inject_audit_trail(request: &mut ChatCompletionRequest, snippet: &str) {
    let line = format!(
        "[Noted: \"{}\". Auto-recording unless corrected.]",
        truncate_for_audit_line(snippet, 120)
    );
    if let Some(sys) = request.messages.iter_mut().find(|m| m.role == "system") {
        if !sys.content.is_empty() {
            sys.content.push_str("\n\n");
        }
        sys.content.push_str(&line);
    } else {
        request.messages.insert(0, ChatMessage::new("system", &line));
    }
}

/// Truncate `s` at a UTF-8-safe boundary (we don't want to slice
/// in the middle of a multi-byte character) and append `…` if
/// trimmed.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Walk forward to a char boundary at-or-before `max`.
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    format!("{}…", &s[..idx])
}

/// Like [`truncate`] but tighter — used for the audit line that
/// goes into the system prompt. Quotes need to fit without
/// cluttering the prompt with multi-paragraph snippets.
fn truncate_for_audit_line(s: &str, max: usize) -> String {
    truncate(&s.replace('\n', " "), max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::ChatMessage;

    fn ctx() -> PipelineContext {
        PipelineContext {
            pipeline_name: "test".into(),
            model_id: "qwen-27b-coder".into(),
            context_config: Default::default(),
            feature_id: Some("foo".into()),
            session_id: Some("sess-1".into()),
            repo_root: tempfile::tempdir().unwrap().path().to_path_buf(),
        }
    }

    fn req_with(messages: &[(&str, &str)]) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages: messages
                .iter()
                .map(|(role, content)| ChatMessage::new(*role, *content))
                .collect(),
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: None,
            tool_choice: None,
            oicp: None,
            response_format: None,
            chat_template_kwargs: None,
            think_budget: None,
        }
    }

    /// `post_process` mines the assistant response and stages a
    /// candidate on the session.
    #[tokio::test]
    async fn post_process_stages_candidate_from_decision_phrase() {
        let extractor = DecisionExtractor::new();
        let mut session = MiddlewareSession::default();
        let ctx = ctx();
        let view = ResponseView {
            content: "I'll use BTreeMap because we need ordered iteration.",
            finish_reason: Some("stop"),
            tool_calls_emitted: 0,
        };
        extractor
            .post_process(&view, &mut session, &ctx)
            .await
            .unwrap();
        assert!(session.pending_decision.is_some());
        let stored = session.pending_decision.unwrap();
        assert!(stored.contains("BTreeMap"), "got: {stored}");
    }

    /// Empty / non-decision responses don't stage anything.
    #[tokio::test]
    async fn post_process_skips_responses_without_decision_phrasing() {
        let extractor = DecisionExtractor::new();
        let mut session = MiddlewareSession::default();
        let ctx = ctx();
        for content in &[
            "",
            "Looking at the code now.",
            "The function returns 42.",
        ] {
            let view = ResponseView {
                content,
                finish_reason: Some("stop"),
                tool_calls_emitted: 0,
            };
            extractor
                .post_process(&view, &mut session, &ctx)
                .await
                .unwrap();
            assert!(
                session.pending_decision.is_none(),
                "should not stage on: {content:?}"
            );
        }
    }

    /// `process` with a pending candidate AND a user correction
    /// drops the candidate WITHOUT writing a note.
    #[tokio::test]
    async fn process_drops_candidate_on_correction_phrase() {
        let extractor = DecisionExtractor::new();
        let mut session = MiddlewareSession::default();
        session.pending_decision = Some("I'll use BTreeMap".into());
        let ctx = ctx();
        let mut req = req_with(&[
            ("user", "Actually no, that's not a decision; I was thinking aloud."),
        ]);
        extractor
            .process(&mut req, &mut session, &ctx)
            .await
            .unwrap();
        assert!(
            session.pending_decision.is_none(),
            "candidate should have been dropped"
        );
        // No system-prompt injection occurred.
        assert!(req.messages.iter().all(|m| m.role != "system"
            || !m.content.contains("Noted:")));
    }

    /// `process` without a correction phrase persists the note AND
    /// injects the audit-trail line into the system prompt.
    #[tokio::test]
    async fn process_persists_and_injects_audit_trail_when_no_correction() {
        let tmp = tempfile::tempdir().unwrap();
        let extractor = DecisionExtractor::new();
        let mut session = MiddlewareSession {
            feature_id: Some("foo".into()),
            pending_decision: Some(
                "I'll use BTreeMap because ordered iteration matters.".into(),
            ),
            ..Default::default()
        };
        let mut ctx = ctx();
        ctx.repo_root = tmp.path().to_path_buf();
        // Pre-create the .sovereign dir so write_note can open the DB.
        std::fs::create_dir_all(tmp.path().join(".sovereign")).unwrap();

        let mut req = req_with(&[("user", "Sounds good. Let's keep going.")]);
        extractor
            .process(&mut req, &mut session, &ctx)
            .await
            .unwrap();

        // Candidate cleared.
        assert!(session.pending_decision.is_none());
        // Audit-trail line injected.
        let sys = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .expect("system message inserted");
        assert!(
            sys.content.contains("Noted:"),
            "system content should carry the Noted line; got: {}",
            sys.content
        );
        assert!(sys.content.contains("BTreeMap"));

        // Note persisted with source='extracted'.
        let store = NoteStore::open(&tmp.path().join(".sovereign").join("notes.db"))
            .expect("notes DB exists");
        let rows = store
            .read_notes(None, &[], &[], &["decision".into()], 100, false)
            .await
            .unwrap();
        let extracted: Vec<_> = rows
            .iter()
            .filter(|n| n.source == NoteSource::Extracted.as_str())
            .collect();
        assert_eq!(
            extracted.len(),
            1,
            "expected exactly one extracted-source decision note"
        );
        assert!(extracted[0].content.contains("BTreeMap"));
    }

    /// Process is a no-op when there's no pending candidate.
    #[tokio::test]
    async fn process_is_noop_without_pending_candidate() {
        let extractor = DecisionExtractor::new();
        let mut session = MiddlewareSession::default();
        let ctx = ctx();
        let mut req = req_with(&[("user", "hello")]);
        extractor
            .process(&mut req, &mut session, &ctx)
            .await
            .unwrap();
        assert!(session.pending_decision.is_none());
        // No system message added.
        assert!(req.messages.iter().all(|m| m.role != "system"));
    }

    /// `truncate` is char-boundary safe.
    #[test]
    fn truncate_is_char_boundary_safe() {
        // Multi-byte char near the cap.
        let s = "hellö wörld";
        let out = truncate(s, 6);
        assert!(out.ends_with('…'));
        assert!(out.len() < s.len() + 4); // adds the ellipsis byte
    }

    /// All listed correction phrases trigger a drop.
    #[tokio::test]
    async fn every_correction_phrase_triggers_drop() {
        for phrase in CORRECTION_PHRASES {
            let extractor = DecisionExtractor::new();
            let mut session = MiddlewareSession::default();
            session.pending_decision = Some("candidate".into());
            let ctx = ctx();
            let user_msg = format!("OK {phrase}, let me think again.");
            let mut req = req_with(&[("user", &user_msg)]);
            extractor
                .process(&mut req, &mut session, &ctx)
                .await
                .unwrap();
            assert!(
                session.pending_decision.is_none(),
                "phrase {phrase:?} did not trigger drop"
            );
        }
    }
}
