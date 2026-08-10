// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn session` — session continuity: distill a harness transcript into a
//! session frame (`docs/specs/SESSION_CONTINUITY.md`).
//!
//! WHY THIS EXISTS. A 500k-token session's *essential* context is 1–2k
//! tokens: goal, position, next actions, decisions, invariants, dead ends,
//! working set, verification state. When the window dies, that state dies
//! with it — unless it is externalized. `session distill` rescues it after
//! the fact: parse the transcript (same local `~/.claude/projects/*.jsonl`
//! source `cache-audit` reads), extract the **narrative spine** (real user
//! turns + assistant texts + the edit working-set — ~1% of the transcript;
//! tool results and hook payloads are the noise), then synthesize a
//! schema-v1 frame via retrieval practice: one local-daemon chat call per
//! body section, each answering a focused question with mandatory spine
//! citations (uncited bullets are machine-dropped).
//!
//! Glassbox discipline:
//!   - Stage 1 (spine) is deterministic and kept on disk next to the frame,
//!     so a bad frame is diagnosable against its exact input.
//!   - Frontmatter is assembled deterministically from the transcript — the
//!     LLM writes only the eight body sections and is validated against the
//!     schema's section list. A frame that fails validation is still
//!     written, loudly flagged, and the command exits non-zero.
//!   - Daemon down + `--no-llm` degrade to the same honest place: the spine
//!     is written and the command says exactly what didn't happen.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache_audit_cmd::{resolve_transcript_dir, short_session_id};
use crate::session_lineage;
use crate::util::urls::{v1_url, DEFAULT_CLIENT_PORT};

/// The nine body sections of a session-frame/v1, in contract order
/// (`SESSION_CONTINUITY.md §2`). Validation and grading key off this list.
///
/// Must stay in lockstep with `sovereign_tools::code::session_state::
/// FRAME_SECTIONS` (the writer's list, unprefixed). The
/// `the_reader_and_writer_agree_on_the_contract` test is the lock.
pub(crate) const FRAME_SECTIONS: &[&str] = &[
    "## Objective",
    "## Goal",
    "## State",
    "## Next",
    "## Decisions",
    "## Invariants",
    "## Dead ends",
    "## Working set",
    "## Verification",
];

/// Per-item caps keep the spine inside a local model's context. User turns
/// are the highest-signal tokens in a transcript (every goal statement and
/// steer) so they get the larger budget.
const USER_TURN_CAP: usize = 4_000;
const ASSISTANT_TEXT_CAP: usize = 900;
/// Whole-spine cap; beyond it, *middle* assistant texts are dropped first —
/// the opening (goal formation) and the tail (final state) carry the most
/// continuity signal.
const SPINE_CHAR_CAP: usize = 90_000;
/// Initial cap for the copy handed to the synthesis model. Local slots run
/// small context windows (measured: primary = 15,996 tokens ≈ 64k chars,
/// minus the output budget and system prompt). When the daemon still says
/// "Prompt too long", `run_distill` halves this and retries — the on-disk
/// spine keeps the full `SPINE_CHAR_CAP` detail either way.
const PROMPT_CHAR_CAP_INITIAL: usize = 48_000;
const PROMPT_CHAR_CAP_MIN: usize = 12_000;

// ── Stage 1: deterministic spine extraction ──────────────────────────────

#[derive(Default, Clone)]
pub(crate) struct Spine {
    session_id: String,
    model: String,
    cwd: String,
    git_branch: String,
    first_ts: String,
    last_ts: String,
    user_turns: Vec<String>,
    assistant_texts: Vec<String>,
    /// file path → edit count (Edit/Write/NotebookEdit).
    edits: BTreeMap<String, u64>,
    /// tool name → call count.
    tool_calls: BTreeMap<String, u64>,
}

/// Drop `<system-reminder>…</system-reminder>` blocks (hook payloads,
/// harness context) from a user message, keeping the human text around them.
pub(crate) fn strip_reminders(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    loop {
        match rest.find("<system-reminder>") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(s) => {
                out.push_str(&rest[..s]);
                match rest[s..].find("</system-reminder>") {
                    None => break, // unterminated — drop the tail
                    Some(e) => rest = &rest[s + e + "</system-reminder>".len()..],
                }
            }
        }
    }
    out.trim().to_string()
}

/// A user turn worth keeping is human input — not a local-command echo, not
/// a background-task notification, not empty after reminder stripping.
pub(crate) fn keep_user_turn(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty()
        && !t.starts_with("<local-command")
        && !t.starts_with("<command-name>")
        && !t.starts_with("<task-notification>")
}

/// Truncate on a char boundary with a marker, so caps never split UTF-8.
fn cap_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let cut: String = s.chars().take(cap).collect();
    format!("{cut}\n[…truncated]")
}

/// Parse one transcript into its spine. Returns None when the file has no
/// assistant activity at all (nothing to distill).
pub(crate) fn extract_spine(path: &Path) -> Option<Spine> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut spine = Spine {
        session_id: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string(),
        ..Default::default()
    };

    for line in text.lines() {
        let obj: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(ts) = obj.get("timestamp").and_then(|t| t.as_str()) {
            if spine.first_ts.is_empty() {
                spine.first_ts = ts.to_string();
            }
            spine.last_ts = ts.to_string();
        }
        if spine.cwd.is_empty() {
            if let Some(c) = obj.get("cwd").and_then(|c| c.as_str()) {
                spine.cwd = c.to_string();
            }
        }
        if let Some(b) = obj.get("gitBranch").and_then(|b| b.as_str()) {
            if !b.is_empty() {
                spine.git_branch = b.to_string();
            }
        }
        if obj.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
            continue;
        }
        let msg = match obj.get("message").filter(|m| m.is_object()) {
            Some(m) => m,
            None => continue,
        };
        let rec_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if spine.model.is_empty() {
            if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
                spine.model = m.to_string();
            }
        }

        match (rec_type, msg.get("content")) {
            ("user", Some(serde_json::Value::String(s))) => {
                let t = strip_reminders(s);
                if keep_user_turn(&t) {
                    spine.user_turns.push(cap_chars(&t, USER_TURN_CAP));
                }
            }
            ("user", Some(serde_json::Value::Array(blocks))) => {
                // A message carrying any tool_result is a tool result, not a
                // human turn.
                let is_tool_result = blocks
                    .iter()
                    .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
                if !is_tool_result {
                    let joined = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let t = strip_reminders(&joined);
                    if keep_user_turn(&t) {
                        spine.user_turns.push(cap_chars(&t, USER_TURN_CAP));
                    }
                }
            }
            ("assistant", Some(serde_json::Value::Array(blocks))) => {
                for b in blocks {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    spine
                                        .assistant_texts
                                        .push(cap_chars(t.trim(), ASSISTANT_TEXT_CAP));
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            *spine.tool_calls.entry(name.to_string()).or_default() += 1;
                            if matches!(name, "Edit" | "Write" | "NotebookEdit") {
                                if let Some(fp) = b
                                    .get("input")
                                    .and_then(|i| i.get("file_path"))
                                    .and_then(|f| f.as_str())
                                {
                                    *spine.edits.entry(fp.to_string()).or_default() += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if spine.assistant_texts.is_empty() && spine.user_turns.is_empty() {
        return None;
    }
    Some(spine)
}

/// Render the spine as the text handed to the synthesis model (and kept on
/// disk beside the frame for diagnosis). Layout mirrors the validated
/// prototype: working-set first (orients the model), then the full
/// user-turn/assistant-text narrative.
pub(crate) fn render_spine(spine: &Spine) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "SESSION {} | model {} | {} → {} | cwd {} | branch {}\n",
        spine.session_id, spine.model, spine.first_ts, spine.last_ts, spine.cwd, spine.git_branch
    ));

    let total_calls: u64 = spine.tool_calls.values().sum();
    out.push_str(&format!("\nTOOL CALLS ({total_calls}):\n"));
    let mut calls: Vec<(&String, &u64)> = spine.tool_calls.iter().collect();
    calls.sort_by(|a, b| b.1.cmp(a.1));
    for (name, n) in calls {
        out.push_str(&format!("  {n:5}  {name}\n"));
    }

    out.push_str(&format!("\nFILES EDITED ({}):\n", spine.edits.len()));
    let mut edits: Vec<(&String, &u64)> = spine.edits.iter().collect();
    edits.sort_by(|a, b| b.1.cmp(a.1));
    for (fp, n) in edits {
        out.push_str(&format!("  {n:3}  {fp}\n"));
    }

    // Items carry stable citation ids ([uN]/[aN]) — the retrieval-practice
    // synthesis requires every answer bullet to cite the item it came from.
    out.push_str(&format!("\nUSER TURNS ({}):\n", spine.user_turns.len()));
    for (i, t) in spine.user_turns.iter().enumerate() {
        out.push_str(&format!("\n--- [u{}] user turn ---\n{t}\n", i + 1));
    }
    out.push_str(&format!(
        "\nASSISTANT TEXTS ({}):\n",
        spine.assistant_texts.len()
    ));
    for (i, t) in spine.assistant_texts.iter().enumerate() {
        out.push_str(&format!("\n--- [a{}] assistant ---\n{t}\n", i + 1));
    }
    out
}

/// Enforce the whole-spine cap by dropping *middle* assistant texts (the
/// opening and the tail carry the most continuity signal). User turns are
/// never dropped. Pure, so the drop policy is unit-testable.
pub(crate) fn cap_spine_middle(spine: &mut Spine, char_cap: usize) -> usize {
    let mut dropped = 0usize;
    while render_spine(spine).len() > char_cap && spine.assistant_texts.len() > 8 {
        // Remove from the middle outward; keep the first 4 and last 4.
        let mid = spine.assistant_texts.len() / 2;
        spine.assistant_texts.remove(mid);
        dropped += 1;
    }
    if dropped > 0 {
        let mid = spine.assistant_texts.len() / 2;
        spine.assistant_texts.insert(
            mid,
            format!("[… {dropped} mid-session assistant texts omitted for budget …]"),
        );
    }
    dropped
}

/// Sections whose golden content is *mined* from the whole session — often
/// from mid-session debugging — rather than summarized from its ends
/// (measured on e09c5e3d: with the ends-biased fitted spine, Invariants and
/// Dead ends graded 0/9 and 0/2 because their golden items sat in the
/// trimmed middle). These are asked once per chunk of the full spine and the
/// answers unioned; the remaining sections are asked once on the fitted
/// spine, whose kept head (goal formation) and tail (final state) are
/// exactly what they need.
const MINED_SECTIONS: &[&str] = &["## Next", "## Decisions", "## Invariants", "## Dead ends"];

/// Contiguous chunks of the spine's assistant texts, each fitting
/// `char_cap` once rendered alongside the header and all user turns.
/// Returns `(lo, hi)` index ranges over `spine.assistant_texts`.
pub(crate) fn chunk_ranges(spine: &Spine, char_cap: usize) -> Vec<(usize, usize)> {
    let overhead: usize = 200 + spine.user_turns.iter().map(|t| t.len() + 24).sum::<usize>();
    let budget = char_cap.saturating_sub(overhead).max(8_000);
    let mut out = Vec::new();
    let mut lo = 0usize;
    let mut acc = 0usize;
    for (i, t) in spine.assistant_texts.iter().enumerate() {
        let cost = t.len() + 24;
        if acc + cost > budget && i > lo {
            out.push((lo, i));
            lo = i;
            acc = 0;
        }
        acc += cost;
    }
    if lo < spine.assistant_texts.len() || out.is_empty() {
        out.push((lo, spine.assistant_texts.len()));
    }
    out
}

/// Render one chunk: header + all user turns (orientation — they are the
/// smallest, highest-signal part) + assistant texts `[lo..hi)` with their
/// TRUE global ids, so citations stay valid across chunks.
pub(crate) fn render_spine_chunk(
    spine: &Spine,
    lo: usize,
    hi: usize,
    part: usize,
    parts: usize,
) -> String {
    let mut out = format!(
        "SESSION {} (part {part}/{parts}) | model {} | {} → {} | cwd {} | branch {}\n",
        spine.session_id, spine.model, spine.first_ts, spine.last_ts, spine.cwd, spine.git_branch
    );
    out.push_str(&format!("\nUSER TURNS ({}):\n", spine.user_turns.len()));
    for (i, t) in spine.user_turns.iter().enumerate() {
        out.push_str(&format!("\n--- [u{}] user turn ---\n{t}\n", i + 1));
    }
    out.push_str(&format!(
        "\nASSISTANT TEXTS [a{}]..[a{}] of {}:\n",
        lo + 1,
        hi.max(lo + 1),
        spine.assistant_texts.len()
    ));
    for i in lo..hi {
        out.push_str(&format!(
            "\n--- [a{}] assistant ---\n{}\n",
            i + 1,
            spine.assistant_texts[i]
        ));
    }
    out
}

/// Union-dedupe for chunked answers: the same fact re-found in two chunks
/// should appear once. Keyed on the lowercased alphanumeric skeleton so
/// punctuation/whitespace variants collapse.
pub(crate) fn dedupe_bullets(bullets: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    bullets
        .into_iter()
        .filter(|b| {
            let key: String = b
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_lowercase();
            seen.insert(key)
        })
        .collect()
}

// ── Stage 2: frame synthesis ─────────────────────────────────────────────

/// Deterministic frontmatter — the LLM never writes these. `head_at_end` is
/// best-effort: the last commit at/before the session's end in the
/// transcript's cwd (`unknown` outside a git repo).
pub(crate) fn render_frontmatter(spine: &Spine, head_at_end: &str, provenance: &str) -> String {
    let repo = Path::new(&spine.cwd)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("unknown");
    format!(
        "---\nschema: session-frame/v1\nsession_id: {}\nharness: claude-code\nmodel: {}\nrepo: {}\nbranch: {}\nhead_at_end: {}\nstarted_at: {}\nended_at: {}\nstatus: completed\nprovenance: {}\nnotes: []\n---\n",
        spine.session_id,
        if spine.model.is_empty() { "unknown" } else { &spine.model },
        repo,
        if spine.git_branch.is_empty() { "unknown" } else { &spine.git_branch },
        head_at_end,
        if spine.first_ts.is_empty() { "unknown" } else { &spine.first_ts },
        if spine.last_ts.is_empty() { "unknown" } else { &spine.last_ts },
        provenance,
    )
}

fn head_at_end_of(cwd: &str, ended_at: &str) -> String {
    if cwd.is_empty() || ended_at.is_empty() {
        return "unknown".to_string();
    }
    let out = std::process::Command::new("git")
        .args(["log", "-1", "--format=%h", "--before", ended_at])
        .current_dir(cwd)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if sha.is_empty() {
                "unknown".to_string()
            } else {
                sha
            }
        }
        _ => "unknown".to_string(),
    }
}

/// What a successor is about to INHERIT, measured before it starts.
///
/// The write-time advisory (`session_state`) catches a frame recopying a
/// backlog. This catches the same thing one moment earlier — at boot, when
/// the successor is reading the handoff and has not yet decided what to
/// work on. Same combinators (`sovereign_contracts::frame`), so the two
/// surfaces can never disagree.
struct Inherited {
    /// `## Next` items the donor was already carrying from ITS ancestors.
    carried: usize,
    /// Total frames the longest-lived of those has ridden.
    worst_frames: usize,
    /// Consecutive frames stating the donor's objective.
    objective_sessions: usize,
    /// Items in the donor's `## Next` altogether, for proportion.
    next_items: usize,
}

/// One frontmatter scalar. Frames are written by
/// `sovereign_contracts::frame::Frame::render`, so the form is stable:
/// `key: value` inside the leading `---` block.
fn frontmatter_value(text: &str, key: &str) -> Option<String> {
    let body = text.strip_prefix("---")?;
    let end = body.find("\n---")?;
    body[..end].lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// Ancestor frame texts, nearest first, bounded and cycle-safe.
///
/// Prefers the durable `predecessor:` frontmatter and falls back to the
/// `predecessor` sidecar, matching the writer — the sidecar prunes with
/// the window pointers, the frontmatter does not.
fn ancestor_texts(root: &Path, session_id: &str, start_text: &str) -> Vec<String> {
    const MAX_HOPS: usize = 8;
    // The durable stamp first, the prunable sidecar as fallback — the same
    // precedence the writer uses, so both surfaces walk the same chain.
    let step = |text: &str, id: &str| -> Option<String> {
        frontmatter_value(text, "predecessor")
            .or_else(|| std::fs::read_to_string(root.join(id).join("predecessor")).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.contains(['/', '\\']))
    };
    let mut out: Vec<String> = Vec::new();
    let mut seen = vec![session_id.to_string()];
    let mut next_id = step(start_text, session_id);
    while out.len() < MAX_HOPS {
        let Some(prev) = next_id.take() else { break };
        if seen.contains(&prev) {
            break;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&prev).join("frame.md")) else {
            break;
        };
        next_id = step(&text, &prev);
        out.push(text);
        seen.push(prev);
    }
    out
}

/// `None` when there is no lineage to measure or nothing was carried —
/// silence is the correct output for a healthy handoff.
fn inherited_state(root: &Path, session_id: &str, frame_path: &Path) -> Option<Inherited> {
    let text = std::fs::read_to_string(frame_path).ok()?;
    let ancestors = ancestor_texts(root, session_id, &text);
    if ancestors.is_empty() {
        return None;
    }
    let sect = |t: &str, h: &str| section_body(t, h).unwrap_or_default();
    let next = sect(&text, "## Next");
    let carried = sovereign_contracts::frame::carried_across(
        &next,
        &ancestors
            .iter()
            .map(|t| sect(t, "## Next"))
            .collect::<Vec<_>>(),
    );
    Some(Inherited {
        carried: carried.len(),
        // +1: depth counts ANCESTORS carrying it; the donor makes one more.
        worst_frames: carried.first().map(|(_, d)| d + 1).unwrap_or(0),
        objective_sessions: sovereign_contracts::frame::same_across(
            &sect(&text, "## Objective"),
            &ancestors
                .iter()
                .map(|t| sect(t, "## Objective"))
                .collect::<Vec<_>>(),
        ),
        next_items: sovereign_contracts::frame::bullet_items(&next).len(),
    })
}

impl Inherited {
    /// The sentence a successor reads at boot. `None` when there is
    /// nothing worth saying — a fresh objective with nothing carried is
    /// the healthy case and must stay quiet, or the signal becomes noise.
    fn advice(&self) -> Option<String> {
        if self.carried == 0 && self.objective_sessions < 4 {
            return None;
        }
        let mut parts = Vec::new();
        if self.carried > 0 {
            parts.push(format!(
                "**{} of {} `Next` items in this frame were already inherited** — the \
                 longest has ridden {} frames without being done or dropped",
                self.carried, self.next_items, self.worst_frames
            ));
        }
        if self.objective_sessions >= 4 {
            parts.push(format!(
                "this objective has stood unchanged for {} sessions",
                self.objective_sessions
            ));
        }
        Some(format!(
            "⟳ {}. Re-rank against `## Objective` before continuing it: do an item, \
             drop it, or say why it stays. Inheriting a backlog unexamined is how a \
             lineage turns into tweaking.",
            parts.join("; ")
        ))
    }
}

/// Record the incoming session's predecessor beside where its frame will
/// live, for the frame writer to stamp into frontmatter.
///
/// Best-effort by construction: every failure is silent. A missing
/// sidecar costs a lineage hop in an advisory signal — it must never be
/// the reason a boot hook reports an error, and it must never be the
/// reason a frame write fails.
fn record_predecessor(root: &Path, session_id: &str, predecessor: &str) {
    if session_id == predecessor
        || session_id.contains(['/', '\\'])
        || predecessor.contains(['/', '\\'])
    {
        return;
    }
    let dir = root.join(session_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join("predecessor"), predecessor);
}

/// Which contract sections are missing from a synthesized body. Empty =
/// valid. Order is not enforced in v1 (graders read sections by heading).
pub(crate) fn missing_sections(body: &str) -> Vec<&'static str> {
    FRAME_SECTIONS
        .iter()
        .filter(|s| !body.lines().any(|l| l.trim() == **s))
        .copied()
        .collect()
}

/// `## Working set` is assembled deterministically from the spine's edit
/// list — never LLM-written. First live grading run showed why: the local
/// model mangled usernames inside otherwise-plausible absolute paths
/// (a hallucination class the grader scores at −1 each). Paths are
/// relativized to the session cwd so the frame ports across machines.
pub(crate) fn render_working_set(spine: &Spine) -> String {
    let mut out = String::from("## Working set\n");
    if spine.edits.is_empty() {
        out.push_str("none recorded\n");
        return out;
    }
    let prefix = format!("{}/", spine.cwd.trim_end_matches('/'));
    let mut edits: Vec<(&String, &u64)> = spine.edits.iter().collect();
    edits.sort_by(|a, b| b.1.cmp(a.1));
    for (fp, n) in edits {
        let rel = fp.strip_prefix(&prefix).unwrap_or(fp);
        out.push_str(&format!("- {rel} ({n} edits)\n"));
    }
    out
}

/// Stage-2 synthesis prompt, v2 (MEMORY_MODEL E4b): retrieval practice.
/// One call per section — the model answers a focused question about the
/// spine and must cite the spine item(s) each bullet came from. The v1
/// single-shot "write all eight sections" prompt graded 17% against the
/// golden (Next 0/4, Invariants 0/9): one call spread over eight sections
/// summarizes at wrap-up altitude instead of mining the mid-session
/// specifics a successor needs. Citations are machine-enforced —
/// `parse_cited_bullets` drops uncited bullets — because prose instructions
/// alone don't hold on local models (same lesson as the grading judge's
/// contradiction citations).
fn retrieval_system_prompt() -> &'static str {
    "You answer one question about a coding-session transcript spine. Spine items are \
     numbered: user turns [u1], [u2], … and assistant texts [a1], [a2], …\n\
     Rules:\n\
     - Reply with markdown bullets only (`- `), one specific fact per bullet.\n\
     - Every bullet ends with the spine item(s) it came from: [u3] or [a17] or [u3,a17]. \
       A bullet with no citation is discarded unread.\n\
     - Use only facts in the spine. Copy exact numbers, test counts, file names, symbol \
       names, flags, and commands — never approximate or generalize them.\n\
     - Prefer many specific bullets over few broad ones.\n\
     - If the spine holds nothing relevant, reply exactly: none recorded"
}

/// The per-section retrieval question. Phrased to pull the successor-facing
/// content the golden holds — mined specifics, not the session's wrap-up
/// summary. Tune against `quality/session-frame.golden.md`; the questions
/// are the lever now.
fn section_question(heading: &str) -> &'static str {
    match heading {
        // Goal and Objective ask DIFFERENT questions on purpose. Until
        // 2026-07-29 `## Goal` asked for both the task and "what larger
        // objective does it serve", and the objective lost every time —
        // frame `ad5fee8c` even echoed this prompt's own words back as
        // its body ("The larger objective it serves From the assistant
        // text, particularly, I can see…"). One question per section.
        "## Objective" => {
            "What STANDING objective does this session's work serve — the outcome for a \
             user, or the initiative named in a spec or plan document, NOT the increment \
             this session delivered? Quote the doc path and section if one is named. If \
             the transcript never states an objective above the immediate task, answer \
             exactly `none stated` — do not infer one. Then, if the transcript cites \
             ARCH_PRINCIPLES.md sections the work is accountable to, add a final \
             `Anchored in:` line listing just those section numbers. If it cites none, \
             OMIT the line entirely — an invented anchor is worse than an absent one, \
             for the same reason `none stated` beats an inferred objective."
        }
        "## Goal" => "What task was this session working on? Answer in 1-2 bullets.",
        "## State" => {
            "What did this session complete? One bullet per completed piece of work, each \
             with its proof (test counts, live verification, measurements). Add bullets \
             for anything left in flight or explicitly deferred."
        }
        "## Next" => {
            "What should a successor session do next? Mine these items for: unfinished or \
             uncommitted work, untuned values, expectations stated but not yet verified, \
             work explicitly deferred, and open backlog items — watch for phrases like \
             untuned, deferred, later, still open, P1, TODO, not yet. Anchor each bullet \
             to a file, symbol, or command."
        }
        "## Decisions" => {
            "What design choices did this session make between alternatives? One bullet per \
             choice: what was chosen, over what, and the stated reason."
        }
        "## Invariants" => {
            "What operational traps and constraints did this session learn that remain \
             true for a successor — rules about running commands, reading outputs, \
             ordering operations, environment facts, and off-by-ones? Not the bugs this \
             session already fixed; the gotchas a fresh agent would still hit. Keep each \
             exactly as specific as stated: exact numbers, paths, flags, commands."
        }
        "## Dead ends" => {
            "Which approaches were tried or considered and then abandoned, rejected, or \
             superseded? One bullet each, with the reason."
        }
        "## Verification" => {
            "At session end: what were the final build/test results, what was live-verified \
             and how, and what was committed vs still uncommitted?"
        }
        _ => "",
    }
}

/// One parsed retrieval answer: bullets that carried a valid spine citation
/// (citations stripped from the text), plus how many bullets were discarded
/// for citing nothing.
pub(crate) struct CitedBullets {
    pub(crate) kept: Vec<String>,
    pub(crate) dropped: usize,
}

/// Machine-enforcement of the citation rule: extract the reply's bullets
/// (with wrapped continuation lines), keep only those containing at least
/// one citation group whose items all exist in the spine, and strip the
/// citation groups from the kept text. Non-citation bracket text is left
/// alone.
pub(crate) fn parse_cited_bullets(reply: &str, n_user: usize, n_assistant: usize) -> CitedBullets {
    let mut raw: Vec<String> = Vec::new();
    for line in reply.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let bullet_text = if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))
        {
            Some(rest.trim_start().to_string())
        } else {
            t.split_once(". ").and_then(|(num, rest)| {
                (!num.is_empty() && num.chars().all(|c| c.is_ascii_digit()))
                    .then(|| rest.to_string())
            })
        };
        match bullet_text {
            Some(b) => raw.push(b),
            None => {
                // Continuation line of a wrapped bullet; prose outside any
                // bullet (preamble chatter) is dropped.
                if let Some(last) = raw.last_mut() {
                    last.push(' ');
                    last.push_str(t);
                }
            }
        }
    }
    let mut kept = Vec::new();
    let mut dropped = 0usize;
    for b in raw {
        let (text, cited) = strip_valid_citations(&b, n_user, n_assistant);
        if cited && !text.is_empty() {
            kept.push(text);
        } else {
            dropped += 1;
        }
    }
    CitedBullets { kept, dropped }
}

/// Remove every valid citation group (`[u3]`, `[a17]`, `[u3,a17]`) from
/// `text`; returns the cleaned text and whether at least one valid group was
/// found. A group only counts when every token in it names an existing spine
/// item — `[u99]` against a 5-turn spine is not a citation, it's an invented
/// reference, and the bullet carrying only that gets dropped.
fn strip_valid_citations(text: &str, n_user: usize, n_assistant: usize) -> (String, bool) {
    let token_ok = |tok: &str| -> bool {
        if let Some(n) = tok.strip_prefix('u') {
            n.parse::<usize>().is_ok_and(|i| (1..=n_user).contains(&i))
        } else if let Some(n) = tok.strip_prefix('a') {
            n.parse::<usize>()
                .is_ok_and(|i| (1..=n_assistant).contains(&i))
        } else {
            false
        }
    };
    let mut out = String::new();
    let mut cited = false;
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        let Some(off) = rest[start..].find(']') else {
            break;
        };
        let inner = &rest[start + 1..start + off];
        let toks: Vec<&str> = inner
            .split([',', ';', ' '])
            .filter(|s| !s.is_empty())
            .collect();
        if !toks.is_empty() && toks.iter().all(|t| token_ok(t)) {
            cited = true;
            out.push_str(&rest[..start]);
        } else {
            out.push_str(&rest[..start + off + 1]);
        }
        rest = &rest[start + off + 1..];
    }
    out.push_str(rest);
    let cleaned = out.split_whitespace().collect::<Vec<_>>().join(" ");
    // Stripping mid-sentence / consecutive citations leaves punctuation
    // artifacts: " .", " ,", ",,", ",." and trailing separators.
    let mut cleaned = cleaned.replace(" .", ".").replace(" ,", ",");
    while cleaned.contains(",,") {
        cleaned = cleaned.replace(",,", ",");
    }
    let cleaned = cleaned.replace(",.", ".");
    let cleaned = cleaned.trim().trim_end_matches([',', ';', ' ']).to_string();
    (cleaned, cited)
}

/// POST an OpenAI-style chat completion to the daemon and return the
/// assistant text. Mirrors `notes_cmd::daemon_complete` (model "primary"
/// resolves to the primary slot).
async fn daemon_complete(
    base_url: &str,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let url = format!("{base_url}/v1/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": max_tokens,
        "temperature": 0.2,
        "stream": false,
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client.post(&url).json(&body).send().await.map_err(|e| {
        if e.is_connect() {
            format!("daemon not reachable at {url} — start it with `svrn daemon run`")
        } else {
            format!("request failed: {e}")
        }
    })?;
    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("daemon returned {code}: {}", cap_chars(&text, 300)));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("decode: {e}"))?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "response had no choices[0].message.content".to_string())
}

// ── Grading (spec §5) ────────────────────────────────────────────────────

/// Section weight: `## Next` and `## Invariants` are what a successor
/// acts on first and what protects it from repeating pain (spec §5).
fn section_weight(heading: &str) -> usize {
    match heading {
        "## Next" | "## Invariants" => 2,
        _ => 1,
    }
}

/// The body of one `## `-section (text between the heading and the next
/// heading), or `None` when the heading is absent.
pub(crate) fn section_body(frame: &str, heading: &str) -> Option<String> {
    let lines: Vec<&str> = frame.lines().collect();
    let start = lines.iter().position(|l| l.trim() == heading)?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with("## "))
        .map(|off| start + 1 + off)
        .unwrap_or(lines.len());
    Some(lines[start + 1..end].join("\n").trim().to_string())
}

/// A golden section's load-bearing items: its bullets (`- `, `* `, or
/// `1.`-style). A bullet-less but non-empty section counts as one item.
/// Empty / "none recorded" sections grade nothing.
pub(crate) fn golden_items(body: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for line in body.lines() {
        let t = line.trim_start();
        let is_bullet = t.starts_with("- ")
            || t.starts_with("* ")
            || (t
                .split('.')
                .next()
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
                && t.contains(". "));
        if is_bullet {
            let stripped = t.trim_start_matches(['-', '*']).trim_start();
            // Numbered bullets: drop the "<digits>. " prefix too, so the
            // judge sees the item text once, not "1. 1. <text>".
            let stripped = match stripped.split_once(". ") {
                Some((num, rest)) if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) => {
                    rest
                }
                _ => stripped,
            };
            items.push(stripped.to_string());
        } else if let Some(last) = items.last_mut() {
            // Continuation line of a wrapped bullet.
            if !t.is_empty() {
                last.push(' ');
                last.push_str(t);
            }
        }
    }
    if items.is_empty() {
        let t = body.trim();
        if !t.is_empty() && t != "none recorded" {
            items.push(t.to_string());
        }
    }
    items
}

fn judge_system_prompt() -> &'static str {
    // Structured to survive a small local model: the contradiction field
    // is REQUIRED and machine-checked. The first live run showed the
    // model flagging candidate detail merely ABSENT from the reference
    // as hallucination despite a prose instruction not to — so instead
    // of asking it not to, we make every hallucination cite the
    // reference item it contradicts and drop entries that cannot.
    "You grade one section of a session frame against reference items.\n\
     Task 1 — captured: for each numbered reference item, decide whether the \
     candidate text conveys it. Judge wording leniently, facts strictly: a wrong \
     number, commit sha, count, file name, or symbol name means NOT captured.\n\
     Task 2 — contradictions: list candidate claims that state the OPPOSITE of a \
     specific reference item (a different number, sha, name, or outcome for the \
     same fact). Each entry must name the reference item it contradicts.\n\
     Reply with ONLY this JSON, no other text:\n\
     {\"captured\": [<item numbers>], \
      \"contradictions\": [{\"claim\": \"<candidate claim>\", \"contradicts_item\": <number>}]}"
}

/// Extract the first JSON object from a model reply (models under
/// instruction pressure still sometimes wrap JSON in prose).
fn first_json_object(s: &str) -> Option<serde_json::Value> {
    let start = s.find('{')?;
    let mut depth = 0usize;
    for (i, c) in s[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&s[start..start + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

struct SectionGrade {
    heading: &'static str,
    weight: usize,
    items: usize,
    captured: usize,
    hallucinations: Vec<String>,
}

/// Grade a candidate frame against a golden (spec §5): per-section recall
/// of the golden's load-bearing items, judged by the local daemon model;
/// `## Next`/`## Invariants` weighted double; −1 per hallucination; pass
/// at ≥70% weighted recall AND zero Verification-section hallucinations.
async fn run_grade(id_or_path: &str, flags: &BTreeMap<String, String>) -> i32 {
    // Candidate: an explicit path, or a session-id prefix resolved to
    // ~/.svrnmesh/sessions/<sid>/frame.md.
    let candidate_path = match resolve_frame_path(id_or_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("grade: {e}");
            return 2;
        }
    };
    let golden_path = flags
        .get("golden")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("quality/session-frame.golden.md"));

    let candidate = match std::fs::read_to_string(&candidate_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("grade: read {}: {e}", candidate_path.display());
            return 2;
        }
    };
    let golden = match std::fs::read_to_string(&golden_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "grade: read golden {}: {e} (pass --golden <path>)",
                golden_path.display()
            );
            return 2;
        }
    };

    let base_url = flags
        .get("url")
        .cloned()
        .unwrap_or_else(|| "http://localhost:9741".to_string());
    let model = flags
        .get("model")
        .cloned()
        .unwrap_or_else(|| "primary".into());

    let mut grades: Vec<SectionGrade> = Vec::new();
    for heading in FRAME_SECTIONS {
        // Working set is CLI-assembled on both sides — machine-comparable,
        // no judge needed, and its paths would dominate hallucination
        // counts for no signal. Skip.
        if *heading == "## Working set" {
            continue;
        }
        let g_body = section_body(&golden, heading).unwrap_or_default();
        let items = golden_items(&g_body);
        if items.is_empty() {
            continue;
        }
        let c_body = section_body(&candidate, heading).unwrap_or_default();

        let numbered: String = items
            .iter()
            .enumerate()
            .map(|(i, it)| format!("{}. {}\n", i + 1, it))
            .collect();
        let user = format!(
            "REFERENCE ITEMS ({heading}):\n{numbered}\nCANDIDATE TEXT:\n{}",
            if c_body.is_empty() {
                "(section missing)"
            } else {
                &c_body
            }
        );
        let reply =
            match daemon_complete(&base_url, &model, judge_system_prompt(), &user, 600).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("grade: judge call failed on {heading}: {e}");
                    return 2;
                }
            };
        let Some(verdict) = first_json_object(&reply) else {
            eprintln!(
                "grade: judge reply on {heading} had no JSON: {}",
                cap_chars(&reply, 200)
            );
            return 2;
        };
        let captured: std::collections::HashSet<usize> = verdict["captured"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_u64())
                    .map(|n| n as usize)
                    .filter(|n| (1..=items.len()).contains(n))
                    .collect()
            })
            .unwrap_or_default();
        // A hallucination only counts when the judge names a real
        // reference item it contradicts AND did not also mark that item
        // captured (both = the judge arguing with itself; trust the
        // capture). Anything else is detail absent from the golden,
        // which the spec does not penalize.
        let hallucinations: Vec<String> = verdict["contradictions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|v| {
                        v["contradicts_item"]
                            .as_u64()
                            .map(|n| n as usize)
                            .is_some_and(|n| {
                                (1..=items.len()).contains(&n) && !captured.contains(&n)
                            })
                    })
                    .filter_map(|v| v["claim"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        grades.push(SectionGrade {
            heading,
            weight: section_weight(heading),
            items: items.len(),
            captured: captured.len(),
            hallucinations,
        });
    }

    if grades.is_empty() {
        eprintln!(
            "grade: golden has no gradeable sections at {}",
            golden_path.display()
        );
        return 2;
    }

    let denom: usize = grades.iter().map(|g| g.weight * g.items).sum();
    let raw: usize = grades.iter().map(|g| g.weight * g.captured).sum();
    let halluc_total: usize = grades.iter().map(|g| g.hallucinations.len()).sum();
    let numer = raw.saturating_sub(halluc_total);
    let score = numer as f64 / denom as f64;
    let verification_hallucinated = grades
        .iter()
        .any(|g| g.heading == "## Verification" && !g.hallucinations.is_empty());
    let pass = score >= 0.70 && !verification_hallucinated;

    println!(
        "grading {} against {}\n",
        candidate_path.display(),
        golden_path.display()
    );
    for g in &grades {
        let h = if g.hallucinations.is_empty() {
            String::new()
        } else {
            format!(" · {} hallucination(s)", g.hallucinations.len())
        };
        println!(
            "  {:<16} {}/{} (w{}){}",
            g.heading.trim_start_matches("## "),
            g.captured,
            g.items,
            g.weight,
            h
        );
        for claim in &g.hallucinations {
            println!("      ! {}", cap_chars(claim, 120));
        }
    }
    println!(
        "\n  weighted recall {numer}/{denom} = {:.0}%{}",
        score * 100.0,
        if halluc_total > 0 {
            format!(" (after −{halluc_total} hallucination penalty)")
        } else {
            String::new()
        }
    );
    println!(
        "  {}",
        if pass {
            "PASS (bar: ≥70% weighted recall, zero hallucinated verification claims)"
        } else if verification_hallucinated {
            "FAIL — hallucinated verification claim (automatic fail regardless of recall)"
        } else {
            "FAIL (bar: ≥70% weighted recall)"
        }
    );
    if pass {
        0
    } else {
        1
    }
}

// ── Frame index — selection + dereference (MEMORY_MODEL §5 E5 Phase 2) ────
//
// WHY THIS EXISTS. The boot hook used to inject the frame with the newest
// mtime. With several workstreams interleaved (24 live frames when this was
// measured) the newest frame is the successor's only by luck — and a WRONG
// frame costs more than no frame: session 40ab6490 was handed another
// thread's frame and burned 5,872 ramp tokens hunting for the right one by
// hand, against 9.3k total ramp for the session that happened to get the
// right one (E5 R1).
//
// The fix is a POINTER, not a better guess. Boot injects the *index* below —
// one line per live frame, ~200 tokens — and the agent dereferences the one
// it wants with `sovereign session frames <id>`. Selection still happens (the
// first UserPromptSubmit injects the top-ranked frame in full), but it now
// happens where the PROMPT exists, and it is recoverable when it is wrong.
//
// RANKING IS LEXICOGRAPHIC AND DELIBERATELY DUMB. E2's rule — measure before
// tuning — applies here: no weights, no scoring function to over-fit. The
// order is branch match, then prompt overlap (the one signal SessionStart
// could never have, since it has no prompt), then recency. Every component,
// used or not, is emitted in `--json`, so the classifier can later answer
// "would a different order have picked a different frame?" against real
// sessions instead of intuition.
//
// IN-FLIGHT IS RECORDED BUT NOT RANKED ON, and that is a correction to the
// Phase 2 sketch, made against the live store. Two reasons, both observable
// in `sovereign session frames` today: (1) `status` is free text — the 23
// live frames carry `in-flight`, `completed`, AND `work-complete-uncommitted`
// — so any predicate over it is a guess about a string, not a fact about the
// work; (2) sorting in-flight first buried the frame the successor actually
// needed. The session that opened this work was handed a `completed` frame
// whose `## Next` was the entire task; ranked below every `in-flight` frame
// it fell past the 8-line cut and would not have been injected at all. A
// completed frame is the NORMAL good handoff — completion means the donor got
// far enough to write down what comes next.

/// One session frame, as the index sees it: its frontmatter facts, its goal
/// line, and every ranking signal computed for it (recorded even when it did
/// not affect the outcome — that is what makes the ranker auditable).
#[derive(Debug, Clone)]
pub(crate) struct FrameEntry {
    pub(crate) session_id: String,
    pub(crate) path: PathBuf,
    pub(crate) repo: String,
    pub(crate) branch: String,
    pub(crate) status: String,
    pub(crate) provenance: String,
    pub(crate) age_s: u64,
    pub(crate) goal: String,
    pub(crate) next_items: usize,
    pub(crate) chars: usize,
    /// Ranking signals.
    ///
    /// `same_window` is the only one that is a FACT rather than a heuristic:
    /// this frame was banked by the session that previously occupied this
    /// terminal (see `session_lineage`). It outranks everything below it
    /// because the others are evidence and it is an observation.
    pub(crate) same_window: bool,
    pub(crate) branch_match: bool,
    pub(crate) in_flight: bool,
    pub(crate) prompt_overlap: usize,
    pub(crate) overlap_terms: Vec<String>,
}

/// Read one `key: value` line out of a schema-v1 frontmatter block. Stops at
/// the closing `---` so a body line that happens to look like `key: value`
/// can never be mistaken for frontmatter.
pub(crate) fn frontmatter_field(frame: &str, key: &str) -> Option<String> {
    let mut lines = frame.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let t = line.trim();
        if t == "---" {
            return None;
        }
        if let Some(rest) = t.strip_prefix(key) {
            if let Some(v) = rest.strip_prefix(':') {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// The frame's goal as a single display line: first non-empty line of
/// `## Goal`, capped. An index entry that wraps is an index nobody scans.
pub(crate) fn goal_line(frame: &str, cap: usize) -> String {
    let body = section_body(frame, "## Goal").unwrap_or_default();
    let first = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches(['-', '*', ' '])
        .to_string();
    if first.chars().count() <= cap {
        return first;
    }
    let cut: String = first.chars().take(cap).collect();
    format!("{}…", cut.trim_end())
}

/// Identifier-shaped tokens from free text — the same notion `inject-notes.sh`
/// uses for its retrieval log: a token carrying a code/path shape
/// (snake_case, CamelCase, dotted, slashed) or simply long. These are the
/// tokens unlikely to co-occur by chance, so their presence in a frame is
/// weak evidence the frame is about the same work as the prompt.
pub(crate) fn distinctive_terms(text: &str, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cur = String::new();
    // Hand-rolled scan (no regex dep in this crate): a token is
    // [A-Za-z_][A-Za-z0-9_./-]* of length >= 5.
    let push =
        |cur: &mut String, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
            if cur.chars().count() >= 5 {
                let t = cur.clone();
                let distinctive = t.contains('_')
                    || t.contains('.')
                    || t.contains('/')
                    || t.chars().skip(1).any(|c| c.is_ascii_uppercase())
                    || t.chars().count() >= 8;
                let tl = t.to_lowercase();
                if distinctive && seen.insert(tl) {
                    out.push(t);
                }
            }
            cur.clear();
        };
    for ch in text.chars() {
        let starts = ch.is_ascii_alphabetic() || ch == '_';
        let continues = ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-');
        if cur.is_empty() {
            if starts {
                cur.push(ch);
            }
        } else if continues {
            cur.push(ch);
        } else {
            push(&mut cur, &mut out, &mut seen);
            if out.len() >= cap {
                return out;
            }
        }
    }
    push(&mut cur, &mut out, &mut seen);
    out.truncate(cap);
    out
}

/// Ordinary English that `distinctive_terms` lets through on the length rule
/// alone (>= 8 chars, no code shape) and that therefore matched EVERY frame.
///
/// Measured, not guessed: across 22 recorded selections the deciding overlap
/// terms included `everything`, `continue`, `something`, `tolerance`,
/// `solution`, `containing`, `production` — words that carry no information
/// about which workstream a prompt is about, but which broke recency ties and
/// so effectively chose the frame at random. A term must earn its vote.
const GENERIC_TERMS: &[&str] = &[
    "everything",
    "something",
    "anything",
    "continue",
    "continues",
    "continuing",
    "solution",
    "solutions",
    "containing",
    "contains",
    "consider",
    "different",
    "important",
    "probably",
    "actually",
    "possible",
    "question",
    "questions",
    "understand",
    "following",
    "remaining",
    "complete",
    "completed",
    "problem",
    "problems",
    "approach",
    "instead",
    "already",
    "another",
    "because",
    "between",
    "through",
    "without",
    "against",
    "further",
    "however",
    "changes",
    "working",
    "session",
    "sessions",
    "production",
    "tolerance",
];

/// Is this term too common to be evidence? Anything with a code/path shape is
/// kept regardless of length — `session_cmd` and `frame.md` are exactly the
/// tokens this signal exists to catch.
fn is_generic_term(t: &str) -> bool {
    if t.contains('_') || t.contains('.') || t.contains('/') || t.contains('-') {
        return false;
    }
    if t.chars().skip(1).any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    let tl = t.to_lowercase();
    GENERIC_TERMS.contains(&tl.as_str())
}

/// How many of the prompt's distinctive terms appear anywhere in the frame.
/// Case-insensitive substring, because a prompt says `session_cmd` where the
/// frame says `session_cmd.rs`.
fn overlap_with(frame_lower: &str, terms: &[String]) -> Vec<String> {
    terms
        .iter()
        .filter(|t| !is_generic_term(t))
        .filter(|t| frame_lower.contains(&t.to_lowercase()))
        .cloned()
        .collect()
}

/// Load every frame under `~/.svrnmesh/sessions/*/frame.md` newer than
/// `max_age_days`, annotated with the ranking signals for this caller.
pub(crate) fn load_frames(
    root: &Path,
    max_age_days: u64,
    repo: Option<&str>,
    branch: Option<&str>,
    prompt: Option<&str>,
    predecessor: Option<&str>,
) -> Vec<FrameEntry> {
    let terms = prompt.map(|p| distinctive_terms(p, 20)).unwrap_or_default();
    let now = std::time::SystemTime::now();
    let mut out: Vec<FrameEntry> = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path().join("frame.md");
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let age_s = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);
        if age_s > max_age_days.saturating_mul(86_400) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let frame_repo = frontmatter_field(&text, "repo").unwrap_or_default();
        // Hard filter: a frame from another repository is not a candidate for
        // this one. An UNKNOWN repo still passes — old frames predate the
        // field, and dropping them silently would be the same class of lie
        // this whole surface exists to remove.
        if let Some(want) = repo {
            if !frame_repo.is_empty() && frame_repo != want {
                continue;
            }
        }
        let frame_branch = frontmatter_field(&text, "branch").unwrap_or_default();
        let status = frontmatter_field(&text, "status").unwrap_or_default();
        let overlap_terms = if terms.is_empty() {
            Vec::new()
        } else {
            overlap_with(&text.to_lowercase(), &terms)
        };
        let session_id = entry.file_name().to_string_lossy().to_string();
        out.push(FrameEntry {
            same_window: predecessor.is_some_and(|p| p == session_id),
            branch_match: branch.is_some_and(|b| b == frame_branch),
            // `status` is free text written by whoever banked the frame — the
            // live store carries `in-flight`, `completed`, and
            // `work-complete-uncommitted`. Match on the stem, not the whole
            // string, and treat anything unrecognised as still in flight
            // (the safer read: an unknown status is not evidence of done).
            in_flight: {
                let s = status.to_lowercase();
                !(s.contains("complete") || s.contains("abandon") || s.contains("done"))
            },
            prompt_overlap: overlap_terms.len(),
            overlap_terms,
            repo: frame_repo,
            branch: frame_branch,
            status,
            provenance: frontmatter_field(&text, "provenance").unwrap_or_default(),
            age_s,
            goal: goal_line(&text, 96),
            next_items: section_body(&text, "## Next")
                .map(|b| golden_items(&b).len())
                .unwrap_or(0),
            chars: text.len(),
            path,
            session_id,
        });
    }
    retain_listable(&mut out);
    rank_frames(&mut out);
    out
}

/// Drop frames that are not handoff candidates before they reach the
/// index.
///
/// `abandoned` is the ONE status that means "do not continue this" —
/// the donor said so explicitly. `completed` deliberately stays: the
/// module note above is emphatic that a completed frame is the NORMAL
/// good handoff (completion means the donor got far enough to write
/// down what is next), and filtering it was the E5 bug, not the fix.
///
/// The `same_window` exemption is load-bearing: the frame banked by
/// the previous occupant of THIS terminal is observed, not inferred,
/// and hiding it would leave the successor with a silent gap where its
/// own predecessor should be. Better to show it and let the successor
/// read `status: abandoned` for itself.
///
/// Retired frames stay on disk and stay readable by id via
/// `sovereign session frames <id>` — this filters the index, not the
/// store.
pub(crate) fn retain_listable(frames: &mut Vec<FrameEntry>) {
    frames.retain(|f| f.same_window || !f.status.to_lowercase().contains("abandon"));
}

/// Lexicographic order: **same window**, then branch match, then prompt
/// overlap, then recency. See the module note above for why there are no
/// weights, and why `in_flight` is carried on every entry but deliberately not
/// sorted on.
///
/// `same_window` leads because it is the one key that is observed rather than
/// inferred — the frame banked by the previous occupant of this terminal. On a
/// single-branch repo `branch_match` is constant across every candidate and
/// therefore decides nothing, which is how 25–42 frames ended up being
/// separated by whether the prompt happened to contain the word "continue".
pub(crate) fn rank_frames(frames: &mut [FrameEntry]) {
    frames.sort_by(|a, b| {
        b.same_window
            .cmp(&a.same_window)
            .then(b.branch_match.cmp(&a.branch_match))
            .then(b.prompt_overlap.cmp(&a.prompt_overlap))
            .then(a.age_s.cmp(&b.age_s))
    });
}

fn human_age(age_s: u64) -> String {
    if age_s < 3_600 {
        format!("{}m", age_s / 60)
    } else if age_s < 86_400 {
        format!("{}h", age_s / 3_600)
    } else {
        format!("{}d", age_s / 86_400)
    }
}

/// The index as it enters an agent's context: one scannable line per frame,
/// and the verb that dereferences it. Kept tight on purpose — this replaces a
/// 1–2k-token frame injection with roughly 200 tokens.
pub(crate) fn render_index(frames: &[FrameEntry], limit: usize) -> String {
    if frames.is_empty() {
        return String::new();
    }
    let shown = frames.len().min(limit);
    let mut s = format!(
        "### Live session frames ({} live{}) — read one: `sovereign session frames <id>`\n\n",
        frames.len(),
        if shown < frames.len() {
            format!(", {shown} shown")
        } else {
            String::new()
        }
    );
    for f in frames.iter().take(shown) {
        let id = short_session_id(&f.session_id);
        let goal = if f.goal.is_empty() {
            "(no goal recorded)"
        } else {
            &f.goal
        };
        s.push_str(&format!(
            "- `{id}`{win} · {age} · {branch} · {status} · {prov} · {next} next — {goal}\n",
            win = if f.same_window {
                " ← THIS WINDOW"
            } else {
                ""
            },
            age = human_age(f.age_s),
            branch = if f.branch.is_empty() { "?" } else { &f.branch },
            status = if f.status.is_empty() { "?" } else { &f.status },
            prov = if f.provenance.is_empty() {
                "?"
            } else {
                &f.provenance
            },
            next = f.next_items,
        ));
    }
    s
}

fn frame_json(f: &FrameEntry) -> serde_json::Value {
    serde_json::json!({
        "session_id": f.session_id,
        "short_id": short_session_id(&f.session_id),
        "path": f.path.display().to_string(),
        "repo": f.repo,
        "branch": f.branch,
        "status": f.status,
        "provenance": f.provenance,
        "age_s": f.age_s,
        "goal": f.goal,
        "next_items": f.next_items,
        "chars": f.chars,
        "signals": {
            "same_window": f.same_window,
            "branch_match": f.branch_match,
            "in_flight": f.in_flight,
            "prompt_overlap": f.prompt_overlap,
            "overlap_terms": f.overlap_terms,
        },
    })
}

/// Resolve a session-id prefix (or an explicit path) to a frame file.
/// Shared by `frames <id>` and `grade <id>` so the two cannot disagree about
/// what an id means.
pub(crate) fn resolve_frame_path(id_or_path: &str) -> Result<PathBuf, String> {
    let p = Path::new(id_or_path);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    let root = sessions_root().ok_or_else(|| "cannot resolve home directory".to_string())?;
    let mut matches: Vec<PathBuf> = std::fs::read_dir(&root)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|d| {
                    d.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(id_or_path))
                        && d.join("frame.md").is_file()
                })
                .collect()
        })
        .unwrap_or_default();
    match matches.len() {
        1 => Ok(matches.remove(0).join("frame.md")),
        0 => Err(format!(
            "`{id_or_path}` is neither a file nor a session with a frame under {} \
             (see `svrn session frames`)",
            root.display()
        )),
        n => Err(format!(
            "`{id_or_path}` is ambiguous ({n} session matches) — use more of the id"
        )),
    }
}

/// The frame the window lineage points at, resolved to a real file.
///
/// Separated from the ranked candidates on purpose: a predecessor is not a
/// better guess, it is a different KIND of answer. Callers that have one
/// should stop ranking; callers that do not should say so rather than quietly
/// presenting rank #1 as if it were a handoff.
struct Predecessor {
    pointer: session_lineage::Pointer,
    path: Option<PathBuf>,
    frame_age_s: Option<u64>,
}

fn resolve_predecessor(
    win: Option<&session_lineage::WindowKey>,
    self_session: Option<&str>,
) -> Option<Predecessor> {
    let win = win?;
    let pointer = session_lineage::read_pointer(win)?;
    // A boot that re-reads its own binding is not a handoff. This happens on a
    // second SessionStart for the same session (resume), where the `own_full`
    // path is the correct one anyway.
    if self_session.is_some_and(|s| s == pointer.session_id) {
        return None;
    }
    let path = sessions_root()
        .map(|r| r.join(&pointer.session_id).join("frame.md"))
        .filter(|p| p.is_file());
    let frame_age_s = path.as_ref().and_then(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
            .map(|d| d.as_secs())
    });
    Some(Predecessor {
        pointer,
        path,
        frame_age_s,
    })
}

fn predecessor_json(p: &Predecessor, root: &Path) -> serde_json::Value {
    // What the successor is inheriting, computed before it starts. Absent
    // when there is no lineage or nothing is stale — see `Inherited::advice`.
    let inherited = p
        .path
        .as_ref()
        .and_then(|fp| inherited_state(root, &p.pointer.session_id, fp));
    let mut doc = serde_json::json!({
        "session_id": p.pointer.session_id,
        "short_id": short_session_id(&p.pointer.session_id),
        "path": p.path.as_ref().map(|x| x.display().to_string()),
        // `process` — the previous occupant of this terminal, bound
        // automatically. `explicit` — a human ran `session attach`.
        "kind": p.pointer.kind,
        "bound_age_s": p.pointer.age_s(),
        "frame_age_s": p.frame_age_s,
        // False when the predecessor existed but never banked a frame (too
        // short to distill, or the distill is still running). The caller must
        // fall back to the index — and must not claim a handoff it did not get.
        "has_frame": p.path.is_some(),
        "repo": p.pointer.repo,
        "branch": p.pointer.branch,
    });
    if let Some(i) = &inherited {
        doc["carried_items"] = serde_json::json!(i.carried);
        doc["carried_worst_frames"] = serde_json::json!(i.worst_frames);
        doc["next_items"] = serde_json::json!(i.next_items);
        doc["objective_sessions"] = serde_json::json!(i.objective_sessions);
        // Pre-rendered so every consumer says the same thing. The boot hook
        // emits this verbatim rather than composing its own sentence.
        if let Some(advice) = i.advice() {
            doc["inherited_advice"] = serde_json::json!(advice);
        }
    }
    doc
}

fn window_json(win: Option<&session_lineage::WindowKey>) -> serde_json::Value {
    match win {
        Some(w) => serde_json::json!({
            "key": w.key, "pid": w.pid, "tty": w.tty, "started": w.started,
        }),
        // Not an error: no harness ancestor means no window (plain shell, CI,
        // `claude -p`). Ranking proceeds without the lineage signal.
        None => serde_json::Value::Null,
    }
}

fn run_frames(id: Option<&str>, flags: &BTreeMap<String, String>) -> i32 {
    // Dereference: print one frame whole.
    if let Some(id) = id {
        return match resolve_frame_path(id) {
            Ok(p) => match std::fs::read_to_string(&p) {
                Ok(text) => {
                    print!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("frames: cannot read {} ({e})", p.display());
                    2
                }
            },
            Err(e) => {
                eprintln!("frames: {e}");
                2
            }
        };
    }

    let Some(root) = sessions_root() else {
        eprintln!("frames: cannot resolve home directory");
        return 2;
    };
    let max_age_days: u64 = flags
        .get("max-age-days")
        .and_then(|v| v.parse().ok())
        .unwrap_or(14);
    let limit: usize = flags.get("limit").and_then(|v| v.parse().ok()).unwrap_or(8);

    // ── Window lineage ──────────────────────────────────────────────────
    // `--claim-window <session_id>` is an EXCHANGE, and the side effect is in
    // the flag name on purpose: read whoever last occupied this terminal (your
    // predecessor), then record yourself as the occupant so your own successor
    // can find you. The boot hook is the only caller that claims; every other
    // reader gets the pointer without disturbing it.
    let win = if flags.contains_key("no-lineage") {
        None
    } else {
        session_lineage::resolve_window()
    };
    let claiming = flags.get("claim-window").filter(|s| !s.is_empty());
    // Who is asking. A caller that is already the recorded occupant is not its
    // own predecessor — without this, the second hook of the same session
    // would be handed back the binding the first one just wrote.
    let me = claiming
        .or_else(|| flags.get("self").filter(|s| !s.is_empty()))
        .map(String::as_str);
    let predecessor = resolve_predecessor(win.as_ref(), me);
    let mut bind_error: Option<String> = None;
    if let (Some(sid), Some(w)) = (claiming, win.as_ref()) {
        if let Err(e) = session_lineage::write_pointer(
            w,
            sid,
            "process",
            flags.get("repo").map(String::as_str).unwrap_or_default(),
            flags.get("branch").map(String::as_str).unwrap_or_default(),
        ) {
            // Never fatal — a window that cannot be bound simply falls back to
            // ranking next time, which is the pre-lineage behaviour.
            bind_error = Some(e);
        }
    }

    // Hand the incoming session its predecessor's id, as a sidecar next to
    // where its frame will live. THIS IS THE ONLY MOMENT BOTH IDS ARE
    // KNOWN: the window pointer still holds the outgoing session and the
    // claim names the incoming one. The frame writer (`sovereign-tools`)
    // reads it to stamp `predecessor:` into frontmatter, which is what
    // makes a lineage walkable offline — and that walk is what lets a
    // frame notice it is recopying a backlog its ancestors already carried.
    // sovereign-tools cannot call session_lineage (the crates do not see
    // each other under the repo's feature contract), so the hand-off is a
    // file rather than a function call.
    if let (Some(sid), Some(p)) = (claiming, predecessor.as_ref()) {
        record_predecessor(&root, sid, &p.pointer.session_id);
    }

    let frames = load_frames(
        &root,
        max_age_days,
        flags.get("repo").map(String::as_str),
        flags.get("branch").map(String::as_str),
        flags.get("for-prompt").map(String::as_str),
        predecessor.as_ref().map(|p| p.pointer.session_id.as_str()),
    );

    if flags.contains_key("json") {
        let doc = serde_json::json!({
            "schema": "frame-index/v2",
            "root": root.display().to_string(),
            "repo": flags.get("repo").cloned().unwrap_or_default(),
            "branch": flags.get("branch").cloned().unwrap_or_default(),
            "max_age_days": max_age_days,
            "count": frames.len(),
            "window": window_json(win.as_ref()),
            // The deterministic answer when there is one. A caller with a
            // predecessor should inject it and skip selection entirely.
            "predecessor": predecessor.as_ref().map(|p| predecessor_json(p, &root)),
            "bind_error": bind_error,
            // Rank order. The head is the selection; the rest is the evidence
            // for why, so a caller can disagree with it.
            "candidates": frames.iter().take(limit).map(frame_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&doc).unwrap_or_default());
        return 0;
    }

    if let Some(p) = &predecessor {
        let how = if p.pointer.kind == "explicit" {
            "attached explicitly"
        } else {
            "previous occupant of this terminal"
        };
        match &p.path {
            Some(path) => println!(
                "Predecessor in this window: `{}` ({how}, bound {} ago, frame {} old)\n  {}\n",
                short_session_id(&p.pointer.session_id),
                human_age(p.pointer.age_s()),
                p.frame_age_s.map(human_age).unwrap_or_else(|| "?".into()),
                path.display(),
            ),
            None => println!(
                "Predecessor in this window: `{}` ({how}) — but it banked NO frame; \
                 falling back to the index.\n",
                short_session_id(&p.pointer.session_id),
            ),
        }
        // Same sentence the boot hook injects, so a human running this by
        // hand sees exactly what an agent sees.
        if let Some(advice) = p
            .path
            .as_ref()
            .and_then(|fp| inherited_state(&root, &p.pointer.session_id, fp))
            .and_then(|i| i.advice())
        {
            println!("{advice}\n");
        }
    }
    if let Some(e) = &bind_error {
        eprintln!("frames: could not bind this window ({e}) — lineage will not carry forward");
    }

    if frames.is_empty() {
        println!(
            "No session frames under {} (nothing to resume).",
            root.display()
        );
        return 0;
    }
    print!("{}", render_index(&frames, limit));
    0
}

/// `session lineage` — what this terminal is attached to, and how it knows.
/// Pure glassbox: the boot hook's decision is a lookup a human can reproduce.
fn run_lineage(flags: &BTreeMap<String, String>) -> i32 {
    let win = session_lineage::resolve_window();
    let predecessor = resolve_predecessor(win.as_ref(), None);
    if flags.contains_key("json") {
        // No home dir means no frames to measure a lineage against; the
        // window view itself still answers, so degrade rather than fail.
        let root = sessions_root();
        let doc = serde_json::json!({
            "schema": "window-lineage-view/v1",
            "window": window_json(win.as_ref()),
            "attached": predecessor
                .as_ref()
                .zip(root.as_ref())
                .map(|(p, r)| predecessor_json(p, r)),
        });
        println!("{}", serde_json::to_string(&doc).unwrap_or_default());
        return 0;
    }
    match &win {
        Some(w) => println!(
            "window   {} · harness pid {} · started {} · key {}",
            w.tty, w.pid, w.started, w.key
        ),
        None => {
            println!(
                "window   none — no harness process above this one (plain shell, CI, or \
                 headless run).\n         Frame selection falls back to the ranked index."
            );
            return 0;
        }
    }
    match &predecessor {
        Some(p) => {
            println!(
                "attached {} ({}, bound {} ago)",
                short_session_id(&p.pointer.session_id),
                p.pointer.kind,
                human_age(p.pointer.age_s())
            );
            match &p.path {
                Some(path) => println!(
                    "frame    {} ({} old)",
                    path.display(),
                    p.frame_age_s.map(human_age).unwrap_or_else(|| "?".into())
                ),
                None => println!("frame    none banked yet by that session"),
            }
        }
        None => println!(
            "attached nothing yet — the next `/clear` in this window has no predecessor.\n\
             Bind one with `svrn session attach <id>` (see `svrn session frames`)."
        ),
    }
    0
}

/// `session attach <id>` — point this window at a workstream by hand.
///
/// The automatic binding only covers "continue what this terminal was just
/// doing". The other real case is "open a fresh terminal to pick up work that
/// ran somewhere else" — no process lineage exists for that, so a human says
/// it once and the next boot in this window honours it.
fn run_attach(id: Option<&str>, flags: &BTreeMap<String, String>) -> i32 {
    let Some(win) = session_lineage::resolve_window() else {
        eprintln!(
            "attach: no harness window above this process — nothing to attach to.\n\
             (Run this from inside the session you want to bind.)"
        );
        return 2;
    };
    if flags.contains_key("clear") {
        return match session_lineage::clear_pointer(&win) {
            Ok(true) => {
                println!(
                    "Detached window {} — the next session here starts cold.",
                    win.key
                );
                0
            }
            Ok(false) => {
                println!("Window {} was not attached to anything.", win.key);
                0
            }
            Err(e) => {
                eprintln!("attach: {e}");
                2
            }
        };
    }
    let Some(id) = id else {
        eprintln!("attach: needs a session id (see `svrn session frames`), or --clear");
        return 2;
    };
    // Resolve through the same path `frames <id>` uses, so an id that attaches
    // is always an id that dereferences.
    let path = match resolve_frame_path(id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("attach: {e}");
            return 2;
        }
    };
    let session_id = path
        .parent()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| id.to_string());
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let repo = frontmatter_field(&text, "repo").unwrap_or_default();
    let branch = frontmatter_field(&text, "branch").unwrap_or_default();

    if let Some(prev) = session_lineage::read_pointer(&win) {
        if prev.session_id != session_id {
            println!(
                "Replacing this window's attachment: {} → {}",
                short_session_id(&prev.session_id),
                short_session_id(&session_id)
            );
        }
    }
    match session_lineage::write_pointer(&win, &session_id, "explicit", &repo, &branch) {
        Ok(()) => {
            println!(
                "Attached window {} (pid {}) to `{}`.\n\
                 The next session started here — including after `/clear` — is handed that \
                 frame whole, with no guessing.",
                win.key,
                win.pid,
                short_session_id(&session_id)
            );
            0
        }
        Err(e) => {
            eprintln!("attach: {e}");
            2
        }
    }
}

// ── CLI plumbing ─────────────────────────────────────────────────────────

fn sessions_root() -> Option<PathBuf> {
    // Overridable for the same reason as SOVEREIGN_LINEAGE_DIR: the handoff
    // path has to be exercisable end-to-end without writing into the live
    // store the machine is actually using.
    if let Some(d) = session_lineage::env_either("SESSIONS_DIR") {
        return Some(PathBuf::from(d));
    }
    Some(sovereign_contracts::rebrand::svrnmesh_root().join("sessions"))
}

fn print_help() {
    println!(
        "Usage: svrn session <subcommand> [options]\n\n\
         Session continuity — distill a harness transcript into a session frame\n\
         (schema: sovereign/docs/specs/SESSION_CONTINUITY.md).\n\n\
         Subcommands:\n\
         \x20 list               Recent transcripts for this project (newest first).\n\
         \x20 frames             Index of live session frames, one line each, in\n\
         \x20                    selection order (same window, branch match, prompt\n\
         \x20                    overlap, recency). This is what the boot hook\n\
         \x20                    injects when there is no predecessor — a pointer\n\
         \x20                    per frame, not a frame.\n\
         \x20 frames <id>        Dereference: print that frame whole.\n\
         \x20 lineage            What this terminal is attached to and how it knows:\n\
         \x20                    harness pid, window key, bound session, its frame.\n\
         \x20 attach <id>        Bind this terminal to a workstream by hand. The next\n\
         \x20                    session started here — including after /clear — is\n\
         \x20                    handed that frame whole, no guessing. --clear detaches.\n\
         \x20 distill <id>       Extract the spine and synthesize a frame; <id> is a\n\
         \x20                    session-id prefix from `list`.\n\
         \x20 grade <id|path>    Grade a frame against the golden (spec §5): per-section\n\
         \x20                    recall judged by the daemon model, Next/Invariants x2,\n\
         \x20                    -1 per hallucination; PASS at >=70% and zero hallucinated\n\
         \x20                    verification claims. Exit 0 pass / 1 fail / 2 error.\n\
         \x20                    --golden <path> overrides quality/session-frame.golden.md.\n\n\
         Options (distill):\n\
         \x20 --project <path>   Project working dir (default: cwd).\n\
         \x20 --dir <path>       Explicit transcript directory (overrides --project).\n\
         \x20 --no-llm           Stop after the spine (also the daemon-down fallback).\n\
         \x20 --model <id>       Chat model (default: primary).\n\
         \x20 --max-tokens <n>   Per-section answer budget (default 700).\n\
         \x20 --stdout           Print the frame instead of only writing it.\n\
         \x20 --force            Overwrite even a self-reported frame (default: a frame\n\
         \x20                    banked via session_state is never clobbered by distill).\n\n\
         Options (frames):\n\
         \x20 --repo/--branch    Scope the index.\n\
         \x20 --for-prompt <s>   Rank by overlap with this prompt.\n\
         \x20 --claim-window <s> Boot-hook exchange: return whoever last occupied this\n\
         \x20                    terminal, then record <s> as the occupant.\n\
         \x20 --no-lineage       Ignore window lineage; rank as if there were no\n\
         \x20                    predecessor (use to audit the ranker).\n\n\
         Output: ~/.svrnmesh/sessions/<session_id>/{{frame.md,spine.txt}}\n\
         \x20       ~/.svrnmesh/lineage/<pid>-<hash>.json  (window → session pointer)\n"
    );
}

fn find_transcript(dir: &Path, id: &str) -> Result<PathBuf, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("no transcripts at {} ({e})", dir.display()))?;
    let mut matches: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.starts_with(id))
                .unwrap_or(false)
        })
        .collect();
    match matches.len() {
        0 => Err(format!(
            "no session matching `{id}` in {} (see `svrn session list`)",
            dir.display()
        )),
        1 => Ok(matches.remove(0)),
        n => Err(format!(
            "`{id}` is ambiguous ({n} matches) — use more of the session id"
        )),
    }
}

fn run_list(dir: &Path) -> i32 {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("session: no transcripts at {} ({e})", dir.display());
            return 1;
        }
    };
    let mut rows: Vec<(i64, u64, String, String)> = Vec::new(); // (mtime, size, id, hint)
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        // Goal hint: the first kept user turn, first line only. Bounded scan
        // so listing stays cheap on multi-MB transcripts.
        let hint = first_user_turn_hint(&path).unwrap_or_default();
        rows.push((mtime, meta.len(), id, hint));
    }
    if rows.is_empty() {
        eprintln!("session: no transcripts in {}", dir.display());
        return 1;
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    println!(
        "{:<10} {:>9} {:>12}  first user turn",
        "session", "size", "modified"
    );
    println!("{}", "-".repeat(78));
    for (mtime, size, id, hint) in rows {
        println!(
            "{:<10} {:>8}k {:>12}  {}",
            short_session_id(&format!("{id}.jsonl")),
            size / 1024,
            mtime,
            cap_chars(&hint, 60).replace('\n', " ")
        );
    }
    0
}

/// First kept user turn's first line, scanning at most 300 records.
fn first_user_turn_hint(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().take(300) {
        let obj: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if obj.get("type").and_then(|t| t.as_str()) != Some("user")
            || obj.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false)
        {
            continue;
        }
        if let Some(serde_json::Value::String(s)) =
            obj.get("message").and_then(|m| m.get("content"))
        {
            let t = strip_reminders(s);
            if keep_user_turn(&t) {
                return t.lines().next().map(|l| l.to_string());
            }
        }
    }
    None
}

/// True when an existing frame at `path` carries `provenance: self-reported`
/// in its frontmatter. The session banked its own state via `session_state`;
/// that is the strong continuity path (spec §3) and distillation must not
/// clobber it — a distilled overwrite demotes the frame to rescue-only in
/// split-enforce even though the agent-authored content was better.
fn frame_is_self_reported(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return false;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("provenance:") {
            return v.trim() == "self-reported";
        }
    }
    false
}

async fn run_distill(dir: &Path, id: &str, flags: &BTreeMap<String, String>) -> i32 {
    let path = match find_transcript(dir, id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("session: {e}");
            return 1;
        }
    };
    let mut spine = match extract_spine(&path) {
        Some(s) => s,
        None => {
            eprintln!(
                "session: {} has no user/assistant activity to distill",
                path.display()
            );
            return 1;
        }
    };
    let dropped = cap_spine_middle(&mut spine, SPINE_CHAR_CAP);
    let spine_text = render_spine(&spine);

    let out_dir = match sessions_root() {
        Some(r) => r.join(&spine.session_id),
        None => {
            eprintln!("session: could not locate the home directory");
            return 1;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("session: create {} failed: {e}", out_dir.display());
        return 1;
    }
    let spine_path = out_dir.join("spine.txt");
    if let Err(e) = std::fs::write(&spine_path, &spine_text) {
        eprintln!("session: write {} failed: {e}", spine_path.display());
        return 1;
    }
    println!(
        "spine: {} ({} user turns, {} assistant texts{}, {} files edited) → {}",
        spine.session_id,
        spine.user_turns.len(),
        spine.assistant_texts.len(),
        if dropped > 0 {
            format!(" [{dropped} omitted for budget]")
        } else {
            String::new()
        },
        spine.edits.len(),
        spine_path.display()
    );

    if flags.contains_key("no-llm") {
        println!("--no-llm: stopping after the spine (no frame synthesized).");
        return 0;
    }

    let banked_frame = out_dir.join("frame.md");
    if !flags.contains_key("force") && frame_is_self_reported(&banked_frame) {
        println!(
            "frame: skipped — {} is self-reported (banked via session_state); \
             distill is the rescue path and does not overwrite it (--force to override)",
            banked_frame.display()
        );
        return 0;
    }

    let base_url = v1_url(DEFAULT_CLIENT_PORT)
        .trim_end_matches("/v1")
        .to_string();
    let model = flags
        .get("model")
        .cloned()
        .unwrap_or_else(|| "primary".to_string());
    let max_tokens: u32 = flags
        .get("max-tokens")
        .and_then(|s| s.parse().ok())
        .unwrap_or(700);

    println!(
        "synthesizing frame via daemon (model {model}; retrieval practice — one question per section)…"
    );
    // Local slots have small context windows; fit the *prompt* copy of the
    // spine to the slot, shrinking + refitting on the daemon's honest
    // "Prompt too long". The on-disk spine keeps full detail regardless.
    // The spine is the shared PREFIX of every per-section prompt (the
    // question is appended after it), so the daemon's prefix cache absorbs
    // the seven-call fan-out.
    let mut prompt_spine = spine.clone();
    let mut prompt_cap = PROMPT_CHAR_CAP_INITIAL;
    let sections: Vec<(&str, String)> = 'fit: loop {
        let trimmed = cap_spine_middle(&mut prompt_spine, prompt_cap);
        if trimmed > 0 {
            println!("  fitting spine to the model window: {trimmed} more assistant texts trimmed (cap {prompt_cap} chars)");
        }
        let spine_render = render_spine(&prompt_spine);
        let fit_n_user = prompt_spine.user_turns.len();
        let fit_n_assistant = prompt_spine.assistant_texts.len();
        // Mined sections sweep the FULL (on-disk) spine in chunks so
        // mid-session content — where invariants and dead ends live — is
        // never invisible to the model.
        let chunks = chunk_ranges(&spine, prompt_cap);
        if chunks.len() > 1 {
            println!(
                "  mined sections ({}) sweep the full spine in {} chunks",
                MINED_SECTIONS
                    .iter()
                    .map(|h| h.trim_start_matches("## "))
                    .collect::<Vec<_>>()
                    .join(", "),
                chunks.len()
            );
        }
        let mut acc: Vec<(&str, String)> = Vec::new();
        for heading in FRAME_SECTIONS {
            // Working set is assembled deterministically below, never asked.
            if *heading == "## Working set" {
                continue;
            }
            // (prompt, n_user, n_assistant) per call for this section.
            let calls: Vec<(String, usize, usize)> = if MINED_SECTIONS.contains(heading) {
                chunks
                    .iter()
                    .enumerate()
                    .map(|(k, (lo, hi))| {
                        (
                            format!(
                                "{}\nQUESTION (for the {heading} section):\n{}",
                                render_spine_chunk(&spine, *lo, *hi, k + 1, chunks.len()),
                                section_question(heading)
                            ),
                            spine.user_turns.len(),
                            spine.assistant_texts.len(),
                        )
                    })
                    .collect()
            } else {
                vec![(
                    format!(
                        "{spine_render}\nQUESTION (for the {heading} section):\n{}",
                        section_question(heading)
                    ),
                    fit_n_user,
                    fit_n_assistant,
                )]
            };
            let mut kept: Vec<String> = Vec::new();
            let mut dropped = 0usize;
            for (user, n_user, n_assistant) in &calls {
                // Citation compliance is per-call flaky on local models (MoE
                // sampling): one re-ask when every bullet came back uncited
                // recovers the section instead of silently emitting "none
                // recorded" over real content.
                let mut attempts = 0;
                let parsed = loop {
                    attempts += 1;
                    match daemon_complete(
                        &base_url,
                        &model,
                        retrieval_system_prompt(),
                        user,
                        max_tokens,
                    )
                    .await
                    {
                        Ok(reply) => {
                            let parsed = parse_cited_bullets(&reply, *n_user, *n_assistant);
                            if parsed.kept.is_empty() && parsed.dropped > 0 && attempts == 1 {
                                println!(
                                    "  {heading}: {} bullet(s) all uncited — re-asking once",
                                    parsed.dropped
                                );
                                continue;
                            }
                            break Ok(parsed);
                        }
                        Err(e) => break Err(e),
                    }
                };
                match parsed {
                    Ok(parsed) => {
                        kept.extend(parsed.kept);
                        dropped += parsed.dropped;
                    }
                    Err(e) if e.contains("Prompt too long") && prompt_cap > PROMPT_CHAR_CAP_MIN => {
                        prompt_cap /= 2;
                        println!("  model window too small — refitting at {prompt_cap} chars");
                        continue 'fit;
                    }
                    Err(e) => {
                        eprintln!(
                            "session: frame synthesis failed on {heading}: {e}\n\
                             The spine is written at {} — re-run once the daemon is up,\n\
                             or hand-write the frame from the spine.",
                            spine_path.display()
                        );
                        return 1;
                    }
                }
            }
            let kept = dedupe_bullets(kept);
            let body = if kept.is_empty() {
                if dropped > 0 {
                    println!(
                        "  {heading}: all {dropped} bullet(s) dropped — no valid spine citation"
                    );
                } else {
                    println!("  {heading}: none recorded");
                }
                "none recorded\n".to_string()
            } else {
                println!(
                    "  {heading}: {} bullet(s){}",
                    kept.len(),
                    if dropped > 0 {
                        format!(", {dropped} dropped uncited")
                    } else {
                        String::new()
                    }
                );
                kept.iter().map(|b| format!("- {b}\n")).collect()
            };
            acc.push((heading, body));
        }
        break acc;
    };

    let head = head_at_end_of(&spine.cwd, &spine.last_ts);
    let mut body = String::new();
    for heading in FRAME_SECTIONS {
        if *heading == "## Working set" {
            body.push_str(render_working_set(&spine).trim_end());
        } else {
            let sec = sections
                .iter()
                .find(|(h, _)| h == heading)
                .map(|(_, b)| b.as_str())
                .unwrap_or("none recorded");
            body.push_str(heading);
            body.push('\n');
            body.push_str(sec.trim_end());
        }
        body.push_str("\n\n");
    }
    let body = body.trim().to_string();
    let frame = format!(
        "{}\n{}\n",
        render_frontmatter(&spine, &head, "distilled"),
        body.trim()
    );
    let frame_path = out_dir.join("frame.md");
    if let Err(e) = std::fs::write(&frame_path, &frame) {
        eprintln!("session: write {} failed: {e}", frame_path.display());
        return 1;
    }

    let missing = missing_sections(&body);
    if flags.contains_key("stdout") {
        println!("\n{frame}");
    }
    if missing.is_empty() {
        println!(
            "frame: valid (all {} sections) → {}",
            FRAME_SECTIONS.len(),
            frame_path.display()
        );
        0
    } else {
        eprintln!(
            "frame: INVALID — missing sections: {} → {} (written anyway; the synthesis\n\
             prompt may need tuning against quality/session-frame.golden.md)",
            missing.join(", "),
            frame_path.display()
        );
        1
    }
}

pub async fn run(args: &[String]) -> i32 {
    let mut sub: Option<String> = None;
    let mut id: Option<String> = None;
    let mut flags: BTreeMap<String, String> = BTreeMap::new();
    let mut project: Option<String> = None;
    let mut dir: Option<String> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" | "help" => {
                print_help();
                return 0;
            }
            "--project" => project = it.next().cloned(),
            "--dir" => dir = it.next().cloned(),
            "--no-llm" | "--stdout" | "--force" | "--json" | "--clear" | "--no-lineage" => {
                flags.insert(arg.trim_start_matches('-').to_string(), String::new());
            }
            "--claim-window" | "--self" => {
                if let Some(v) = it.next() {
                    flags.insert(arg.trim_start_matches("--").to_string(), v.clone());
                }
            }
            "--repo" | "--branch" | "--for-prompt" | "--limit" | "--max-age-days" => {
                if let Some(v) = it.next() {
                    flags.insert(arg.trim_start_matches("--").to_string(), v.clone());
                }
            }
            "--model" | "--max-tokens" | "--golden" | "--url" => {
                if let Some(v) = it.next() {
                    flags.insert(arg.trim_start_matches('-').to_string(), v.clone());
                }
            }
            other if !other.starts_with('-') && sub.is_none() => sub = Some(other.to_string()),
            other if !other.starts_with('-') && id.is_none() => id = Some(other.to_string()),
            other => {
                eprintln!("session: unknown argument `{other}` (try --help)");
                return 2;
            }
        }
    }

    // `frames` reads only ~/.svrnmesh/sessions — it must work with no
    // transcripts present at all (a fresh machine, or a hook running outside
    // a project), so it dispatches BEFORE the transcript-dir resolution that
    // the transcript-reading subcommands need.
    match sub.as_deref() {
        Some("frames") => return run_frames(id.as_deref(), &flags),
        Some("lineage") => return run_lineage(&flags),
        Some("attach") => return run_attach(id.as_deref(), &flags),
        _ => {}
    }

    let target_dir = match resolve_transcript_dir(project.as_deref(), dir.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("session: {e}");
            return 2;
        }
    };

    match sub.as_deref() {
        Some("list") => run_list(&target_dir),
        Some("distill") => match id {
            Some(id) => run_distill(&target_dir, &id, &flags).await,
            None => {
                eprintln!("session: distill needs a session id (see `svrn session list`)");
                2
            }
        },
        Some("grade") => match id {
            Some(id) => run_grade(&id, &flags).await,
            None => {
                eprintln!("session: grade needs a frame path or session id");
                2
            }
        },
        _ => {
            print_help();
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boot advisory must stay QUIET for a healthy handoff. A signal
    /// that fires on every boot is a signal agents learn to skip.
    #[test]
    fn a_healthy_handoff_produces_no_boot_advisory() {
        let clean = Inherited {
            carried: 0,
            worst_frames: 0,
            objective_sessions: 2,
            next_items: 5,
        };
        assert!(clean.advice().is_none());
    }

    /// Two independent triggers: a recopied backlog, and an objective that
    /// has stood so long it is worth re-examining even with a clean Next.
    #[test]
    fn the_boot_advisory_names_the_count_and_the_worst_age() {
        let stale = Inherited {
            carried: 2,
            worst_frames: 3,
            objective_sessions: 3,
            next_items: 6,
        };
        let msg = stale.advice().expect("carried items must speak up");
        assert!(msg.contains("2 of 6"), "names the proportion: {msg}");
        assert!(msg.contains("3 frames"), "names the worst age: {msg}");
        assert!(
            !msg.contains("stood unchanged"),
            "3 sessions is not yet long enough to nag about the objective: {msg}"
        );

        let long = Inherited {
            carried: 0,
            worst_frames: 0,
            objective_sessions: 6,
            next_items: 3,
        };
        let msg = long
            .advice()
            .expect("a long-standing objective speaks up alone");
        assert!(msg.contains("6 sessions"), "{msg}");
    }

    /// The chain walk must survive the states it will actually meet: a
    /// legacy frame with no `predecessor:`, and a hand-edited cycle.
    #[test]
    fn the_ancestor_walk_terminates_on_legacy_frames_and_cycles() {
        let tmp = std::env::temp_dir().join(format!("anc_walk_{}", std::process::id()));
        let write = |id: &str, body: &str| {
            let d = tmp.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("frame.md"), body).unwrap();
        };
        // a → b (stamped) → c (legacy, no stamp) → stop.
        write(
            "a",
            "---\nsession_id: a\npredecessor: b\n---\n\n## Next\n\n- x\n",
        );
        write(
            "b",
            "---\nsession_id: b\npredecessor: c\n---\n\n## Next\n\n- x\n",
        );
        write("c", "---\nsession_id: c\n---\n\n## Next\n\n- x\n");
        assert_eq!(
            ancestor_texts(
                &tmp,
                "a",
                &std::fs::read_to_string(tmp.join("a").join("frame.md")).unwrap()
            )
            .len(),
            2,
            "walks to the legacy frame and stops there"
        );

        write(
            "p",
            "---\nsession_id: p\npredecessor: q\n---\n\n## Next\n\n- y\n",
        );
        write(
            "q",
            "---\nsession_id: q\npredecessor: p\n---\n\n## Next\n\n- y\n",
        );
        let p_text = std::fs::read_to_string(tmp.join("p").join("frame.md")).unwrap();
        assert_eq!(
            ancestor_texts(&tmp, "p", &p_text).len(),
            1,
            "the cycle is cut"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn frontmatter_value_reads_only_the_leading_block() {
        let f = "---\nsession_id: abc\npredecessor: xyz\n---\n\n## Goal\n\npredecessor: lie\n";
        assert_eq!(frontmatter_value(f, "predecessor").as_deref(), Some("xyz"));
        assert_eq!(frontmatter_value(f, "missing"), None);
        assert_eq!(
            frontmatter_value("no frontmatter here", "predecessor"),
            None
        );
    }

    /// The boot hand-off writes the sidecar the frame writer reads. The
    /// two crates cannot see each other, so the FILE is the contract —
    /// its name and its bare-id contents are load-bearing on both sides
    /// (`sovereign-tools::code::session_state::PREDECESSOR_FILE`).
    #[test]
    fn the_predecessor_handoff_writes_a_bare_id_the_writer_can_read() {
        let tmp = std::env::temp_dir().join(format!("pred_handoff_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        record_predecessor(&tmp, "child-sess", "parent-sess");
        assert_eq!(
            std::fs::read_to_string(tmp.join("child-sess").join("predecessor")).unwrap(),
            "parent-sess",
            "a bare id, no newline or decoration — the reader only trims"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Every failure path is silent by construction: a lineage hop is an
    /// advisory signal, never a reason for a boot hook to error.
    #[test]
    fn the_predecessor_handoff_refuses_nonsense_without_failing() {
        let tmp = std::env::temp_dir().join(format!("pred_bad_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        record_predecessor(&tmp, "same", "same");
        assert!(
            !tmp.join("same").exists(),
            "a session is not its own parent"
        );
        record_predecessor(&tmp, "../escape", "p");
        record_predecessor(&tmp, "c", "../escape");
        assert!(!tmp.join("c").exists(), "path separators are refused");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn section_body_extracts_between_headings() {
        let f = "---\nx: y\n---\n\n## Goal\nShip it.\n\n## State\n- done\n- more\n\n## Next\nnone recorded\n";
        assert_eq!(section_body(f, "## Goal").as_deref(), Some("Ship it."));
        assert_eq!(
            section_body(f, "## State").as_deref(),
            Some("- done\n- more")
        );
        assert_eq!(section_body(f, "## Missing"), None);
    }

    #[test]
    fn golden_items_bullets_wrapping_and_fallbacks() {
        // Bullets with a wrapped continuation line join into one item.
        let items = golden_items("- first item\n  continues here\n- second\n1. third numbered");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "first item continues here");
        assert_eq!(items[2], "third numbered");
        // Bullet-less prose = one item; empty / none recorded = zero.
        assert_eq!(golden_items("just a paragraph").len(), 1);
        assert!(golden_items("").is_empty());
        assert!(golden_items("none recorded").is_empty());
    }

    #[test]
    fn first_json_object_tolerates_prose_wrapping() {
        let v =
            first_json_object("Sure! {\"captured\": [1, 2], \"hallucinations\": []} done").unwrap();
        assert_eq!(v["captured"][1], 2);
        assert!(first_json_object("no json here").is_none());
    }

    #[test]
    fn strip_reminders_removes_blocks_keeps_text() {
        let s = "before <system-reminder>noise</system-reminder> after";
        assert_eq!(strip_reminders(s), "before  after");
        assert_eq!(strip_reminders("no blocks"), "no blocks");
        // Unterminated block drops the tail rather than leaking it.
        assert_eq!(strip_reminders("keep <system-reminder>oops"), "keep");
    }

    #[test]
    fn keep_user_turn_filters_harness_noise() {
        assert!(keep_user_turn("Fix the P0 first"));
        assert!(!keep_user_turn(""));
        assert!(!keep_user_turn(
            "<local-command-stdout>x</local-command-stdout>"
        ));
        assert!(!keep_user_turn("<command-name>/clear</command-name>"));
        assert!(!keep_user_turn(
            "<task-notification>done</task-notification>"
        ));
    }

    fn synthetic_transcript() -> String {
        [
            // Real user turn (string content) with an embedded reminder.
            r#"{"type":"user","timestamp":"2026-07-23T05:00:00Z","cwd":"/tmp/demo-repo","gitBranch":"main","message":{"role":"user","content":"Fix the wipe bug <system-reminder>ctx</system-reminder> please"}}"#,
            // Tool-result carrier — must NOT count as a user turn.
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"big output"}]}}"#,
            // Assistant: text + an Edit tool_use.
            r#"{"type":"assistant","timestamp":"2026-07-23T06:00:00Z","message":{"role":"assistant","model":"claude-opus-4-8","content":[{"type":"text","text":"Found the bug at export.rs:347."},{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/tmp/demo-repo/src/export.rs"}}]}}"#,
            // Local-command echo — dropped.
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>"}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn extract_spine_keeps_signal_drops_noise() {
        let dir = std::env::temp_dir().join(format!("session_cmd_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abcd1234-session.jsonl");
        std::fs::write(&path, synthetic_transcript()).unwrap();

        let spine = extract_spine(&path).expect("has activity");
        assert_eq!(spine.session_id, "abcd1234-session");
        assert_eq!(spine.model, "claude-opus-4-8");
        assert_eq!(spine.cwd, "/tmp/demo-repo");
        assert_eq!(spine.git_branch, "main");
        assert_eq!(spine.first_ts, "2026-07-23T05:00:00Z");
        assert_eq!(spine.last_ts, "2026-07-23T06:00:00Z");
        // One real user turn (reminder stripped, carrier + command echo dropped).
        assert_eq!(spine.user_turns.len(), 1);
        assert_eq!(spine.user_turns[0], "Fix the wipe bug  please");
        assert_eq!(spine.assistant_texts.len(), 1);
        assert_eq!(spine.edits.get("/tmp/demo-repo/src/export.rs"), Some(&1));
        assert_eq!(spine.tool_calls.get("Edit"), Some(&1));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cap_spine_middle_drops_middle_keeps_ends() {
        let mut spine = Spine {
            assistant_texts: (0..40)
                .map(|i| format!("text-{i} {}", "x".repeat(400)))
                .collect(),
            ..Default::default()
        };
        let dropped = cap_spine_middle(&mut spine, 6_000);
        assert!(dropped > 0, "cap should have dropped something");
        let joined = spine.assistant_texts.join("|");
        assert!(joined.contains("text-0 "), "first text kept");
        assert!(joined.contains("text-39 "), "last text kept");
        assert!(joined.contains("omitted for budget"));
        assert!(render_spine(&spine).len() <= 7_000);
    }

    #[test]
    fn frontmatter_is_deterministic_and_schema_shaped() {
        let spine = Spine {
            session_id: "abc".into(),
            model: "claude-opus-4-8".into(),
            cwd: "/Users/x/dev/commonwealth-ai".into(),
            git_branch: "main".into(),
            first_ts: "2026-07-23T05:00:00Z".into(),
            last_ts: "2026-07-23T17:35:00Z".into(),
            ..Default::default()
        };
        let fm = render_frontmatter(&spine, "71d7ac20", "distilled");
        assert!(fm.starts_with("---\nschema: session-frame/v1\n"));
        assert!(fm.contains("session_id: abc\n"));
        assert!(fm.contains("repo: commonwealth-ai\n"));
        assert!(fm.contains("head_at_end: 71d7ac20\n"));
        assert!(fm.contains("provenance: distilled\n"));
        assert!(fm.trim_end().ends_with("---"));
    }

    #[test]
    fn self_reported_frame_is_detected_and_others_are_not() {
        let dir = std::env::temp_dir().join(format!("session_frame_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let banked = dir.join("banked.md");
        std::fs::write(
            &banked,
            "---\nschema: session-frame/v1\nsession_id: s1\nprovenance: self-reported\n---\n\n## Goal\nx\n",
        )
        .unwrap();
        assert!(frame_is_self_reported(&banked));

        let distilled = dir.join("distilled.md");
        std::fs::write(
            &distilled,
            "---\nschema: session-frame/v1\nsession_id: s1\nprovenance: distilled\n---\n\n## Goal\nx\n",
        )
        .unwrap();
        assert!(!frame_is_self_reported(&distilled));

        // provenance mentioned in the body must not count — frontmatter only
        let body_only = dir.join("body.md");
        std::fs::write(
            &body_only,
            "---\nschema: session-frame/v1\nsession_id: s1\n---\n\nprovenance: self-reported\n",
        )
        .unwrap();
        assert!(!frame_is_self_reported(&body_only));

        assert!(!frame_is_self_reported(&dir.join("missing.md")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn working_set_is_deterministic_and_relative() {
        let mut spine = Spine {
            cwd: "/Users/x/dev/repo".into(),
            ..Default::default()
        };
        spine.edits.insert("/Users/x/dev/repo/src/a.rs".into(), 5);
        spine.edits.insert("/elsewhere/b.rs".into(), 1);
        let ws = render_working_set(&spine);
        assert!(ws.starts_with("## Working set\n"));
        assert!(ws.contains("- src/a.rs (5 edits)"));
        assert!(ws.contains("- /elsewhere/b.rs (1 edits)")); // outside cwd stays absolute
    }

    #[test]
    fn chunks_cover_all_items_with_global_ids() {
        let spine = Spine {
            user_turns: vec!["fix the bug".into()],
            assistant_texts: (0..30)
                .map(|i| format!("text-{i} {}", "x".repeat(900)))
                .collect(),
            ..Default::default()
        };
        let chunks = chunk_ranges(&spine, 12_000);
        assert!(chunks.len() > 1, "30x900 chars must not fit one 12k chunk");
        // Full coverage, no overlap, in order.
        assert_eq!(chunks[0].0, 0);
        assert_eq!(chunks.last().unwrap().1, 30);
        for w in chunks.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
        // Global ids: the second chunk's first item keeps its true id.
        let (lo, hi) = chunks[1];
        let rendered = render_spine_chunk(&spine, lo, hi, 2, chunks.len());
        assert!(rendered.contains(&format!("--- [a{}] assistant ---", lo + 1)));
        assert!(
            rendered.contains("[u1] user turn"),
            "user turns ride every chunk"
        );
        assert!(rendered.contains(&format!("(part 2/{})", chunks.len())));

        // A tiny spine is one chunk covering everything.
        let small = Spine {
            assistant_texts: vec!["short".into()],
            ..Default::default()
        };
        assert_eq!(chunk_ranges(&small, 48_000), vec![(0, 1)]);
    }

    #[test]
    fn dedupe_bullets_collapses_punctuation_variants() {
        let out = dedupe_bullets(vec![
            "Suite: 7928 pass / 0 fail".into(),
            "suite 7928 pass, 0 fail".into(),
            "different fact".into(),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn cited_bullets_kept_stripped_and_uncited_dropped() {
        let reply = "Here are the facts:\n\
                     - suite went 7888 to 7928 tests [a12]\n\
                     - uncited claim that must be dropped\n\
                     - wrapped bullet about the export\n  fail-closed guard [u2,a30]\n\
                     1. numbered bullet with mid-cite [a3] and a tail\n\
                     - bad range citation [a999]\n";
        let parsed = parse_cited_bullets(reply, 5, 40);
        assert_eq!(parsed.kept.len(), 3);
        assert_eq!(parsed.dropped, 2);
        assert_eq!(parsed.kept[0], "suite went 7888 to 7928 tests");
        assert_eq!(
            parsed.kept[1],
            "wrapped bullet about the export fail-closed guard"
        );
        assert_eq!(parsed.kept[2], "numbered bullet with mid-cite and a tail");
    }

    #[test]
    fn cited_bullets_preserve_noncitation_brackets() {
        let parsed = parse_cited_bullets("- keep [sic] the brackets [u1]", 2, 2);
        assert_eq!(parsed.kept, vec!["keep [sic] the brackets"]);
        // A citation-shaped token out of range is not a citation.
        let parsed = parse_cited_bullets("- only invalid [u3]", 2, 2);
        assert!(parsed.kept.is_empty());
        assert_eq!(parsed.dropped, 1);
        // Prose outside bullets is not a bullet.
        let parsed = parse_cited_bullets("no bullets here [u1]", 2, 2);
        assert!(parsed.kept.is_empty());
        assert_eq!(parsed.dropped, 0);
    }

    #[test]
    fn missing_sections_validates_the_contract() {
        let full = FRAME_SECTIONS
            .iter()
            .map(|s| format!("{s}\ncontent\n"))
            .collect::<String>();
        assert!(missing_sections(&full).is_empty());

        let partial = "## Goal\nx\n## State\ny\n";
        let missing = missing_sections(partial);
        assert!(missing.contains(&"## Next"));
        assert!(missing.contains(&"## Verification"));
        assert_eq!(missing.len(), FRAME_SECTIONS.len() - 2);
    }

    // ── Frame index (E5 Phase 2) ─────────────────────────────────────────

    #[test]
    fn frontmatter_field_stops_at_the_closing_fence() {
        let f =
            "---\nrepo: commonwealth-ai\nbranch: main\n---\n\n## Goal\nbranch: not-frontmatter\n";
        assert_eq!(
            frontmatter_field(f, "repo").as_deref(),
            Some("commonwealth-ai")
        );
        // A body line shaped like frontmatter is NOT frontmatter — the scan
        // must stop at the closing fence, or `## Goal` prose could rewrite
        // the branch the ranker filters on.
        assert_eq!(frontmatter_field(f, "branch").as_deref(), Some("main"));
        assert_eq!(frontmatter_field(f, "missing"), None);
        // No frontmatter at all is not an error, just absent.
        assert_eq!(frontmatter_field("## Goal\nx\n", "repo"), None);
    }

    #[test]
    fn goal_line_takes_first_nonempty_and_caps() {
        let f = "---\nx: y\n---\n\n## Goal\n\nShip the frame index.\nSecond line ignored.\n\n## State\nz\n";
        assert_eq!(goal_line(f, 96), "Ship the frame index.");
        // Capping appends an ellipsis rather than wrapping the index line.
        let capped = goal_line(f, 8);
        assert_eq!(capped, "Ship the…");
        // A frame with no Goal section yields an empty line, not a panic.
        assert_eq!(goal_line("## State\nonly\n", 96), "");
    }

    #[test]
    fn distinctive_terms_keeps_identifier_shapes_only() {
        let terms = distinctive_terms(
            "fix session_cmd.rs and the boot hook because regression tests broke",
            20,
        );
        assert!(terms.contains(&"session_cmd.rs".to_string()));
        // Short, generic words carry no evidence and must not inflate overlap.
        assert!(!terms.iter().any(|t| t == "and"));
        assert!(!terms.iter().any(|t| t == "the"));
        assert!(!terms.iter().any(|t| t == "boot"));
        assert!(!terms.iter().any(|t| t == "tests"));
        // Long words qualify even without a code shape.
        assert!(terms.contains(&"regression".to_string()), "{terms:?}");
        // Dedup is case-insensitive.
        let dup = distinctive_terms("session_cmd SESSION_CMD session_cmd", 20);
        assert_eq!(dup.len(), 1);
    }

    fn entry(
        id: &str,
        branch_match: bool,
        in_flight: bool,
        overlap: usize,
        age_s: u64,
    ) -> FrameEntry {
        FrameEntry {
            session_id: id.to_string(),
            path: PathBuf::from("/tmp").join(id).join("frame.md"),
            repo: "commonwealth-ai".into(),
            branch: "main".into(),
            status: if in_flight { "active" } else { "completed" }.into(),
            provenance: "self-reported".into(),
            age_s,
            goal: "g".into(),
            next_items: 1,
            chars: 100,
            same_window: false,
            branch_match,
            in_flight,
            prompt_overlap: overlap,
            overlap_terms: Vec::new(),
        }
    }

    /// An abandoned frame is not a handoff candidate and must not cost
    /// every future successor a line of context to read and reject.
    /// A completed one MUST survive — it is the normal good handoff.
    #[test]
    fn retain_listable_drops_abandoned_but_keeps_completed() {
        let mut v = vec![
            entry("done", true, false, 0, 60),
            entry("live", true, true, 0, 60),
            entry("gave-up", true, true, 0, 60),
        ];
        v[2].status = "abandoned".into();
        retain_listable(&mut v);
        let ids: Vec<&str> = v.iter().map(|f| f.session_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["done", "live"],
            "abandoned dropped, completed kept"
        );
    }

    /// Never hide this terminal's own predecessor, whatever its status —
    /// a silent gap where the successor's own lineage should be is worse
    /// than one line saying the donor gave up.
    #[test]
    fn retain_listable_never_hides_the_same_window_predecessor() {
        let mut v = vec![entry("mine", true, true, 0, 60)];
        v[0].status = "abandoned".into();
        v[0].same_window = true;
        retain_listable(&mut v);
        assert_eq!(v.len(), 1, "same-window frame survives even when abandoned");
    }

    #[test]
    fn rank_frames_is_lexicographic_branch_overlap_recency() {
        // Newest frame LOSES to an older one on the caller's branch — the
        // whole point of E5 R1: newest-mtime is the bug being fixed.
        let mut v = vec![
            entry("newest-wrong-branch", false, true, 5, 60),
            entry("older-right-branch", true, false, 0, 9_000),
        ];
        rank_frames(&mut v);
        assert_eq!(v[0].session_id, "older-right-branch");

        // Then prompt overlap — the signal SessionStart could never have.
        let mut v = vec![
            entry("recent-unrelated", true, true, 0, 60),
            entry("older-on-topic", true, true, 3, 9_000),
        ];
        rank_frames(&mut v);
        assert_eq!(v[0].session_id, "older-on-topic");

        // With those tied, recency decides — the old behaviour, kept as the
        // fallback rather than the rule.
        let mut v = vec![
            entry("old", true, true, 1, 9_000),
            entry("new", true, true, 1, 60),
        ];
        rank_frames(&mut v);
        assert_eq!(v[0].session_id, "new");

        // REGRESSION (observed against the live store, 2026-07-26): a
        // COMPLETED frame must not be pushed below in-flight frames. The
        // successor's correct handoff was `completed`, and ranking in-flight
        // first dropped it past the index cut entirely.
        let mut v = vec![
            entry("stale-in-flight", true, true, 0, 90_000),
            entry("fresh-completed", true, false, 0, 60),
        ];
        rank_frames(&mut v);
        assert_eq!(v[0].session_id, "fresh-completed");
    }

    #[test]
    fn same_window_outranks_every_heuristic() {
        // THE MEASURED FAILURE, as a test (2026-07-27). Session 963fc519 was
        // the `/clear` successor of a05e2bd1 in the same terminal, and was
        // handed an unrelated frame that won on prompt overlap + recency. The
        // predecessor must win even when it loses every other signal: wrong
        // branch, zero overlap, and much older.
        let mut predecessor = entry("a05e2bd1", false, true, 0, 90_000);
        predecessor.same_window = true;
        let mut v = vec![entry("ad5fee8c", true, true, 9, 60), predecessor];
        rank_frames(&mut v);
        assert_eq!(v[0].session_id, "a05e2bd1");

        // And it is a tiebreak, not a bulldozer: with no window signal the
        // established order is untouched.
        let mut v = vec![
            entry("older", true, true, 0, 90_000),
            entry("newer", true, true, 0, 60),
        ];
        rank_frames(&mut v);
        assert_eq!(v[0].session_id, "newer");
    }

    #[test]
    fn generic_prose_terms_cast_no_vote() {
        // Every one of these decided a real selection on this machine, purely
        // by being >= 8 chars with no code shape. A frame containing the word
        // "everything" is not evidence of anything.
        for t in [
            "everything",
            "continue",
            "solution",
            "containing",
            "production",
            "session",
        ] {
            assert!(is_generic_term(t), "{t} should not vote");
        }
        // Code shapes survive at any length — this signal exists for them.
        for t in [
            "session_cmd",
            "frame.md",
            "sovereign/docs",
            "SplitInferenceProvider",
            "rpc-warm",
        ] {
            assert!(!is_generic_term(t), "{t} must keep voting");
        }
        let terms: Vec<String> = ["everything", "session_cmd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hits = overlap_with("we changed everything in session_cmd today", &terms);
        assert_eq!(hits, vec!["session_cmd".to_string()]);
    }

    #[test]
    fn render_index_marks_the_frame_from_this_window() {
        let mut v = vec![entry("aaaaaaaa-1111", true, true, 0, 120)];
        v[0].same_window = true;
        let out = render_index(&v, 8);
        assert!(out.contains("THIS WINDOW"), "{out}");
        // And says nothing when there is no lineage to report.
        let plain = render_index(&[entry("bbbbbbbb-2222", true, true, 0, 120)], 8);
        assert!(!plain.contains("THIS WINDOW"), "{plain}");
    }

    #[test]
    fn load_frames_flags_the_predecessor_and_nothing_else() {
        let tmp = std::env::temp_dir().join(format!("svrn-pred-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for id in ["pred", "other"] {
            let d = tmp.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("frame.md"),
                "---\nrepo: r\nbranch: main\nstatus: active\n---\n\n## Goal\n\ng\n",
            )
            .unwrap();
        }
        let frames = load_frames(&tmp, 14, Some("r"), Some("main"), None, Some("pred"));
        assert_eq!(frames[0].session_id, "pred");
        assert!(frames[0].same_window);
        assert!(frames.iter().filter(|f| f.same_window).count() == 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn in_flight_reads_the_status_stem_not_an_exact_string() {
        let tmp = std::env::temp_dir().join(format!("svrn-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for (id, status) in [
            ("a", "in-flight"),
            ("b", "completed"),
            // Real value from the live store — an exact-match predicate read
            // this as in-flight.
            ("c", "work-complete-uncommitted"),
        ] {
            let d = tmp.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("frame.md"),
                format!("---\nrepo: r\nbranch: main\nstatus: {status}\n---\n\n## Goal\n\ng\n"),
            )
            .unwrap();
        }
        let frames = load_frames(&tmp, 14, Some("r"), Some("main"), None, None);
        let by = |id: &str| {
            frames
                .iter()
                .find(|f| f.session_id == id)
                .unwrap()
                .in_flight
        };
        assert!(by("a"));
        assert!(!by("b"));
        assert!(!by("c"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_index_is_one_line_per_frame_and_names_the_deref_verb() {
        let v = vec![
            entry("aaaaaaaa-1111", true, true, 0, 120),
            entry("bbbbbbbb-2222", true, false, 0, 7_200),
        ];
        let out = render_index(&v, 8);
        assert!(out.contains("sovereign session frames <id>"), "{out}");
        // Two frames = two bullets, and the count is honest.
        assert_eq!(out.lines().filter(|l| l.starts_with("- ")).count(), 2);
        assert!(out.contains("2 live"), "{out}");
        // A limit below the candidate count says so rather than silently
        // truncating — a silent cap reads as "that's all there is".
        let capped = render_index(&v, 1);
        assert!(capped.contains("1 shown"), "{capped}");
        assert_eq!(capped.lines().filter(|l| l.starts_with("- ")).count(), 1);
        assert!(render_index(&[], 8).is_empty());
    }

    #[test]
    fn load_frames_filters_by_repo_and_ranks() {
        let tmp = std::env::temp_dir().join(format!("svrn-frames-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |id: &str, repo: &str, branch: &str, status: &str, goal: &str| {
            let d = tmp.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("frame.md"),
                format!(
                    "---\nsession_id: {id}\nrepo: {repo}\nbranch: {branch}\nstatus: {status}\nprovenance: self-reported\n---\n\n## Goal\n\n{goal}\n\n## Next\n\n- one\n- two\n"
                ),
            )
            .unwrap();
        };
        write(
            "aaa",
            "commonwealth-ai",
            "main",
            "completed",
            "Fix the frame index",
        );
        write("bbb", "other-repo", "main", "active", "Unrelated repo work");
        write(
            "ccc",
            "commonwealth-ai",
            "side-branch",
            "active",
            "Side branch work on session_cmd.rs",
        );

        let frames = load_frames(&tmp, 14, Some("commonwealth-ai"), Some("main"), None, None);
        // The other repo is filtered out entirely, not merely down-ranked.
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.repo == "commonwealth-ai"));
        // Branch match is the first key, so the on-branch COMPLETED frame
        // outranks the off-branch in-flight one.
        assert_eq!(frames[0].session_id, "aaa");
        assert!(frames[0].branch_match);
        assert_eq!(frames[0].next_items, 2);
        assert_eq!(frames[0].goal, "Fix the frame index");

        // The prompt names an identifier the side-branch frame mentions.
        // Overlap is deliberately identifier-shaped: prose words like "work"
        // or "branch" carry no evidence and must not move the ranking.
        let frames = load_frames(
            &tmp,
            14,
            Some("commonwealth-ai"),
            Some("side-branch"),
            Some("continue the session_cmd.rs work"),
            None,
        );
        assert_eq!(frames[0].session_id, "ccc");
        assert!(frames[0].prompt_overlap > 0);
        let prose_only = load_frames(
            &tmp,
            14,
            Some("commonwealth-ai"),
            Some("side-branch"),
            Some("continue the work please"),
            None,
        );
        assert_eq!(prose_only[0].prompt_overlap, 0);

        // The age window is real: backdate one frame past it and it drops out
        // (the boot hook relies on this to stop resurrecting dead threads).
        let aged = tmp.join("aaa").join("frame.md");
        std::fs::File::options()
            .write(true)
            .open(&aged)
            .unwrap()
            .set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(20 * 86_400),
            )
            .unwrap();
        let recent = load_frames(&tmp, 14, Some("commonwealth-ai"), Some("main"), None, None);
        assert!(recent.iter().all(|f| f.session_id != "aaa"), "{recent:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
