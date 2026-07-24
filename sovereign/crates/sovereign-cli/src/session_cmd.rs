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
//! schema-v1 frame via one local-daemon chat call.
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
use crate::util::urls::{v1_url, DEFAULT_CLIENT_PORT};

/// The eight body sections of a session-frame/v1, in contract order
/// (`SESSION_CONTINUITY.md §2`). Validation and grading key off this list.
pub(crate) const FRAME_SECTIONS: &[&str] = &[
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
                            let name =
                                b.get("name").and_then(|n| n.as_str()).unwrap_or("?");
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

    out.push_str(&format!("\nUSER TURNS ({}):\n", spine.user_turns.len()));
    for (i, t) in spine.user_turns.iter().enumerate() {
        out.push_str(&format!("\n--- user {} ---\n{t}\n", i + 1));
    }
    out.push_str(&format!(
        "\nASSISTANT TEXTS ({}):\n",
        spine.assistant_texts.len()
    ));
    for (i, t) in spine.assistant_texts.iter().enumerate() {
        out.push_str(&format!("\n--- assistant {} ---\n{t}\n", i + 1));
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
        spine
            .assistant_texts
            .insert(mid, format!("[… {dropped} mid-session assistant texts omitted for budget …]"));
    }
    dropped
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
            if sha.is_empty() { "unknown".to_string() } else { sha }
        }
        _ => "unknown".to_string(),
    }
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

/// Splice the deterministic Working set over whatever the model emitted for
/// that section (or append it if the model dropped the heading).
pub(crate) fn replace_working_set(body: &str, working_set: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.iter().position(|l| l.trim() == "## Working set");
    match start {
        None => format!("{}\n\n{}", body.trim_end(), working_set.trim_end()),
        Some(s) => {
            let end = lines[s + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with("## "))
                .map(|off| s + 1 + off)
                .unwrap_or(lines.len());
            let mut out = lines[..s].join("\n");
            out.push('\n');
            out.push_str(working_set.trim_end());
            out.push('\n');
            if end < lines.len() {
                out.push_str(&lines[end..].join("\n"));
            }
            out
        }
    }
}

/// The synthesis prompt. Succinct and non-contradictory by convention (this
/// runs on local open-weight models); the schema's section rules are the
/// whole instruction. Expect to tune this against
/// `quality/session-frame.golden.md` — the prompt is the lever.
fn synthesis_system_prompt() -> String {
    format!(
        "You distill a coding-session transcript spine into a session frame: the essential \
         state a successor agent needs to seamlessly continue the work.\n\
         Output ONLY markdown body sections — no YAML, no preamble, no code fences.\n\
         Emit exactly these eight sections, in this order:\n{}\n\
         Section rules:\n\
         - Goal: the task and the larger objective it serves. At most 3 sentences.\n\
         - State: what was completed WITH its proof (test counts, live verification), what is \
           in flight, what was not started. Facts only, no narrative.\n\
         - Next: ranked concrete actions a successor should take, each anchored to a file, \
           symbol, or command from the spine.\n\
         - Decisions: each significant choice plus the stated reason for it.\n\
         - Invariants: constraints and gotchas learned that would bite a fresh session. Copy \
           each one as specifically as the spine states it — keep exact numbers, conventions, \
           and command names; never generalize a gotcha into a principle.\n\
         - Dead ends: approaches tried and abandoned, with the reason.\n\
         - Working set: emit the heading followed by exactly one line: assembled automatically\n\
         - Verification: final build/test results, deploy state, and uncommitted work.\n\
         Hard rules: use only facts present in the spine; never invent numbers, commit shas, \
         file names, or symbol names. If a section has no facts, write exactly: none recorded.\n\
         Keep the whole output under 1500 words.",
        FRAME_SECTIONS.join("\n")
    )
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
            || (t.split('.').next().is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
                && t.contains(". "));
        if is_bullet {
            let stripped = t.trim_start_matches(['-', '*']).trim_start();
            // Numbered bullets: drop the "<digits>. " prefix too, so the
            // judge sees the item text once, not "1. 1. <text>".
            let stripped = match stripped.split_once(". ") {
                Some((num, rest))
                    if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) =>
                {
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
    // ~/.sovereign/sessions/<sid>/frame.md.
    let candidate_path = {
        let p = Path::new(id_or_path);
        if p.is_file() {
            p.to_path_buf()
        } else {
            let Some(root) = sessions_root() else {
                eprintln!("grade: cannot resolve home directory");
                return 2;
            };
            let matches: Vec<PathBuf> = std::fs::read_dir(&root)
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
                1 => matches[0].join("frame.md"),
                0 => {
                    eprintln!(
                        "grade: `{id_or_path}` is neither a file nor a session with a frame under {}",
                        root.display()
                    );
                    return 2;
                }
                n => {
                    eprintln!("grade: `{id_or_path}` is ambiguous ({n} session matches)");
                    return 2;
                }
            }
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
    let model = flags.get("model").cloned().unwrap_or_else(|| "primary".into());

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
            if c_body.is_empty() { "(section missing)" } else { &c_body }
        );
        let reply = match daemon_complete(&base_url, &model, judge_system_prompt(), &user, 600).await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("grade: judge call failed on {heading}: {e}");
                return 2;
            }
        };
        let Some(verdict) = first_json_object(&reply) else {
            eprintln!("grade: judge reply on {heading} had no JSON: {}", cap_chars(&reply, 200));
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
        eprintln!("grade: golden has no gradeable sections at {}", golden_path.display());
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

// ── CLI plumbing ─────────────────────────────────────────────────────────

fn sessions_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".sovereign").join("sessions"))
}

fn print_help() {
    println!(
        "Usage: svrn session <subcommand> [options]\n\n\
         Session continuity — distill a harness transcript into a session frame\n\
         (schema: sovereign/docs/specs/SESSION_CONTINUITY.md).\n\n\
         Subcommands:\n\
         \x20 list               Recent transcripts for this project (newest first).\n\
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
         \x20 --max-tokens <n>   Synthesis output budget (default 3000).\n\
         \x20 --stdout           Print the frame instead of only writing it.\n\n\
         Output: ~/.sovereign/sessions/<session_id>/{{frame.md,spine.txt}}\n"
    );
}

fn find_transcript(dir: &Path, id: &str) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("no transcripts at {} ({e})", dir.display()))?;
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
    println!("{:<10} {:>9} {:>12}  first user turn", "session", "size", "modified");
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
        .unwrap_or(3000);

    println!("synthesizing frame via daemon (model {model}, this can take a few minutes on a local model)…");
    // Local slots have small context windows; fit the *prompt* copy of the
    // spine to the slot, shrinking + retrying on the daemon's honest
    // "Prompt too long". The on-disk spine keeps full detail regardless.
    let mut prompt_spine = spine.clone();
    let mut prompt_cap = PROMPT_CHAR_CAP_INITIAL;
    let body = loop {
        let trimmed = cap_spine_middle(&mut prompt_spine, prompt_cap);
        if trimmed > 0 {
            println!("  fitting spine to the model window: {trimmed} more assistant texts trimmed (cap {prompt_cap} chars)");
        }
        match daemon_complete(
            &base_url,
            &model,
            &synthesis_system_prompt(),
            &render_spine(&prompt_spine),
            max_tokens,
        )
        .await
        {
            Ok(b) => break b,
            Err(e) if e.contains("Prompt too long") && prompt_cap > PROMPT_CHAR_CAP_MIN => {
                prompt_cap /= 2;
                println!("  model window too small for the spine — retrying at {prompt_cap} chars");
            }
            Err(e) => {
                eprintln!(
                    "session: frame synthesis failed: {e}\n\
                     The spine is written at {} — re-run once the daemon is up,\n\
                     or hand-write the frame from the spine.",
                    spine_path.display()
                );
                return 1;
            }
        }
    };

    let head = head_at_end_of(&spine.cwd, &spine.last_ts);
    let body = replace_working_set(body.trim(), &render_working_set(&spine));
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
        println!("frame: valid (all {} sections) → {}", FRAME_SECTIONS.len(), frame_path.display());
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
            "--no-llm" | "--stdout" => {
                flags.insert(arg.trim_start_matches('-').to_string(), String::new());
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

    #[test]
    fn section_body_extracts_between_headings() {
        let f = "---\nx: y\n---\n\n## Goal\nShip it.\n\n## State\n- done\n- more\n\n## Next\nnone recorded\n";
        assert_eq!(section_body(f, "## Goal").as_deref(), Some("Ship it."));
        assert_eq!(section_body(f, "## State").as_deref(), Some("- done\n- more"));
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
        let v = first_json_object("Sure! {\"captured\": [1, 2], \"hallucinations\": []} done")
            .unwrap();
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
        assert!(!keep_user_turn("<local-command-stdout>x</local-command-stdout>"));
        assert!(!keep_user_turn("<command-name>/clear</command-name>"));
        assert!(!keep_user_turn("<task-notification>done</task-notification>"));
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
            assistant_texts: (0..40).map(|i| format!("text-{i} {}", "x".repeat(400))).collect(),
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

        // Splice replaces the model's (possibly hallucinated) section…
        let body = "## Goal\ng\n## Working set\n- /Users/aexsbrayn/mangled.rs\n## Verification\nv";
        let spliced = replace_working_set(body, &ws);
        assert!(!spliced.contains("mangled.rs"));
        assert!(spliced.contains("- src/a.rs (5 edits)"));
        assert!(spliced.contains("## Verification\nv"));

        // …and appends when the model dropped the heading entirely.
        let appended = replace_working_set("## Goal\ng", &ws);
        assert!(appended.contains("## Working set"));
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
}
