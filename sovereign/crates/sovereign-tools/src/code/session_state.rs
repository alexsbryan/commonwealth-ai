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
//! all nine sections always present, and the whole document must fit
//! the 2,150-token budget — an over-budget upsert is REJECTED with
//! per-section token counts so the caller trims instead of shipping a
//! bloated frame (the spec: "a frame that cannot fit must drop
//! detail, never sections").
//!
//! `## Objective` (2026-07-29) is the one section a successor must
//! INHERIT rather than re-author. It exists because the frames on this
//! host measurably lost their own point across a lineage: 21 of 63
//! non-empty frames stated their goal as a delta from a previous frame,
//! and in one three-frame chain the objective's own name disappeared
//! entirely. The write-time guard in [`upsert_frame`] is what turns the
//! spec's long-standing "a successor must know *why*" from an
//! aspiration into a checked precondition.

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

/// The nine schema-v1 sections, in contract order (SESSION_CONTINUITY §2).
///
/// `Objective` is FIRST deliberately: it is the altitude-setter, and
/// [`Frame::render_for_prompt`] preserves this order, so a successor
/// reads the forest before the trees.
pub const FRAME_SECTIONS: [&str; 9] = [
    "Objective",
    "Goal",
    "State",
    "Next",
    "Decisions",
    "Invariants",
    "Dead ends",
    "Working set",
    "Verification",
];

/// The sections that describe *work*. A frame carrying any of these is
/// making claims about an initiative, so [`upsert_frame`] requires it to
/// also say what the initiative IS — see the `Objective` guard there.
const WORK_SECTIONS: [&str; 4] = ["Goal", "State", "Next", "Decisions"];

/// Hard cap on the rendered frame (SESSION_CONTINUITY §2).
///
/// Raised 2,000 → 2,100 when `Objective` was split out of `Goal`: the
/// objective got ~150 and `Goal` dropped 100 → ~50, since half its
/// contract moved out. A hundred tokens per boot is a trivial price
/// against a session of specious tweaking — the failure this buys off.
///
/// Raised 2,100 → 2,150 on 2026-08-02 when `Objective` gained
/// `Anchored in:` (SESSION_CONTINUITY §2.1 part 4) — the
/// `ARCH_PRINCIPLES.md` sections the initiative's shape answers to.
/// Same trade at the same price: fifty tokens per boot against a
/// lineage that keeps the work and loses the rules it was built under.
pub const FRAME_TOKEN_BUDGET: usize = 2_150;

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
    /// `## Next` items this frame's ancestors were also carrying, worst
    /// first. Advisory — see [`carried_items`].
    pub carried: Vec<Carried>,
    /// How many consecutive frames (this one included) have stated the
    /// same `## Objective`. 1 = fresh or changed. A large number is not
    /// automatically bad — a long initiative is legitimate — but it is
    /// the number to look at when the work starts feeling like tweaking.
    pub objective_sessions: usize,
}

/// One `## Next` item that predates this frame.
#[derive(Debug, Clone)]
pub struct Carried {
    /// The item as written in THIS frame.
    pub item: String,
    /// Consecutive ancestor frames also carrying it. `2` means two
    /// frames before this one already listed it.
    pub depth: usize,
}

/// The predecessor sidecar. Written by `sovereign session frames
/// --claim-window` at the boot hand-off — the one moment both session
/// ids are known — and read here, so the writer needs no lineage
/// machinery of its own (that lives in `sovereign-cli`, which this crate
/// cannot see under the repo's feature contract).
const PREDECESSOR_FILE: &str = "predecessor";

/// Bound on the ancestor walk. Matches `session_lineage::MAX_HOPS`; a
/// cycle or a pathological chain must not turn a frame write into an
/// unbounded filesystem crawl.
const MAX_LINEAGE_HOPS: usize = 8;

/// Where frames live, honouring the same override the CLI reader uses
/// (`sovereign-cli`'s `session_cmd::sessions_root`).
///
/// The two MUST agree. Until 2026-07-29 this side hardcoded
/// `~/.sovereign/sessions` while the reader honoured `SESSIONS_DIR`, so
/// setting the override moved the reader and left the writer pointed at
/// the live store — a sandboxed end-to-end run silently wrote real
/// frames, which is the exact failure the override exists to prevent.
/// Found by trying to test the boot advisory without touching the live
/// store, and getting six junk frames in it instead.
fn default_sessions_root() -> PathBuf {
    let override_dir = ["SVRNMESH_", "SOVEREIGN_"]
        .iter()
        .find_map(|p| std::env::var(format!("{p}SESSIONS_DIR")).ok());
    sessions_root_from(override_dir.as_deref())
}

/// The pure half, so the precedence is testable without mutating process
/// environment inside a shared test binary.
fn sessions_root_from(override_dir: Option<&str>) -> PathBuf {
    match override_dir.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".sovereign")
            .join("sessions"),
    }
}

/// This session's predecessor, if the boot hand-off recorded one.
fn predecessor_of(sessions_root: &Path, session_id: &str) -> Option<String> {
    let raw =
        std::fs::read_to_string(sessions_root.join(session_id).join(PREDECESSOR_FILE)).ok()?;
    let id = raw.trim();
    // A sidecar naming this session, or naming a path, is corrupt input
    // rather than a lineage — refuse it instead of walking it.
    if id.is_empty() || id == session_id || id.contains(['/', '\\']) {
        return None;
    }
    Some(id.to_string())
}

/// Ancestor frames, nearest first, following the recorded chain.
///
/// Prefers each ancestor frame's own `predecessor:` FRONTMATTER over the
/// sidecar: the sidecar is per-machine and prunes with the window
/// pointers (14 days), while the frontmatter is durable. The sidecar is
/// the bootstrap for the newest hop, which has not been stamped yet.
/// `first` is the nearest ancestor's id — the caller supplies it because
/// only the caller holds the current frame (whose `predecessor:` stamp is
/// the durable answer, with the sidecar as bootstrap).
fn ancestors(sessions_root: &Path, session_id: &str, first: Option<&str>) -> Vec<Frame> {
    let mut out: Vec<Frame> = Vec::new();
    let mut seen = vec![session_id.to_string()];
    let mut next_id = first.map(str::to_string);
    while out.len() < MAX_LINEAGE_HOPS {
        let Some(prev) = next_id.take() else {
            break;
        };
        // Cycles are possible if a window pointer is hand-edited or a
        // session re-attaches to its own descendant.
        if seen.contains(&prev) {
            break;
        }
        let Ok(text) = std::fs::read_to_string(sessions_root.join(&prev).join("frame.md")) else {
            break;
        };
        let parsed = SCHEMA.parse(&text);
        next_id = parsed
            .get("predecessor")
            .map(str::to_string)
            .filter(|p| !p.is_empty() && !p.contains(['/', '\\']))
            .or_else(|| predecessor_of(sessions_root, &prev));
        out.push(parsed);
        seen.push(prev);
    }
    out
}

/// One section's body from each ancestor, nearest first — the shape the
/// shared combinators in `sovereign_contracts::frame` consume.
fn ancestor_bodies(ancestors: &[Frame], section: &str) -> Vec<String> {
    ancestors
        .iter()
        .map(|f| f.body(section).unwrap_or_default().to_string())
        .collect()
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
        debug_assert!(
            applied,
            "schema section `{canonical}` must exist in the frame"
        );
        sections_updated.push(canonical.to_string());
    }

    // A frame may not claim work without saying what the work is FOR.
    //
    // WHY THIS IS A HARD ERROR AND NOT A LINT. Audited 2026-07-29 over the
    // 67 frames banked on this host: 21 of 63 non-empty frames define
    // their goal as a DELTA from a previous frame ("Item One's remaining
    // half", "continue frame `d9935a7b`'s Next item 2"). Walk one lineage
    // — 311ec4b7 → 8815fdb9 → c96d55a6, all on 2026-07-29 — and the word
    // "wedge", which 311ec4b7 named as the whole point, is simply gone two
    // frames later. The objective was never absent from the CONTRACT
    // (SESSION_CONTINUITY §2 has always asked `Goal` for "the task AND the
    // standing objective it serves"); it was absent because it was the
    // second half of a ~100-token field whose first half always wins.
    //
    // The check is on the POST-WRITE body, not on `created`, so a legacy
    // eight-section frame is also asked once — the successors most at risk
    // of ratholing are precisely the ones resuming a long lineage.
    //
    // Timing is what makes this cheap to satisfy rather than a nag: an
    // agent's first frame write happens while the predecessor's frame is
    // still whole in its context (the boot hook injects it), so inheriting
    // the objective is a COPY, not a re-derivation.
    let touches_work = update
        .sections
        .iter()
        .any(|(n, _)| canonical_section(n).is_some_and(|c| WORK_SECTIONS.contains(&c)));
    let objective_blank = frame.body("Objective").is_none_or(|b| b.trim().is_empty());
    if touches_work && objective_blank {
        return Err(format!(
            "session_state: refusing to write {} without an `objective`.\n\
             \n\
             The objective is the standing outcome this session's work serves — \
             what a USER gets when the initiative lands, not this session's \
             increment. It must carry:\n\
             \x20 • the outcome, and where it is specified (doc path + section, or plan path)\n\
             \x20 • `Done when:` — a falsifiable test at INITIATIVE altitude\n\
             \x20 • `Not worth continuing if:` — the exit condition\n\
             \x20 • `Anchored in:` — the ARCH_PRINCIPLES.md sections this initiative's\n\
             \x20   shape answers to, and where it knowingly deviates. Section numbers,\n\
             \x20   not prose. Open the section before you cite it.\n\
             \n\
             If you are continuing a predecessor, COPY its `## Objective` verbatim \
             (`sovereign session frames <predecessor-id>`) and edit it only if the \
             objective genuinely changed. Restating it as a delta from the last \
             frame (\"item two's remaining half\") is the failure this guard exists \
             to catch.",
            sections_updated
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(" + ")
        ));
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

    // Record the chain in the frame itself. The sidecar is per-machine
    // and prunes with the window pointers; the frontmatter is durable, so
    // once stamped a lineage stays walkable offline and forever.
    // The frame's own stamp wins over the sidecar: it is durable, while
    // the sidecar prunes with the window pointers after 14 days. The
    // sidecar is how the stamp gets there in the first place.
    let nearest = frame
        .get("predecessor")
        .map(str::to_string)
        .filter(|p| !p.is_empty() && p != session_id && !p.contains(['/', '\\']))
        .or_else(|| predecessor_of(sessions_root, session_id));
    if let Some(prev) = &nearest {
        frame.set("predecessor", prev.clone());
    }
    let ancestry = ancestors(sessions_root, session_id, nearest.as_deref());

    let carried = sovereign_contracts::frame::carried_across(
        frame.body("Next").unwrap_or_default(),
        &ancestor_bodies(&ancestry, "Next"),
    )
    .into_iter()
    .map(|(item, depth)| Carried { item, depth })
    .collect::<Vec<_>>();
    let objective_sessions = sovereign_contracts::frame::same_across(
        frame.body("Objective").unwrap_or_default(),
        &ancestor_bodies(&ancestry, "Objective"),
    );

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
        carried,
        objective_sessions,
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
        Self {
            sessions_root: default_sessions_root(),
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

const SECTION_PARAMS: [&str; 9] = [
    "objective",
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
            // `objective` earns a bespoke description: it is the one
            // section a successor must NOT re-derive, and the generic
            // "replacement body" wording invited exactly the delta-goal
            // restatement the audit found in 21 of 63 frames.
            let description = if p == "objective" {
                "The STANDING outcome this session's work serves — what a user gets when the \
                 initiative lands, NOT this session's increment. Inherited, not re-authored: \
                 when continuing a predecessor, copy its `## Objective` verbatim and edit only \
                 if the objective genuinely changed. Must carry the outcome + where it is \
                 specified (doc path/section or plan path), a `Done when:` line that is \
                 falsifiable at INITIATIVE altitude, a `Not worth continuing if:` exit \
                 condition, and an `Anchored in:` line naming the ARCH_PRINCIPLES.md \
                 sections this initiative's shape answers to (section numbers, not prose \
                 — open the section before citing it). Never phrase it as a delta from \
                 the last frame."
                    .to_string()
            } else {
                format!(
                    "Replacement markdown body for the `{}` section. Pointers over prose: cite file:line, symbols, note ids.",
                    canonical_section(p).unwrap_or(p)
                )
            };
            properties.insert(
                p.into(),
                json!({ "type": "string", "description": description }),
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
            description: "Upsert your session frame (the successor-facing gist: objective, \
                          goal, state, next, decisions, invariants, dead ends, working set, \
                          verification) at transitions — task start, plan step done, \
                          blocker hit — NOT only at session end. Provided sections \
                          replace their previous body; others are preserved. Writes \
                          are rejected over the 2.1k-token budget with per-section \
                          counts so you trim instead of bloating. Encode-time writes \
                          are the strong continuity path (self-reported frames grade \
                          100% vs 17% for post-hoc distillation); a current frame is \
                          what lets a successor session resume your work without \
                          re-reading the repo. `objective` is the standing outcome the \
                          work serves and is REQUIRED alongside any of goal/state/next/\
                          decisions — inherit it from your predecessor rather than \
                          restating it as a delta. Sections are FLAT string params — one \
                          key per section (objective, goal, state, next, decisions, \
                          invariants, dead_ends, working_set, verification); there is no \
                          `sections` array and no `section`/`content` pair."
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
                    "budget_tokens": { "type": "integer" },
                    "objective_sessions": { "type": "integer" },
                    "carried": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "item": { "type": "string" },
                                "depth": { "type": "integer" }
                            }
                        }
                    },
                    "advice": { "type": "string" }
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

        // The advisory rides the WRITE response on purpose. It is the one
        // moment the author is holding the backlog and can still act —
        // a report delivered at boot arrives before there is anything to
        // compare, and one delivered at session end arrives too late.
        let carried = json!(outcome
            .carried
            .iter()
            .map(|c| json!({
                "depth": c.depth,
                // Enough to recognise the item, not enough to re-paste it.
                "item": c.item.chars().take(120).collect::<String>(),
            }))
            .collect::<Vec<_>>());
        let mut doc = json!({
            "path": outcome.path.display().to_string(),
            "created": outcome.created,
            "sections_updated": outcome.sections_updated,
            "approx_tokens": outcome.approx_tokens,
            "budget_tokens": FRAME_TOKEN_BUDGET,
            "objective_sessions": outcome.objective_sessions,
            "carried": carried,
        });
        if let Some(worst) = outcome.carried.first() {
            doc["advice"] = json!(format!(
                "{} `Next` item(s) predate this frame; the oldest has ridden {} consecutive \
                 frames unacted-on. Carrying an item is a decision — do it, or drop it, or \
                 say in `Objective` why it stays. Recopying a backlog is how a lineage \
                 turns into tweaking.",
                outcome.carried.len(),
                worst.depth + 1
            ));
        }
        Ok(StepOutput::Json(doc))
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

    /// A standing objective, for tests whose subject is something other
    /// than the objective guard. Real ones carry `Done when:` /
    /// `Not worth continuing if:` — see SESSION_CONTINUITY §2.1.
    const OBJ: &str = "- E4 continuity: a successor resumes without re-reading the repo.";

    #[test]
    fn creates_a_schema_v1_frame_with_all_sections() {
        let root = tmp_root("create");
        let out = upsert_frame(
            &root,
            "sess-1",
            None,
            update_with(&[
                ("objective", OBJ),
                ("goal", "- ship E4a"),
                ("next", "- register the tool"),
            ]),
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
        upsert_frame(
            &root,
            "s",
            None,
            update_with(&[("objective", OBJ), ("goal", "original goal")]),
        )
        .unwrap();
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
        let err = upsert_frame(
            &root,
            "s3",
            None,
            update_with(&[("objective", OBJ), ("state", &big)]),
        )
        .unwrap_err();
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
            .execute(&json!({"session_id": "s9", "next": ["a", "b"]}), &ctx())
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
                &json!({
                    "session_id": "s10",
                    "objective": "- E4 continuity: successors resume without re-reading.",
                    "goal": "- ship E4a",
                    "next": "- register"
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let StepOutput::Json(v) = out else {
            panic!("expected json output")
        };
        assert_eq!(v["created"], json!(true));
        assert_eq!(v["sections_updated"], json!(["Objective", "Goal", "Next"]));
        let text = std::fs::read_to_string(root.join("s10").join("frame.md")).unwrap();
        assert!(text.contains("- ship E4a"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// The guard's whole reason for existing: a frame may claim work
    /// only if it also says what the work is FOR.
    #[test]
    fn work_without_a_standing_objective_is_rejected() {
        let root = tmp_root("obj_guard");
        let err = upsert_frame(
            &root,
            "o1",
            None,
            update_with(&[("goal", "- finish item one's remaining half")]),
        )
        .unwrap_err();
        assert!(err.contains("objective"), "names the missing field: {err}");
        assert!(
            err.contains("Done when:") && err.contains("Not worth continuing if:"),
            "teaches the required shape, not just the field name: {err}"
        );
        assert!(
            err.contains("sovereign session frames"),
            "tells the caller how to INHERIT rather than re-derive: {err}"
        );
        assert!(
            !root.join("o1").join("frame.md").exists(),
            "a rejected write must bank nothing"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A blank-but-present objective is the same failure as an absent
    /// one — the guard reads the body, not the key.
    #[test]
    fn a_whitespace_objective_does_not_satisfy_the_guard() {
        let root = tmp_root("obj_blank");
        let err = upsert_frame(
            &root,
            "o2",
            None,
            update_with(&[("objective", "   \n  "), ("state", "- done")]),
        )
        .unwrap_err();
        assert!(err.contains("objective"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The check is on the POST-WRITE body, not on `created` — so the 67
    /// legacy eight-section frames banked before 2026-07-29 are asked
    /// once, on their next work write. Those lineages are exactly the
    /// ones most at risk of ratholing.
    #[test]
    fn a_legacy_frame_without_an_objective_is_asked_once() {
        let root = tmp_root("obj_legacy");
        let dir = root.join("o3");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("frame.md"),
            "---\nschema: session-frame/v1\nsession_id: o3\n---\n\n## Goal\n\nold goal\n",
        )
        .unwrap();

        let err = upsert_frame(&root, "o3", None, update_with(&[("state", "- more")])).unwrap_err();
        assert!(err.contains("objective"), "legacy frame is asked: {err}");
        assert!(
            std::fs::read_to_string(dir.join("frame.md"))
                .unwrap()
                .contains("old goal"),
            "the rejected write must not have clobbered the legacy frame"
        );

        // ...and once answered, it is answered for good: the objective
        // persists through later section patches without being restated.
        upsert_frame(
            &root,
            "o3",
            None,
            update_with(&[("objective", OBJ), ("state", "- more")]),
        )
        .unwrap();
        let out = upsert_frame(&root, "o3", None, update_with(&[("next", "- step 2")])).unwrap();
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(
            text.contains(OBJ),
            "objective must survive a later patch — inheritance is the point"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The guard must not become a tax on non-work writes. Banking a
    /// verification result or flipping status is not a claim about an
    /// initiative, so it proceeds with a blank objective.
    #[test]
    fn non_work_writes_are_not_gated_on_the_objective() {
        let root = tmp_root("obj_nonwork");
        upsert_frame(
            &root,
            "o4",
            None,
            update_with(&[("verification", "- suite 8618/0")]),
        )
        .expect("a verification-only write is not a work claim");
        let mut update = update_with(&[]);
        update.status = Some("abandoned".into());
        upsert_frame(&root, "o4", None, update).expect("status-only write is not a work claim");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Guard order matters: an unknown section name must be reported as
    /// such, not masked by the objective guard.
    #[test]
    fn shape_errors_are_reported_before_the_objective_guard() {
        let root = tmp_root("obj_order");
        let err = upsert_frame(&root, "o5", None, update_with(&[("vibes", "x")])).unwrap_err();
        assert!(
            err.contains("unknown section"),
            "the caller's actual mistake wins: {err}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The writer's section list is half of a contract whose other half
    /// lives in `sovereign-cli`'s `session_cmd::FRAME_SECTIONS` (the
    /// `## `-prefixed twin). The crates cannot see each other under the
    /// repo's feature contract — `sovereign-tools` reaches `sovereign-cli`
    /// only via the `awareness` feature, which the lint/test gate does
    /// not enable — so this pins the contract on each side instead.
    #[test]
    fn the_section_contract_is_pinned() {
        assert_eq!(
            FRAME_SECTIONS,
            [
                "Objective",
                "Goal",
                "State",
                "Next",
                "Decisions",
                "Invariants",
                "Dead ends",
                "Working set",
                "Verification",
            ],
            "SESSION_CONTINUITY §2 order — update session_cmd::FRAME_SECTIONS too"
        );
        assert_eq!(
            FRAME_SECTIONS[0], "Objective",
            "the altitude-setter renders first, so a successor reads why before what"
        );
        assert_eq!(SECTION_PARAMS.len(), FRAME_SECTIONS.len());
    }

    /// Link `child`'s frame to `parent`'s, the way the boot hand-off
    /// (`session frames --claim-window`) does.
    fn link(root: &Path, child: &str, parent: &str) {
        let dir = root.join(child);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PREDECESSOR_FILE), parent).unwrap();
    }

    /// The end-to-end case this whole mechanism exists for, rebuilt from
    /// the frames that motivated it: `311ec4b7` → `8815fdb9` →
    /// `c96d55a6`, all banked 2026-07-29, all carrying the same two
    /// backlog items nobody did and nobody dropped.
    #[test]
    fn a_recopied_backlog_is_reported_with_its_depth() {
        let root = tmp_root("carried");
        let pin = "- Retire `SOVEREIGN_RPC_BLOCK_SPLIT=12,36` — needs BeefyMac.";
        let overflow = "- WorkerOverflow capacity basis (note cc8d033f) — park on `total`, \
                        retry on `free`.";

        upsert_frame(
            &root,
            "gen1",
            None,
            update_with(&[("objective", OBJ), ("next", &format!("{pin}\n{overflow}"))]),
        )
        .unwrap();

        link(&root, "gen2", "gen1");
        upsert_frame(
            &root,
            "gen2",
            None,
            update_with(&[("objective", OBJ), ("next", &format!("{pin}\n{overflow}"))]),
        )
        .unwrap();

        // Third hop: the pin item is ELABORATED rather than recopied —
        // the case a strict string compare would miss — and a genuinely
        // new item joins it.
        link(&root, "gen3", "gen2");
        let out = upsert_frame(
            &root,
            "gen3",
            None,
            update_with(&[
                ("objective", OBJ),
                (
                    "next",
                    "- Retire `SOVEREIGN_RPC_BLOCK_SPLIT=12,36` — `mesh plan` on the 35B reports \
                 the pin does NOT apply (needs 41 blocks) and the loader rejects it too.\n\
                 - WorkerOverflow capacity basis (note cc8d033f) — park on `total`, retry on \
                 `free`.\n\
                 - Brand new item nobody has ever carried before, about gossip drains.",
                ),
            ]),
        )
        .unwrap();

        assert_eq!(
            out.carried.len(),
            2,
            "exactly the two recopied items: {:?}",
            out.carried
        );
        assert!(
            out.carried.iter().all(|c| c.depth == 2),
            "both rode two ancestors: {:?}",
            out.carried
        );
        assert!(
            !out.carried.iter().any(|c| c.item.contains("Brand new")),
            "a fresh item is not a carried one"
        );
        assert_eq!(
            out.objective_sessions, 3,
            "the objective rode all three frames unchanged"
        );

        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(
            text.contains("predecessor: gen2"),
            "the chain is stamped into the frame, so it outlives the sidecar"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// CONSECUTIVE is load-bearing. An item that was dropped and came
    /// back is a re-prioritisation, not a rut, and flagging it would
    /// punish exactly the behaviour this feature wants to encourage.
    #[test]
    fn an_item_that_was_dropped_and_returned_is_not_a_rut() {
        let root = tmp_root("carried_gap");
        let item = "- WorkerOverflow capacity basis (note cc8d033f) — park on `total`, retry \
                    on `free`.";
        upsert_frame(
            &root,
            "g1",
            None,
            update_with(&[("objective", OBJ), ("next", item)]),
        )
        .unwrap();
        link(&root, "g2", "g1");
        upsert_frame(
            &root,
            "g2",
            None,
            update_with(&[
                ("objective", OBJ),
                ("next", "- something else entirely, about TLS"),
            ]),
        )
        .unwrap();
        link(&root, "g3", "g2");
        let out = upsert_frame(
            &root,
            "g3",
            None,
            update_with(&[("objective", OBJ), ("next", item)]),
        )
        .unwrap();
        assert!(
            out.carried.is_empty(),
            "the chain was broken at g2, so g3 is not recopying: {:?}",
            out.carried
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A changed objective resets the streak — that is the whole signal.
    #[test]
    fn changing_the_objective_resets_its_streak() {
        let root = tmp_root("obj_streak");
        upsert_frame(
            &root,
            "o_a",
            None,
            update_with(&[("objective", OBJ), ("goal", "g")]),
        )
        .unwrap();
        link(&root, "o_b", "o_a");
        let out = upsert_frame(
            &root,
            "o_b",
            None,
            update_with(&[
                (
                    "objective",
                    "- Something completely different: ship the mesh installer.",
                ),
                ("goal", "g"),
            ]),
        )
        .unwrap();
        assert_eq!(out.objective_sessions, 1, "a new objective starts at one");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A hand-edited pointer must not turn a frame write into an
    /// unbounded crawl — or a hang.
    #[test]
    fn a_lineage_cycle_terminates() {
        let root = tmp_root("cycle");
        link(&root, "c_a", "c_b");
        link(&root, "c_b", "c_a");
        upsert_frame(
            &root,
            "c_a",
            None,
            update_with(&[
                ("objective", OBJ),
                ("next", "- do a thing that is long enough"),
            ]),
        )
        .unwrap();
        let out = upsert_frame(
            &root,
            "c_b",
            None,
            update_with(&[
                ("objective", OBJ),
                ("next", "- do a thing that is long enough"),
            ]),
        )
        .unwrap();
        assert_eq!(out.carried.len(), 1, "one hop, then the cycle is cut");
        assert_eq!(out.carried[0].depth, 1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// No lineage recorded is the common case (a first session, a plain
    /// shell, no `ps`). It must be silent, not degraded.
    #[test]
    fn a_frame_without_a_predecessor_reports_no_carry() {
        let root = tmp_root("no_pred");
        let out = upsert_frame(
            &root,
            "solo",
            None,
            update_with(&[
                ("objective", OBJ),
                ("next", "- a perfectly ordinary next item"),
            ]),
        )
        .unwrap();
        assert!(out.carried.is_empty());
        assert_eq!(out.objective_sessions, 1);
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(
            !text.contains("predecessor:"),
            "no lineage means no stamp, not an empty one"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The writer must honour the SAME root override the CLI reader does.
    /// When it did not, a sandboxed end-to-end run moved the reader and
    /// left the writer pointed at the live store — six junk frames landed
    /// in it before this was caught (2026-07-29).
    #[test]
    fn the_sessions_root_override_wins_over_the_home_default() {
        assert_eq!(
            sessions_root_from(Some("/tmp/sandbox")),
            PathBuf::from("/tmp/sandbox")
        );
        assert!(
            sessions_root_from(None).ends_with(".sovereign/sessions"),
            "no override falls back to the live store"
        );
        assert!(
            sessions_root_from(Some("   ")).ends_with(".sovereign/sessions"),
            "a blank override is not an override"
        );
    }

    /// A sidecar pointing at itself is corrupt input, not a lineage.
    #[test]
    fn a_self_referential_sidecar_is_refused() {
        let root = tmp_root("self_ref");
        link(&root, "s_x", "s_x");
        assert_eq!(predecessor_of(&root, "s_x"), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn completed_status_stamps_ended_at() {
        let root = tmp_root("ended");
        let mut update = update_with(&[("objective", OBJ), ("goal", "g")]);
        update.status = Some("completed".into());
        let out = upsert_frame(&root, "s5", None, update).unwrap();
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(text.contains("status: completed"));
        assert!(!text.contains("ended_at: null"));
        std::fs::remove_dir_all(&root).ok();
    }
}
