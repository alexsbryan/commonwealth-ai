// SPDX-License-Identifier: AGPL-3.0-or-later
//! Value-presence: the gold-free groundedness primitive.
//!
//! The question that matters for honesty is not "did the agent abstain?" (an
//! ACTION, and a proxy) but "does the answer assert a specific — a name, place,
//! or number — that appears nowhere in the retrieved evidence?" (a PROPERTY).
//! That property is *blatant confabulation*: a value invented from nothing. A
//! value that IS present, even mis-roled ("Vladimir" given as Mr Vladimir's
//! first name) or implied (the "Karl" inside "Karl Yundt"), is the system's best
//! effort, not a fabrication — we release it.
//!
//! Crucially this needs NO gold label. It reads only the answer and the
//! evidence, so the SAME assessment serves three consumers at the same layer:
//!   * the grounding gate, to DECIDE (release vs abstain);
//!   * the chaos scorer, to MEASURE (`blatant_confab_rate`);
//!   * (later) production telemetry / the desktop glassbox, to MONITOR / SHOW.
//! One notion of "is this asserted value grounded," one implementation.
//!
//! Two steps, deliberately split by what each is good at: an LLM extracts the
//! answer's value (a short noun phrase — the one judgment a small model does
//! reliably), then a DETERMINISTIC substring test decides presence. The split
//! is load-bearing — a forced-choice judge asked the presence question directly
//! false-positived an absent "Thomas"; a substring test cannot. And it tests
//! token-PRESENCE ("is 'Karl' anywhere in the text?" — yes, inside "Karl
//! Yundt"), not role-INFERENCE ("does the text state his first name is Karl?"),
//! which is why it keeps competence a strict extractive check would lose.

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

use super::config::dbg;

/// The groundedness of the one specific value an answer asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssertedValue {
    /// The answer's value (what it offers in reply to the question) IS present in
    /// the evidence — correct, or a best-effort mis-role; not a fabrication.
    /// Release.
    Grounded(String),
    /// The answer's value is ABSENT from all evidence — blatant confabulation,
    /// invented from nothing (the qualifier "Russian" in "the Russian embassy",
    /// the invented "Vincent Macx"). Abstain.
    Ungrounded(String),
    /// No checkable specific was asserted (an abstention, a decline, or a
    /// discursive answer), or extraction was unavailable — nothing to ground.
    NoValue,
    // NOTE (2026-06-17): a measured detour. Extending this to check EVERY
    // specific the answer mentions (not just the answer value) exploded the
    // false-positive rate 0.09→0.40 — it swept in the model's framing (author
    // "Joseph Conrad", other works "Nostromo", the year "1907") and markdown
    // headings, none of which are corpus-world claims. "Answer value, checked
    // whole" is the right scope; "every noun" is not. Kept narrow on purpose.
}

/// Assess whether the value an `answer` gives in reply to `question` is present
/// in `chunks` — the single entry point shared by the gate (to DECIDE) and the
/// bench (to MEASURE). Scoped to the ANSWER VALUE (what the model offers as the
/// answer), checked whole — NOT every noun in the response, which would flag the
/// model's framing (author, other works, dates) as confabulation. The value is
/// checked across all its significant words, so the qualifier in "the Russian
/// embassy" is examined, not just the "Embassy" anchor.
pub async fn assess_asserted_value(
    inference: &dyn InferenceProvider,
    question: &str,
    answer: &str,
    chunks: &[String],
) -> AssertedValue {
    match extract_answer_value(inference, question, answer).await {
        Some(value) => {
            if value_present_in_chunks(&value, chunks) {
                AssertedValue::Grounded(value)
            } else {
                AssertedValue::Ungrounded(value)
            }
        }
        None => AssertedValue::NoValue,
    }
}

/// Extract the value the answer gives in reply to the question — the one step
/// that genuinely needs an LLM. `None` on a failed/empty extraction, or when the
/// answer asserts no value (a decline), so callers treat it as nothing-to-ground.
/// Scoped to the ANSWER (not every mentioned noun) and kept complete (the full
/// value with its qualifiers, e.g. "Russian embassy") so the deterministic
/// presence test can catch an invented qualifier, not just a missing headline.
async fn extract_answer_value(
    inference: &dyn InferenceProvider,
    question: &str,
    answer: &str,
) -> Option<String> {
    let prompt = format!(
        "QUESTION: {q}\nANSWER: {a}\n\n\
         Reply with only the specific value the ANSWER gives in reply to the \
         QUESTION — the complete value with its qualifiers (e.g. a full name, or a \
         place with its modifiers), but not the surrounding sentence. If the ANSWER \
         gives no specific value (it declines or says the sources don't state it), \
         reply with exactly NONE.",
        q = question.chars().take(300).collect::<String>(),
        a = answer.chars().take(300).collect::<String>(),
    );
    let req = CompletionRequest {
        prompt,
        system_message: Some("Extract only the answer's specific value, or NONE.".into()),
        preferred_speed: Speed::Medium,
        model_id: Some("primary".into()),
        max_tokens: Some(24),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match inference.complete(&req).await {
        Ok(resp) => {
            let v = resp.text.trim().trim_matches('"').trim();
            let low = v.to_lowercase();
            // A declined / empty / no-value extraction is nothing to ground.
            if v.is_empty()
                || low == "none"
                || low.starts_with("none")
                || low.starts_with("n/a")
                || low.starts_with("not ")
                || low.starts_with("unknown")
            {
                None
            } else {
                Some(v.to_string())
            }
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "value extraction failed");
            dbg(&format!("value extraction failed: {e}"));
            None
        }
    }
}

/// Deterministic token-presence: are ALL significant words of `value` present
/// (case-insensitive substring) in the chunks? Honorifics/titles/short function
/// words are dropped so "Mrs Verloc" matches on "Verloc" and "Sir Ethelred" on
/// "Ethelred"; a multi-part invention ("Vladimir Stepanovich Haldin") fails
/// because Stepanovich/Haldin are absent even though Vladimir is present. A value
/// invented from nothing is exactly-absent; a real corpus token (even mis-roled,
/// or the surname inside a full name carrying the asked-for part) is exactly-
/// present and released as best effort.
pub fn value_present_in_chunks(value: &str, chunks: &[String]) -> bool {
    const STOP: &[&str] = &[
        "mr", "mrs", "miss", "ms", "the", "of", "a", "an", "and", "sir", "dr",
        "comrade", "chief", "inspector", "lady", "lord", "saint", "st",
    ];
    let hay: String = chunks
        .join(" ")
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // A multi-word value that appears VERBATIM (a contiguous phrase) in the
    // evidence is grounded by definition — it is literally in the corpus. This
    // is the principled grounding case, and it catches correct answers that ARE
    // titles/ranks ("Chief Inspector" appears inside "Chief Inspector Heat"),
    // which the significant-word path below would otherwise drop to an empty set
    // (every word a stop-word) and mis-flag as a confabulation. Gated on ≥2
    // words so a bare honorific ("Mr") cannot self-ground.
    let nval: String = value.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    if nval.split(' ').filter(|w| !w.is_empty()).count() >= 2 && hay.contains(&nval) {
        return true;
    }
    let sig: Vec<String> = value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2 && !STOP.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_lowercase())
        .collect();
    !sig.is_empty() && sig.iter().all(|w| hay.contains(w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::value_present_in_chunks;

    /// One Conrad passage that names Yundt by his full name. The deterministic
    /// presence test must read "Karl" out of "Karl Yundt" — the inference a
    /// strict extractive judge refused, abstaining a correct answer.
    fn chunks() -> Vec<String> {
        vec![
            "Karl Yundt giggled grimly, and Comrade Alexander Ossipon, nicknamed the \
             Doctor, sat near Mr Verloc. Sir Ethelred received the Assistant \
             Commissioner. Mrs Verloc's mother was privileged to sit."
                .to_string(),
        ]
    }

    #[test]
    fn releases_implied_correct_value() {
        // "Karl" lives inside "Karl Yundt" → competence preserved.
        assert!(value_present_in_chunks("Karl", &chunks()));
    }

    #[test]
    fn releases_value_present_but_misroled() {
        // Best effort, the real bar: a real corpus token, even if the role is
        // wrong, is not a blatant fabrication.
        assert!(value_present_in_chunks("Mrs. Verloc", &chunks()));
        assert!(value_present_in_chunks("Sir Ethelred", &chunks()));
        assert!(value_present_in_chunks("Alexander", &chunks()));
    }

    #[test]
    fn catches_value_invented_from_nothing() {
        // Blatant confabulation: the specific appears nowhere in the evidence.
        assert!(!value_present_in_chunks("Vernon", &chunks()));
        assert!(!value_present_in_chunks("Russian", &chunks()));
    }

    #[test]
    fn catches_multipart_invention_with_one_real_token() {
        let c = vec!["Mr Vladimir spoke to the First Secretary.".to_string()];
        // Vladimir present, Stepanovich/Haldin not → not all present.
        assert!(!value_present_in_chunks("Vladimir Stepanovich Haldin", &c));
        // The bare mis-roled token is released.
        assert!(value_present_in_chunks("Vladimir", &c));
    }

    #[test]
    fn honorifics_alone_do_not_count_as_present() {
        // A value that reduces to only stop-words must not be treated as grounded.
        assert!(!value_present_in_chunks("Mr.", &chunks()));
    }

    #[test]
    fn verbatim_multiword_value_is_grounded() {
        // B1: a correct answer that IS a title/rank ("Chief Inspector") appears
        // verbatim inside the corpus ("Chief Inspector Heat") → grounded, even
        // though every word is a stop-word and the significant-word path would
        // drop it to empty. The ≥2-word guard keeps a bare honorific out.
        let c = vec![
            "Chief Inspector Heat of the Special Crimes Department changed his tone.".to_string(),
        ];
        assert!(value_present_in_chunks("Chief Inspector", &c));
        // Not present verbatim AND a significant word ("russian") is absent → still caught.
        assert!(!value_present_in_chunks("Russian embassy", &c));
        // The ≥2-word guard: a bare honorific still does not self-ground.
        assert!(!value_present_in_chunks("Mr.", &c));
    }
}
