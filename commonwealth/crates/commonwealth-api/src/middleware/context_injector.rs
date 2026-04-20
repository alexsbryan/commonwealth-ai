//! `ContextInjector` — prepends ATOS context to the system prompt so
//! the model "happens to" know the current notes digest + the
//! feature spec without the agent having to remember to load them.
//!
//! Composition (in order):
//!
//! 1. A fixed `<atos-instructions/>` block — tells the model when to
//!    write decision/invariant/attempt/uncertainty notes. Baked
//!    into the source so operators can edit it without a config
//!    redeploy.
//! 2. The notes digest for `scope=[Global, Feature(feature_id)]`.
//!    Cached in SQLite by `NoteStore::digest_cache_*`; on miss we
//!    concatenate note headers rather than call the Fast slot here
//!    (middleware doesn't do inference — the Fast-slot summary is
//!    the MCP tool's job).
//! 3. `.sovereign/features/<id>/spec.md` content — the approved
//!    specification.
//! 4. A drift-acknowledgment suffix when
//!    `session.pending_deviation_ack` is set, naming the deviation
//!    note id so the agent can look it up.
//!
//! The composed block is prepended to the first `role = system`
//! message; when the request has no system message, one is inserted
//! at the top.

use std::path::Path;

use async_trait::async_trait;

use corpus_engine::{NoteStore, ScopeFilter};
use sovereign_atos::approval;

use super::{Middleware, MiddlewareError, MiddlewareSession, PipelineContext};
use crate::openai_types::{ChatCompletionRequest, ChatMessage};

const ATOS_INSTRUCTIONS: &str = r#"<atos-instructions>
You are working inside an ATOS-orchestrated session.

- **Before editing code**: call `read_notes` with the relevant symbols
  or files. Honour invariants. Build on decisions. Don't repeat
  documented failed attempts.
- **When you choose one approach over another**: call `write_note`
  with `kind="decision"`, including the alternatives you rejected
  and why.
- **When you discover a constraint** (something that would break if
  violated): `kind="invariant"`.
- **When an approach fails**: `kind="attempt"`, explaining why.
- **When a spec clause under-specifies a real case**:
  `kind="uncertainty"`, describing the case and your interim
  decision. These surface in the epistemic report for human review.
- Stop condition outputs are captured by the orchestrator. You do
  NOT need to paste test output unless asked.
</atos-instructions>
"#;

pub struct ContextInjector;

impl ContextInjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ContextInjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for ContextInjector {
    fn id(&self) -> &'static str {
        "context_injector"
    }

    async fn process(
        &self,
        request: &mut ChatCompletionRequest,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        let Some(feature_id) = ctx.feature_id.clone() else {
            tracing::debug!("context_injector: no feature_id; skipping");
            return Ok(());
        };

        let mut preamble = String::new();
        preamble.push_str(ATOS_INSTRUCTIONS);

        // Notes digest (if enabled for this pipeline).
        if ctx.context_config.inject_notes {
            let digest = compose_notes_digest(&ctx.repo_root, &feature_id).await;
            if !digest.trim().is_empty() {
                preamble.push_str("\n## Active notes\n\n");
                preamble.push_str(&digest);
                preamble.push('\n');
            }
        }

        // Spec.md (if enabled).
        if ctx.context_config.inject_spec {
            if let Some(spec) = read_spec(
                &ctx.repo_root,
                &feature_id,
                ctx.context_config.inject_invariants_only,
            ) {
                preamble.push_str("\n## Feature specification\n\n");
                preamble.push_str(&spec);
                preamble.push('\n');
            }
        }

        // Drift acknowledgment (only when the approval gate set the
        // flag earlier in the chain).
        if session.pending_deviation_ack {
            preamble.push_str("\n⚠ **Spec drift detected since approval.** ");
            if let Some(ref note_id) = session.deviation_note_id {
                preamble.push_str(&format!("See `[note:{note_id}]`. "));
            }
            preamble.push_str(
                "Either write an intentional deviation note explaining the change, or \
                 revert spec.md to the approved version before proceeding.\n",
            );
        }

        prepend_to_system(request, &preamble);

        Ok(())
    }
}

// ─── Composition helpers ─────────────────────────────────────────────────────

/// Pull a compact digest from the SQLite cache. Cache hit = cheap.
/// Cache miss = fall back to concatenated note headers (the same
/// shape as `read_note_digest`'s no-inference path), so the model
/// always gets something even if the Fast slot hasn't run yet.
async fn compose_notes_digest(repo_root: &Path, feature_id: &str) -> String {
    let notes_path = repo_root.join(".sovereign").join("notes.db");
    let Ok(store) = NoteStore::open(&notes_path) else {
        return String::new();
    };

    // Try cache first — digest keyed on (scope_hash, notes_version).
    // We don't recompute the hash here; the MCP tool owns that. If
    // cache misses, we fall back below.
    // The cache is keyed by an external hash function; reading it
    // directly would require duplicating ReadNoteDigestTool's
    // hash. Keep it simple: read headers from NoteStore.
    use corpus_engine::NoteScope;
    let filter = ScopeFilter {
        scopes: vec![NoteScope::Global, NoteScope::Feature],
        feature_id: Some(feature_id.to_string()),
    };
    let notes = store
        .read_notes_scoped(
            None,
            &[],
            &[],
            &[
                "decision".to_string(),
                "invariant".to_string(),
                "attempt".to_string(),
                "uncertainty".to_string(),
                "postmortem_pointer".to_string(),
                "deviation".to_string(),
            ],
            40,
            false,
            &filter,
        )
        .await
        .unwrap_or_default();

    if notes.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    for n in notes {
        let scope_tag = if n.scope == "global" {
            "global".into()
        } else {
            format!("{}:{}", n.scope, n.feature_id.as_deref().unwrap_or(""))
        };
        let first_line: String = n.content.lines().next().unwrap_or("").chars().take(160).collect();
        out.push_str(&format!(
            "- `[note:{}]` [{}] [{}] {}\n",
            n.id, n.kind, scope_tag, first_line
        ));
    }
    out
}

/// Read spec.md and optionally slice to the `## Invariants` section
/// (red-team pipelines set `inject_invariants_only = true`).
fn read_spec(repo_root: &Path, feature_id: &str, invariants_only: bool) -> Option<String> {
    let path = approval::spec_path(repo_root, feature_id);
    let content = std::fs::read_to_string(&path).ok()?;
    if !invariants_only {
        return Some(content);
    }
    Some(extract_invariants_section(&content))
}

/// Slice out the `## Invariants` (or `## Invariants to respect`)
/// section — everything from that heading up to the next `##` or
/// EOF. Falls back to the full document when no matching heading
/// exists so red-team sessions don't silently see nothing.
fn extract_invariants_section(md: &str) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let mut start: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate() {
        let t = line.trim_start_matches(' ').to_lowercase();
        if t.starts_with("## invariants") {
            start = Some(idx);
            break;
        }
    }
    let Some(start_idx) = start else {
        return md.to_string();
    };
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start_idx + 1) {
        if line.starts_with("## ") {
            end = i;
            break;
        }
    }
    lines[start_idx..end].join("\n")
}

/// Prepend `text` to the first system message; or insert a fresh
/// system message at position 0 when none exists.
fn prepend_to_system(request: &mut ChatCompletionRequest, text: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::ChatMessage;
    use std::path::PathBuf;

    fn minimal_request() -> ChatCompletionRequest {
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
        }
    }

    fn ctx_with(feature_id: Option<&str>, repo: PathBuf) -> PipelineContext {
        PipelineContext {
            pipeline_name: "test".into(),
            model_id: "qwen-27b-coder".into(),
            context_config: Default::default(),
            feature_id: feature_id.map(String::from),
            session_id: Some("sess-1".into()),
            repo_root: repo,
        }
    }

    #[tokio::test]
    async fn no_feature_id_is_noop() {
        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(None, std::env::temp_dir());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();
        // No system message was injected.
        assert!(!req.messages.iter().any(|m| m.role == "system"));
    }

    #[tokio::test]
    async fn inject_creates_system_message_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path().join(".sovereign").join("features").join("fx");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(
            spec_dir.join("spec.md"),
            "# fx — Title\n\n## Invariants\n\n- foo must not bar.\n",
        )
        .unwrap();

        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();

        // Now there's a system message at position 0 with the
        // ATOS instructions and the spec.
        assert_eq!(req.messages[0].role, "system");
        assert!(req.messages[0].content.contains("<atos-instructions>"));
        assert!(req.messages[0].content.contains("foo must not bar"));
    }

    #[tokio::test]
    async fn existing_system_message_prepended_to() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path().join(".sovereign").join("features").join("fx");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), "# minimal spec\n").unwrap();

        let inj = ContextInjector::new();
        let mut req = minimal_request();
        req.messages.insert(0, ChatMessage::new("system", "Original system directive."));
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();

        assert_eq!(req.messages[0].role, "system");
        assert!(req.messages[0].content.contains("<atos-instructions>"));
        assert!(req.messages[0].content.contains("Original system directive."));
    }

    #[tokio::test]
    async fn drift_ack_rendered_when_session_flag_set() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path().join(".sovereign").join("features").join("fx");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), "# spec\n").unwrap();

        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        session.pending_deviation_ack = true;
        session.deviation_note_id = Some("abc-123".into());
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();

        let sys = &req.messages[0].content;
        assert!(sys.contains("Spec drift detected"));
        assert!(sys.contains("[note:abc-123]"));
    }

    #[test]
    fn invariants_extraction_slices_correctly() {
        let md = "# title\n\nblah\n\n## Overview\n\noverview body\n\n## Invariants\n\n- one\n- two\n\n## Next\n\nother\n";
        let slice = extract_invariants_section(md);
        assert!(slice.contains("## Invariants"));
        assert!(slice.contains("- one"));
        assert!(slice.contains("- two"));
        assert!(!slice.contains("overview body"));
        assert!(!slice.contains("other"));
    }

    #[test]
    fn invariants_extraction_falls_back_when_missing() {
        let md = "# title\n\nno invariants heading here\n";
        let slice = extract_invariants_section(md);
        assert!(slice.contains("no invariants"));
    }
}
