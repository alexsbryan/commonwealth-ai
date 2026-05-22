//! `SessionBriefing` — prepends a "Welcome back" frame at the start
//! of a fresh or stale session.
//!
//! Yara closes her laptop on Thursday, comes back Tuesday morning. Her
//! opencode session is brand new but the feature isn't. Without this
//! middleware the model has to piece together "where are we" from the
//! notes digest + spec (which is accurate but not *foregrounded*).
//! SessionBriefing renders a compact state summary and pops any
//! `pending_artifact_delta` from the last turn into the same block,
//! so the very first preamble she sees is:
//!
//! ```text
//! ## Welcome back
//!
//! Feature: zotero-acquirer
//! Last milestone: 1 PASSED (45 minutes ago)
//! Next milestone: 2 — RDF parser integration
//! Stop: `cargo test -p corpus-engine extractors::zotero_rdf`
//!
//! ### Since last session
//! - ✓ Milestone 1 PASSED → `.sovereign/features/.../milestone-1.md`
//! - 2 uncertainties from last session
//!   - `[note:abc-1]` [uncertainty]
//! ```
//!
//! Ordering: runs BEFORE ContextInjector so the briefing is the
//! first block in the system prompt. ContextInjector then layers the
//! notes digest + spec beneath. The model's first response naturally
//! references the briefing.
//!
//! When-to-fire: `last_seen_at == 0` (fresh) OR
//! `now - last_seen_at > STALE_THRESHOLD` (2 hours). Short-gap
//! sessions skip the briefing entirely — Yara is mid-flow, she
//! doesn't need a re-orientation.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use corpus_engine::FeatureStore;

use super::{Middleware, MiddlewareError, MiddlewareSession, PipelineContext};
use crate::openai_types::{ChatCompletionRequest, ChatMessage};

/// Gap in seconds after which a session is considered "stale" and
/// the briefing re-fires. 2 hours — short enough to catch a lunch
/// break that turned into an afternoon detour, long enough that
/// normal flow (minute-scale turns) doesn't re-trigger.
const STALE_THRESHOLD_SECS: i64 = 2 * 60 * 60;

pub struct SessionBriefing;

impl SessionBriefing {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SessionBriefing {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for SessionBriefing {
    fn id(&self) -> &'static str {
        "session_briefing"
    }

    async fn process(
        &self,
        request: &mut ChatCompletionRequest,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        let Some(feature_id) = ctx.feature_id.clone() else {
            return Ok(());
        };
        if !is_session_start(session.last_seen_at) {
            return Ok(());
        }

        let delta = session.pending_artifact_delta.take();
        let briefing = compose_briefing(&ctx.repo_root, &feature_id, delta.as_ref()).await;
        if briefing.trim().is_empty() {
            return Ok(());
        }
        prepend_to_system(request, &briefing);
        Ok(())
    }
}

fn is_session_start(last_seen_at: i64) -> bool {
    if last_seen_at == 0 {
        return true;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    now.saturating_sub(last_seen_at) > STALE_THRESHOLD_SECS
}

async fn compose_briefing(
    repo_root: &Path,
    feature_id: &str,
    delta: Option<&sovereign_atos::session::ArtifactDelta>,
) -> String {
    let mut out = String::from("## Welcome back\n\n");
    out.push_str(&format!("Feature: `{feature_id}`\n"));

    // Pull feature + milestone + run state. Failure is non-fatal —
    // a briefing with just the feature id is still useful.
    let features_db = repo_root.join(".sovereign").join("features.db");
    if let Ok(store) = FeatureStore::open(&features_db) {
        append_milestone_state(&mut out, &store, feature_id).await;
    }

    if let Some(d) = delta {
        let block = render_delta_block(d);
        if !block.trim().is_empty() {
            out.push('\n');
            out.push_str(&block);
        }
    }

    out
}

async fn append_milestone_state(out: &mut String, store: &FeatureStore, feature_id: &str) {
    let milestones = store.list_milestones(feature_id).await.unwrap_or_default();
    if milestones.is_empty() {
        return;
    }
    let runs = store.list_runs_for_feature(feature_id).await.unwrap_or_default();

    // "Last milestone passed" = milestone with highest ordinal whose
    // normal-mode run has stop_passed=true. "Next milestone" = the
    // first unordered-passing milestone, which is the lowest ordinal
    // that hasn't yet passed in normal mode.
    let mut passed_ordinals: Vec<i64> = Vec::new();
    for m in &milestones {
        let passed_normal = runs.iter().any(|r| {
            r.milestone_id == m.id && r.mode == "normal" && r.stop_passed == Some(true)
        });
        if passed_normal {
            passed_ordinals.push(m.ordinal);
        }
    }
    passed_ordinals.sort_unstable();

    if let Some(&last_passed) = passed_ordinals.last() {
        out.push_str(&format!("Last passed milestone: {last_passed}\n"));
    } else {
        out.push_str("Last passed milestone: none yet\n");
    }

    let next = milestones
        .iter()
        .filter(|m| !passed_ordinals.contains(&m.ordinal))
        .min_by_key(|m| m.ordinal);
    if let Some(m) = next {
        let stop = extract_stop_condition(&m.brief_md);
        out.push_str(&format!(
            "Next milestone: {} — {}\n",
            m.ordinal,
            derive_title(&m.brief_md)
        ));
        if !stop.is_empty() {
            out.push_str(&format!("Stop: `{stop}`\n"));
        }
    } else {
        out.push_str("No open milestones — feature may be ready for teardown.\n");
    }
}

fn render_delta_block(delta: &sovereign_atos::session::ArtifactDelta) -> String {
    let mut out = String::from("### Since last session\n\n");
    let mut any = false;
    for event in &delta.milestones_passed {
        any = true;
        out.push_str(&format!(
            "- ✓ Milestone {} PASSED → `{}`\n",
            event.ordinal, event.artifact_path
        ));
    }
    if !delta.notes_by_kind.is_empty() {
        any = true;
        let summary: Vec<String> = delta
            .notes_by_kind
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect();
        out.push_str(&format!("- Notes: {}\n", summary.join(", ")));
        for kind in ["uncertainty", "deviation", "postmortem_pointer"] {
            if let Some(ids) = delta.recent_note_ids.get(kind) {
                for id in ids {
                    out.push_str(&format!("  - `[note:{id}]` [{kind}]\n"));
                }
            }
        }
    }
    if !any {
        return String::new();
    }
    out
}

/// Pull the stop-command line out of a milestone brief. Briefs
/// canonically embed it as `<!-- atos:stop_condition:<cmd> -->`.
fn extract_stop_condition(brief: &str) -> String {
    for line in brief.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("<!-- atos:stop_condition:") {
            if let Some(cmd) = rest.strip_suffix("-->") {
                return cmd.trim().to_string();
            }
        }
    }
    String::new()
}

fn derive_title(brief: &str) -> String {
    for line in brief.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("### ") {
            return rest.trim().to_string();
        }
        if let Some(rest) = t.strip_prefix("# ") {
            return rest.trim().to_string();
        }
    }
    "(untitled)".into()
}

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

    #[test]
    fn fresh_session_triggers_briefing() {
        assert!(is_session_start(0));
    }

    #[test]
    fn recent_session_skips_briefing() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(!is_session_start(now - 60));
        assert!(!is_session_start(now - 30 * 60));
    }

    #[test]
    fn stale_session_triggers_briefing() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // 3 hours ago — past the 2h threshold.
        assert!(is_session_start(now - 3 * 60 * 60));
    }

    #[tokio::test]
    async fn no_feature_id_is_noop() {
        let mw = SessionBriefing::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(None, std::env::temp_dir());
        mw.process(&mut req, &mut session, &ctx).await.unwrap();
        assert!(!req.messages.iter().any(|m| m.role == "system"));
    }

    #[tokio::test]
    async fn mid_session_skips_briefing() {
        let mw = SessionBriefing::new();
        let mut req = minimal_request();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let mut session = MiddlewareSession {
            last_seen_at: now - 60,
            ..Default::default()
        };
        let ctx = ctx_with(Some("fx"), std::env::temp_dir());
        mw.process(&mut req, &mut session, &ctx).await.unwrap();
        assert!(!req.messages.iter().any(|m| m.role == "system"));
    }

    #[tokio::test]
    async fn fresh_session_injects_briefing_with_feature_id() {
        let mw = SessionBriefing::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession {
            last_seen_at: 0,
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(Some("zotero-probe"), tmp.path().to_path_buf());
        mw.process(&mut req, &mut session, &ctx).await.unwrap();
        let system = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .expect("briefing must inject a system message");
        assert!(system.content.contains("## Welcome back"));
        assert!(system.content.contains("zotero-probe"));
    }

    #[tokio::test]
    async fn briefing_pops_pending_artifact_delta() {
        let mw = SessionBriefing::new();
        let mut req = minimal_request();

        let mut delta = sovereign_atos::session::ArtifactDelta::default();
        delta.milestones_passed.push(sovereign_atos::session::MilestonePassEvent {
            feature_id: "fx".into(),
            ordinal: 1,
            artifact_path: ".sovereign/features/fx/milestone-1.md".into(),
        });
        delta
            .notes_by_kind
            .insert("uncertainty".into(), 2);
        delta
            .recent_note_ids
            .insert("uncertainty".into(), vec!["abc-1".into(), "abc-2".into()]);

        let mut session = MiddlewareSession {
            last_seen_at: 0,
            pending_artifact_delta: Some(delta),
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        mw.process(&mut req, &mut session, &ctx).await.unwrap();

        let system = req.messages.iter().find(|m| m.role == "system").unwrap();
        assert!(system.content.contains("Since last session"));
        assert!(system.content.contains("Milestone 1 PASSED"));
        assert!(system.content.contains("[note:abc-1]"));
        // Delta popped — ContextInjector won't see it.
        assert!(session.pending_artifact_delta.is_none());
    }
}
