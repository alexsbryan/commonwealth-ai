// SPDX-License-Identifier: AGPL-3.0-or-later
//! One reported answer — the artifact behind "this answer was wrong".
//!
//! [`crate::health`] answers *is my install OK?* and
//! [`crate::crash_bundle`] answers *what happened when it stopped?*.
//! Neither can answer the complaint people actually make, which is
//! about a specific reply: it named a book we don't have, it ignored
//! my folder, it went to a peer when I asked about my own notes. Until
//! now that complaint arrived as prose in a chat message and every
//! support conversation started by asking the person to reproduce it.
//!
//! Three design commitments, each load-bearing:
//!
//! 1. **The snapshot comes from the frontend, not from the runtime.**
//!    `TurnProvenance` (sovereign-core) is captured only on the
//!    Relational/witness register and only for the *most recent* turn
//!    per conversation, in memory. The complaint is usually about a
//!    knowledge query, often several turns back, sometimes after a
//!    restart. The assistant message the user is looking at already
//!    carries the whole story in its persisted `metadata` — route,
//!    sources, backend, latency, gate action — so we report what they
//!    are pointing at rather than what the runtime happens to still
//!    remember.
//!
//! 2. **The reference code is derived, never stored.** A person on the
//!    phone can read `K7M-2QP` out loud; nobody reads a UUID out loud.
//!    Deriving it from `message_id` with a pinned hash means no new
//!    persistence, stability across restarts, and — the part that
//!    matters for support — that the same answer always produces the
//!    same code, so a screenshot and a report file can be matched
//!    without either one being authoritative.
//!
//! 3. **Source *text* is opt-in, per report, by the person whose text
//!    it is.** Titles and corpus ids answer "did retrieval find the
//!    right document?", which is most triage. The passage bodies answer
//!    "did it read the document correctly?", which is the rest — but
//!    they are the user's documents, and this product's whole promise
//!    is that those stay put. So the dialog asks, the default is off,
//!    and [`render_turn_section`] refuses to print snippets the flag
//!    did not authorise even if a caller supplies them.

use serde::Deserialize;

/// Caps. Generous enough that a real answer survives intact, bounded
/// enough that the report stays something you can attach to an email.
/// Truncation is always marked — a silently clipped answer would read
/// as a model that stopped early, which is a bug report about us.
const MAX_QUESTION_CHARS: usize = 4_000;
const MAX_ANSWER_CHARS: usize = 12_000;
const MAX_SNIPPET_CHARS: usize = 800;
const MAX_RETRIEVED: usize = 12;

/// One corpus's contribution to the answer, mirroring the desktop's
/// `provenance.sources` entries.
#[derive(Debug, Clone, Deserialize)]
pub struct TurnSource {
    pub origin: String,
    pub count: usize,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Set when this corpus was searched on a peer rather than locally.
    #[serde(default)]
    pub from_peer: Option<String>,
}

/// One retrieved passage. `title` + `corpus_id` + `chunk_id` are
/// enough for the maintainer to pull the same passage locally when
/// they have the same knowledge base; `snippet` is the passage text
/// itself and is present only when the reporter opted in.
#[derive(Debug, Clone, Deserialize)]
pub struct RetrievedRef {
    pub title: String,
    #[serde(default)]
    pub corpus_id: Option<String>,
    #[serde(default)]
    pub chunk_id: Option<i64>,
    #[serde(default)]
    pub snippet: Option<String>,
}

/// Everything the reported turn contributes to the report. Every field
/// past the two ids is optional: a turn from an older build, a
/// naked-model reply, or an errored stream all still produce a report,
/// they just produce a thinner one. A missing field renders as
/// "not recorded" — never as a guess.
#[derive(Debug, Clone, Deserialize)]
pub struct TurnSnapshot {
    pub conversation_id: String,
    pub message_id: String,
    /// What the user asked. The anchor of the whole document: without
    /// it the reader cannot judge whether the answer was wrong.
    #[serde(default)]
    pub question: Option<String>,
    /// What the app replied — the thing being reported.
    #[serde(default)]
    pub answer: Option<String>,
    /// `provenance.intent` — the fine-grained route the turn took.
    #[serde(default)]
    pub route: Option<String>,
    /// `provenance.coarse_intent` — SIMPLE / LOOKUP / REASONING / ACTION.
    #[serde(default)]
    pub coarse_intent: Option<String>,
    /// Why the router chose that route, in its own words.
    #[serde(default)]
    pub routing_trigger: Option<String>,
    /// Retrieval path label, e.g. `CorpusEngine`, `document`.
    #[serde(default)]
    pub search_method: Option<String>,
    /// `provenance.inference_backend` — which model, and which peer if
    /// it wasn't this machine.
    #[serde(default)]
    pub answered_by: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub tokens_used: Option<u64>,
    /// `"length"` here is the single most common cause of "it stopped
    /// mid-sentence" reports, and the one the user cannot diagnose.
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub max_tokens_budget: Option<u64>,
    /// The grounding gate's own verdict for this turn, when it ran.
    #[serde(default)]
    pub gate_action: Option<String>,
    /// Pre-rendered coverage note ("Thin coverage: your …"), matching
    /// what the user saw under the answer.
    #[serde(default)]
    pub coverage_note: Option<String>,
    #[serde(default)]
    pub sources: Vec<TurnSource>,
    #[serde(default)]
    pub retrieved: Vec<RetrievedRef>,
    /// The reporter's explicit consent to include passage text. See the
    /// module note — this is honoured as a *gate*, not as a hint.
    #[serde(default)]
    pub include_source_text: bool,
}

/// Crockford base32 minus the ambiguous letters. `I`/`L`/`O`/`U` are
/// absent so nothing in a code can be misheard as `1`, `0`, or read as
/// a word — the code exists to be spoken.
const CODE_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A short, speakable handle for one answer, derived from its
/// `message_id`.
///
/// FNV-1a rather than `DefaultHasher`: the standard hasher is
/// explicitly not stable across Rust releases, and a reference code
/// that changes when we upgrade the toolchain is worse than no code at
/// all — the user's screenshot would stop matching their own report.
/// This function is a wire format; treat changes to it as breaking.
pub fn reference_code(message_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in message_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut chars = [0u8; 7];
    for (i, slot) in chars.iter_mut().enumerate() {
        if i == 3 {
            *slot = b'-';
            continue;
        }
        let shift = (if i < 3 { i } else { i - 1 }) * 5;
        *slot = CODE_ALPHABET[((hash >> shift) & 0x1f) as usize];
    }
    String::from_utf8_lossy(&chars).into_owned()
}

/// Truncate on a character boundary, marking the cut. Character-based
/// rather than byte-based so a multi-byte script isn't split mid-glyph.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n\n_(truncated at {max} characters)_")
}

fn or_unrecorded(v: Option<&str>) -> String {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("`{s}`"),
        None => "_not recorded_".to_string(),
    }
}

/// Render the reported-answer section of the report.
///
/// Pure — no IO, no clock — so the wire format is pinnable in tests.
pub fn render_turn_section(turn: &TurnSnapshot) -> String {
    let mut out = String::new();
    let code = reference_code(&turn.message_id);

    out.push_str("## The answer being reported\n\n");
    out.push_str(&format!("- Reference: **{code}**\n"));
    out.push_str(&format!("- Conversation: `{}`\n", turn.conversation_id));
    out.push_str(&format!("- Message: `{}`\n\n", turn.message_id));

    out.push_str("### The question\n\n");
    match turn
        .question
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(q) => out.push_str(&format!(
            "> {}\n\n",
            clip(q, MAX_QUESTION_CHARS).replace('\n', "\n> ")
        )),
        None => out.push_str("_(not recorded)_\n\n"),
    }

    out.push_str("### The answer\n\n");
    match turn
        .answer
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(a) => {
            // Fenced rather than quoted: answers contain markdown, and
            // a quoted heading would restyle the whole report.
            out.push_str("```\n");
            out.push_str(&clip(a, MAX_ANSWER_CHARS));
            out.push_str("\n```\n\n");
        }
        None => out.push_str("_(not recorded)_\n\n"),
    }

    out.push_str("### How this answer was produced\n\n");
    out.push_str(&format!(
        "- Route: {}\n",
        or_unrecorded(turn.route.as_deref())
    ));
    out.push_str(&format!(
        "- Classified as: {}\n",
        or_unrecorded(turn.coarse_intent.as_deref())
    ));
    if let Some(why) = turn
        .routing_trigger
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        out.push_str(&format!("- Router's reason: {why}\n"));
    }
    out.push_str(&format!(
        "- Retrieval: {}\n",
        or_unrecorded(turn.search_method.as_deref())
    ));
    out.push_str(&format!(
        "- Answered by: {}\n",
        or_unrecorded(turn.answered_by.as_deref())
    ));
    match turn.latency_ms {
        Some(ms) => out.push_str(&format!("- Took: `{:.1}s`\n", ms as f64 / 1000.0)),
        None => out.push_str("- Took: _not recorded_\n"),
    }
    match turn.tokens_used {
        Some(t) => out.push_str(&format!("- Tokens: `{t}`\n")),
        None => out.push_str("- Tokens: _not recorded_\n"),
    }
    if let Some(fr) = turn
        .finish_reason
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        // Call the truncation case out in words. `finish_reason: length`
        // means nothing to the person filing the report, and it is the
        // explanation for a large share of "it stopped mid-sentence".
        if fr == "length" {
            let budget = turn
                .max_tokens_budget
                .map(|b| format!(" ({b}-token limit)"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- Stopped because: **it hit the response-length limit{budget}** — the reply was cut off, not finished\n"
            ));
        } else {
            out.push_str(&format!("- Stopped because: `{fr}`\n"));
        }
    }
    if let Some(g) = turn.gate_action.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("- Grounding gate: `{g}`\n"));
    }
    if let Some(c) = turn
        .coverage_note
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        out.push_str(&format!("- Coverage note shown to the user: {c}\n"));
    }
    out.push('\n');

    out.push_str("### Where it looked\n\n");
    if turn.sources.is_empty() {
        out.push_str("_(no knowledge base contributed to this answer)_\n\n");
    } else {
        for s in &turn.sources {
            let name = s.display_name.as_deref().unwrap_or(&s.origin);
            let peer = s
                .from_peer
                .as_deref()
                .map(|p| format!(" — searched on peer `{p}`"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **{name}** (`{}`): {} passage{}{peer}\n",
                s.origin,
                s.count,
                if s.count == 1 { "" } else { "s" }
            ));
        }
        out.push('\n');
    }

    out.push_str("### What it read\n\n");
    if turn.retrieved.is_empty() {
        out.push_str("_(nothing was retrieved for this turn)_\n\n");
        return out;
    }
    let shown = turn.retrieved.len().min(MAX_RETRIEVED);
    for r in turn.retrieved.iter().take(shown) {
        let corpus = r
            .corpus_id
            .as_deref()
            .map(|c| format!(" — `{c}`"))
            .unwrap_or_default();
        let chunk = r.chunk_id.map(|c| format!(" #{c}")).unwrap_or_default();
        out.push_str(&format!("- {}{corpus}{chunk}\n", r.title));
        // The gate, applied here rather than at the call site: a bug
        // that populates `snippet` without consent must not be able to
        // leak the user's documents through this renderer.
        if turn.include_source_text {
            if let Some(sn) = r
                .snippet
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                for line in clip(sn, MAX_SNIPPET_CHARS).lines() {
                    out.push_str(&format!("  > {line}\n"));
                }
            }
        }
    }
    if turn.retrieved.len() > shown {
        out.push_str(&format!(
            "- _(+{} more not listed)_\n",
            turn.retrieved.len() - shown
        ));
    }
    if !turn.include_source_text {
        out.push_str(
            "\n_The reporter did not include the text of these passages — only which \
             documents were consulted._\n",
        );
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> TurnSnapshot {
        TurnSnapshot {
            conversation_id: "conv-1".into(),
            message_id: "3f7c1e2a-0000-4000-8000-000000000001".into(),
            question: Some("who wrote the Phenomenology?".into()),
            answer: Some("Hegel, in 1807.".into()),
            route: Some("KnowledgeQuery".into()),
            coarse_intent: Some("LOOKUP".into()),
            routing_trigger: Some("factual-lookup shape".into()),
            search_method: Some("CorpusEngine".into()),
            answered_by: Some("Darwin-36B".into()),
            latency_ms: Some(4200),
            tokens_used: Some(931),
            finish_reason: Some("stop".into()),
            max_tokens_budget: Some(2048),
            gate_action: Some("citation_grounded".into()),
            coverage_note: None,
            sources: vec![TurnSource {
                origin: "sep".into(),
                count: 4,
                display_name: Some("Stanford Encyclopedia".into()),
                from_peer: None,
            }],
            retrieved: vec![RetrievedRef {
                title: "Hegel's Dialectics".into(),
                corpus_id: Some("sep".into()),
                chunk_id: Some(42),
                snippet: Some("The Phenomenology of Spirit (1807) …".into()),
            }],
            include_source_text: false,
        }
    }

    /// covers: UI-31
    ///
    /// Two halves of the clause at once: SPEAKABLE (the alphabet excludes the
    /// glyphs that can be misheard) and PINNED (the value itself is asserted, so
    /// a change to the derivation breaks the build rather than the user's
    /// screenshot).
    #[test]
    fn reference_code_is_stable_and_speakable() {
        let a = reference_code("3f7c1e2a-0000-4000-8000-000000000001");
        // Pinned: this value is a wire format. A change here breaks the
        // match between a user's screenshot and their report file.
        assert_eq!(a, "2AM-QSC", "reference code drifted — see fn docs");
        assert_eq!(a.len(), 7);
        assert_eq!(&a[3..4], "-");
        for c in a.chars().filter(|c| *c != '-') {
            assert!(
                CODE_ALPHABET.contains(&(c as u8)),
                "code must avoid ambiguous glyphs, got {c}"
            );
        }
    }

    /// covers: UI-31
    ///
    /// "Derived from the message identity": the same id gives the same code, and
    /// two ids do not collide. A code that does not distinguish turns is not a
    /// reference to anything.
    #[test]
    fn reference_code_is_deterministic_and_distinguishes_turns() {
        let a = reference_code("message-a");
        assert_eq!(a, reference_code("message-a"));
        assert_ne!(a, reference_code("message-b"));
    }

    /// covers: UI-32
    ///
    /// The clause's load-bearing half, and the reason it says RENDERER rather than
    /// caller: the fixture carries a snippet AND withholds consent — the exact
    /// shape a frontend bug produces — and the text must still not appear.
    #[test]
    fn source_text_is_withheld_unless_the_reporter_opted_in() {
        // The snapshot carries a snippet AND withholds consent — the
        // exact shape a frontend bug would produce. The renderer, not
        // the caller, is what keeps the document out of the report.
        let s = snap();
        assert!(!s.include_source_text);
        let out = render_turn_section(&s);
        assert!(
            !out.contains("Phenomenology of Spirit (1807)"),
            "passage text leaked without consent:\n{out}"
        );
        assert!(
            out.contains("Hegel's Dialectics"),
            "title should still appear"
        );
        assert!(out.contains("did not include the text"));
    }

    /// covers: UI-32
    ///
    /// The other side of the gate. Without this, withholding everything
    /// unconditionally would satisfy the test above, and the opt-in would be a
    /// switch wired to nothing.
    #[test]
    fn source_text_appears_when_the_reporter_opted_in() {
        let mut s = snap();
        s.include_source_text = true;
        let out = render_turn_section(&s);
        assert!(out.contains("The Phenomenology of Spirit (1807)"));
        assert!(!out.contains("did not include the text"));
    }

    /// covers: UI-31
    ///
    /// "EACH report MUST carry" it. Until this tag the render was never asserted
    /// to print the code at all — `reference_code` was tested as a function and
    /// the wire format stopped at its return value.
    #[test]
    fn a_thin_snapshot_still_renders_without_guessing() {
        // Everything a legacy or errored turn cannot supply. The report
        // must still generate — this user is the one who most needs it.
        let s = TurnSnapshot {
            conversation_id: "c".into(),
            message_id: "m".into(),
            question: None,
            answer: None,
            route: None,
            coarse_intent: None,
            routing_trigger: None,
            search_method: None,
            answered_by: None,
            latency_ms: None,
            tokens_used: None,
            finish_reason: None,
            max_tokens_budget: None,
            gate_action: None,
            coverage_note: None,
            sources: Vec::new(),
            retrieved: Vec::new(),
            include_source_text: false,
        };
        let out = render_turn_section(&s);
        assert!(out.contains("_not recorded_") || out.contains("_(not recorded)_"));
        assert!(out.contains("no knowledge base contributed"));
        assert!(out.contains("nothing was retrieved"));
        // EVERY report carries the code — including this one, where nothing
        // else could be recorded. A user with a broken turn is the one most
        // likely to be reading a code out loud to somebody.
        assert!(
            out.contains(&format!("Reference: **{}**", reference_code("m"))),
            "the report must carry the reference code:\n{out}"
        );
    }

    #[test]
    fn truncation_is_visible_rather_than_silent() {
        let mut s = snap();
        s.answer = Some("x".repeat(MAX_ANSWER_CHARS + 500));
        let out = render_turn_section(&s);
        assert!(
            out.contains("truncated at"),
            "a clipped answer must say so, or it reads as a model that stopped early"
        );
    }

    #[test]
    fn length_truncation_is_explained_in_words() {
        let mut s = snap();
        s.finish_reason = Some("length".into());
        let out = render_turn_section(&s);
        assert!(out.contains("response-length limit"));
        assert!(out.contains("2048-token limit"));
    }

    #[test]
    fn peer_attribution_survives_into_the_report() {
        // "It answered from the wrong machine" is unfalsifiable without
        // this line, and it is a top-three complaint on a fresh mesh.
        let mut s = snap();
        s.sources[0].from_peer = Some("mac-peer".into());
        let out = render_turn_section(&s);
        assert!(out.contains("searched on peer `mac-peer`"));
    }

    /// covers: UI-32
    ///
    /// "Defaulted off." A payload that never mentions consent deserializes to
    /// withheld, so an older frontend cannot opt the user in by omission.
    #[test]
    fn snapshot_deserializes_from_a_minimal_frontend_payload() {
        // The frontend sends what it has. A turn whose metadata predates
        // half these fields must not fail to report.
        let v: TurnSnapshot =
            serde_json::from_str(r#"{"conversation_id":"c","message_id":"m"}"#).unwrap();
        assert_eq!(v.message_id, "m");
        assert!(!v.include_source_text);
        assert!(v.retrieved.is_empty());
    }
}
