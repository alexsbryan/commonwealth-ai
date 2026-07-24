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
        spine
            .assistant_texts
            .insert(mid, format!("[… {dropped} mid-session assistant texts omitted for budget …]"));
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
    let overhead: usize =
        200 + spine.user_turns.iter().map(|t| t.len() + 24).sum::<usize>();
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
        "## Goal" => {
            "What task was this session working on, and what larger objective does it \
             serve? Answer in 1-2 bullets."
        }
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
        let bullet_text = if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
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
            n.parse::<usize>().is_ok_and(|i| (1..=n_assistant).contains(&i))
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
        let toks: Vec<&str> = inner.split([',', ';', ' ']).filter(|s| !s.is_empty()).collect();
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
         \x20 --max-tokens <n>   Per-section answer budget (default 700).\n\
         \x20 --stdout           Print the frame instead of only writing it.\n\
         \x20 --force            Overwrite even a self-reported frame (default: a frame\n\
         \x20                    banked via session_state is never clobbered by distill).\n\n\
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
                MINED_SECTIONS.iter().map(|h| h.trim_start_matches("## ")).collect::<Vec<_>>().join(", "),
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
                    match daemon_complete(&base_url, &model, retrieval_system_prompt(), user, max_tokens)
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
            "--no-llm" | "--stdout" | "--force" => {
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
            assistant_texts: (0..30).map(|i| format!("text-{i} {}", "x".repeat(900))).collect(),
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
        assert!(rendered.contains("[u1] user turn"), "user turns ride every chunk");
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
        assert_eq!(parsed.kept[1], "wrapped bullet about the export fail-closed guard");
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
}
