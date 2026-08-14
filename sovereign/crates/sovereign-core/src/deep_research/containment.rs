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
/// about the evidence body. Case-insensitive, matching the one
/// matcher's discipline (value_present_in_chunks) — the demo flight
/// recorded the case-gap: evidence "launched from pad 39A", claim
/// "Pad 39A": same fact, token-different, judged absent (directive
/// 6c25d88e, defect class 2).
fn appears_in_body(specific: &str, evidence: &str) -> bool {
    let specific = specific.to_lowercase();
    evidence.lines().any(|line| {
        if is_heading_shaped(line) {
            false
        } else {
            line.to_lowercase().contains(&specific)
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

/// Negative-claim markers — the lexical shapes that assert an absence
/// about the evidence ("none of the sources list X", "X is not
/// mentioned", "X is absent from the record"). Bounded, C-class,
/// matched case-insensitively as substrings. The demo flight recorded
/// the defect (directive 6c25d88e, class 3): "none of the provided
/// sources list the crew names" passed vacuously as an un-witnessable
/// negative claim.
const NEGATIVE_MARKERS: &[&str] = &[
    "none of the",
    "no source",
    "no sources",
    "no evidence",
    "no mention",
    "not list",
    "not mention",
    "never mention",
    "absent from",
    "nowhere in",
    "does not contain",
    "do not contain",
    "did not contain",
    "doesn't contain",
    "don't contain",
    "didn't contain",
];

/// Is the claim lexically asserting an absence about the evidence? For
/// such claims the presence test inverts: a present specific
/// contradicts the negation (downgrade); an absent one holds it (no
/// downgrade).
fn is_negative_claim(claim: &str) -> bool {
    let low = claim.to_lowercase();
    NEGATIVE_MARKERS.iter().any(|m| low.contains(m))
}

/// Strip a leading colon-terminated label phrase ("Date:", "Tensile
/// strength:", "Number:") — the extraction reshape class the T1a read
/// quantified (ap-02/ap-03/ap-05: "412 megapascals" → "Tensile
/// strength: 412 MPa"). C-class and bounded: one alphabetic label
/// phrase of ≤32 chars; a URL's `//` guard keeps scheme prefixes
/// intact.
fn strip_label_prefix(specific: &str) -> &str {
    let specific = specific.trim();
    let Some(colon) = specific.find(':') else {
        return specific;
    };
    let (label, rest) = specific.split_at(colon);
    let rest = rest[1..].trim_start();
    let label = label.trim();
    if !label.is_empty()
        && label.chars().count() <= 32
        && label
            .chars()
            .all(|c| c.is_alphabetic() || c.is_whitespace())
        && !rest.starts_with("//")
    {
        rest
    } else {
        specific
    }
}

/// The anchor filter: a specific the claim does not assert cannot flip
/// the witness. Case-insensitive containment in the claim text, after
/// label stripping — the phantom class ("Date: 1973 (inauguration)",
/// present in neither claim nor evidence) drops here instead of
/// becoming an all-absent false downgrade (directive 6c25d88e, class
/// 1).
fn is_anchored_in(specific: &str, claim: &str) -> bool {
    !specific.is_empty() && claim.to_lowercase().contains(&specific.to_lowercase())
}

fn anchor_filter(specifics: Vec<String>, claim: &str) -> Vec<String> {
    specifics
        .into_iter()
        .filter_map(|s| {
            let s = strip_label_prefix(s.trim());
            is_anchored_in(s, claim).then(|| s.to_string())
        })
        .collect()
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
    // The witness-fix (directive 6c25d88e): a negative claim about the
    // evidence inverts the presence test; the anchor filter drops a
    // specific the claim does not assert (the phantom class) before it
    // can flip the witness.
    let negative = is_negative_claim(claim);
    let witnessable: Vec<String> = anchor_filter(specifics, claim)
        .into_iter()
        .filter(|s| is_witnessable(s))
        .collect();
    if witnessable.is_empty() {
        if negative {
            // Not vacuous: an unverifiable negative claim is
            // could-not-judge with the reason recorded — never a pass.
            return WitnessOutcome {
                ran: true,
                specifics: Vec::new(),
                all_absent: true,
                reason: Some(
                    "negative claim about the evidence with no checkable specifics — does not pass vacuously"
                        .to_string(),
                ),
            };
        }
        return WitnessOutcome::not_witnessable(
            "no checkable specifics (NONE sentinel or unwitnessable shapes)",
        );
    }
    if negative {
        // The negation inverts presence: any witnessable specific in
        // the evidence contradicts the claim (downgrade); all absent →
        // the negation holds (no downgrade).
        if witness_presence(&witnessable, chunks) {
            return WitnessOutcome {
                ran: true,
                specifics: witnessable,
                all_absent: true,
                reason: Some(
                    "negative claim contradicted: a witnessable specific is present in the evidence"
                        .to_string(),
                ),
            };
        }
        return WitnessOutcome::no_downgrade(witnessable);
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
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;

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
            "Budget Forcing\n\nThe system card describes the technique for constraining model spend.".to_string(),
        ];
        // "Budget Forcing" appears only as the heading line. (The old
        // fixture relied on case as the heading/body discriminator —
        // "Budget Forcing" vs "budget forcing" — a channel the
        // case-insensitive fix deliberately removed; the occurrence is
        // now the ONLY one, which is the stronger pin.)
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

    // ---- Witness-fix (directive 6c25d88e): the three demo-flight
    // defect classes, pinned by scripted extraction (deterministic red
    // at HEAD, green after the fix — no live model). ----

    /// Scripted extractor: returns `text` verbatim for every
    /// completion. The I-class extraction is pinned to the exact defect
    /// shape.
    struct ScriptedExtractor(&'static str);

    #[async_trait]
    impl InferenceProvider for ScriptedExtractor {
        async fn complete(
            &self,
            _r: &crate::types::CompletionRequest,
        ) -> crate::error::Result<crate::types::CompletionResponse> {
            Ok(crate::types::CompletionResponse {
                text: self.0.to_string(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "test".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _r: &crate::types::CompletionRequest,
        ) -> crate::error::Result<Pin<Box<dyn Stream<Item = crate::error::Result<String>> + Send>>>
        {
            unimplemented!()
        }
        async fn embed(&self, _t: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![])
        }
        fn capabilities(&self) -> crate::types::ProviderCapabilities {
            crate::types::ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: crate::types::Depth::Moderate,
            }
        }
    }

    /// Run the witness with a scripted extraction over one window.
    async fn witness_with(
        extraction: &'static str,
        claim: &str,
        window: &[&str],
    ) -> WitnessOutcome {
        let provider: Arc<dyn InferenceProvider> = Arc::new(ScriptedExtractor(extraction));
        let chunks: Vec<String> = window.iter().map(|s| s.to_string()).collect();
        containment_witness(
            &provider,
            claim,
            &chunks,
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
        )
        .await
    }

    // Defect 1: the phantom specific — "Date: 1973 (inauguration)",
    // present in neither claim nor evidence, alone flipped the witness
    // to all-absent. The anchor filter drops a specific the claim does
    // not assert.
    #[tokio::test]
    async fn phantom_specifics_do_not_flip_the_witness() {
        let claim = "The Larkhall viaduct across the Clyde valley was inaugurated in 1973.";
        let window = [concat!(
            "The Larkhall viaduct across the Clyde valley was inaugurated in 1973, ",
            "and its opening ceremony drew crowds from both banks of the river."
        )];
        let outcome = witness_with("Date: 1973 (inauguration)", claim, &window).await;
        assert!(
            !outcome.all_absent,
            "a specific anchored in neither claim nor evidence must not flip the witness"
        );
        assert!(!outcome.ran, "nothing witnessable after the anchor filter");
    }

    // Defect 1b: the anchored reshape survives label stripping and is
    // still checked — "Date: 1973" → "1973", anchored in the claim,
    // present in the window.
    #[tokio::test]
    async fn label_prefixes_are_stripped_then_anchored_and_checked() {
        let claim = "The Larkhall viaduct across the Clyde valley was inaugurated in 1973.";
        let window = [concat!(
            "The Larkhall viaduct across the Clyde valley was inaugurated in 1973, ",
            "and its opening ceremony drew crowds from both banks of the river."
        )];
        let outcome = witness_with("Date: 1973", claim, &window).await;
        assert!(outcome.ran);
        assert!(
            !outcome.all_absent,
            "'1973' is anchored in the claim and present in the window after label stripping"
        );
    }

    // Defect 2: the case gap — evidence "launched from pad 39A", claim
    // "Pad 39A": same fact, token-different, judged absent. The body
    // containment check must match case-insensitively like the one
    // matcher.
    #[tokio::test]
    async fn body_containment_is_case_insensitive() {
        let claim = "The Apollo 11 mission launched from Pad 39A on July 16, 1969.";
        let window = [concat!(
            "The Apollo 11 mission launched from pad 39A on July 16, 1969, ",
            "carrying Neil Armstrong, Buzz Aldrin, and Michael Collins toward the Moon."
        )];
        let outcome = witness_with("Pad 39A", claim, &window).await;
        assert!(outcome.ran);
        assert!(
            !outcome.all_absent,
            "case-shifted proper nouns are the same fact — must be present"
        );
    }

    // Defect 3: a negative claim about the evidence. Presence of a
    // witnessable specific CONTRADICTS the negation → downgrade with
    // the reason recorded.
    #[tokio::test]
    async fn negative_claims_are_contradicted_by_present_specifics() {
        let claim = "None of the provided sources list the crew members of the Apollo 11 mission.";
        let window = [concat!(
            "The Apollo 11 mission launched on July 16, 1969, and its crew of three ",
            "— Neil Armstrong, Buzz Aldrin, and Michael Collins — landed on the Moon on July 20."
        )];
        let outcome = witness_with("Apollo 11", claim, &window).await;
        assert!(
            outcome.ran && outcome.all_absent,
            "the window lists the crew — the negation is contradicted"
        );
        assert!(
            outcome
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("negative")),
            "the contradicted-negative downgrade records its reason"
        );
    }

    // Defect 3b: a TRUE negative (the specific is genuinely absent from
    // the evidence) holds — the negation is not downgraded.
    #[tokio::test]
    async fn negative_claims_hold_when_their_specifics_are_absent() {
        let claim = "None of the provided sources list the crew members of the Apollo 11 mission.";
        let window = [concat!(
            "The Apollo 11 mission launched on July 16, 1969, and its crew of three ",
            "— Neil Armstrong, Buzz Aldrin, and Michael Collins — landed on the Moon on July 20."
        )];
        let outcome = witness_with("crew members", claim, &window).await;
        assert!(
            outcome.ran && !outcome.all_absent,
            "a true negative (specific absent from the evidence) must not downgrade"
        );
    }

    // Defect 3c: an unverifiable negative (NONE extraction) does NOT
    // pass vacuously as un-witnessable — it is could-not-judge with the
    // reason recorded.
    #[tokio::test]
    async fn negative_claims_do_not_pass_vacuously_on_no_specifics() {
        let claim = "None of the provided sources list the crew members of the Apollo 11 mission.";
        let window = [concat!(
            "The Apollo 11 mission launched on July 16, 1969, and its crew of three ",
            "— Neil Armstrong, Buzz Aldrin, and Michael Collins — landed on the Moon on July 20."
        )];
        let outcome = witness_with("NONE", claim, &window).await;
        assert!(
            outcome.ran && outcome.all_absent,
            "an unverifiable negative claim is could-not-judge, never a vacuous pass"
        );
    }
}
