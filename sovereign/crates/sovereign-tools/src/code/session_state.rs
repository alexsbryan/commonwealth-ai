// SPDX-License-Identifier: AGPL-3.0-or-later
//! `session_state` — encode-time session-frame upsert (write-path 1).
//!
//! MEMORY_MODEL.md §5 E4a / SESSION_CONTINUITY.md §3 path 1: the agent
//! holding the state writes the frame at transitions (task start, plan
//! step done, blocker hit), so the frame is continuously current and
//! session end needs no LLM reconstruction. Rationale (P2,
//! write-at-encoding-time): self-reported frames measure 100% recall
//! against the golden; post-hoc distillation measures 17%.
//!
//! The tool is a section-level UPSERT over
//! `~/.sovereign/sessions/<session_id>/frame.md`: provided sections
//! replace their previous bodies wholesale, everything else is
//! preserved, and every write re-stamps `provenance: self-reported`
//! (an encode-time write upgrades a distilled frame — the stronger
//! evidence wins). The schema-v1 contract is enforced at write time:
//! all eight sections always present, and the whole document must fit
//! the 2,000-token budget — an over-budget upsert is REJECTED with
//! per-section token counts so the caller trims instead of shipping a
//! bloated frame (the spec: "a frame that cannot fit must drop
//! detail, never sections").

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
    ToolExample,
};

/// The eight schema-v1 sections, in contract order (SESSION_CONTINUITY §2).
pub const FRAME_SECTIONS: [&str; 8] = [
    "Goal",
    "State",
    "Next",
    "Decisions",
    "Invariants",
    "Dead ends",
    "Working set",
    "Verification",
];

/// Hard cap on the rendered frame (SESSION_CONTINUITY §2).
pub const FRAME_TOKEN_BUDGET: usize = 2_000;

/// ~4 chars per token — same heuristic as `cache-audit`.
fn approx_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

/// Map a caller-supplied section name (canonical, lowercase, or
/// snake_case param id) to its canonical heading.
pub fn canonical_section(name: &str) -> Option<&'static str> {
    let norm = name.trim().to_lowercase().replace('_', " ");
    FRAME_SECTIONS
        .iter()
        .find(|s| s.to_lowercase() == norm)
        .copied()
}

/// One upsert's payload. Sections are `(canonical name, new body)`.
#[derive(Default)]
pub struct FrameUpdate {
    pub sections: Vec<(String, String)>,
    /// `in-flight` | `completed` | `abandoned`. Completed/abandoned also
    /// stamp `ended_at`.
    pub status: Option<String>,
    /// Appended (deduped) to the frontmatter `notes:` list.
    pub note_ids: Vec<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug)]
pub struct UpsertOutcome {
    pub path: PathBuf,
    pub created: bool,
    pub sections_updated: Vec<String>,
    pub approx_tokens: usize,
}

/// Ordered frontmatter + section bodies. Unknown frontmatter keys are
/// preserved verbatim so an upsert never strips fields a newer writer
/// added.
struct Frame {
    front: Vec<(String, String)>,
    bodies: Vec<(String, String)>, // canonical section -> body (may be empty)
}

impl Frame {
    fn get(&self, key: &str) -> Option<&str> {
        self.front
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn set(&mut self, key: &str, value: String) {
        match self.front.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => self.front.push((key.to_string(), value)),
        }
    }

    fn body_mut(&mut self, section: &str) -> &mut String {
        // All eight sections are materialized at construction; this
        // only ever finds an existing entry.
        &mut self
            .bodies
            .iter_mut()
            .find(|(name, _)| name == section)
            .expect("all frame sections materialized")
            .1
    }

    fn render(&self) -> String {
        let mut out = String::from("---\n");
        for (k, v) in &self.front {
            out.push_str(&format!("{k}: {v}\n"));
        }
        out.push_str("---\n");
        for (name, body) in &self.bodies {
            out.push_str(&format!("\n## {name}\n\n"));
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                out.push_str(trimmed);
                out.push('\n');
            }
        }
        out
    }
}

fn now_iso() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn git_capture(repo_root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Parse an existing frame.md. Lenient: sections not in the schema are
/// dropped (the upsert re-normalizes to the eight-section contract);
/// missing sections materialize empty.
fn parse_frame(text: &str) -> Frame {
    let mut front = Vec::new();
    let mut rest = text;
    if let Some(after) = text.strip_prefix("---") {
        if let Some(end) = after.find("\n---") {
            for line in after[..end].lines() {
                if let Some((k, v)) = line.split_once(':') {
                    front.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            rest = &after[end + 4..];
        }
    }
    let mut bodies: Vec<(String, String)> = FRAME_SECTIONS
        .iter()
        .map(|s| (s.to_string(), String::new()))
        .collect();
    let mut current: Option<usize> = None;
    for line in rest.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            current = canonical_section(heading)
                .and_then(|c| bodies.iter().position(|(n, _)| n == c));
            continue;
        }
        if let Some(idx) = current {
            bodies[idx].1.push_str(line);
            bodies[idx].1.push('\n');
        }
    }
    Frame { front, bodies }
}

fn new_frame(session_id: &str, repo_root: Option<&Path>, update: &FrameUpdate) -> Frame {
    let repo = repo_root
        .and_then(|r| r.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let branch = repo_root
        .and_then(|r| git_capture(r, &["rev-parse", "--abbrev-ref", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    let front = vec![
        ("schema".into(), "session-frame/v1".into()),
        ("session_id".into(), session_id.to_string()),
        (
            "harness".into(),
            update.harness.clone().unwrap_or_else(|| "unknown".into()),
        ),
        (
            "model".into(),
            update.model.clone().unwrap_or_else(|| "unknown".into()),
        ),
        ("repo".into(), repo),
        ("branch".into(), branch),
        ("head_at_end".into(), "unknown".into()),
        ("started_at".into(), now_iso()),
        ("ended_at".into(), "null".into()),
        ("status".into(), "in-flight".into()),
        ("provenance".into(), "self-reported".into()),
        ("notes".into(), "[]".into()),
    ];
    let bodies = FRAME_SECTIONS
        .iter()
        .map(|s| (s.to_string(), String::new()))
        .collect();
    Frame { front, bodies }
}

/// Section-level upsert of a schema-v1 frame. See module docs for the
/// contract; returns `Err` (and writes nothing) when the result would
/// bust the token budget or a section name is unknown.
pub fn upsert_frame(
    sessions_root: &Path,
    session_id: &str,
    repo_root: Option<&Path>,
    update: FrameUpdate,
) -> std::result::Result<UpsertOutcome, String> {
    if session_id.trim().is_empty() || session_id.contains(['/', '\\']) {
        return Err("session_state: session_id must be a plain id (no path separators)".into());
    }
    for (name, _) in &update.sections {
        if canonical_section(name).is_none() {
            return Err(format!(
                "session_state: unknown section `{name}` — sections are {}",
                FRAME_SECTIONS.join(" | ")
            ));
        }
    }

    let dir = sessions_root.join(session_id);
    let path = dir.join("frame.md");
    let existing = std::fs::read_to_string(&path).ok();
    let created = existing.is_none();
    let mut frame = match &existing {
        Some(text) => parse_frame(text),
        None => new_frame(session_id, repo_root, &update),
    };

    let mut sections_updated = Vec::new();
    for (name, body) in &update.sections {
        let canonical = canonical_section(name).expect("validated above");
        *frame.body_mut(canonical) = body.clone();
        sections_updated.push(canonical.to_string());
    }

    if let Some(status) = &update.status {
        if !matches!(status.as_str(), "in-flight" | "completed" | "abandoned") {
            return Err(format!(
                "session_state: status `{status}` — use in-flight | completed | abandoned"
            ));
        }
        frame.set("status", status.clone());
        if status != "in-flight" {
            frame.set("ended_at", now_iso());
        }
    }
    if !update.note_ids.is_empty() {
        let mut ids: Vec<String> = frame
            .get("notes")
            .unwrap_or("[]")
            .trim_matches(['[', ']'])
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for id in &update.note_ids {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        frame.set("notes", format!("[{}]", ids.join(", ")));
    }
    if let Some(h) = &update.harness {
        frame.set("harness", h.clone());
    }
    if let Some(m) = &update.model {
        frame.set("model", m.clone());
    }
    if let Some(root) = repo_root {
        if let Some(head) = git_capture(root, &["rev-parse", "--short", "HEAD"]) {
            frame.set("head_at_end", head);
        }
    }
    // The encode-time write is the strongest evidence path — always.
    frame.set("provenance", "self-reported".into());

    let rendered = frame.render();
    let total = approx_tokens(&rendered);
    if total > FRAME_TOKEN_BUDGET {
        let per_section: Vec<String> = frame
            .bodies
            .iter()
            .map(|(n, b)| format!("{n} {}t", approx_tokens(b)))
            .collect();
        return Err(format!(
            "session_state: frame would be ~{total} tokens (budget {FRAME_TOKEN_BUDGET}) — \
             trim before writing. Per section: {}. The spec: drop detail, never sections.",
            per_section.join(", ")
        ));
    }

    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("session_state: mkdir {}: {e}", dir.display()))?;
    let tmp = dir.join("frame.md.tmp");
    std::fs::write(&tmp, &rendered)
        .map_err(|e| format!("session_state: write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("session_state: rename to {}: {e}", path.display()))?;

    Ok(UpsertOutcome {
        path,
        created,
        sections_updated,
        approx_tokens: total,
    })
}

/// MCP wrapper. Section params are flat snake_case strings so an agent
/// can update one section in one small call at each transition.
pub struct SessionStateTool {
    sessions_root: PathBuf,
    workspace_root: Option<PathBuf>,
}

impl SessionStateTool {
    pub fn new() -> Self {
        let root = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".sovereign")
            .join("sessions");
        Self {
            sessions_root: root,
            workspace_root: None,
        }
    }

    /// Repo used for `head_at_end`/`branch` stamps.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Test hook: write frames under a different root.
    pub fn with_sessions_root(mut self, root: PathBuf) -> Self {
        self.sessions_root = root;
        self
    }
}

impl Default for SessionStateTool {
    fn default() -> Self {
        Self::new()
    }
}

const SECTION_PARAMS: [&str; 8] = [
    "goal",
    "state",
    "next",
    "decisions",
    "invariants",
    "dead_ends",
    "working_set",
    "verification",
];

#[async_trait]
impl Tool for SessionStateTool {
    fn descriptor(&self) -> ToolDescriptor {
        let mut properties = serde_json::Map::new();
        properties.insert(
            "session_id".into(),
            json!({
                "type": "string",
                "description": "The harness session/transcript id this frame belongs to."
            }),
        );
        for p in SECTION_PARAMS {
            properties.insert(
                p.into(),
                json!({
                    "type": "string",
                    "description": format!(
                        "Replacement markdown body for the `{}` section. Pointers over prose: cite file:line, symbols, note ids.",
                        canonical_section(p).unwrap_or(p)
                    )
                }),
            );
        }
        properties.insert(
            "status".into(),
            json!({
                "type": "string",
                "enum": ["in-flight", "completed", "abandoned"],
                "description": "Frame status; completed/abandoned also stamp ended_at."
            }),
        );
        properties.insert(
            "note_ids".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Note ids written this session — appended (deduped) to the frontmatter notes list."
            }),
        );
        ToolDescriptor {
            id: "session_state".to_string(),
            name: "Session State Upsert".to_string(),
            description: "Upsert your session frame (the successor-facing gist: goal, \
                          state, next, decisions, invariants, dead ends, working set, \
                          verification) at transitions — task start, plan step done, \
                          blocker hit — NOT only at session end. Provided sections \
                          replace their previous body; others are preserved. Writes \
                          are rejected over the 2k-token budget with per-section \
                          counts so you trim instead of bloating. Encode-time writes \
                          are the strong continuity path (self-reported frames grade \
                          100% vs 17% for post-hoc distillation); a current frame is \
                          what lets a successor session resume your work without \
                          re-reading the repo."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": ["session_id"]
            }),
            examples: vec![ToolExample {
                situation: "A plan step just completed — bank the position before moving on."
                    .into(),
                call: json!({
                    "session_id": "3fabc9ed-…",
                    "state": "- H5 lever shipped + fleet-measured (f92bb3e7)\n- suite 7893/0",
                    "next": "- E4a: register session_state tool daemon-side"
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "created": { "type": "boolean" },
                    "sections_updated": { "type": "array", "items": { "type": "string" } },
                    "approx_tokens": { "type": "integer" },
                    "budget_tokens": { "type": "integer" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("session_state: `session_id` is required".into()))?;

        let mut update = FrameUpdate::default();
        for p in SECTION_PARAMS {
            if let Some(body) = params.get(p).and_then(|v| v.as_str()) {
                update.sections.push((p.to_string(), body.to_string()));
            }
        }
        update.status = params
            .get("status")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        update.note_ids = params
            .get("note_ids")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let outcome = upsert_frame(
            &self.sessions_root,
            session_id,
            self.workspace_root.as_deref(),
            update,
        )
        .map_err(Error::InvalidInput)?;

        Ok(StepOutput::Json(json!({
            "path": outcome.path.display().to_string(),
            "created": outcome.created,
            "sections_updated": outcome.sections_updated,
            "approx_tokens": outcome.approx_tokens,
            "budget_tokens": FRAME_TOKEN_BUDGET,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "session_state_test_{}_{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn update_with(sections: &[(&str, &str)]) -> FrameUpdate {
        FrameUpdate {
            sections: sections
                .iter()
                .map(|(n, b)| (n.to_string(), b.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn creates_a_schema_v1_frame_with_all_sections() {
        let root = tmp_root("create");
        let out = upsert_frame(
            &root,
            "sess-1",
            None,
            update_with(&[("goal", "- ship E4a"), ("next", "- register the tool")]),
        )
        .unwrap();
        assert!(out.created);
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(text.starts_with("---\nschema: session-frame/v1\n"));
        for s in FRAME_SECTIONS {
            assert!(text.contains(&format!("## {s}")), "missing section {s}");
        }
        assert!(text.contains("- ship E4a"));
        assert!(text.contains("provenance: self-reported"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn patch_replaces_named_section_and_preserves_others() {
        let root = tmp_root("patch");
        upsert_frame(&root, "s", None, update_with(&[("goal", "original goal")])).unwrap();
        let out = upsert_frame(
            &root,
            "s",
            None,
            update_with(&[("state", "- step 1 done")]),
        )
        .unwrap();
        assert!(!out.created);
        assert_eq!(out.sections_updated, vec!["State"]);
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(text.contains("original goal"), "goal must survive a state patch");
        assert!(text.contains("- step 1 done"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn upsert_upgrades_distilled_provenance_and_merges_notes() {
        let root = tmp_root("prov");
        let dir = root.join("s2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("frame.md"),
            "---\nschema: session-frame/v1\nsession_id: s2\nprovenance: distilled\n\
             notes: [aaa]\n---\n\n## Goal\n\nold goal\n",
        )
        .unwrap();
        let mut update = update_with(&[]);
        update.note_ids = vec!["aaa".into(), "bbb".into()];
        let out = upsert_frame(&root, "s2", None, update).unwrap();
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(text.contains("provenance: self-reported"));
        assert!(text.contains("notes: [aaa, bbb]"), "dedup + append: {text}");
        assert!(text.contains("old goal"), "unpatched section preserved");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn over_budget_upsert_is_rejected_and_writes_nothing() {
        let root = tmp_root("budget");
        let big = "word ".repeat(3000); // ~3.7k tokens
        let err = upsert_frame(&root, "s3", None, update_with(&[("state", &big)]))
            .unwrap_err();
        assert!(err.contains("budget"), "{err}");
        assert!(err.contains("State"), "per-section counts named: {err}");
        assert!(!root.join("s3").join("frame.md").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_section_and_bad_status_are_rejected() {
        let root = tmp_root("reject");
        let err =
            upsert_frame(&root, "s4", None, update_with(&[("vibes", "x")])).unwrap_err();
        assert!(err.contains("unknown section"));
        let mut update = update_with(&[]);
        update.status = Some("paused".into());
        let err = upsert_frame(&root, "s4", None, update).unwrap_err();
        assert!(err.contains("in-flight | completed | abandoned"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn completed_status_stamps_ended_at() {
        let root = tmp_root("ended");
        let mut update = update_with(&[("goal", "g")]);
        update.status = Some("completed".into());
        let out = upsert_frame(&root, "s5", None, update).unwrap();
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(text.contains("status: completed"));
        assert!(!text.contains("ended_at: null"));
        std::fs::remove_dir_all(&root).ok();
    }
}
