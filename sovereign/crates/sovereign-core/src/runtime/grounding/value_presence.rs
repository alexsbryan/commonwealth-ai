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
//! # Presence VETOES; the probe DECIDES (2026-09-04)
//!
//! Three steps, and the middle one may only ever REFUSE. An LLM extracts the
//! answer's value (a short noun phrase — the one judgment a small model does
//! reliably); a DETERMINISTIC substring test asks whether that value's tokens
//! appear in the evidence AT ALL, and a value that appears nowhere is refused
//! on the spot, cheaply, with no model call; only then does
//! [`judge::claim_chunk_support`] — the calibrated forced-choice register the
//! audit pass itself runs, at the audit's own tau — decide whether the
//! evidence actually SUPPORTS the value as the answer to this question.
//!
//! Until this change the substring test decided BOTH directions, and its
//! positive direction was the footgun: its own doc said "a real corpus token
//! (even mis-roled …) is exactly-present and released as best effort", which
//! is how a fabricated "Winnie's mother is Mrs Neale" scored GROUNDED —
//! Mrs Neale is the charwoman of Brett Street, and "Neale" is in the chunks.
//! Measured on the chaos corpus's own paragraphs with the probe that now
//! decides (`svrn bench judge-replay --register chunk_judge`, 3 repeats,
//! identical every time): "Winnie's mother is Mrs Neale" scores support
//! 0.0045, "Mrs Neale is the charwoman" scores 0.9999 — three orders of
//! magnitude apart on the SAME evidence, which is the separation a substring
//! test cannot express at all.
//!
//! Competence survives, and that was measured too rather than hoped for:
//! "Yundt's first name is Karl" against the paragraph that only ever writes
//! "Karl Yundt" scores 0.9997. The register's own prompt is what does the
//! work — it asks whether the passage "states or clearly implies" the claim
//! and says outright that "merely mentioning the people or things involved,
//! without establishing the claimed connection between them, does NOT count".
//!
//! The split is still load-bearing, in the other direction now: a
//! forced-choice judge asked the presence question DIRECTLY false-positived an
//! absent "Thomas", so presence is never asked of the model — it is a
//! substring test, and it is a veto (ARCH §7.6: the exact rule survives in
//! code, and code may only refuse).

use crate::oicp::ShardingPrivacy;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

use super::config::dbg;
use super::judge::CHUNK_JUDGE_PASSAGE_CHARS;

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
    /// Nothing was decided. Either no checkable specific was asserted (an
    /// abstention, a decline, a discursive answer), or the assessment could
    /// not be completed — extraction was unavailable, or the probe returned
    /// no verdict.
    ///
    /// **This is the could-not-judge bucket, not a pass** (ARCH §18.1). Both
    /// consumers already treat it that way and neither had to change: the
    /// gate falls through to its confirmatory loop rather than deciding on
    /// nothing, and the chaos scorer records `grounded: null`, which
    /// `bench_verdict` routes to `answered_novalue` and the scoreboard
    /// excludes from the decline rate.
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
    posture: ShardingPrivacy,
) -> AssertedValue {
    match value_presence_of(inference, question, answer, chunks, posture).await {
        ValuePresence::NoValue => AssertedValue::NoValue,
        ValuePresence::Absent(value) => AssertedValue::Ungrounded(value),
        ValuePresence::Present(value) => {
            match value_is_supported(inference, question, &value, chunks, posture).await {
                Some(true) => AssertedValue::Grounded(value),
                Some(false) => AssertedValue::Ungrounded(value),
                None => AssertedValue::NoValue,
            }
        }
    }
}

/// What the deterministic veto found. **`Present` is not a verdict** — it is
/// the absence of a refusal, and only [`value_is_supported`] can turn it into
/// one.
///
/// The two halves are split because they have different costs and different
/// callers. The veto is one cheap extraction call plus a substring test, and
/// it applies to EVERY gated turn; the probe is a second model call, needed
/// only where the mechanism is allowed to return a POSITIVE verdict. A caller
/// that can only ever refuse must not pay for a verdict it would discard.
pub(crate) enum ValuePresence {
    /// No checkable specific was asserted, or extraction was unavailable.
    NoValue,
    /// The value's tokens appear NOWHERE in the evidence — invented from
    /// nothing. Refuse; no probe verdict can license it.
    Absent(String),
    /// The value's tokens are in the evidence. That is all this says.
    Present(String),
}

/// THE VETO, on its own: one extraction call and a substring test, no probe,
/// and no verdict about groundedness.
pub(crate) async fn value_presence_of(
    inference: &dyn InferenceProvider,
    question: &str,
    answer: &str,
    chunks: &[String],
    posture: ShardingPrivacy,
) -> ValuePresence {
    let Some(value) = extract_answer_value(inference, question, answer, posture).await else {
        return ValuePresence::NoValue;
    };
    if value_present_in_chunks(&value, chunks) {
        return ValuePresence::Present(value);
    }
    tracing::info!(
        target: "grounding_gate",
        event = "value_presence",
        value = %value,
        decision = "vetoed_absent",
        "the answer's specific is absent from the evidence"
    );
    ValuePresence::Absent(value)
}

/// THE DECIDER: does the evidence SUPPORT this value as the answer to this
/// question? A judgement about open text, so it goes to the one register that
/// already owns it — the audit pass's calibrated forced-choice probe, at the
/// audit's own threshold. `None` = the probe did not answer.
///
/// Only ever asked about a value the veto already found PRESENT. Asking it
/// about an absent one would let a yes-biased judge license a fabrication,
/// which is exactly why the veto runs first and may not be overruled.
pub(crate) async fn value_is_supported(
    inference: &dyn InferenceProvider,
    question: &str,
    value: &str,
    chunks: &[String],
    posture: ShardingPrivacy,
) -> Option<bool> {
    let (evidence, held, dropped) = value_evidence(value, chunks);
    let claim = value_claim(question, value);
    let tau = super::config::grounding_gate_threshold();
    match super::judge::claim_chunk_support(inference, &evidence, &claim, posture).await {
        Some(support) => {
            // THE AUDIT'S OWN COMPARISON, not a second threshold (§10.6):
            // `verify_grounding` computes `violation_prob = 1 - max_support`
            // and `gate_answer_inner` releases iff `violation_prob < tau`.
            let violation_prob = 1.0 - support;
            let grounded = violation_prob < tau;
            // ONE glassbox channel for one decision. The `dbg()` mirror
            // this file used to carry beside every event was the §10.6
            // smell 571849a89 removed from the citation stage for the same
            // reason; the structured event carries strictly more, and
            // `dbg()` still serves the extraction site below, which has no
            // structured event of its own.
            tracing::debug!(
                target: "grounding_gate",
                event = "value_presence",
                value = %value,
                support = format!("{support:.4}").as_str(),
                violation_prob = format!("{violation_prob:.4}").as_str(),
                tau,
                evidence_chunks = held,
                evidence_dropped = dropped,
                decision = if grounded { "grounded" } else { "unsupported" },
                "value-presence probe"
            );
            Some(grounded)
        }
        // The probe did not answer. That is a fact about the instrument,
        // never a verdict about the answer — could-not-judge (ARCH §18.1).
        None => {
            tracing::warn!(
                target: "grounding_gate",
                event = "value_presence",
                value = %value,
                decision = "could_not_judge",
                "the value-support probe returned no verdict"
            );
            None
        }
    }
}

/// The claim the probe is asked, built from the question's own frame and the
/// value the answer offered.
///
/// Deterministic on purpose: composing it with a second model call would
/// double the cost of every entity-anchored turn for a sentence the question
/// already contains. Measured equivalent to a hand-written natural claim on
/// the chaos corpus (`judge-replay --register chunk_judge`, 3 repeats):
/// this framing scores 0.0016 / 0.9999 / 0.9997 on the mother-fabrication /
/// charwoman-truth / Karl-implied triple where hand-written claims score
/// 0.0011 / 0.9999 / 0.9998.
fn value_claim(question: &str, value: &str) -> String {
    format!(
        "The answer to the question \"{q}\" is: {value}.",
        q = question.trim().chars().take(300).collect::<String>(),
    )
}

/// The evidence the probe is shown: a window around the value in EVERY chunk
/// that carries it, sized so they all fit the register's own passage cap.
///
/// Returns `(passage, chunks_shown, chunks_dropped)`.
///
/// Which chunks carry the value is decided by the same predicate the veto just
/// ran, applied one chunk at a time — one implementation of "does this text
/// carry the value", never two (ARCH §10.6).
///
/// # Why every carrier, and why a window
///
/// Showing the probe ONE chunk was measured and rejected: the same fabrication
/// scores 0.2956 against the paragraph where Mrs Neale scrubs a floor near
/// Winnie and 0.4001 against the retrieved summary that mentions them in one
/// sentence — both RELEASE at tau — and 0.0060 against those two together. The
/// register can only apply its own rule ("merely mentioning the people or
/// things involved, without establishing the claimed connection, does NOT
/// count") when it can see what the evidence does and does not establish.
///
/// Taking WHOLE chunks in order was measured and rejected too, and this is the
/// sharper failure: with twenty 2,000-char chunks the cap admits ONE, and if
/// the establishing chunk is not that one the probe refuses a correct answer
/// for a reason that is about the budget rather than the evidence. Measured on
/// a real turn — "On what street is Verloc's shop located?" / "Brett Street",
/// four carrying chunks, three dropped: support 0.0066 on the kept chunk,
/// which says only "Brett Street was not very far away. It branched off,
/// narrow, from…" and genuinely does not place the shop. The probe was right
/// about what it was shown and wrong about the corpus.
///
/// So the budget is SPLIT rather than spent first-come: each carrier
/// contributes a window of `cap / carriers` characters centred on the value's
/// longest token. The share is arithmetic over the two quantities already in
/// hand, not a tuned size. Same turn, same evidence, after the split: 0.9631 —
/// grounded, correctly. The mother fabrication is unmoved at 0.0052 and "Mrs
/// Neale is the charwoman" at 0.9994, so the split buys the competence case
/// without spending the refusal it was built for.
///
/// Truncation is still reported (§18.3) and is still safe in one direction
/// only: less evidence can make the probe refuse, never approve.
fn value_evidence(value: &str, chunks: &[String]) -> (String, usize, usize) {
    let carrying: Vec<&String> = chunks
        .iter()
        .filter(|c| value_present_in_chunks(value, std::slice::from_ref(*c)))
        .collect();
    // The veto passed, so the value's tokens are in the evidence — but they
    // may be spread across chunks with no single chunk carrying all of them.
    // Then the honest passage is what the veto itself looked at.
    let selected: Vec<&String> = if carrying.is_empty() {
        chunks.iter().collect()
    } else {
        carrying
    };
    let share = (CHUNK_JUDGE_PASSAGE_CHARS / selected.len().max(1)).max(1);
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;
    for c in &selected {
        if used >= CHUNK_JUDGE_PASSAGE_CHARS {
            break;
        }
        let room = share.min(CHUNK_JUDGE_PASSAGE_CHARS - used);
        let w = window_around(value, c, room);
        used += w.chars().count() + 2;
        parts.push(w);
    }
    let shown = parts.len();
    (
        parts.join("\n\n"),
        shown,
        selected.len().saturating_sub(shown),
    )
}

/// At most `room` characters of `chunk`, centred on the first occurrence of
/// the value's LONGEST token — the one most likely to be the value itself
/// rather than a qualifier the corpus uses everywhere ("Mrs", "the").
///
/// A chunk that already fits comes back whole; a chunk with no token match
/// (unreachable for a carrier, reachable on the all-chunks fallback) comes
/// back from its start, which is where its own topic sentence is.
fn window_around(value: &str, chunk: &str, room: usize) -> String {
    let chars: Vec<char> = chunk.chars().collect();
    if chars.len() <= room {
        return chunk.to_string();
    }
    let low = chunk.to_lowercase();
    let mut tokens: Vec<&str> = value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .collect();
    tokens.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
    let at = tokens
        .iter()
        .find_map(|t| low.find(&t.to_lowercase()))
        .map(|byte_idx| low[..byte_idx].chars().count())
        .unwrap_or(0);
    let start = at.saturating_sub(room / 2).min(chars.len() - room);
    chars[start..start + room].iter().collect()
}

async fn extract_answer_value(
    inference: &dyn InferenceProvider,
    question: &str,
    answer: &str,
    posture: ShardingPrivacy,
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
        preferred_speed: Speed::Slow,
        // SLOT_POLICY §7: OICP envelope instead of a `model_id: "primary"`
        // pin (a latent privacy hole — see judge.rs). Carries the session
        // posture so the judge offloads only when the turn permits.
        oicp: Some(Workload::Judge.requirements(posture)),
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
            //
            // KNOWN DIVERGENCE from `kernel_types::is_absent_marker`, which is
            // the one decider for "does this text name an absence" and which
            // the atlas extractor uses. This chain is a bare `starts_with`, so
            // it reads a judge answering "unknown-type sceatta series" as a
            // DECLINE and grounds nothing — the same §18.1 defect that was
            // fixed in the extractor. It is not switched here in a cleanup
            // commit because this is a GATE input: making it stricter about
            // declines sends more answers to the presence check, which changes
            // what the grounding gate suppresses. That is a §18.6 change and
            // needs the grounding battery reported in both directions, not a
            // one-line substitution. Tracked in note `d61eb8d4` item 11.
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

/// Deterministic token-presence: are ALL words of `value` (two characters or
/// more) present, case-insensitively, somewhere in the chunks?
///
/// **This is a VETO and it answers a literal question.** It says whether the
/// value's tokens are in the text — nothing about whether the text SUPPORTS
/// the value as an answer, which is [`judge::claim_chunk_support`]'s job. A
/// multi-part invention ("Vladimir Stepanovich Haldin") is exactly-absent
/// because Stepanovich and Haldin are not there; a value invented from
/// nothing is exactly-absent; everything else is merely NOT REFUSED.
///
/// # The role-word stop list is gone (2026-09-04)
///
/// It held `mr / mrs / miss / ms / sir / dr / comrade / chief / inspector /
/// lady / lord / saint / st` plus five function words, and it existed for one
/// reason: to make presence generous enough to say GROUNDED for a value whose
/// ROLE was wrong — "Mrs Verloc" clearing on "Verloc". Presence no longer says
/// grounded, so the generosity has no job, and the words it was generous about
/// are exactly the ones a mis-roled fabrication gets wrong. A stop list that
/// only vetoes needs no role words.
///
/// Dropping it makes the veto STRICTER, which is the safe direction for a
/// veto: it can refuse a little more and can never approve anything. The
/// verbatim-phrase path below is what keeps a correct title answer ("Chief
/// Inspector", every word of which the list used to swallow) from being
/// refused on a technicality.
pub fn value_present_in_chunks(value: &str, chunks: &[String]) -> bool {
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
    let nval: String = value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if nval.split(' ').filter(|w| !w.is_empty()).count() >= 2 && hay.contains(&nval) {
        return true;
    }
    let sig: Vec<String> = value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_lowercase())
        .collect();
    !sig.is_empty() && sig.iter().all(|w| hay.contains(w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::{
        assess_asserted_value, value_claim, value_evidence, value_present_in_chunks, AssertedValue,
        CHUNK_JUDGE_PASSAGE_CHARS,
    };
    use crate::oicp::ShardingPrivacy;
    use crate::runtime::grounding::tests::GateMock;

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

    /// THE VETO'S TWO HALVES, on the same fixture. What it must still catch is
    /// the invention; what it must NOT do is pronounce anything grounded.
    #[test]
    fn the_veto_still_refuses_a_value_invented_from_nothing() {
        assert!(!value_present_in_chunks("Vernon", &chunks()));
        assert!(!value_present_in_chunks("Russian", &chunks()));
        // Vladimir present, Stepanovich/Haldin not → not all present.
        let c = vec!["Mr Vladimir spoke to the First Secretary.".to_string()];
        assert!(!value_present_in_chunks("Vladimir Stepanovich Haldin", &c));
    }

    /// The veto lets through what it cannot refuse — and "lets through" is all
    /// it means now. Each of these once RELEASED an answer on this function's
    /// say-so; today each only reaches the probe.
    #[test]
    fn the_veto_passes_a_present_token_without_grounding_it() {
        assert!(value_present_in_chunks("Karl", &chunks())); // inside "Karl Yundt"
        assert!(value_present_in_chunks("Mrs. Verloc", &chunks())); // mis-role
        assert!(value_present_in_chunks("Sir Ethelred", &chunks()));
        assert!(value_present_in_chunks("Alexander", &chunks()));
    }

    /// The verbatim-phrase path is what survives the stop list's deletion: a
    /// correct answer that IS a title ("Chief Inspector") appears contiguously
    /// in the corpus, and every one of its words used to be a stop word.
    #[test]
    fn a_verbatim_multiword_value_still_passes_the_veto() {
        let c = vec![
            "Chief Inspector Heat of the Special Crimes Department changed his tone.".to_string(),
        ];
        assert!(value_present_in_chunks("Chief Inspector", &c));
        // Not present verbatim AND a word ("russian") is absent → refused.
        assert!(!value_present_in_chunks("Russian embassy", &c));
    }

    // ── The mother / charwoman pair, from the chaos corpus's own prose ──
    //
    // `chaos-secret-agent` is Conrad's *The Secret Agent*; these are the
    // paragraphs that carry "Mrs Neale". The pair is the whole point of the
    // F1 conversion: "Neale" is in the evidence either way, so PRESENCE
    // cannot tell the fabrication from the fact and the probe must.
    //
    // Live, against the register that now decides (`svrn bench judge-replay
    // --register chunk_judge`, 3 repeats, identical each time):
    //   "the answer to 'What is the first name of Winnie's mother?' is Mrs Neale"
    //        support 0.0045  → violation 0.9955 ≥ tau → UNGROUNDED
    //   "the answer to 'Who is the charwoman of Brett Street?' is Mrs Neale"
    //        support 0.9999  → violation 0.0001 <  tau → grounded
    fn neale_chunks() -> Vec<String> {
        vec![
            "Mrs Neale was the charwoman of Brett Street. Victim of her marriage with a \
             debauched joiner, she was oppressed by the needs of many infant children."
                .to_string(),
            "There Mrs Neale was scrubbing the floor. At Stevie's appearance she groaned \
             lamentably, having observed that he could be induced easily to bestow for the \
             benefit of her infant children the shilling his sister Winnie presented him \
             with from time to time."
                .to_string(),
        ]
    }

    const MOTHER_Q: &str = "What is the first name of Winnie's mother?";
    const CHARWOMAN_Q: &str = "Who is the charwoman of Brett Street?";

    /// THE DECIDER DECIDES. Same value, same evidence, same veto verdict
    /// (present, both times) — and the outcome follows the PROBE.
    ///
    /// FAILS IF the positive-presence branch comes back: presence says
    /// "Neale is in the chunks" on both halves, so a presence-decided
    /// assessment returns `Grounded` for both and the first half reddens.
    #[tokio::test]
    async fn the_probe_and_not_presence_decides_a_value_it_can_see() {
        let refusing = GateMock {
            support: Some(false),
        };
        let m = assess_asserted_value(
            &refusing,
            MOTHER_Q,
            "Mrs Neale",
            &neale_chunks(),
            ShardingPrivacy::LocalOnly,
        )
        .await;
        assert_eq!(
            m,
            AssertedValue::Ungrounded("Mrs Neale".into()),
            "the evidence carries the token and does not support the claim"
        );

        let supporting = GateMock {
            support: Some(true),
        };
        let c = assess_asserted_value(
            &supporting,
            CHARWOMAN_Q,
            "Mrs Neale",
            &neale_chunks(),
            ShardingPrivacy::LocalOnly,
        )
        .await;
        assert_eq!(
            c,
            AssertedValue::Grounded("Mrs Neale".into()),
            "the same evidence DOES support the charwoman claim"
        );
    }

    /// A VETO CANNOT APPROVE. The probe is told to support everything; the
    /// value is absent; the answer is still ungrounded, and the probe is
    /// never even asked.
    ///
    /// FAILS IF the veto is moved after the probe, or softened into an
    /// input the probe may overrule.
    #[tokio::test]
    async fn an_absent_value_is_refused_even_when_the_probe_approves() {
        let approving = GateMock {
            support: Some(true),
        };
        let v = assess_asserted_value(
            &approving,
            "Which country's embassy employs Mr Vladimir?",
            "Russian",
            &neale_chunks(),
            ShardingPrivacy::LocalOnly,
        )
        .await;
        assert_eq!(v, AssertedValue::Ungrounded("Russian".into()));
    }

    /// A probe that does not answer is could-not-judge, never a release and
    /// never a refusal attributed to the answer (ARCH §18.1/§18.3).
    #[tokio::test]
    async fn a_probe_that_does_not_answer_grounds_nothing_and_refuses_nothing() {
        let silent = GateMock { support: None };
        let v = assess_asserted_value(
            &silent,
            CHARWOMAN_Q,
            "Mrs Neale",
            &neale_chunks(),
            ShardingPrivacy::LocalOnly,
        )
        .await;
        assert_eq!(v, AssertedValue::NoValue);
    }

    /// An answer that asserts no value never reaches either half.
    #[tokio::test]
    async fn a_decline_asserts_no_value() {
        let approving = GateMock {
            support: Some(true),
        };
        let v = assess_asserted_value(
            &approving,
            MOTHER_Q,
            "NONE",
            &neale_chunks(),
            ShardingPrivacy::LocalOnly,
        )
        .await;
        assert_eq!(v, AssertedValue::NoValue);
    }

    /// The probe is shown a window from EVERY chunk that carries the value,
    /// and from no other — measured better than any single chunk (0.0060
    /// across the pair vs 0.2956 and 0.4001 against each alone, both of which
    /// would have released).
    #[test]
    fn the_probe_sees_every_chunk_that_carries_the_value() {
        let mut chunks = neale_chunks();
        chunks.push("The Assistant Commissioner walked to Westminster.".to_string());
        let (passage, shown, dropped) = value_evidence("Mrs Neale", &chunks);
        assert_eq!(
            (shown, dropped),
            (2, 0),
            "both Neale chunks, and only those"
        );
        assert!(passage.contains("charwoman") && passage.contains("scrubbing"));
        assert!(!passage.contains("Westminster"));
    }

    /// THE BUDGET IS SPLIT, NOT SPENT FIRST-COME. With more carriers than the
    /// register's cap can hold whole, taking them in order shows the probe one
    /// chunk and drops the rest — and if the establishing chunk is among the
    /// dropped, a correct answer is refused for a reason about the budget.
    /// Measured on a real turn: "Brett Street" scored 0.0066 whole-chunk-first
    /// (the kept chunk says only "Brett Street was not very far away") and
    /// 0.9631 once every carrier contributed a window.
    ///
    /// FAILS IF the selector goes back to whole chunks in order: `shown` falls
    /// to 1 and the last carrier's text disappears from the passage.
    #[test]
    fn every_carrier_gets_a_share_of_the_budget() {
        let filler = "x".repeat(1_800);
        let chunks: Vec<String> = (0..4)
            .map(|i| format!("{filler} chunk{i} names Mrs Neale here. {filler}"))
            .collect();
        let (passage, shown, dropped) = value_evidence("Mrs Neale", &chunks);
        assert_eq!(
            (shown, dropped),
            (4, 0),
            "every carrier is represented, none dropped"
        );
        assert!(
            passage.chars().count() <= CHUNK_JUDGE_PASSAGE_CHARS,
            "the register's own cap is respected: {}",
            passage.chars().count()
        );
        for i in 0..4 {
            assert!(
                passage.contains(&format!("chunk{i} names Mrs Neale here.")),
                "carrier {i} lost its window; passage={passage}"
            );
        }
    }

    /// The claim carries the QUESTION'S frame, not just the value — without
    /// it the probe is asked whether a passage supports the bare string
    /// "Mrs Neale", which every one of these chunks does.
    #[test]
    fn the_claim_carries_the_questions_frame() {
        let c = value_claim(MOTHER_Q, "Mrs Neale");
        assert!(c.contains("first name of Winnie's mother"), "{c}");
        assert!(c.contains("Mrs Neale"), "{c}");
    }
}
