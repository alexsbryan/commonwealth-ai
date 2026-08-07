// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared middleware conventions — the implicit contract the ATOS
//! middlewares previously mirrored by hand.
//!
//! An 18-month git temporal-coupling pass found `approval_gate`,
//! `decision_extractor`, `context_injector`, `tool_injector`, and
//! `session_briefing` co-changing in 10+ commits with no structural
//! edge between them. The shared substance was threefold:
//!
//! 1. **On-disk layout of the `.sovereign/` state dir** — every
//!    middleware that persists or reads ATOS state re-derived
//!    `<repo_root>/.sovereign/notes.db` (and `features.db`) locally;
//!    `decision_extractor` even carried a "Mirror
//!    `approval_gate::notes_db_path`" comment. [`notes_db_path`] /
//!    [`features_db_path`] are now the single source of that
//!    convention.
//! 2. **System-prompt injection** — `context_injector` and
//!    `session_briefing` duplicated `prepend_to_system` verbatim.
//!    One copy lives here now; the byte-level behavior (prepend to
//!    the FIRST system message with a `\n` join, else insert a new
//!    system message at position 0) is pinned by unit tests below.
//! 3. **Test fixtures** — each middleware's test module hand-rolled
//!    the same all-`None` [`ChatCompletionRequest`] literal and
//!    [`PipelineContext`] literal. Eight of the ten joint commits
//!    were one-line additions of a new request field to every copy
//!    (`chat_template_kwargs`, `think_budget`, `tool_profile`,
//!    `sampling_mode`, `assistant_prefix`, `cmd_prefix` +
//!    `url_allowlist`, `evidence_id_allowlist`, `lark_grammar`).
//!    [`fixtures`] holds the one canonical copy; the next
//!    `ChatCompletionRequest` field addition touches this file only.

use std::path::{Path, PathBuf};

#[cfg(feature = "atos")]
use crate::openai_types::ChatCompletionRequest;

/// Canonical location of the notes store relative to the repo the
/// daemon is anchored to: `<repo_root>/.sovereign/notes.db`.
///
/// Every middleware that opens a [`corpus_engine_notes::NoteStore`]
/// must derive the path through here so they all read/write the
/// same file.
pub(crate) fn notes_db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("notes.db")
}

/// Canonical location of the ATOS feature store:
/// `<repo_root>/.sovereign/features.db`. Same single-source rationale
/// as [`notes_db_path`].
#[cfg(feature = "atos")]
pub(crate) fn features_db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("features.db")
}

/// Prepend `text` to the first system message; or insert a fresh
/// system message at position 0 when none exists.
///
/// The join is exactly `"{text}\n{existing}"` — middlewares that run
/// earlier in the pipeline end up ABOVE later ones in the final
/// system prompt, which is what the declared pipeline order means.
#[cfg(feature = "atos")]
pub(crate) fn prepend_to_system(request: &mut ChatCompletionRequest, text: &str) {
    use crate::openai_types::ChatMessage;

    for msg in &mut request.messages {
        if msg.role == "system" {
            msg.content = format!("{text}\n{}", msg.content);
            return;
        }
    }
    request
        .messages
        .insert(0, ChatMessage::new("system", text.to_string()));
}

/// Test fixtures shared by every middleware test module.
///
/// The single field-enumerating `ChatCompletionRequest` literal in
/// the crate lives in [`fixtures::minimal_request`]; everything else
/// builds on it with struct-update syntax. Adding a field to
/// `ChatCompletionRequest` therefore requires exactly one edit here
/// instead of one per middleware test module.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::PathBuf;

    use super::super::PipelineContext;
    use crate::openai_types::{ChatCompletionRequest, ChatMessage};

    /// A request with a single `user: "hi"` message and every
    /// optional field `None`.
    pub(crate) fn minimal_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages: vec![ChatMessage::new("user", "hi")],
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
            tool_profile: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            stable_prefix_len: None,
            ..Default::default()
        }
    }

    /// [`minimal_request`] with the message list replaced by the
    /// given `(role, content)` pairs.
    pub(crate) fn request_with_messages(messages: &[(&str, &str)]) -> ChatCompletionRequest {
        ChatCompletionRequest {
            messages: messages
                .iter()
                .map(|(role, content)| ChatMessage::new(*role, *content))
                .collect(),
            ..minimal_request()
        }
    }

    /// A `PipelineContext` for the given feature id + repo root,
    /// with neutral defaults everywhere else.
    pub(crate) fn ctx_with(feature_id: Option<&str>, repo: PathBuf) -> PipelineContext {
        PipelineContext {
            pipeline_name: "test".into(),
            model_id: "qwen-27b-coder".into(),
            context_config: Default::default(),
            feature_id: feature_id.map(String::from),
            session_id: Some("sess-1".into()),
            repo_root: repo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_db_lives_under_dot_sovereign() {
        let root = Path::new("/repo");
        assert_eq!(
            notes_db_path(root),
            PathBuf::from("/repo/.sovereign/notes.db")
        );
    }

    #[cfg(feature = "atos")]
    #[test]
    fn features_db_lives_under_dot_sovereign() {
        let root = Path::new("/repo");
        assert_eq!(
            features_db_path(root),
            PathBuf::from("/repo/.sovereign/features.db")
        );
    }

    #[cfg(feature = "atos")]
    mod prepend {
        use super::super::{fixtures, prepend_to_system};

        #[test]
        fn inserts_system_message_at_front_when_absent() {
            let mut req = fixtures::minimal_request();
            prepend_to_system(&mut req, "PREAMBLE");
            assert_eq!(req.messages.len(), 2);
            assert_eq!(req.messages[0].role, "system");
            assert_eq!(req.messages[0].content, "PREAMBLE");
            assert_eq!(req.messages[1].role, "user");
        }

        #[test]
        fn prepends_to_existing_system_message_with_newline_join() {
            let mut req = fixtures::request_with_messages(&[
                ("system", "Original directive."),
                ("user", "hi"),
            ]);
            prepend_to_system(&mut req, "PREAMBLE");
            // No new message; exact byte contract on the join.
            assert_eq!(req.messages.len(), 2);
            assert_eq!(req.messages[0].content, "PREAMBLE\nOriginal directive.");
        }

        #[test]
        fn only_the_first_system_message_is_touched() {
            let mut req = fixtures::request_with_messages(&[
                ("system", "first"),
                ("system", "second"),
                ("user", "hi"),
            ]);
            prepend_to_system(&mut req, "P");
            assert_eq!(req.messages[0].content, "P\nfirst");
            assert_eq!(req.messages[1].content, "second");
            assert_eq!(req.messages[2].content, "hi");
        }

        #[test]
        fn repeated_prepends_stack_newest_first() {
            // Pipeline order semantics: a middleware that runs later
            // ends up ABOVE earlier content only if it prepends —
            // two prepends stack newest-first.
            let mut req = fixtures::minimal_request();
            prepend_to_system(&mut req, "first");
            prepend_to_system(&mut req, "second");
            assert_eq!(req.messages[0].content, "second\nfirst");
        }
    }
}
