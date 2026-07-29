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

use sovereign_contracts::frame::{Frame, FrameSchema};
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

/// The session frame's contract, expressed in the shared frame
/// primitive. Parse / upsert / render / budget mechanics live in
/// [`sovereign_contracts::frame`]; what stays here is what is specific
/// to a CODING session — the file layout under
/// `~/.sovereign/sessions/<id>/`, the git frontmatter, and the
/// status/notes/provenance stamping.
const SCHEMA: FrameSchema = FrameSchema {
    schema_id: "session-frame/v1",
    sections: &FRAME_SECTIONS,
    token_budget: FRAME_TOKEN_BUDGET,
};

/// Map a caller-supplied section name (canonical, lowercase, or
/// snake_case param id) to its canonical heading.
pub fn canonical_section(name: &str) -> Option<&'static str> {
    SCHEMA.canonical_section(name)
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

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
        ("schema".into(), SCHEMA.schema_id.into()),
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
    let mut frame = SCHEMA.empty();
    frame.front = front;
    frame
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
    // An upsert carrying no section body, no status and no note id still
    // CREATED the file and reported `created: true` — a success-shaped
    // response for a frame with eight empty sections, which then goes on
    // to advertise nothing to the successor that boots from it. Three
    // such frames were found banked on disk (2026-07-29), one of them
    // live in the boot index. A write that writes nothing is an error.
    if update.sections.is_empty() && update.status.is_none() && update.note_ids.is_empty() {
        return Err(format!(
            "session_state: nothing to write — supply at least one section body ({}), \
             a status, or note_ids. Refusing to bank an empty frame.",
            FRAME_SECTIONS.join(" | ")
        ));
    }

    let dir = sessions_root.join(session_id);
    let path = dir.join("frame.md");
    let existing = std::fs::read_to_string(&path).ok();
    let created = existing.is_none();
    let mut frame = match &existing {
        Some(text) => SCHEMA.parse(text),
        None => new_frame(session_id, repo_root, &update),
    };

    let mut sections_updated = Vec::new();
    for (name, body) in &update.sections {
        let canonical = canonical_section(name).expect("validated above");
        // Bind first: a `debug_assert!(frame.set_body(..))` would compile
        // the write itself out of a release build.
        let applied = frame.set_body(canonical, body.clone());
        debug_assert!(applied, "schema section `{canonical}` must exist in the frame");
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
    let total = frame
        .check_budget(&SCHEMA)
        .map_err(|e| format!("session_state: {e}"))?;

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

/// The non-section parameters. Kept beside [`SECTION_PARAMS`] so
/// [`is_known_param`] cannot drift away from the descriptor.
const META_PARAMS: [&str; 3] = ["session_id", "status", "note_ids"];

fn is_known_param(key: &str) -> bool {
    SECTION_PARAMS.contains(&key) || META_PARAMS.contains(&key)
}

/// The JSON type name, so a type-mismatch message can name what the
/// caller actually sent rather than only what was expected.
fn json_type(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

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
                          re-reading the repo. Sections are FLAT string params — one \
                          key per section (goal, state, next, decisions, invariants, \
                          dead_ends, working_set, verification); there is no `sections` \
                          array and no `section`/`content` pair."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": ["session_id"],
                // The declarative twin of the unknown-key guard in
                // `execute`: a validating client rejects the wrong shape
                // before it costs a round trip.
                "additionalProperties": false
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
        let obj = params.as_object().ok_or_else(|| {
            Error::InvalidInput("session_state: params must be a JSON object".into())
        })?;

        // An unrecognised key is a REJECTED call, never a dropped one.
        // Two plausible-but-wrong shapes showed up in real transcripts
        // (2026-07-29): `sections: [{name, content}]`, and a `section` +
        // `content` pair. Both sailed through as `created: true,
        // sections_updated: []` and overwrote the caller's frame with an
        // empty one — the caller had no way to tell the write was lost.
        let unknown: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|k| !is_known_param(k))
            .collect();
        if !unknown.is_empty() {
            return Err(Error::InvalidInput(format!(
                "session_state: unknown parameter(s) `{}`. Sections are FLAT string params \
                 — one key per section, e.g. `goal: \"- ship E4a\"`. There is no `sections` \
                 array and no `section`/`content` pair. Accepted: {} | {}",
                unknown.join("`, `"),
                SECTION_PARAMS.join(" | "),
                META_PARAMS.join(" | "),
            )));
        }

        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("session_state: `session_id` is required".into()))?;

        let mut update = FrameUpdate::default();
        for p in SECTION_PARAMS {
            match obj.get(p) {
                None | Some(serde_json::Value::Null) => {}
                Some(serde_json::Value::String(body)) => {
                    update.sections.push((p.to_string(), body.clone()));
                }
                Some(other) => {
                    return Err(Error::InvalidInput(format!(
                        "session_state: `{p}` must be a markdown string, got {}. Join list \
                         items into one string with newlines.",
                        json_type(other)
                    )));
                }
            }
        }
        update.status = match obj.get("status") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(other) => {
                return Err(Error::InvalidInput(format!(
                    "session_state: `status` must be a string, got {}",
                    json_type(other)
                )));
            }
        };
        update.note_ids = match obj.get("note_ids") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(a)) => {
                let mut ids = Vec::with_capacity(a.len());
                for v in a {
                    match v.as_str() {
                        Some(s) => ids.push(s.to_string()),
                        None => {
                            return Err(Error::InvalidInput(format!(
                                "session_state: `note_ids` must be an array of strings, got a \
                                 {} element",
                                json_type(v)
                            )));
                        }
                    }
                }
                ids
            }
            Some(other) => {
                return Err(Error::InvalidInput(format!(
                    "session_state: `note_ids` must be an array of strings, got {}",
                    json_type(other)
                )));
            }
        };

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
        let dir =
            std::env::temp_dir().join(format!("session_state_test_{}_{tag}", std::process::id()));
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
        let out =
            upsert_frame(&root, "s", None, update_with(&[("state", "- step 1 done")])).unwrap();
        assert!(!out.created);
        assert_eq!(out.sections_updated, vec!["State"]);
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(
            text.contains("original goal"),
            "goal must survive a state patch"
        );
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
        let err = upsert_frame(&root, "s3", None, update_with(&[("state", &big)])).unwrap_err();
        assert!(err.contains("budget"), "{err}");
        assert!(err.contains("State"), "per-section counts named: {err}");
        assert!(!root.join("s3").join("frame.md").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_section_and_bad_status_are_rejected() {
        let root = tmp_root("reject");
        let err = upsert_frame(&root, "s4", None, update_with(&[("vibes", "x")])).unwrap_err();
        assert!(err.contains("unknown section"));
        let mut update = update_with(&[]);
        update.status = Some("paused".into());
        let err = upsert_frame(&root, "s4", None, update).unwrap_err();
        assert!(err.contains("in-flight | completed | abandoned"));
        std::fs::remove_dir_all(&root).ok();
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "session-state-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    /// A write that would write nothing is an error, and banks no file.
    /// This is the guard that turns the silent failure loud even for a
    /// caller that reaches the function directly.
    #[test]
    fn empty_update_is_rejected_and_banks_no_frame() {
        let root = tmp_root("noop");
        let err = upsert_frame(&root, "s6", None, update_with(&[])).unwrap_err();
        assert!(err.contains("nothing to write"), "{err}");
        assert!(
            !root.join("s6").join("frame.md").exists(),
            "a no-op upsert must not create an empty frame"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The `sections: [{name, content}]` shape observed in real
    /// transcripts (2026-07-29). It used to return `created: true,
    /// sections_updated: []` and overwrite the frame with an empty one.
    #[tokio::test]
    async fn sections_array_shape_is_rejected_not_silently_dropped() {
        let root = tmp_root("shape_array");
        let tool = SessionStateTool::new().with_sessions_root(root.clone());
        let err = tool
            .execute(
                &json!({
                    "session_id": "s7",
                    "sections": [{"name": "Goal", "content": "ship the thing"}]
                }),
                &ctx(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown parameter"), "{err}");
        assert!(err.contains("sections"), "names the offending key: {err}");
        assert!(err.contains("goal"), "teaches the real shape: {err}");
        assert!(!root.join("s7").join("frame.md").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other wild shape: a singular `section` + `content` pair.
    #[tokio::test]
    async fn section_content_pair_shape_is_rejected() {
        let root = tmp_root("shape_pair");
        let tool = SessionStateTool::new().with_sessions_root(root.clone());
        let err = tool
            .execute(
                &json!({"session_id": "s8", "section": "Goal", "content": "ship it"}),
                &ctx(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown parameter"), "{err}");
        assert!(err.contains("section"), "{err}");
        assert!(err.contains("content"), "both keys named: {err}");
        assert!(!root.join("s8").join("frame.md").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// A known section param with a non-string value used to be dropped
    /// by `as_str()`. Now it names the type it got.
    #[tokio::test]
    async fn non_string_section_body_is_rejected() {
        let root = tmp_root("shape_type");
        let tool = SessionStateTool::new().with_sessions_root(root.clone());
        let err = tool
            .execute(
                &json!({"session_id": "s9", "next": ["a", "b"]}),
                &ctx(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("`next` must be a markdown string"), "{err}");
        assert!(err.contains("got array"), "names what it got: {err}");
        assert!(!root.join("s9").join("frame.md").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The correct flat shape still works end-to-end through the MCP
    /// path — the guards reject wrong shapes, not the real one.
    #[tokio::test]
    async fn flat_section_params_still_write_through_the_mcp_path() {
        let root = tmp_root("shape_ok");
        let tool = SessionStateTool::new().with_sessions_root(root.clone());
        let out = tool
            .execute(
                &json!({"session_id": "s10", "goal": "- ship E4a", "next": "- register"}),
                &ctx(),
            )
            .await
            .unwrap();
        let StepOutput::Json(v) = out else {
            panic!("expected json output")
        };
        assert_eq!(v["created"], json!(true));
        assert_eq!(v["sections_updated"], json!(["Goal", "Next"]));
        let text = std::fs::read_to_string(root.join("s10").join("frame.md")).unwrap();
        assert!(text.contains("- ship E4a"));
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
