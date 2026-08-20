// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation frames — what a conversation established, carried past
//! the point where its verbatim turns roll out of the prompt window.
//!
//! This is the payload of the dropped-history compaction channel. It
//! replaced a single ≤120-word prose preamble on 2026-07-26, for two
//! reasons that are worth keeping straight because only the first is
//! about cost:
//!
//! 1. **A prose blob has to be re-narrated to be updated.** Every fold
//!    asked the model to rewrite the whole summary, and re-narration is
//!    where named entities get dropped. Named sections are updated by
//!    replacing only what changed, so a fold cannot quietly forget
//!    "Marie Curie" while restating the topic.
//! 2. **A blob is not renderable.** "What do you remember about this
//!    conversation?" can be answered from sections; it cannot be
//!    answered from a paragraph without handing the user the paragraph.
//!
//! The container mechanics (parse, upsert, render, budget) are shared
//! with session frames via [`sovereign_contracts::frame`]. What lives
//! here is the section vocabulary, the fold prompt, and — the one place
//! this path deliberately diverges from the session path — budget
//! ENFORCEMENT rather than budget rejection.
//!
//! ## Why this writer trims where the session writer rejects
//!
//! `session_state` rejects an over-budget write and reports per-section
//! counts, because its writer is an agent that can read the message and
//! trim deliberately. This writer is a small model summarising turns it
//! did not author; there is nobody to hand a rejection to, and a
//! rejected fold would silently stall the watermark and re-fold the same
//! turns forever. So [`enforce_budget`] trims the largest sections until
//! the document fits, and says so in a trace.

use sovereign_contracts::frame::{approx_tokens, Frame, FrameSchema};

use crate::error::Result;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Message, Role};

/// Frame sections, in render order.
///
/// Chosen so that each slot answers a question a user actually asks a
/// long conversation, and so that a small model can tell them apart in
/// one pass:
///
/// * `Topics` — "what have we covered?"
/// * `Entities` — the proper nouns coreference needs to resolve
///   ("that element she discovered")
/// * `Stated goals` — what the user said they are trying to do, and how
///   they want answers ("shorter, please"), which is the highest-value
///   thing to never forget
/// * `Commitments` — what the assistant promised, so a dropped promise
///   is visible rather than merely forgotten
/// * `Open threads` — questions raised and not answered
pub const CONV_FRAME_SECTIONS: [&str; 5] = [
    "Topics",
    "Entities",
    "Stated goals",
    "Commitments",
    "Open threads",
];

/// Token cap on the rendered frame.
///
/// This document rides EVERY prompt on a long conversation, so the
/// budget is prompt real estate, not storage. It replaces a preamble
/// that was ≤120 words (~160 tokens); 320 buys five sections' worth of
/// structure for roughly one extra sentence of cost, against a
/// conversation-memory channel budget of ~2.8k tokens total.
pub const CONV_FRAME_TOKEN_BUDGET: usize = 320;

/// The conversation frame's contract.
pub const CONV_FRAME_SCHEMA: FrameSchema = FrameSchema {
    schema_id: "conversation-frame/v1",
    sections: &CONV_FRAME_SECTIONS,
    token_budget: CONV_FRAME_TOKEN_BUDGET,
};

/// Frontmatter key holding the fold watermark: the number of LEADING
/// conversation messages this frame accounts for. Stored in the document
/// so persistence is one column and a resumed conversation keeps folding
/// incrementally.
pub const FRONT_COVERED_UPTO: &str = "covered_upto";

/// Frontmatter key counting messages inside the covered range that no
/// fold ever read (a fold window hit [`crate::runtime::CONV_COMPACT_MAX_FOLD_MSGS`]).
/// Non-zero means the frame is knowingly partial.
pub const FRONT_ELIDED: &str = "elided";

/// Parse a stored frame document, or build an empty one.
pub fn parse(stored: Option<&str>) -> Frame {
    match stored {
        Some(text) if !text.trim().is_empty() => CONV_FRAME_SCHEMA.parse(text),
        _ => {
            let mut f = CONV_FRAME_SCHEMA.empty();
            f.set("schema", CONV_FRAME_SCHEMA.schema_id.to_string());
            f.set(FRONT_COVERED_UPTO, "0".to_string());
            f.set(FRONT_ELIDED, "0".to_string());
            f
        }
    }
}

/// The fold watermark, or 0 when absent/unparseable. A corrupt watermark
/// reads as 0, which costs one cold fold — never a wrong answer.
pub fn covered_upto(frame: &Frame) -> usize {
    frame
        .get(FRONT_COVERED_UPTO)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Messages inside the covered range that no fold ever read.
pub fn elided(frame: &Frame) -> usize {
    frame
        .get(FRONT_ELIDED)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// Bring a frame inside its budget by trimming the largest sections
/// first. Returns the sections that were trimmed, for the trace.
///
/// "Drop detail, never sections" is preserved: a trimmed section keeps
/// its opening content and gains an explicit `…` so a reader (human or
/// model) can see something was cut, rather than inferring completeness
/// from a clean-looking body.
pub fn enforce_budget(frame: &mut Frame) -> Vec<String> {
    let mut trimmed = Vec::new();
    // Bounded loop: each pass halves the largest section, so a
    // pathological input converges instead of spinning.
    for _ in 0..32 {
        if frame.check_budget(&CONV_FRAME_SCHEMA).is_ok() {
            break;
        }
        let Some((section, len)) = frame
            .bodies
            .iter()
            .map(|(n, b)| (n.clone(), b.chars().count()))
            .max_by_key(|(_, len)| *len)
        else {
            break;
        };
        if len == 0 {
            break;
        }
        let body = frame.body(&section).unwrap_or_default().to_string();
        let keep = (len / 2).max(1);
        let mut end = body
            .char_indices()
            .nth(keep)
            .map(|(i, _)| i)
            .unwrap_or(body.len());
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        frame.set_body(&section, format!("{}…", body[..end].trim_end()));
        if !trimmed.contains(&section) {
            trimmed.push(section);
        }
    }
    trimmed
}

/// Fold `newly_dropped` into `stored`, returning the rendered document.
///
/// Incremental by construction: the prompt carries the current frame
/// plus only the new turns, so its size is set by what changed, not by
/// how long the conversation has run. Callers keep `newly_dropped`
/// bounded (see `context::fold_window`) and pass `elided_before` for the
/// messages they chose not to show.
///
/// Soft-fail by design, at every step: an inference error, an
/// unparseable reply, or an empty update all leave the stored frame
/// unchanged and return `Ok(None)`. The caller then keeps using the
/// frame it already had — a fold that fails must never cost the
/// conversation its memory.
pub async fn fold(
    inference: &dyn InferenceProvider,
    stored: Option<&str>,
    newly_dropped: &[Message],
    elided_before: usize,
    covered_after: usize,
) -> Result<Option<String>> {
    if newly_dropped.is_empty() {
        return Ok(None);
    }
    let mut frame = parse(stored);
    let existing = frame.render_for_prompt();

    let transcript: String = newly_dropped
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            let mut end = m.content.len().min(400);
            while end > 0 && !m.content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{role}: {}", &m.content[..end])
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Parsimonious on purpose: this runs on the Fast slot, and long
    // multi-clause instructions destabilise small models more than they
    // steer them. The section semantics are carried by the slot names
    // and one clause each.
    let elision_note = if elided_before > 0 {
        format!(" {elided_before} even earlier messages were never shown to you; do not imply the record is complete.")
    } else {
        String::new()
    };
    let notes_block = if existing.trim().is_empty() {
        "(nothing recorded yet)".to_string()
    } else {
        existing.clone()
    };

    let prompt = format!(
        "You maintain running notes on a conversation. Below are the \
         current notes, then new turns from that conversation.\n\n\
         Return ONLY the note sections that the new turns CHANGE. Leave \
         out any section that should stay as it is. Add to a section \
         rather than rewriting it — never drop a name or a stated \
         preference that is already recorded.{elision_note}\n\n\
         Sections:\n\
         - topics: subjects discussed, most recent last\n\
         - entities: people, places, works, and things named\n\
         - stated_goals: what the user said they want, including how they \
         want answers written\n\
         - commitments: what the assistant promised to do\n\
         - open_threads: questions raised but not answered\n\n\
         Current notes:\n{notes_block}\n\n\
         New turns:\n{transcript}\n\n\
         Reply with JSON only, omitting unchanged sections:\n\
         {{\"topics\": \"…\", \"entities\": \"…\"}}"
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "topics": {"type": "string"},
            "entities": {"type": "string"},
            "stated_goals": {"type": "string"},
            "commitments": {"type": "string"},
            "open_threads": {"type": "string"},
        },
    });

    // SLOT_POLICY §3 Housekeep: conversation-frame fold.
    let mut request = Workload::Housekeep.request(prompt).with_output_budget(400);
    request.temperature = Some(0.0);
    request.structured_output = Some(schema);

    let response = inference.complete(&request).await?;
    let raw = response.text.trim();
    let json_str = raw
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(raw)
        .trim();

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, raw = %raw, "conv_frame: fold reply did not parse");
            return Ok(None);
        }
    };
    let Some(obj) = parsed.as_object() else {
        tracing::debug!(raw = %raw, "conv_frame: fold reply was not an object");
        return Ok(None);
    };

    let mut updated: Vec<String> = Vec::new();
    for (key, value) in obj {
        let Some(section) = CONV_FRAME_SCHEMA.canonical_section(key) else {
            tracing::debug!(key = %key, "conv_frame: fold reply named an unknown section");
            continue;
        };
        let Some(body) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        frame.set_body(section, body.to_string());
        updated.push(section.to_string());
    }
    if updated.is_empty() {
        // The model saw nothing worth recording. Advancing the watermark
        // anyway is correct and important: these turns HAVE been
        // considered, and re-offering them next turn would re-pay the
        // fold forever on a conversation of pleasantries.
        tracing::debug!("conv_frame: fold changed no section; advancing watermark only");
    }

    frame.set(FRONT_COVERED_UPTO, covered_after.to_string());
    frame.set(FRONT_ELIDED, (elided(&frame) + elided_before).to_string());
    let trimmed = enforce_budget(&mut frame);
    if !trimmed.is_empty() {
        tracing::info!(
            trimmed = %trimmed.join(","),
            budget = CONV_FRAME_TOKEN_BUDGET,
            "conv_frame: frame over budget — trimmed largest sections"
        );
    }

    let rendered = frame.render();
    tracing::info!(
        folded_msgs = newly_dropped.len(),
        sections_updated = %updated.join(","),
        covered_upto = covered_after,
        elided_before,
        approx_tokens = approx_tokens(&rendered),
        "conv_frame: folded"
    );
    Ok(Some(rendered))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watermark_round_trips_through_the_document() {
        let mut f = parse(None);
        assert_eq!(covered_upto(&f), 0);
        f.set(FRONT_COVERED_UPTO, "72".into());
        f.set_body("Topics", "polonium".into());

        let reparsed = parse(Some(&f.render()));
        assert_eq!(
            covered_upto(&reparsed),
            72,
            "the watermark must survive storage — it IS the persistence contract"
        );
        assert_eq!(reparsed.body("Topics").map(str::trim), Some("polonium"));
    }

    #[test]
    fn corrupt_watermark_reads_as_zero_not_as_garbage() {
        let mut f = parse(None);
        f.set(FRONT_COVERED_UPTO, "not-a-number".into());
        assert_eq!(
            covered_upto(&f),
            0,
            "an unparseable watermark must cost a cold fold, never a wrong offset"
        );
    }

    #[test]
    fn enforce_budget_trims_the_largest_section_and_marks_the_cut() {
        let mut f = parse(None);
        f.set_body("Topics", "t".repeat(4000));
        f.set_body("Entities", "Marie Curie".into());

        let trimmed = enforce_budget(&mut f);
        assert!(trimmed.contains(&"Topics".to_string()));
        assert!(
            f.check_budget(&CONV_FRAME_SCHEMA).is_ok(),
            "enforcement must actually converge"
        );
        assert!(
            f.body("Topics").unwrap().contains('…'),
            "a trimmed section must show that it was cut"
        );
        assert_eq!(
            f.body("Entities"),
            Some("Marie Curie"),
            "trimming takes from the biggest section, not from every section"
        );
    }

    #[test]
    fn prompt_form_carries_sections_without_frontmatter() {
        let mut f = parse(None);
        f.set_body("Stated goals", "wants short answers".into());
        let p = f.render_for_prompt();
        assert!(p.contains("Stated goals: wants short answers"));
        assert!(!p.contains("covered_upto"));
    }
}
