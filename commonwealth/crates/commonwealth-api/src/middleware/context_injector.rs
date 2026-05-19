//! `ContextInjector` — prepends ATOS context to the system prompt so
//! the model "happens to" know the current notes digest + the
//! feature spec without the agent having to remember to load them.
//!
//! Composition (in order):
//!
//! 1. A fixed `<atos-instructions/>` block — tells the model when to
//!    write decision/invariant/attempt/uncertainty notes. Sourced
//!    from `assets/atos_instructions.md` via `include_str!` so the
//!    instructions live as data, not code, and an operator can diff
//!    / propose edits without reading Rust source.
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

/// Agent-facing preamble (the `<atos-instructions>` XML block). Kept as
/// an external asset so the instructions — which are *data* the model
/// reads — don't have to round-trip through Rust source edits to
/// update. `include_str!` pins them at compile time, so the final
/// binary carries a copy and there is no runtime file dependency.
const ATOS_INSTRUCTIONS: &str = include_str!("../../assets/atos_instructions.md");

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

        // Sovereign CLI tools catalog — Phase 3.6. Pulled live from
        // the shared manifest so it can't drift. The CLI path is the
        // MCP-less fallback (and preferred surface for ad-hoc
        // scripting); agents with MCP configured should still prefer
        // the native MCP calls because they're faster and structured,
        // but we want opencode to KNOW the CLI exists so it can use
        // it when MCP isn't available.
        preamble.push_str(&compose_cli_tools_catalog());

        // "Since last turn" block — populated by ArtifactSurface's
        // post_process on the PREVIOUS turn. Popped on render so it
        // shows up exactly once.
        if let Some(delta) = session.pending_artifact_delta.take() {
            let block = render_artifact_delta(&delta);
            if !block.trim().is_empty() {
                preamble.push_str("\n");
                preamble.push_str(&block);
            }
        }

        // Project charter (M7.2) — the project-layer invariants +
        // current phase live ABOVE the feature spec so they frame
        // every turn. When the project hasn't been founded, we
        // silently skip — feature work on pre-M6 repos still works.
        if let Some(block) = compose_charter_frame(&ctx.repo_root) {
            preamble.push_str("\n");
            preamble.push_str(&block);
        }

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

/// Phase 3.6: live catalog of `sovereign tools` CLI commands,
/// grouped by effect + scope. Pulled from
/// `sovereign_tools::manifest::all_descriptors` (cached via
/// `OnceLock`), so adding a tool auto-surfaces in the agent
/// preamble.
///
/// Rendered as a compact markdown block — one line per tool, effect
/// tag up front, one-sentence description. The agent uses this to
/// know a CLI alternative exists when MCP isn't available; it
/// doesn't replace the MCP tool list the agent sees via the
/// client-side `tools/list` handshake.
fn compose_cli_tools_catalog() -> String {
    use sovereign_core::types::{Effect, Scope, ToolDescriptor};
    use std::collections::BTreeMap;

    let descriptors: Vec<ToolDescriptor> =
        sovereign_tools::manifest::all_descriptors().to_vec();
    if descriptors.is_empty() {
        return String::new();
    }

    // Stable group order — reads first, writes after, session before
    // persistent before external. Matches what `sovereign tools list`
    // produces so the CLI and the preamble tell the same story.
    let group_order: &[(Effect, Scope, &str)] = &[
        (Effect::Read, Scope::Session, "Read · Session"),
        (Effect::Read, Scope::Persistent, "Read · Persistent"),
        (Effect::Read, Scope::External, "Read · External"),
        (Effect::Write, Scope::Session, "Write · Session"),
        (Effect::Write, Scope::Persistent, "Write · Persistent"),
        (Effect::Write, Scope::External, "Write · External"),
        (Effect::ReadWrite, Scope::Session, "ReadWrite · Session"),
        (Effect::ReadWrite, Scope::Persistent, "ReadWrite · Persistent"),
        (Effect::ReadWrite, Scope::External, "ReadWrite · External"),
    ];
    let mut grouped: BTreeMap<(u8, u8), Vec<&ToolDescriptor>> = BTreeMap::new();
    for d in &descriptors {
        let e = match d.effect {
            Effect::Read => 0,
            Effect::Write => 1,
            Effect::ReadWrite => 2,
        };
        let s = match d.scope {
            Scope::Session => 0,
            Scope::Persistent => 1,
            Scope::External => 2,
        };
        grouped.entry((e, s)).or_default().push(d);
    }

    let mut out = String::from(
        "\n## Available via `sovereign tools` CLI\n\n\
         These tools are callable as shell commands when MCP isn't available.\n\
         `sovereign tools call <id> --key=value ...` invokes; \
         `sovereign tools describe <id>` prints the full schema incl. output keys.\n\n",
    );
    for (idx, (effect, scope, label)) in group_order.iter().enumerate() {
        let e_idx = match effect {
            Effect::Read => 0u8,
            Effect::Write => 1,
            Effect::ReadWrite => 2,
        };
        let s_idx = match scope {
            Scope::Session => 0u8,
            Scope::Persistent => 1,
            Scope::External => 2,
        };
        let Some(tools) = grouped.get(&(e_idx, s_idx)) else {
            continue;
        };
        let _ = idx;
        out.push_str(&format!("**{label}**  \n"));
        let mut sorted = tools.clone();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        for d in sorted {
            // First sentence, capped at 80 chars, matches the CLI's
            // `tools list` formatting.
            let desc = first_sentence(&d.description);
            out.push_str(&format!("- `{}` — {desc}\n", d.id));
        }
        out.push('\n');
    }
    out
}

fn first_sentence(desc: &str) -> String {
    let cleaned: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = cleaned.find(". ").map(|i| &cleaned[..i]).unwrap_or(&cleaned);
    if cut.len() > 80 {
        format!("{}…", &cut[..77])
    } else {
        cut.to_string()
    }
}

/// M7.2: assemble the project-layer charter frame for the
/// preamble. Returns `None` when:
/// - the repo hasn't been `sovereign project found`ed (no
///   `.sovereign/CHARTER.md`),
/// - reading fails,
/// - the invariants section is empty.
///
/// The frame intentionally prepends ABOVE the feature spec — the
/// agent's first-pass reading should see project invariants,
/// then the current phase (so it knows where in the plan it is),
/// then feature specifics. Keeping it compact (invariants +
/// one-line phase marker) avoids drowning the spec.
fn compose_charter_frame(repo_root: &Path) -> Option<String> {
    let sov = repo_root.join(".sovereign");
    let charter = std::fs::read_to_string(sov.join("CHARTER.md")).ok()?;
    let invariants = extract_named_section(&charter, "Invariants");
    let phase_marker = read_current_phase_marker(&sov);
    let drift_tag = detect_charter_drift(&sov, &charter);

    let mut out = String::from("## Project charter\n\n");
    if let Some(p) = phase_marker {
        out.push_str(&format!("_{}_\n\n", p));
    }
    if let Some(tag) = drift_tag {
        out.push_str(&format!("{tag}\n\n"));
    }
    let inv = invariants.unwrap_or_default();
    if inv.trim().is_empty() {
        // No invariants → frame is just the phase marker + drift.
        // If BOTH are also empty, there's nothing to say.
        if phase_marker_present(repo_root) || drift_tag_present(&sov) {
            return Some(out);
        }
        return None;
    }
    out.push_str("**Invariants**\n\n");
    out.push_str(inv.trim());
    out.push('\n');
    Some(out)
}

/// Minimal section extractor — `## <name>` body up to the next
/// `## ` heading or EOF. Avoids a full markdown parse for a cheap
/// read path the middleware runs on every turn.
fn extract_named_section(md: &str, name: &str) -> Option<String> {
    let heading = format!("## {name}");
    let idx = md.find(&heading)?;
    let rest = &md[idx + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn read_current_phase_marker(sovereign_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(sovereign_dir.join("project.toml")).ok()?;
    // Cheap scan for `current_phase = N` — avoids pulling the
    // toml crate into this hot path. Middleware runs per-turn.
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("current_phase") {
            if let Some(val) = rest.split('=').nth(1) {
                let n: i64 = val.trim().parse().ok()?;
                return Some(format!("current phase: {n}"));
            }
        }
    }
    None
}

fn phase_marker_present(repo_root: &Path) -> bool {
    read_current_phase_marker(&repo_root.join(".sovereign")).is_some()
}

/// Read `lifecycle.charter_hash` from project.toml without pulling
/// in the toml crate for this hot path.
fn recorded_charter_hash(sovereign_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(sovereign_dir.join("project.toml")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("charter_hash") {
            if let Some(val) = rest.split('=').nth(1) {
                let v = val.trim().trim_matches('"').to_string();
                if v.is_empty() {
                    return None;
                }
                return Some(v);
            }
        }
    }
    None
}

fn hash_sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Returns `Some("⚠ ...")` when on-disk CHARTER.md has drifted
/// from the hash recorded at founding/amend time. `None` when the
/// hash is unknown (never founded) OR matches.
fn detect_charter_drift(sovereign_dir: &Path, charter: &str) -> Option<String> {
    let recorded = recorded_charter_hash(sovereign_dir)?;
    let current = hash_sha256_hex(charter);
    if recorded == current {
        None
    } else {
        Some(format!(
            "⚠ **Charter drift** — CHARTER.md on disk differs from the recorded hash. \
             Run `sovereign project amend` to reconcile, or `git checkout --` \
             the file."
        ))
    }
}

fn drift_tag_present(sovereign_dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(sovereign_dir.join("CHARTER.md")) else {
        return false;
    };
    detect_charter_drift(sovereign_dir, &text).is_some()
}

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

/// Render the "Since last turn" preamble block from an
/// [`ArtifactDelta`]. Compact by design — a few bullets per kind,
/// no note bodies (just ids so the agent can expand via
/// `read_note_by_id`).
fn render_artifact_delta(delta: &sovereign_atos::session::ArtifactDelta) -> String {
    let mut out = String::from("## Since last turn\n\n");
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
        out.push_str(&format!(
            "- Notes written this session: {}\n",
            summary.join(", ")
        ));
        // Render the most interesting kinds' recent ids so the
        // agent can reference them by `[note:<id>]`.
        for kind in ["uncertainty", "deviation", "postmortem_pointer"] {
            if let Some(ids) = delta.recent_note_ids.get(kind) {
                for id in ids {
                    out.push_str(&format!("  - `[note:{}]` [{kind}]\n", id));
                }
            }
        }
    }
    if !any {
        return String::new();
    }
    out
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
            response_format: None,
            chat_template_kwargs: None,
            think_budget: None,
            tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
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
    async fn cli_tools_catalog_is_surfaced_in_preamble() {
        // Phase 3.6 pin — every session preamble carries a live
        // `sovereign tools` catalog so opencode agents discover the
        // CLI path without reading CLAUDE.md. Regresses if someone
        // removes the compose_cli_tools_catalog call.
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path().join(".sovereign").join("features").join("fx");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), "# fx\n").unwrap();

        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();
        let content = &req.messages[0].content;

        assert!(
            content.contains("Available via `sovereign tools` CLI"),
            "CLI catalog header missing from preamble"
        );
        // Sample tool from the manifest — regresses if the manifest
        // integration stops pulling descriptors. Phase 2 of the CLI
        // refactor renamed `symbol_lookup` → `symbols` at the
        // descriptor layer, so the canonical id is what the
        // catalog renders.
        assert!(
            content.contains("symbols"),
            "expected a code-intel tool id in the catalog"
        );
        // Grouping tag from the catalog layout.
        assert!(
            content.contains("Read · Persistent"),
            "expected Effect × Scope grouping in the catalog"
        );
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

    #[tokio::test]
    async fn pending_artifact_delta_rendered_then_popped() {
        use sovereign_atos::session::{ArtifactDelta, MilestonePassEvent};

        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path().join(".sovereign").join("features").join("fx");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), "# fx\n").unwrap();

        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let mut delta = ArtifactDelta::default();
        delta
            .notes_by_kind
            .insert("uncertainty".into(), 2);
        delta
            .recent_note_ids
            .insert("uncertainty".into(), vec!["abc-1".into(), "abc-2".into()]);
        delta.milestones_passed.push(MilestonePassEvent {
            feature_id: "fx".into(),
            ordinal: 2,
            artifact_path: ".sovereign/features/fx/milestone-2.md".into(),
        });
        session.pending_artifact_delta = Some(delta);
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();

        // Render landed in system message.
        let sys = &req.messages[0].content;
        assert!(sys.contains("Since last turn"));
        assert!(sys.contains("Milestone 2 PASSED"));
        assert!(sys.contains("[note:abc-1]"));
        assert!(sys.contains("2 uncertainty"));

        // Delta was popped — next call renders no "Since last turn"
        // section.
        assert!(session.pending_artifact_delta.is_none());
        let mut req2 = minimal_request();
        inj.process(&mut req2, &mut session, &ctx).await.unwrap();
        assert!(!req2.messages[0].content.contains("Since last turn"));
    }

    // ── M7.2: charter frame ───────────────────────────────────

    fn seed_founded_project(repo_root: &Path, charter_body: &str) {
        let sov = repo_root.join(".sovereign");
        std::fs::create_dir_all(&sov).unwrap();
        std::fs::write(sov.join("CHARTER.md"), charter_body).unwrap();
        let hash = hash_sha256_hex(charter_body);
        let toml_body = format!(
            "schema_version = 1\n\n[observation]\nobserved_at = 0\nhas_git = true\nembed_model_available = true\n\n[lifecycle]\nfounded = true\ncharter_version = 1\ncurrent_phase = 1\ncharter_hash = \"{hash}\"\n"
        );
        std::fs::write(sov.join("project.toml"), toml_body).unwrap();
        // Feature spec so the injector has something to work with.
        let feat_dir = sov.join("features").join("fx");
        std::fs::create_dir_all(&feat_dir).unwrap();
        std::fs::write(feat_dir.join("spec.md"), "# fx — Feature\n").unwrap();
    }

    const CHARTER_SAMPLE: &str = r#"# proj — Charter

## System design

Build a thing.

## Invariants

- Persistence contract X: schema-stable.
- External assumption Y: rate limits apply.

## Resolved decisions

- Decided Z.

## Open questions

## Amendment log

_(empty)_
"#;

    #[tokio::test]
    async fn charter_frame_injected_when_project_is_founded() {
        let tmp = tempfile::tempdir().unwrap();
        seed_founded_project(tmp.path(), CHARTER_SAMPLE);
        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();
        let sys = &req.messages[0].content;
        assert!(sys.contains("## Project charter"));
        assert!(sys.contains("current phase: 1"));
        assert!(sys.contains("**Invariants**"));
        assert!(sys.contains("Persistence contract X"));
        assert!(sys.contains("External assumption Y"));
        // Frame appears BEFORE the feature spec section.
        let charter_idx = sys.find("## Project charter").unwrap();
        let feature_idx = sys.find("## Feature specification").unwrap();
        assert!(charter_idx < feature_idx);
    }

    #[tokio::test]
    async fn charter_frame_absent_when_not_founded() {
        let tmp = tempfile::tempdir().unwrap();
        // Feature spec only — no CHARTER.md or project.toml.
        let feat_dir = tmp.path().join(".sovereign").join("features").join("fx");
        std::fs::create_dir_all(&feat_dir).unwrap();
        std::fs::write(feat_dir.join("spec.md"), "# fx\n").unwrap();

        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();
        let sys = &req.messages[0].content;
        assert!(
            !sys.contains("## Project charter"),
            "pre-founding repos must not get a charter frame"
        );
    }

    #[tokio::test]
    async fn charter_drift_flagged_inline_in_frame() {
        let tmp = tempfile::tempdir().unwrap();
        seed_founded_project(tmp.path(), CHARTER_SAMPLE);
        // Mutate CHARTER.md so its hash no longer matches project.toml.
        std::fs::write(
            tmp.path().join(".sovereign").join("CHARTER.md"),
            format!("{CHARTER_SAMPLE}\n\nMUTATED CONTENT\n"),
        )
        .unwrap();

        let inj = ContextInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        let ctx = ctx_with(Some("fx"), tmp.path().to_path_buf());
        inj.process(&mut req, &mut session, &ctx).await.unwrap();
        let sys = &req.messages[0].content;
        assert!(
            sys.contains("Charter drift"),
            "drift must be flagged in the frame so the agent doesn't operate on stale context"
        );
    }

    #[test]
    fn extract_named_section_pulls_invariants_body_only() {
        let body = extract_named_section(CHARTER_SAMPLE, "Invariants").unwrap();
        assert!(body.contains("Persistence contract X"));
        assert!(
            !body.contains("## Resolved decisions"),
            "extraction must stop at the next heading"
        );
    }
}
