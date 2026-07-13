// SPDX-License-Identifier: AGPL-3.0-or-later
//! TEACHABLE P0 — lessons: coach the assistant in chat, own what it
//! learns in settings (`sovereign-desktop/TEACHABLE.md`).
//!
//! A lesson is a conation made durable. The ConationQuery handler
//! already parses coaching imperatives ("shorter please") into
//! one-turn transforms; this module adds the durable half: a
//! **durative lexical floor** separates "make this shorter" (deictic,
//! one turn) from "keep answers short from now on" (a standing
//! preference), a **deterministic compiler** picks the cheapest
//! enforcement rung (param → transform → prompt), and a detached
//! capture spawn proposes the lesson on a consent card. Nothing is
//! stored without an explicit user Save.
//!
//! System-of-record split (TEACHABLE §11): `{display, taught_from}`
//! is the source of record; `{prompt_form, enforcement, params}` is a
//! DERIVED artifact stamped [`COMPILER_VERSION`], re-derivable
//! wholesale. The sycophancy counterweight is structural: nothing in
//! this module touches the grounding gate — the term-avoid transform
//! runs post-gate and post-citation, and the prompt block is
//! textually subordinated to grounding rules.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::slot_policy::Workload;
use crate::title::strip_think_blocks;
use crate::traits::{ApprovalChannel, InferenceProvider, RoutingEventSink};
use crate::types::{
    CompletionRequest, LessonProposedPayload, NarrationEvent, NarrationPhase, TurnNarration,
};

/// Note kind for the lesson lane (`corpus-engine-notes` MIGRATION_V11).
pub const LESSON_KIND: &str = "lesson";

/// Version stamp on the DERIVED fields (`prompt_form`, `enforcement`,
/// `params`). Bump when the compile ladder changes shape; a recompile
/// pass (P1) re-derives every lesson whose stamp is older.
pub const COMPILER_VERSION: u32 = 1;

/// A compiled prompt_form longer than this is a clause pile, not a
/// lesson — the drafter output is dropped rather than trimmed.
pub const LESSON_PROMPT_MAX_CHARS: usize = 120;

/// Display sentences are for the settings pane — one plain sentence.
const LESSON_DISPLAY_MAX_CHARS: usize = 200;

/// Provenance excerpt cap (chars) for `taught_from`.
const TAUGHT_FROM_MAX_CHARS: usize = 500;

/// A message longer than this many words is content the user pasted,
/// not coaching — the durative floor never fires on it.
const DURATIVE_WORD_CAP: usize = 40;

/// Rung-1 targets: "shorter" caps the resolved soft target; "longer"
/// floors it. These steer the length DIRECTIVE rendered into the
/// prompt — never the `max_tokens` hard ceiling (the truncation-bug
/// class the output-budget redesign killed must not regress).
const SHORT_SOFT_TARGET_CAP: usize = 300;
const LONG_SOFT_TARGET_FLOOR: usize = 1200;

/// Output budget for the prompt-rung drafter (two short strings).
const DRAFT_MAX_TOKENS: usize = 160;

// ─── The lesson object ───────────────────────────────────────────────

/// Enforcement rung (TEACHABLE §7), cheapest first. P0 ships three;
/// `retrieval`/config is P1, `conditioning` (cartridge/adapter) is the
/// declared rung 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonEnforcement {
    Param,
    Transform,
    Prompt,
}

impl LessonEnforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Param => "param",
            Self::Transform => "transform",
            Self::Prompt => "prompt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "param" => Some(Self::Param),
            "transform" => Some(Self::Transform),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }
}

/// Provenance: the verbatim coaching moment a lesson was taught from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaughtFrom {
    pub excerpt: String,
    pub conversation_id: String,
    /// Prior assistant message the coaching referred to ("" when none).
    pub message_id: String,
}

/// The `payload_json` schema for `kind = "lesson"` notes — the single
/// source of truth both the runtime loader and the desktop commands
/// deserialize.
///
/// Source of record: `display`, `taught_from`, lifecycle fields.
/// Derived (re-derivable, stamped `compiler_version`): `prompt_form`,
/// `enforcement`, `params`. `drafted_display` is Some only when the
/// user edited the draft on the card before saving — the consented
/// correction pair (TEACHABLE §11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonPayload {
    pub display: String,
    #[serde(default)]
    pub prompt_form: String,
    pub enforcement: LessonEnforcement,
    #[serde(default)]
    pub params: serde_json::Value,
    /// Activation scopes. Empty = global (all P0 lessons).
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub taught_from: TaughtFrom,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Unix seconds at save time.
    #[serde(default)]
    pub created: i64,
    /// Unix seconds when the lesson first influenced an answer — the
    /// whisper-once marker. `None` until first application.
    #[serde(default)]
    pub first_applied_at: Option<i64>,
    #[serde(default)]
    pub last_affirmed: Option<i64>,
    #[serde(default = "default_compiler_version")]
    pub compiler_version: u32,
    /// The pre-edit draft sentence, kept only when the user edited the
    /// card before saving.
    #[serde(default)]
    pub drafted_display: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_compiler_version() -> u32 {
    1
}

impl LessonPayload {
    /// Build the note payload from a proposal the user kept (the
    /// desktop save command's helper — no duplicate schema in the
    /// desktop crate). The caller sets `drafted_display` afterwards
    /// when the user edited the card.
    pub fn from_proposed(p: &LessonProposedPayload, now: i64) -> Self {
        Self {
            display: p.display.clone(),
            prompt_form: p.prompt_form.clone(),
            // Unknown strings compile to Prompt — the most visible rung
            // (its cost shows in settings) and the only one that can
            // carry an arbitrary rule.
            enforcement: LessonEnforcement::parse(&p.enforcement)
                .unwrap_or(LessonEnforcement::Prompt),
            params: p.params.clone(),
            scope: Vec::new(),
            taught_from: TaughtFrom {
                excerpt: p.taught_from.clone(),
                conversation_id: p.conversation_id.clone(),
                message_id: p.message_id.clone(),
            },
            enabled: true,
            created: now,
            first_applied_at: None,
            last_affirmed: None,
            compiler_version: COMPILER_VERSION,
            drafted_display: None,
        }
    }
}

// ─── Durative floor — teaching vs one-turn adjustment ────────────────

/// Explicit durative markers, matched on word boundaries. Deliberately
/// small: false fires are the failure mode (a consent card on a
/// non-coaching turn spends trust for nothing); misses are fine — the
/// conation transform still honored the turn, and restating with
/// "always" is a natural retry.
const DURATIVE_MARKERS: &[&str] = &[
    "always",
    "never",
    "from now on",
    "going forward",
    "every time",
];

/// Habitual verb heads: `<head> <gerund>` reads as a standing pattern
/// ("stop mentioning", "keep repeating"), while the bare head does not
/// ("stop", "stop it" — the cancel sub-shape keeps its precision).
const HABITUAL_HEADS: &[&str] = &["stop", "quit", "keep"];

/// True when `lower_message` (already lowercased) carries an explicit
/// durative marker — the lexical floor that separates teaching from a
/// deictic one-turn adjustment. Precision-first by construction.
pub fn detect_durative(lower_message: &str) -> bool {
    let words: Vec<&str> = lower_message.split_whitespace().collect();
    if words.is_empty() || words.len() > DURATIVE_WORD_CAP {
        // A long paste is content, not coaching.
        return false;
    }

    if DURATIVE_MARKERS
        .iter()
        .any(|m| contains_word_bounded(lower_message, m))
    {
        return true;
    }

    // Habitual shape: head word immediately followed by a gerund.
    for pair in words.windows(2) {
        let head = trim_word(pair[0]);
        let next = trim_word(pair[1]);
        if HABITUAL_HEADS.contains(&head) && next.len() >= 5 && next.ends_with("ing") {
            return true;
        }
    }
    false
}

/// Word-boundary containment: `needle` occurs in `haystack` with
/// non-alphanumeric (or edge) characters on both sides. Handles
/// multi-word needles ("from now on") without regex.
fn contains_word_bounded(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let before_ok = haystack[..abs]
            .chars()
            .next_back()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        let after_ok = haystack[abs + needle.len()..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        if before_ok && after_ok {
            return true;
        }
        start = abs + needle.len();
    }
    false
}

/// Strip leading/trailing non-alphanumeric chars from a whitespace
/// token ("mentioning," → "mentioning").
fn trim_word(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}

// ─── Deterministic compile (TEACHABLE §7, cheapest rung first) ───────

/// Output of the compile ladder. Param and Transform carry
/// deterministic display templates; Prompt defers phrasing to the
/// fast-slot drafter (the one rung where the model's judgment is
/// already required).
#[derive(Debug, Clone, PartialEq)]
pub enum CompiledDirective {
    Param {
        params: serde_json::Value,
        display: String,
    },
    Transform {
        terms: Vec<String>,
        display: String,
    },
    Prompt,
}

// "short" (the canonical coaching adjective: "keep your answers short")
// is included alongside the comparative forms — these cues only run on
// messages that already passed the durative floor AND routed to
// ConationQuery, so plain-"short" false positives ("in short, …") are
// not reachable shapes here. Caught live by preflight-lessons leg 2
// (2026-07-11): the flagship phrase fell through to the drafter.
const SHORTER_CUES: &[&str] = &[
    "shorter", "short", "terse", "concise", "tldr", "brief", "briefer",
];
const LONGER_CUES: &[&str] = &["longer", "more detail", "expand", "elaborate", "more depth"];

/// Verb phrases whose object names the terms to avoid. Ordered longest
/// first so "stop using the word" wins over "stop using".
const AVOID_HEADS: &[&str] = &[
    "stop using the words",
    "stop using the word",
    "stop using the phrase",
    "stop talking about",
    "stop mentioning",
    "stop saying",
    "quit mentioning",
    "quit saying",
    "don't use the words",
    "don't use the word",
    "do not use the word",
    "don't mention",
    "do not mention",
    "don't say",
    "do not say",
    "never mention",
    "never say",
];

/// Trailing qualifiers cut from the avoid-object span before term
/// splitting ("stop mentioning corpora from now on" → "corpora").
const OBJECT_TAILS: &[&str] = &[
    " from now on",
    " going forward",
    " ever again",
    " again",
    " anymore",
    " any more",
    " in your answers",
    " in answers",
    " in your replies",
    " when you answer",
    " please",
];

/// Map a durative coaching message (lowercased) to its cheapest
/// enforcement rung. Ordered ladder, stop at first match — the same
/// discipline as the conation handler's one-turn taxonomy.
pub fn compile_directive(lower_message: &str) -> CompiledDirective {
    if SHORTER_CUES
        .iter()
        .any(|c| contains_word_bounded(lower_message, c))
    {
        return CompiledDirective::Param {
            params: serde_json::json!({ "soft_target_cap": SHORT_SOFT_TARGET_CAP }),
            display: "Keep answers short — a tight paragraph unless asked to go deeper."
                .to_string(),
        };
    }
    if LONGER_CUES
        .iter()
        .any(|c| contains_word_bounded(lower_message, c))
    {
        return CompiledDirective::Param {
            params: serde_json::json!({ "soft_target_floor": LONG_SOFT_TARGET_FLOOR }),
            display: "Give fuller answers — more depth and detail by default.".to_string(),
        };
    }
    if let Some(terms) = extract_avoid_terms(lower_message) {
        let display = format!("Don't use: {}.", terms.join(", "));
        return CompiledDirective::Transform { terms, display };
    }
    CompiledDirective::Prompt
}

/// Extract the avoid-term list from a "stop saying X" shaped message.
/// Conservative: returns `None` (falling through to the Prompt rung)
/// unless the object span parses into 1–5 clean terms of 1–3 words
/// each — a mis-parsed term list would strip the wrong words from
/// every future answer.
pub fn extract_avoid_terms(lower_message: &str) -> Option<Vec<String>> {
    let (head_end, _) = AVOID_HEADS
        .iter()
        .filter_map(|h| lower_message.find(h).map(|pos| (pos + h.len(), h.len())))
        // First occurrence in the message; longest head on ties (the
        // AVOID_HEADS ordering already prefers longer variants, and
        // max_by_key on (−pos, len) keeps that stable).
        .min_by_key(|(end, len)| (*end - *len, usize::MAX - *len))?;

    let mut object = &lower_message[head_end..];
    // Cut at the first sentence boundary.
    if let Some(cut) = object.find(['.', '!', '?', ';', '\n']) {
        object = &object[..cut];
    }
    // Cut trailing qualifiers.
    let mut object = object.to_string();
    loop {
        let mut trimmed_any = false;
        for tail in OBJECT_TAILS {
            if let Some(stripped) = object.strip_suffix(tail) {
                object = stripped.to_string();
                trimmed_any = true;
            }
        }
        if !trimmed_any {
            break;
        }
    }

    let raw_terms: Vec<&str> = object
        .split(',')
        .flat_map(|part| part.split(" and "))
        .flat_map(|part| part.split(" or "))
        .collect();

    let mut terms = Vec::new();
    for raw in raw_terms {
        let cleaned = raw
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
            .trim();
        let cleaned = cleaned
            .strip_prefix("the word ")
            .or_else(|| cleaned.strip_prefix("the phrase "))
            .or_else(|| cleaned.strip_prefix("words like "))
            .or_else(|| cleaned.strip_prefix("terms like "))
            .or_else(|| cleaned.strip_prefix("the "))
            .unwrap_or(cleaned)
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
        if cleaned.is_empty() {
            continue;
        }
        let word_count = cleaned.split_whitespace().count();
        let char_ok = cleaned
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_');
        if cleaned.len() < 3 || word_count == 0 || word_count > 3 || !char_ok {
            return None;
        }
        terms.push(cleaned.to_string());
    }

    if terms.is_empty() || terms.len() > 5 {
        return None;
    }
    Some(terms)
}

// ─── Rung 2: conservative term-avoid transform ───────────────────────

/// Remove whole-word, case-insensitive occurrences of `terms` from
/// `text`, skipping `[Source: …]` citation spans and fenced code
/// blocks so citation anchors and code are structurally untouchable.
/// Runs post-grounding-gate and post-citation-passes (TEACHABLE §7).
/// Tidies the whitespace/punctuation a removal leaves behind.
/// Idempotent. Returns `(output, changed)`.
pub fn apply_term_avoid(text: &str, terms: &[String]) -> (String, bool) {
    if terms.is_empty() || text.is_empty() {
        return (text.to_string(), false);
    }

    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut in_fence = false;

    // Process line-wise: fence state toggles on ``` lines; protected
    // lines pass through verbatim.
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;

        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        let (stripped, line_changed) = strip_terms_outside_citations(line, terms);
        changed |= line_changed;
        out.push_str(&stripped);
    }
    (out, changed)
}

/// Strip terms from one line, skipping `[Source: …]` spans.
fn strip_terms_outside_citations(line: &str, terms: &[String]) -> (String, bool) {
    let lower = line.to_lowercase();
    let mut out = String::with_capacity(line.len());
    let mut changed = false;
    let mut i = 0;

    while i < line.len() {
        // Protected citation span: copy verbatim through the closing ']'.
        if lower[i..].starts_with("[source:") {
            let end = lower[i..]
                .find(']')
                .map(|p| i + p + 1)
                .unwrap_or(line.len());
            out.push_str(&line[i..end]);
            i = end;
            continue;
        }

        // Try each term at this position (longest first so "corpus
        // engine" wins over "corpus").
        let mut matched_len = 0;
        for term in terms {
            let t = term.to_lowercase();
            if t.is_empty() || !lower[i..].starts_with(t.as_str()) {
                continue;
            }
            let before_ok = line[..i]
                .chars()
                .next_back()
                .map(|c| !c.is_alphanumeric() && c != '-')
                .unwrap_or(true);
            let after_ok = line[i + t.len()..]
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric() && c != '-')
                .unwrap_or(true);
            if before_ok && after_ok && t.len() > matched_len {
                matched_len = t.len();
            }
        }
        if matched_len > 0 {
            changed = true;
            i += matched_len;
            // Swallow one following space so "the corpus index" →
            // "the index", not "the  index".
            if line[i..].starts_with(' ') && out.ends_with([' ', '(']) {
                i += 1;
            }
            continue;
        }

        // Copy one char.
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    if changed {
        out = tidy_after_removal(&out);
    }
    (out, changed)
}

/// Collapse the artifacts a whole-word removal leaves: doubled spaces,
/// space-before-punctuation, and leading/trailing run-on spaces.
fn tidy_after_removal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
            out.push(ch);
        } else {
            if prev_space && matches!(ch, ',' | '.' | ';' | ':' | '!' | '?' | ')') {
                out.pop();
            }
            prev_space = false;
            out.push(ch);
        }
    }
    out.trim_end().to_string()
}

// ─── Rung 4: prompt-lesson drafter (fast slot, guarded) ──────────────

/// Phrase a prompt-rung lesson from the user's coaching message:
/// `(display, prompt_form)`. The ONLY model call in the capture path —
/// param/transform lessons use deterministic templates. Silently
/// returns `None` on any malformed output: no card at all beats a
/// wrong card (TEACHABLE §4).
pub(crate) async fn draft_prompt_lesson(
    inference: &dyn InferenceProvider,
    taught_from: &str,
) -> Option<(String, String)> {
    let excerpt = truncate_chars(taught_from, TAUGHT_FROM_MAX_CHARS);
    let prompt = format!(
        "A user is coaching their assistant with a standing preference.\n\n\
         User said: \"{excerpt}\"\n\n\
         Respond with JSON: {{\"display\": \"<the rule as one plain sentence, \
         close to the user's own words>\", \"prompt_form\": \"<the same rule as \
         the fewest-words imperative to the assistant>\"}}\n\n\
         Output the JSON object only — no preface."
    );
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "display":     { "type": "string" },
            "prompt_form": { "type": "string" }
        },
        "required": ["display", "prompt_form"]
    });

    // SLOT_POLICY §3 Housekeep: post-turn drafting, same as identify_gap.
    let mut request = CompletionRequest::for_workload(Workload::Housekeep, prompt)
        .with_system(
            "You turn user coaching into terse standing rules. Output only \
             the requested JSON object — no thinking, no preface.",
        )
        .with_output_budget(DRAFT_MAX_TOKENS as u32);
    request.temperature = Some(0.0);
    request.structured_output = Some(schema);

    let response = match inference.complete(&request).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "lessons", error = %e, "lesson drafter: inference failed — dropping capture");
            return None;
        }
    };
    parse_draft_response(&response.text)
}

/// Parse + guard the drafter output. Every rejection is a silent drop.
fn parse_draft_response(raw: &str) -> Option<(String, String)> {
    let cleaned = strip_think_blocks(raw);
    let candidate = extract_json_object(cleaned.trim())?;
    let val: serde_json::Value = serde_json::from_str(&candidate).ok()?;

    let display = val.get("display")?.as_str()?.trim().to_string();
    let prompt_form = val.get("prompt_form")?.as_str()?.trim().to_string();

    let ok = !display.is_empty()
        && !prompt_form.is_empty()
        && display.chars().count() <= LESSON_DISPLAY_MAX_CHARS
        && prompt_form.chars().count() <= LESSON_PROMPT_MAX_CHARS
        && !display.contains('\n')
        && !prompt_form.contains('\n')
        && !prompt_form.ends_with('?');
    if !ok {
        tracing::debug!(target: "lessons", "lesson drafter: output failed guards — dropping capture");
        return None;
    }
    Some((display, prompt_form))
}

/// First balanced `{…}` object in `text` (the model may wrap JSON in
/// prose or fences despite instructions). Same conservative shape as
/// the gap/judge parsers.
fn extract_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escape = false;
    for (i, ch) in text[start..].char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_str => escape = true,
            '"' => in_str = !in_str,
            '{' if !in_str => depth += 1,
            '}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Char-boundary-safe truncation with ellipsis.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

// ─── Turn-time loading + application ─────────────────────────────────

/// One active lesson, with its note id (the citation handle for
/// `lessons_applied` metadata and the settings pane).
#[derive(Debug, Clone)]
pub struct ActiveLesson {
    pub note_id: String,
    pub payload: LessonPayload,
}

/// The per-turn snapshot: at most one active lesson per rung (the
/// desktop save command supersedes-and-retires the previous lesson of
/// a rung, so K=1 per rung is structural — this loader just takes the
/// newest as belt-and-braces). P0 has NO selector by design
/// (TEACHABLE §6).
#[derive(Debug, Clone, Default)]
pub struct ActiveLessonSet {
    pub length: Option<ActiveLesson>,
    pub term_avoid: Option<ActiveLesson>,
    pub prompt: Option<ActiveLesson>,
}

impl ActiveLessonSet {
    pub fn is_empty(&self) -> bool {
        self.length.is_none() && self.term_avoid.is_none() && self.prompt.is_none()
    }

    /// Rung 1: clamp the resolved output-budget SOFT target by the
    /// active length lesson. Returns `(new_target, changed)` —
    /// `changed` feeds `lessons_applied` and the whisper.
    pub fn adjust_soft_target(&self, target: usize) -> (usize, bool) {
        let Some(lesson) = &self.length else {
            return (target, false);
        };
        let params = &lesson.payload.params;
        let mut new_target = target;
        if let Some(cap) = params.get("soft_target_cap").and_then(|v| v.as_u64()) {
            new_target = new_target.min(cap as usize);
        }
        if let Some(floor) = params.get("soft_target_floor").and_then(|v| v.as_u64()) {
            new_target = new_target.max(floor as usize);
        }
        (new_target, new_target != target)
    }

    /// Rung 2: the active term-avoid list ([] when none).
    pub fn term_list(&self) -> Vec<String> {
        self.term_avoid
            .as_ref()
            .and_then(|l| l.payload.params.get("terms"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Load the active-lesson snapshot: ONE NoteStore read per turn
/// (newest-first, retired excluded — supersede is honored by
/// construction; disabled and malformed rows are skipped). `None`
/// store (CLI paths without notes wired) → empty set.
pub async fn load_active_lessons(
    store: Option<&corpus_engine_notes::NoteStore>,
) -> ActiveLessonSet {
    let Some(store) = store else {
        return ActiveLessonSet::default();
    };
    let rows = match store
        .read_notes(None, &[], &[], &[LESSON_KIND.to_string()], 20, false)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(target: "lessons", error = %e, "lesson load failed — turn proceeds unlessoned");
            return ActiveLessonSet::default();
        }
    };

    let mut set = ActiveLessonSet::default();
    for row in rows {
        let Some(raw) = row.payload_json.as_deref() else {
            continue;
        };
        let payload: LessonPayload = match serde_json::from_str(raw) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(target: "lessons", note_id = %row.id, error = %e, "malformed lesson payload — skipped");
                continue;
            }
        };
        if !payload.enabled {
            continue;
        }
        let active = ActiveLesson {
            note_id: row.id,
            payload,
        };
        // Rows arrive newest-first; first seen per rung wins.
        let slot = match active.payload.enforcement {
            LessonEnforcement::Param => &mut set.length,
            LessonEnforcement::Transform => &mut set.term_avoid,
            LessonEnforcement::Prompt => &mut set.prompt,
        };
        if slot.is_none() {
            *slot = Some(active);
        }
    }
    set
}

/// What a turn records in `Message.metadata.lessons_applied` —
/// glassbox provenance for the settings pane and the QA harness.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedLessonMeta {
    pub id: String,
    pub enforcement: &'static str,
}

/// Render the K=1 compiled coaching sentence for the system message —
/// appended OUTERMOST (after custom instructions), textually
/// subordinated to the safety/grounding rules above it.
pub fn render_lesson_block(prompt_form: &str) -> String {
    format!(
        "The user has taught you this standing rule. Follow it unless it \
         conflicts with a safety or grounding rule above:\n{prompt_form}"
    )
}

/// Whisper-once bookkeeping: for each applied lesson whose
/// `first_applied_at` is unset, stamp it now (via
/// `NoteStore::update_note_payload`) and return the FIRST such
/// lesson's `{id, display}` for `metadata.kept_lesson` — at most one
/// whisper per message even when two lessons first-apply on the same
/// turn. A failed stamp is traced and the whisper may repeat next
/// turn (accepted: single-user desktop, one-UPDATE window).
pub(crate) async fn note_first_applications(
    store: Option<&corpus_engine_notes::NoteStore>,
    applied: &[&ActiveLesson],
    now: i64,
) -> Option<serde_json::Value> {
    let store = store?;
    let mut whisper = None;
    for lesson in applied {
        if lesson.payload.first_applied_at.is_some() {
            continue;
        }
        let mut updated = lesson.payload.clone();
        updated.first_applied_at = Some(now);
        match serde_json::to_string(&updated) {
            Ok(json) => {
                if let Err(e) = store.update_note_payload(&lesson.note_id, &json).await {
                    tracing::warn!(target: "lessons", note_id = %lesson.note_id, error = %e,
                        "first-application stamp failed — whisper may repeat");
                }
            }
            Err(e) => {
                tracing::warn!(target: "lessons", note_id = %lesson.note_id, error = %e,
                    "first-application serialize failed");
            }
        }
        if whisper.is_none() {
            whisper = Some(serde_json::json!({
                "id": lesson.note_id,
                "display": lesson.payload.display,
            }));
        }
    }
    whisper
}

// ─── Capture orchestrator (the conation handler's detached spawn) ────

/// Draft and propose a lesson from a durative coaching message. Runs
/// in a detached spawn — the turn is already answered; nothing here
/// can block or fail it. Emits a `LessonDrafted` narration chip (when
/// a live session is known) and the fire-and-forget `lesson-proposed`
/// card payload. Stores NOTHING — consent happens in the desktop save
/// command.
pub(crate) async fn capture_lesson(
    inference: Arc<dyn InferenceProvider>,
    approval: Arc<dyn ApprovalChannel>,
    routing_events: Arc<dyn RoutingEventSink>,
    session_id: Option<String>,
    conversation_id: String,
    prior_assistant_id: String,
    message: String,
) {
    let lower = message.to_lowercase();
    let (display, prompt_form, enforcement, params) = match compile_directive(&lower) {
        CompiledDirective::Param { params, display } => {
            (display, String::new(), LessonEnforcement::Param, params)
        }
        CompiledDirective::Transform { terms, display } => (
            display,
            String::new(),
            LessonEnforcement::Transform,
            serde_json::json!({ "terms": terms }),
        ),
        CompiledDirective::Prompt => {
            match draft_prompt_lesson(inference.as_ref(), &message).await {
                Some((display, prompt_form)) => (
                    display,
                    prompt_form,
                    LessonEnforcement::Prompt,
                    serde_json::json!({}),
                ),
                // Malformed draft → no card at all (precision-first).
                None => return,
            }
        }
    };

    let payload = LessonProposedPayload {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        message_id: prior_assistant_id,
        display,
        prompt_form,
        enforcement: enforcement.as_str().to_string(),
        params,
        taught_from: truncate_chars(&message, TAUGHT_FROM_MAX_CHARS),
    };

    tracing::info!(
        target: "lessons",
        draft_id = %payload.id,
        enforcement = enforcement.as_str(),
        "lesson capture: durative coaching → drafted card"
    );

    if let Some(sid) = session_id {
        routing_events
            .emit_turn_narration(TurnNarration {
                session_id: sid,
                conversation_id,
                event: NarrationEvent {
                    phase: NarrationPhase::LessonDrafted,
                    text:
                        "That sounds like a standing preference — drafting a lesson you can save."
                            .to_string(),
                    elapsed_ms: 0,
                },
            })
            .await;
    }

    approval.emit_lesson_proposed(payload);
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Durative floor ────────────────────────────────────────────

    #[test]
    fn durative_floor_fires_on_explicit_markers_only() {
        // Positives: explicit durative language.
        for msg in [
            "always keep it short",
            "keep answers short from now on",
            "never mention the mesh again",
            "going forward, skip the preamble",
            "every time you answer, be brief",
            "stop mentioning the corpus",
            "quit hedging so much", // "hedging" gerund after quit
            "you keep repeating yourself",
        ] {
            assert!(detect_durative(msg), "should fire: {msg}");
        }
        // Negatives: deictic adjustments and non-coaching.
        for msg in [
            "make this shorter",
            "shorter please",
            "tldr",
            "stop",
            "stop it",
            "stop that",
            "try again",
            "expand on the second point",
            // "nevertheless" must not match "never" (word boundary).
            "nevertheless, continue",
        ] {
            assert!(!detect_durative(msg), "should NOT fire: {msg}");
        }
        // A long paste containing "always" is content, not coaching.
        let paste = format!(
            "please review this: {}",
            "lorem ipsum ".repeat(30) + "always"
        );
        assert!(!detect_durative(&paste.to_lowercase()));
    }

    // ── Compile ladder ────────────────────────────────────────────

    #[test]
    fn compile_maps_directives_to_cheapest_rung() {
        match compile_directive("keep answers shorter from now on") {
            CompiledDirective::Param { params, .. } => {
                assert_eq!(params["soft_target_cap"], SHORT_SOFT_TARGET_CAP);
            }
            other => panic!("expected Param, got {other:?}"),
        }
        // The canonical coaching adjective — plain "short" — must hit
        // the param rung deterministically, not the drafter.
        match compile_directive(
            "from now on, keep your answers short — a paragraph at most unless i ask for more.",
        ) {
            CompiledDirective::Param { params, .. } => {
                assert_eq!(params["soft_target_cap"], SHORT_SOFT_TARGET_CAP);
            }
            other => panic!("expected Param for plain 'short', got {other:?}"),
        }
        match compile_directive("always give more detail") {
            CompiledDirective::Param { params, .. } => {
                assert_eq!(params["soft_target_floor"], LONG_SOFT_TARGET_FLOOR);
            }
            other => panic!("expected Param, got {other:?}"),
        }
        match compile_directive("stop mentioning corpora, indexes and retrieval from now on") {
            CompiledDirective::Transform { terms, display } => {
                assert_eq!(terms, vec!["corpora", "indexes", "retrieval"]);
                assert!(display.starts_with("Don't use:"));
            }
            other => panic!("expected Transform, got {other:?}"),
        }
        // Free-form durative coaching falls to the prompt rung.
        assert_eq!(
            compile_directive("always explain like i'm five"),
            CompiledDirective::Prompt
        );
    }

    #[test]
    fn avoid_term_extraction_is_conservative() {
        // Quoted terms are unwrapped.
        assert_eq!(
            extract_avoid_terms("never say \"leverage\" or \"synergy\""),
            Some(vec!["leverage".to_string(), "synergy".to_string()])
        );
        // "the word X" wrapper is stripped.
        assert_eq!(
            extract_avoid_terms("stop using the word corpus from now on"),
            Some(vec!["corpus".to_string()])
        );
        // An over-long object (not a term list) falls through to Prompt.
        assert_eq!(
            extract_avoid_terms(
                "stop mentioning that thing where you explain the whole retrieval pipeline to me"
            ),
            None
        );
        // Sentence boundary cuts the span.
        assert_eq!(
            extract_avoid_terms("don't mention chunks. also be nicer"),
            Some(vec!["chunks".to_string()])
        );
    }

    // ── Term-avoid transform ──────────────────────────────────────

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn term_avoid_strips_whole_words_only() {
        let (out, changed) = apply_term_avoid(
            "The corpus index helps. Indexing continues.",
            &terms(&["index"]),
        );
        assert!(changed);
        assert_eq!(out, "The corpus helps. Indexing continues.");
    }

    #[test]
    fn term_avoid_skips_citations_and_code() {
        let text = "Facts here [Source: Corpus Handbook] and more corpus talk.\n```\nlet corpus = 1;\n```\nEnd corpus.";
        let (out, changed) = apply_term_avoid(text, &terms(&["corpus"]));
        assert!(changed);
        assert!(
            out.contains("[Source: Corpus Handbook]"),
            "citation span must survive: {out}"
        );
        assert!(
            out.contains("let corpus = 1;"),
            "code fence must survive: {out}"
        );
        assert!(!out.contains("corpus talk"));
        assert!(out.ends_with("End."), "trailing removal tidied: {out}");
    }

    #[test]
    fn term_avoid_is_idempotent_and_tidies_punctuation() {
        let (once, _) = apply_term_avoid(
            "We searched the corpus, then answered.",
            &terms(&["corpus"]),
        );
        assert_eq!(once, "We searched the, then answered.");
        let (twice, changed) = apply_term_avoid(&once, &terms(&["corpus"]));
        assert!(!changed);
        assert_eq!(twice, once);
    }

    #[test]
    fn term_avoid_no_terms_is_noop() {
        let (out, changed) = apply_term_avoid("unchanged", &[]);
        assert!(!changed);
        assert_eq!(out, "unchanged");
    }

    // ── Drafter parse guards ──────────────────────────────────────

    #[test]
    fn draft_parse_accepts_clean_and_rejects_malformed() {
        let ok = parse_draft_response(
            r#"{"display": "Explain things simply.", "prompt_form": "Explain like I'm five."}"#,
        );
        assert_eq!(
            ok,
            Some((
                "Explain things simply.".to_string(),
                "Explain like I'm five.".to_string()
            ))
        );
        // Fenced output still parses.
        assert!(parse_draft_response(
            "```json\n{\"display\": \"d\", \"prompt_form\": \"do it\"}\n```"
        )
        .is_some());
        // Rejections: missing field, empty, over-long, question-shaped.
        assert!(parse_draft_response(r#"{"display": "only"}"#).is_none());
        assert!(parse_draft_response(r#"{"display": "", "prompt_form": "x"}"#).is_none());
        let long = "x".repeat(LESSON_PROMPT_MAX_CHARS + 1);
        assert!(
            parse_draft_response(&format!(r#"{{"display": "d", "prompt_form": "{long}"}}"#))
                .is_none()
        );
        assert!(parse_draft_response(r#"{"display": "d", "prompt_form": "should I?"}"#).is_none());
        assert!(parse_draft_response("not json at all").is_none());
    }

    // ── Payload serde + soft-target adjust ────────────────────────

    #[test]
    fn payload_serde_tolerates_missing_optionals() {
        let minimal = r#"{
            "display": "Keep answers short.",
            "enforcement": "param",
            "taught_from": {"excerpt": "shorter always", "conversation_id": "c1", "message_id": ""},
            "created": 1000
        }"#;
        let p: LessonPayload = serde_json::from_str(minimal).unwrap();
        assert!(p.enabled, "enabled defaults true");
        assert_eq!(p.compiler_version, 1, "compiler_version defaults 1");
        assert!(p.first_applied_at.is_none());
        assert!(p.drafted_display.is_none());
        // Round-trip.
        let json = serde_json::to_string(&p).unwrap();
        let p2: LessonPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(p2.display, p.display);
    }

    fn length_lesson(params: serde_json::Value) -> ActiveLesson {
        ActiveLesson {
            note_id: "n1".to_string(),
            payload: LessonPayload {
                display: "Keep answers short.".to_string(),
                prompt_form: String::new(),
                enforcement: LessonEnforcement::Param,
                params,
                scope: vec![],
                taught_from: TaughtFrom::default(),
                enabled: true,
                created: 0,
                first_applied_at: None,
                last_affirmed: None,
                compiler_version: 1,
                drafted_display: None,
            },
        }
    }

    #[test]
    fn adjust_soft_target_caps_floors_and_noops() {
        let mut set = ActiveLessonSet::default();
        assert_eq!(set.adjust_soft_target(800), (800, false));

        set.length = Some(length_lesson(serde_json::json!({"soft_target_cap": 300})));
        assert_eq!(set.adjust_soft_target(800), (300, true));
        assert_eq!(set.adjust_soft_target(250), (250, false));

        set.length = Some(length_lesson(
            serde_json::json!({"soft_target_floor": 1200}),
        ));
        assert_eq!(set.adjust_soft_target(800), (1200, true));
        assert_eq!(set.adjust_soft_target(1500), (1500, false));
    }

    // ── Loader + whisper against a real NoteStore ─────────────────

    async fn store_with_lesson(
        payload: &LessonPayload,
    ) -> (tempfile::TempDir, corpus_engine_notes::NoteStore, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = corpus_engine_notes::NoteStore::open(&dir.path().join("notes.db")).unwrap();
        let json = serde_json::to_string(payload).unwrap();
        let id = store
            .write_note_full(
                LESSON_KIND,
                &payload.display,
                vec![],
                vec![],
                "s1",
                corpus_engine_notes::NoteScope::Global,
                None,
                None,
                corpus_engine_notes::NoteSource::Agent,
                None,
                Some(&json),
            )
            .await
            .unwrap();
        (dir, store, id)
    }

    #[tokio::test]
    async fn loader_honors_enabled_flag_and_supersede() {
        let lesson = length_lesson(serde_json::json!({"soft_target_cap": 300})).payload;
        let (_dir, store, id_a) = store_with_lesson(&lesson).await;

        // Active lesson loads into the param slot.
        let set = load_active_lessons(Some(&store)).await;
        assert_eq!(set.length.as_ref().unwrap().note_id, id_a);
        assert!(set.term_avoid.is_none() && set.prompt.is_none());

        // Disabled → skipped.
        let mut disabled = lesson.clone();
        disabled.enabled = false;
        store
            .update_note_payload(&id_a, &serde_json::to_string(&disabled).unwrap())
            .await
            .unwrap();
        assert!(load_active_lessons(Some(&store)).await.length.is_none());

        // Supersede: a retired predecessor never loads.
        let json_b = serde_json::to_string(&lesson).unwrap();
        let id_b = store
            .write_note_full(
                LESSON_KIND,
                "Keep answers short.",
                vec![],
                vec![],
                "s1",
                corpus_engine_notes::NoteScope::Global,
                None,
                None,
                corpus_engine_notes::NoteSource::Agent,
                Some(&id_a),
                Some(&json_b),
            )
            .await
            .unwrap();
        store
            .retire_by_id(&id_a, &format!("superseded by {id_b}"))
            .await
            .unwrap();
        let set = load_active_lessons(Some(&store)).await;
        assert_eq!(set.length.as_ref().unwrap().note_id, id_b);

        // No store → empty set, no error.
        assert!(load_active_lessons(None).await.is_empty());
    }

    #[tokio::test]
    async fn whisper_fires_exactly_once() {
        let lesson = length_lesson(serde_json::json!({"soft_target_cap": 300})).payload;
        let (_dir, store, _id) = store_with_lesson(&lesson).await;

        let set = load_active_lessons(Some(&store)).await;
        let active = set.length.as_ref().unwrap();

        // First application: whisper carries {id, display} and stamps.
        let whisper = note_first_applications(Some(&store), &[active], 12345).await;
        let whisper = whisper.expect("first application must whisper");
        assert_eq!(whisper["display"], "Keep answers short.");

        // Reload: first_applied_at is now stamped → no second whisper.
        let set = load_active_lessons(Some(&store)).await;
        let active = set.length.as_ref().unwrap();
        assert_eq!(active.payload.first_applied_at, Some(12345));
        assert!(
            note_first_applications(Some(&store), &[active], 99999)
                .await
                .is_none(),
            "whisper must fire exactly once"
        );
    }

    #[test]
    fn render_lesson_block_subordinates_to_grounding() {
        let block = render_lesson_block("Explain like I'm five.");
        assert!(block.contains("Explain like I'm five."));
        assert!(
            block.contains("unless it conflicts with a safety or grounding rule"),
            "the prompt block must stay subordinate to the gate"
        );
    }

    #[test]
    fn from_proposed_maps_wire_payload() {
        let wire = LessonProposedPayload {
            id: "draft-1".to_string(),
            conversation_id: "c1".to_string(),
            message_id: "m1".to_string(),
            display: "Keep it short.".to_string(),
            prompt_form: String::new(),
            enforcement: "param".to_string(),
            params: serde_json::json!({"soft_target_cap": 300}),
            taught_from: "keep answers short from now on".to_string(),
        };
        let p = LessonPayload::from_proposed(&wire, 777);
        assert_eq!(p.enforcement, LessonEnforcement::Param);
        assert_eq!(p.created, 777);
        assert_eq!(p.taught_from.conversation_id, "c1");
        assert_eq!(p.compiler_version, COMPILER_VERSION);
        assert!(p.enabled && p.first_applied_at.is_none());
        // Unknown enforcement strings land on the visible rung.
        let mut odd = wire.clone();
        odd.enforcement = "mystery".to_string();
        assert_eq!(
            LessonPayload::from_proposed(&odd, 0).enforcement,
            LessonEnforcement::Prompt
        );
    }
}
