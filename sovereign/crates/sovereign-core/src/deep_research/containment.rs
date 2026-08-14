// SPDX-License-Identifier: AGPL-3.0-or-later
//! The C-class containment witness — the FR-6 REDESIGN half.
//!
//! The FR-6 measurement found the dual-string premise dead and the
//! residual failure shape to be shared world-knowledge bias: the judge
//! marks a claim `supported` because the fact is in the *model*, not in
//! the evidence. The redesign (directive c45d8625; decision record
//! `research/deep-research/notes/gate-redesign.md`): single-string judge
//! + this containment witness — a deterministic presence check over the
//! claim's extracted checkable specifics, generalized from the in-tree
//! `value_present_in_chunks` / absent-name-attribution / absent-
//! identifier-attribution vetoes.
//!
//! Contract (gate-redesign.md §2-§4):
//! - trigger: judge-`supported` claims only;
//! - extraction: LLM specifics (I-class instrument — an extraction error
//!   costs a witness *miss*, never a false pass), tiny budget, `NONE`
//!   sentinel;
//! - presence: reuse `value_present_in_chunks` per specific (one
//!   matcher, ARCH §10.6);
//! - verdict effect: all extracted specifics absent from the evidence →
//!   `supported` downgrades to `could-not-judge`, recorded. The witness
//!   only downgrades; it never upgrades.
//!
//! The witness is composed by the loop's audit path
//! (`deep_research/audit.rs`); it does not edit the judge.

use crate::oicp::ShardingPrivacy;
use crate::runtime::grounding::value_present_in_chunks;
use crate::traits::InferenceProvider;
use std::sync::Arc;

/// The witness's configuration — frozen in the run charter at launch
/// (FR-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainmentConfig {
    pub extraction_max_tokens: u32,
    pub specifics_max: usize,
}

impl Default for ContainmentConfig {
    fn default() -> Self {
        ContainmentConfig {
            extraction_max_tokens: 32,
            specifics_max: 4,
        }
    }
}

/// The witness's outcome for one claim.
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessOutcome {
    /// True when the witness actually ran (trigger met + specifics
    /// extracted). False = not witnessable — the claim keeps the judge's
    /// verdict.
    pub ran: bool,
    /// The extracted checkable specifics (witnessable ones).
    pub specifics: Vec<String>,
    /// All witnessable specifics absent from the evidence → the
    /// downgrade condition.
    pub all_absent: bool,
    /// Why the witness did not run (trigger not met / no specifics /
    /// extraction failed).
    pub reason: Option<String>,
}

impl WitnessOutcome {
    pub fn not_witnessable(reason: impl Into<String>) -> Self {
        WitnessOutcome {
            ran: false,
            specifics: Vec::new(),
            all_absent: false,
            reason: Some(reason.into()),
        }
    }

    pub fn no_downgrade(specifics: Vec<String>) -> Self {
        WitnessOutcome {
            ran: true,
            specifics,
            all_absent: false,
            reason: None,
        }
    }
}

/// The artifact nouns whose bare presence proves nothing (the
/// absent-name-attribution refusal guard, judge.rs) — a specific that is
/// exactly one of these is unwitnessable.
const ARTIFACT_WORDS: &[&str] = &[
    "email",
    "letter",
    "memo",
    "message",
    "document",
    "passage",
    "chapter",
    "section",
    "thread",
    "forwarded",
    "sent",
    "wrote",
    "authored",
    "signed",
    "replied",
];

/// The extraction prompt — parsimonious, temp 0, tiny budget. The
/// `NONE` sentinel is the no-specifics answer.
fn extraction_prompt(claim: &str, specifics_max: usize) -> (String, String) {
    (
        "Extract the checkable specifics from the claim: named entities, dates, figures, \
         causal links. Output each specific on its own line. If the claim has no checkable \
         specifics, output exactly: NONE"
            .to_string(),
        format!("Claim: {claim}\nSpecifics (max {specifics_max}):"),
    )
}

/// Strip `[Source: …]` citation spans from text — mirrors the judge's
/// `strip_citation_spans` contract (judge.rs:998). Deliberately a local
/// implementation (the judge's is `pub(super)`); the fixture test below
/// pins this one to the documented behavior (drop the span, keep the
/// rest).
pub fn strip_citation_spans(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[Source:") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        if let Some(end) = after.find(']') {
            rest = &after[end + 1..];
        } else {
            // Unterminated span: strip to end-of-line (the judge's
            // bounded-bracket contract — grounding/judge.rs:997).
            rest = after.find('\n').map(|e| &after[e..]).unwrap_or("");
        }
    }
    out.push_str(rest);
    out
}

/// A line is heading-shaped when it is short, title-like, and carries no
/// sentence-final punctuation — the shape that proves nothing about the
/// evidence body (the hand-run's "budget forcing" lesson: the technique
/// name appeared in a heading, never in the body).
fn is_heading_shaped(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.chars().count() > 80 {
        return false;
    }
    let no_sentence_end = !line.ends_with('.') && !line.ends_with('!') && !line.ends_with('?');
    let not_continuation = !line.ends_with(',') && !line.ends_with(';');
    no_sentence_end && not_continuation
}

/// Does the specific appear in any non-heading (body) line? Occurrences
/// inside heading-shaped lines do not count — a heading proves nothing
/// about the evidence body.
fn appears_in_body(specific: &str, evidence: &str) -> bool {
    evidence.lines().any(|line| {
        if is_heading_shaped(line) {
            false
        } else {
            line.contains(specific)
        }
    })
}

/// The witness's deterministic presence check over the extracted
/// specifics (C-class — the same matcher discipline as
/// `value_present_in_chunks`: whole specifics, never raw nouns).
pub fn witness_presence(specifics: &[String], evidence: &[String]) -> bool {
    let stripped: Vec<String> = evidence.iter().map(|e| strip_citation_spans(e)).collect();
    let joined = stripped.join("\n");
    // One matcher: value_present_in_chunks, per specific, against the
    // stripped evidence. A specific that only ever appears inside
    // heading-shaped lines counts as absent.
    let present = |s: &str| value_present_in_chunks(s, &stripped) && appears_in_body(s, &joined);
    specifics.iter().any(|s| present(s))
}

/// Is this specific witnessable? A bare artifact noun proves nothing
/// (its presence is not evidence about the claim's content). The
/// artifact-word test matches any whitespace-delimited token, so
/// "The Email" (a determiner + artifact noun) is as unwitnessable as
/// the bare noun — both are presence-proof vacuities.
fn is_witnessable(specific: &str) -> bool {
    let low = specific.trim().to_lowercase();
    !low.is_empty()
        && !low.split_whitespace().any(|w| {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric());
            ARTIFACT_WORDS.contains(&w)
        })
}

/// Run the witness over one claim: extract specifics (I-class) then
/// check presence (C-class). `chunks` is the evidence window.
pub async fn containment_witness(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[String],
    config: &ContainmentConfig,
    posture: ShardingPrivacy,
) -> WitnessOutcome {
    if claim.trim().is_empty() || chunks.is_empty() {
        return WitnessOutcome::not_witnessable("trigger not met: empty claim or evidence window");
    }
    let (system, user) = extraction_prompt(claim, config.specifics_max);
    let req = crate::types::CompletionRequest {
        prompt: user,
        system_message: Some(system),
        max_tokens: Some(config.extraction_max_tokens as usize),
        temperature: Some(0.0),
        ..Default::default()
    };
    let raw = match inference.complete(&req).await {
        Ok(r) => r,
        Err(e) => {
            return WitnessOutcome::not_witnessable(format!("extraction failed: {e}"));
        }
    };
    let mut specifics: Vec<String> = raw
        .text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .take(config.specifics_max)
        .collect();
    // The NONE sentinel.
    if specifics.iter().any(|s| s.eq_ignore_ascii_case("none")) {
        specifics.clear();
    }
    let witnessable: Vec<String> = specifics
        .into_iter()
        .filter(|s| is_witnessable(s))
        .collect();
    if witnessable.is_empty() {
        return WitnessOutcome::not_witnessable(
            "no checkable specifics (NONE sentinel or unwitnessable shapes)",
        );
    }
    if witness_presence(&witnessable, chunks) {
        WitnessOutcome::no_downgrade(witnessable)
    } else {
        WitnessOutcome {
            ran: true,
            specifics: witnessable,
            all_absent: true,
            reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_span_strip_matches_the_judge_contract() {
        let text = "The bridge was completed in 1873 [Source: web-1]. Its span is 240 meters [Source: web-2].";
        assert_eq!(
            strip_citation_spans(text),
            "The bridge was completed in 1873 . Its span is 240 meters ."
        );
        let unterminated = "text [Source: web-1";
        assert_eq!(strip_citation_spans(unterminated), "text ");
        let plain = "no citations here.";
        assert_eq!(strip_citation_spans(plain), plain);
    }

    #[test]
    fn presence_is_whole_specifics_not_raw_nouns() {
        let evidence = vec![
            "The Meridian Bridge across the Selune river was completed in 1873 by the engineer Helena Voss.".to_string(),
        ];
        // Whole specific present → present.
        assert!(witness_presence(&["1873".to_string()], &evidence));
        assert!(witness_presence(&["Helena Voss".to_string()], &evidence));
        // Absent specific → absent.
        assert!(!witness_presence(
            &["Larkhall viaduct".to_string()],
            &evidence
        ));
    }

    #[test]
    fn heading_occurrences_do_not_count() {
        let evidence = vec![
            "Budget Forcing\n\nThe system card describes budget forcing as a technique for constraining model spend.".to_string(),
        ];
        // "Budget Forcing" appears only as the heading line.
        assert!(!witness_presence(
            &["Budget Forcing".to_string()],
            &evidence
        ));
        // The body phrase is present.
        assert!(witness_presence(
            &["constraining model spend".to_string()],
            &evidence
        ));
    }

    #[test]
    fn artifact_words_are_unwitnessable() {
        assert!(!is_witnessable("email"));
        assert!(!is_witnessable("The Email"));
        assert!(is_witnessable("Helena Voss"));
        assert!(is_witnessable("completed in 1873"));
    }
}
