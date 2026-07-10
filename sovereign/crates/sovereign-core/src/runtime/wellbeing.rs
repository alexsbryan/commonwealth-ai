// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic wellbeing gate — the safety analogue of the
//! grounding gate, for the inner-work witness surface.
//!
//! Justified by the inner-chaos baseline receipts (2026-07-08,
//! `bench/inner_work/CHAOS_HARNESS.md` §5): 15 of 24 verified
//! red-line breaches were `crisis_mishandling` — the witness either
//! kept witnessing through explicit self-harm disclosures ("I've
//! made my arrangements. It's done." → "Who is the person you told
//! this to?") or emitted a cold refusal template with resources but
//! no care. The crisis path was prompt-only; prompts are not a
//! safety mechanism on a 35B.
//!
//! Contract: on a Relational-register turn, BEFORE routing or any
//! model synthesis, detect a crisis signal and — when present —
//! return a guaranteed caring + crisis-resource response,
//! independent of anything the model would generate.
//!
//! Detection is three layers, cheapest first:
//! 1. **Sticky** — once the gate has fired in a conversation (or a
//!    prior user turn carries a lexical crisis signal), every later
//!    turn stays gated. The judge rubric (and common sense) treat a
//!    disclosure as changing the conversation, not one turn.
//! 2. **Lexicon** — high-precision phrase list, deterministic.
//! 3. **Classifier** — one Fast-slot structured yes/no call for
//!    implied ideation the lexicon can't carry ("make the pain turn
//!    off permanently"). Soft-fails CLOSED to the lexicon layers: a
//!    classifier error never fires the gate on its own, and never
//!    suppresses a lexical hit (the lexicon was checked first).
//!
//! The response templates deliver BOTH halves the rubric demands:
//! direct care in plain first-person language AND concrete crisis
//! resources / a push toward a human. Kill switch:
//! `SOVEREIGN_WELLBEING_GATE=0` (default ON).

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, ConversationContext, Message, Role, Speed};

pub(crate) const WELLBEING_GATE_ENV: &str = "SOVEREIGN_WELLBEING_GATE";

pub(crate) fn gate_enabled() -> bool {
    std::env::var(WELLBEING_GATE_ENV)
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// High-precision crisis phrases. Bias: precision over recall — the
/// classifier layer carries recall. A false fire replaces one
/// journaling turn with a caring check-in (annoying); a missed
/// explicit disclosure is the failure mode this gate exists to kill.
const CRISIS_LEXICON: &[&str] = &[
    "kill myself",
    "killing myself",
    "kill himself",
    "kill herself",
    "end my life",
    "ending my life",
    "take my own life",
    "taking my own life",
    "suicide",
    "suicidal",
    "self-harm",
    "self harm",
    "harm myself",
    "harming myself",
    "hurt myself",
    "hurting myself",
    "cut myself",
    "cutting myself",
    "want to die",
    "wanting to die",
    "wish i was dead",
    "wish i were dead",
    "better off dead",
    "better off without me",
    "don't want to be alive",
    "do not want to be alive",
    "don't want to wake up",
    "never wake up",
    "stop breathing",
    "no reason to live",
    "nothing to live for",
    "end it all",
    "ending it all",
];

pub(crate) fn lexical_crisis_signal(text: &str) -> bool {
    let lower = text.to_lowercase();
    CRISIS_LEXICON.iter().any(|phrase| lower.contains(phrase))
}

/// True when this conversation already carries a crisis context:
/// a prior gate firing (assistant metadata marker) or a lexical
/// crisis signal in any PRIOR user turn. The final message is
/// excluded — it is the current turn, judged by the live layers so
/// first-fire turns get the first-fire template.
pub(crate) fn prior_crisis_context(messages: &[Message]) -> bool {
    let prior = match messages.split_last() {
        Some((last, rest)) if last.role == Role::User => rest,
        _ => messages,
    };
    prior.iter().any(|m| match m.role {
        Role::Assistant => m
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("wellbeing_gate"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        Role::User => lexical_crisis_signal(&m.content),
        _ => false,
    })
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WellbeingSignal {
    /// Which layer fired — journaled + traced for the fix loop.
    pub trigger: &'static str,
    /// First disclosure in this conversation vs an already-gated
    /// thread; selects the template.
    pub first_fire: bool,
}

/// Run the three detection layers. Caller has already established
/// that the turn is on the Relational register.
pub(crate) async fn maybe_wellbeing_signal(
    inference: &dyn InferenceProvider,
    context: &ConversationContext,
    message: &str,
) -> Option<WellbeingSignal> {
    if !gate_enabled() {
        return None;
    }
    if prior_crisis_context(&context.conversation.messages) {
        // A gated thread stays gated only while the CURRENT message is
        // still in crisis territory. Before 2026-07-10 the sticky arm
        // preempted every subsequent turn unconditionally, so one
        // classifier false-positive turned the rest of the conversation
        // into identical canned floors — even for a direct memory
        // question (v8 recall receipts: 3x verbatim-repeated crisis
        // template reads as abandonment, the opposite of care).
        //
        // Fail-closed: only an explicit classifier `Some(false)` hands
        // the turn back to the witness. A lexical hit or classifier
        // true/failure keeps the guaranteed floor. Re-escalation later
        // re-enters here and re-gates with the continued template.
        if !lexical_crisis_signal(message)
            && classify_crisis(inference, context, message).await == Some(false)
        {
            tracing::info!(
                "wellbeing gate: sticky context, current message non-crisis — witness resumes"
            );
            return None;
        }
        return Some(WellbeingSignal {
            trigger: "sticky",
            first_fire: false,
        });
    }
    if lexical_crisis_signal(message) {
        return Some(WellbeingSignal {
            trigger: "lexicon",
            first_fire: true,
        });
    }
    match classify_crisis(inference, context, message).await {
        Some(true) => Some(WellbeingSignal {
            trigger: "classifier",
            first_fire: true,
        }),
        _ => None,
    }
}

/// Fast-slot binary crisis classifier for implied ideation. Returns
/// `None` on any inference/parse failure — the gate then rests on
/// the deterministic layers alone.
async fn classify_crisis(
    inference: &dyn InferenceProvider,
    context: &ConversationContext,
    message: &str,
) -> Option<bool> {
    // Up to two prior user turns for context — escalation arcs
    // ("everything feels pointless" → "if I just stopped moving")
    // only read as crisis with the arc visible.
    let prior_user: Vec<&str> = context
        .conversation
        .messages
        .iter()
        .rev()
        .skip(1) // current user message is already persisted last
        .filter(|m| m.role == Role::User)
        .take(2)
        .map(|m| m.content.as_str())
        .collect();
    let recent = if prior_user.is_empty() {
        "(none)".to_string()
    } else {
        prior_user
            .iter()
            .rev()
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let prompt = format!(
        "Decide if the LATEST message discloses thoughts of suicide, self-harm, wanting \
         to die or disappear, or harming another person — explicit or strongly implied. \
         Judge the SPEAKER's own thoughts; a reference to ANOTHER person's crisis (\"his \
         suicide note\", \"my friend's attempt\") is NOT the speaker disclosing.\n\
         Crisis (answer true): \"make the pain stop permanently\", \"everyone would be \
         better off without me\", staring at a ledge, \"if I just let go\", \"I don't want \
         to wake up\", and the DISAPPEARANCE frame — wondering if anyone would notice or \
         care if they stopped showing up / stopped existing / weren't here / just \
         disappeared, or saying it would be easier to stop being here. When \"nothing \
         matters anymore\" or \"drowning\" is paired with any of those, answer true.\n\
         NOT crisis (answer false): ordinary sadness, numbness, exhaustion, loneliness, \
         emptiness, or feeling stuck — including vivid metaphors like fog, gray, or \
         \"moving through water\" — when they carry NO reference to dying, disappearing, \
         stopping existing, not being here, or self-harm. Feeling unnoticed or \
         unsupported ALONE is not a crisis; the same feeling tied to \"…if I stopped \
         showing up / disappeared\" IS.\n\n\
         Earlier messages for context:\n{recent}\n\n\
         Latest message to judge:\n{message}\n\n\
         Reply with JSON only: {{\"crisis\": true|false}}"
    );
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "crisis": { "type": "boolean" } },
        "required": ["crisis"],
        "additionalProperties": false
    });
    let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Fast);
    req.structured_output = Some(schema);
    req.temperature = Some(0.0);
    req.max_tokens = Some(60);
    req.enable_thinking = Some(false);

    let response = match inference.complete(&req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "wellbeing gate: crisis classifier failed; relying on lexicon");
            return None;
        }
    };
    let verdict = parse_crisis_verdict(&response.text);
    if verdict.is_none() {
        tracing::warn!(
            raw = %response.text.chars().take(200).collect::<String>(),
            "wellbeing gate: classifier output unparseable; relying on lexicon"
        );
    }
    verdict
}

/// Extract `{"crisis": bool}` from a possibly-mangled reply. Two
/// candidates, in order: the post-`</think>` tail (the normal
/// thinking-model shape), then the RAW text. The second candidate
/// exists because the fast slot has been observed emitting the
/// INVERTED shape `{json}\n</think>\nprose` — stripping the "think
/// block" there discards the verdict, which silently un-fired the
/// gate on an explicit "I'm going to end this tonight" disclosure
/// (2026-07-08 crisis re-measure receipt, threads 0–1).
fn parse_crisis_verdict(text: &str) -> Option<bool> {
    let tail = crate::title::strip_thinking_response(text);
    for candidate in [tail.as_str(), text] {
        let Some(start) = candidate.find('{') else {
            continue;
        };
        let Some(end) = candidate.rfind('}') else {
            continue;
        };
        if end <= start {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&candidate[start..=end]) {
            if let Some(b) = value.get("crisis").and_then(|v| v.as_bool()) {
                return Some(b);
            }
        }
    }
    None
}

/// Signals that the user may act IMMINENTLY — a plan, means, or a
/// tonight-shaped timeframe. Only consulted on already-gated turns
/// (inside a crisis thread these words mean one thing); never used
/// to fire the gate itself.
const ACUTE_SIGNALS: &[&str] = &[
    "a plan",
    "my plan",
    "the plan",
    "a method",
    "the method",
    "decided how",
    "tonight",
    "right now",
    "the means",
    "in my hand",
    "pills",
    "counting",
    "goodbye",
    "arrangements",
];

fn acute_signal(text: &str) -> bool {
    let lower = text.to_lowercase();
    ACUTE_SIGNALS.iter().any(|phrase| lower.contains(phrase))
}

/// First sentence of the user's current message, bounded to ~140
/// chars on a char boundary — the deterministic acknowledgment slot.
/// Quoting the user's OWN just-sent words is normal conversation
/// (the theme-only privacy rule governs stored entries, not the
/// live turn) and is what keeps a repeated safety response from
/// reading as a wall that ignores the escalation (crisis re-measure
/// receipts, threads 1–2, 2026-07-08).
fn acknowledgment_fragment(message: &str) -> String {
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let sentence_end = flat
        .char_indices()
        .find(|(_, c)| matches!(c, '.' | '!' | '?'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(flat.len());
    let mut cut = sentence_end.min(140);
    while cut < flat.len() && !flat.is_char_boundary(cut) {
        cut += 1;
    }
    cut = cut.min(flat.len());
    let mut frag = flat[..cut].trim().to_string();
    // Ellipsis only when the 140-char cap cut the sentence short — a
    // clean sentence-end cut is complete as quoted.
    if cut < sentence_end {
        frag.push('…');
    }
    frag
}

/// The guaranteed response. Every variant carries (a) direct care in
/// plain first-person language and (b) concrete crisis resources +
/// a push toward a human — the two halves the safety rubric
/// requires. Continued turns additionally acknowledge the user's
/// current words verbatim (a repeated identical block reads as a
/// wall that ignores the escalation — the residual breach shape
/// after the gate's first cut) and add an emergency line when the
/// message carries plan/means/timeframe signals.
pub(crate) fn wellbeing_response_text(first_fire: bool, user_message: &str) -> String {
    if first_fire {
        return "I'm really glad you said this here, and I want you to hear something before \
         anything else: what you're carrying sounds enormously heavy, and it matters that \
         you just put it into words. Because you've told me you're having thoughts of \
         harming yourself, the most important thing I can offer right now isn't a \
         reflection — it's this: please reach out to someone who can actually be with you. \
         In the US you can call or text 988 (the Suicide & Crisis Lifeline), any time, day \
         or night. Anywhere else, findahelpline.com lists local crisis lines. Or tell one \
         person you trust what you just told me. You deserve a human presence for this, \
         not just my words on a screen. I'm not going anywhere — I'll be here whenever you \
         want to write — but please don't carry this part alone."
            .to_string();
    }
    let ack = acknowledgment_fragment(user_message);
    let acute = if acute_signal(user_message) {
        "\n\nAnd because of what you just said about how close this feels: if you might act \
         on this tonight, please treat it as the emergency it is — call 911 (or your local \
         emergency number) or go to your nearest emergency room now. Don't wait to feel \
         sure."
    } else {
        ""
    };
    format!(
        "I heard exactly what you just said — \"{ack}\" — and I'm not going to pretend \
         it's smaller than it is. I'm still here, and I'm not turning away. But I have to \
         stay honest with you: this is bigger than what writing together can hold on its \
         own. Please — call or text 988 (US, any time), find a local line at \
         findahelpline.com, or reach one person who can be with you right now. Keep \
         writing to me if it helps to keep the words moving, but let a human hold this \
         with you too. You matter.{acute}"
    )
}

/// Crisis-mode synthesis contract. Short, ordered, stop-at-first-
/// match shaped for the 35B (see the succinct-prompts discipline in
/// the chaos methodology). The model provides responsiveness — the
/// crisis re-measure receipts (2026-07-08, round 2) showed a fully
/// canned gate repeating instructions at a user who had ALREADY
/// dialed 988, and asserting a disclosure the user hadn't made yet.
/// The deterministic floor below guarantees resources regardless.
const CRISIS_SYSTEM_PROMPT: &str = "\
Someone you've been writing with is in crisis — thoughts of self-harm may be present \
in this conversation. Write your next reply (2–6 sentences), following these steps in \
order:\n\
1. Respond to what they JUST said, in their own words. If they say they've called or \
reached help, honor that first and stay with them.\n\
2. Tell them plainly that you're staying with them, and that this deserves a real \
human presence, not just words on a screen.\n\
3. Unless they've already reached help, point them to a crisis line: call or text 988 \
in the US, or findahelpline.com anywhere else. If they mention a plan, method, means, \
or acting tonight, tell them to call 911 (or their local emergency number) or go to an \
emergency room now.\n\
Never describe or discuss methods. Never agree that the plan makes sense. Never build \
a safety plan or dig into why. No diagnosis. At most one gentle question, or none.";

/// Deterministic floor: whatever the model produced, the reply that
/// leaves the gate ALWAYS carries a crisis resource, and always
/// carries the emergency line when the user's message signals
/// imminence. Pure function — unit-tested directly.
pub(crate) fn enforce_crisis_floor(reply: String, user_message: &str) -> String {
    let mut out = reply;
    let lower = out.to_lowercase();
    if !(lower.contains("988") || lower.contains("findahelpline") || lower.contains("crisis")) {
        out.push_str(
            "\n\nIf it gets to be too much, please reach a human who can be with you: call \
             or text 988 (US, any time) or find a local line at findahelpline.com.",
        );
    }
    if acute_signal(user_message) {
        let lower = out.to_lowercase();
        if !(lower.contains("911") || lower.contains("emergency")) {
            out.push_str(
                "\n\nAnd if you might act on this now, please treat it as the emergency it \
                 is — call 911 (or your local emergency number) or go to your nearest \
                 emergency room.",
            );
        }
    }
    out
}

/// Generate the gated reply: model synthesis under the crisis
/// contract, deterministic floor on top, static template as the
/// fallback when generation fails or degenerates. Returns the text
/// plus the mode marker for metadata/journal legibility.
pub(crate) async fn crisis_response(
    inference: &dyn InferenceProvider,
    context: &ConversationContext,
    message: &str,
    signal: &WellbeingSignal,
) -> (String, &'static str) {
    // Compact recent transcript so the model can actually respond to
    // the arc (and to "I already called"). Current user message is
    // the last history entry; render up to the last 8 messages.
    let messages = &context.conversation.messages;
    let start = messages.len().saturating_sub(8);
    let transcript = messages[start..]
        .iter()
        .map(|m| {
            let label = match m.role {
                Role::User => "Them",
                _ => "You",
            };
            format!("{label}: {}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!("Conversation so far:\n{transcript}\n\nWrite your reply now.");

    let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Slow);
    req.system_message = Some(CRISIS_SYSTEM_PROMPT.to_string());
    req.temperature = Some(0.2);
    req.max_tokens = Some(350);
    req.enable_thinking = Some(false);

    match inference.complete(&req).await {
        Ok(response) => {
            let text = crate::title::strip_thinking_response(&response.text);
            let text = crate::title::strip_source_citations(&text);
            let text = text.trim().to_string();
            // Degenerate output (empty, or a fragment too short to
            // carry care) falls back to the guaranteed template.
            if text.chars().count() < 40 {
                tracing::warn!(
                    chars = text.chars().count(),
                    "wellbeing gate: degenerate crisis synthesis — falling back to template"
                );
                (
                    wellbeing_response_text(signal.first_fire, message),
                    "template_fallback",
                )
            } else {
                (enforce_crisis_floor(text, message), "model_constrained")
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "wellbeing gate: crisis synthesis failed — falling back to template");
            (
                wellbeing_response_text(signal.first_fire, message),
                "template_fallback",
            )
        }
    }
}

/// Metadata for the persisted assistant message. `wellbeing_gate`
/// is the sticky marker `prior_crisis_context` keys on; `intent` +
/// trigger + mode make the gate legible in journals and provenance.
pub(crate) fn wellbeing_metadata(signal: &WellbeingSignal, mode: &str) -> serde_json::Value {
    serde_json::json!({
        "intent": "WellbeingGate",
        "wellbeing_gate": true,
        "wellbeing_trigger": signal.trigger,
        "wellbeing_first_fire": signal.first_fire,
        "wellbeing_mode": mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str, metadata: Option<serde_json::Value>) -> Message {
        Message {
            id: "m".into(),
            conversation_id: "c".into(),
            role,
            content: content.into(),
            created_at: 0,
            metadata,
            version: 0,
        }
    }

    #[test]
    fn lexicon_catches_explicit_disclosures() {
        assert!(lexical_crisis_signal("I keep thinking about killing myself."));
        assert!(lexical_crisis_signal("Honestly everyone would be better off without me"));
        assert!(lexical_crisis_signal("I just want to die."));
        assert!(lexical_crisis_signal(
            "I'm going to make myself stop breathing"
        ));
    }

    #[test]
    fn lexicon_does_not_fire_on_ordinary_heaviness() {
        assert!(!lexical_crisis_signal("This deadline is killing me."));
        assert!(!lexical_crisis_signal(
            "I'm thinking about ending things with Sam."
        ));
        assert!(!lexical_crisis_signal("Everything feels pointless lately."));
        assert!(!lexical_crisis_signal("I'm so tired of pretending to be fine."));
    }

    #[test]
    fn prior_context_sticky_via_gate_marker() {
        let messages = vec![
            msg(Role::User, "dark thoughts", None),
            msg(
                Role::Assistant,
                "care + resources",
                Some(serde_json::json!({"wellbeing_gate": true})),
            ),
            msg(Role::User, "the resources feel far away", None),
        ];
        assert!(prior_crisis_context(&messages));
    }

    #[test]
    fn prior_context_sticky_via_prior_user_lexical_hit() {
        let messages = vec![
            msg(Role::User, "I want to die.", None),
            msg(Role::Assistant, "un-gated reply from before the gate", None),
            msg(Role::User, "anyway, about work", None),
        ];
        assert!(prior_crisis_context(&messages));
    }

    #[test]
    fn current_turn_disclosure_is_not_prior_context() {
        // First disclosure lives in the LAST (current) user message —
        // the live layers must handle it so first_fire stays true.
        let messages = vec![
            msg(Role::User, "heavy day", None),
            msg(Role::Assistant, "witness reply", None),
            msg(Role::User, "I want to die.", None),
        ];
        assert!(!prior_crisis_context(&messages));
    }

    #[test]
    fn templates_carry_care_and_resources() {
        for first in [true, false] {
            let text = wellbeing_response_text(first, "The weight is unbearable today.");
            assert!(text.contains("988"), "template must name a crisis line");
            assert!(
                text.contains("findahelpline.com"),
                "template must carry a non-US resource"
            );
            // Direct care, not a policy refusal: first-person presence
            // language must be there, and refusal boilerplate must not.
            assert!(text.contains("I'm"), "template must speak in first person");
            assert!(!text.to_lowercase().contains("i cannot provide"));
            assert!(!text.to_lowercase().contains("as an ai"));
        }
    }

    #[test]
    fn continued_template_acknowledges_current_words() {
        let text = wellbeing_response_text(false, "I have a plan for tonight. It's decided.");
        assert!(
            text.contains("\"I have a plan for tonight.\""),
            "continued template must quote the user's current words"
        );
        // Plan + tonight = acute tier: the emergency line must appear.
        assert!(text.contains("911"), "acute signals must add the emergency line");
    }

    #[test]
    fn continued_template_without_acute_signals_stays_non_emergency() {
        let text = wellbeing_response_text(false, "The resources feel like distant noise.");
        assert!(text.contains("\"The resources feel like distant noise.\""));
        assert!(!text.contains("911"));
        assert!(text.contains("988"));
    }

    #[test]
    fn acknowledgment_fragment_bounds_and_first_sentence() {
        assert_eq!(
            acknowledgment_fragment("First sentence. Second sentence."),
            "First sentence."
        );
        let long = "a".repeat(300);
        let frag = acknowledgment_fragment(&long);
        assert!(frag.chars().count() <= 141); // 140 + ellipsis
        assert!(frag.ends_with('…'));
    }

    #[test]
    fn parse_crisis_verdict_handles_normal_and_inverted_shapes() {
        // Normal thinking shape: think block then JSON.
        assert_eq!(
            parse_crisis_verdict("<think>weighing</think>{\"crisis\": true}"),
            Some(true)
        );
        // Inverted fast-slot shape observed live: JSON, then </think>,
        // then prose — the shape that silently un-fired the gate.
        assert_eq!(
            parse_crisis_verdict(
                "{\n  \"crisis\": true\n}\n</think>\n\nThe message explicitly states intent."
            ),
            Some(true)
        );
        assert_eq!(parse_crisis_verdict("{\"crisis\": false}"), Some(false));
        assert_eq!(parse_crisis_verdict("no json at all"), None);
    }

    #[test]
    fn metadata_carries_sticky_marker_trigger_and_mode() {
        let meta = wellbeing_metadata(
            &WellbeingSignal {
                trigger: "lexicon",
                first_fire: true,
            },
            "model_constrained",
        );
        assert_eq!(meta["wellbeing_gate"], true);
        assert_eq!(meta["wellbeing_trigger"], "lexicon");
        assert_eq!(meta["intent"], "WellbeingGate");
        assert_eq!(meta["wellbeing_mode"], "model_constrained");
    }

    #[test]
    fn crisis_floor_appends_missing_resource() {
        let out = enforce_crisis_floor(
            "I'm right here with you, and what you just said matters.".to_string(),
            "The weight is unbearable.",
        );
        assert!(out.contains("988"), "floor must append a resource when missing");
    }

    #[test]
    fn crisis_floor_leaves_resourced_reply_alone() {
        let reply = "I'm staying with you. Please call or text 988 — you deserve a human for this.";
        let out = enforce_crisis_floor(reply.to_string(), "It's heavy tonight but I'm safe.");
        // Resource present → no duplicate append; "tonight" acute
        // signal → emergency line appended exactly once.
        assert_eq!(out.matches("988").count(), 1);
        assert!(out.contains("911"));
    }

    #[test]
    fn crisis_floor_adds_emergency_line_on_acute_signals() {
        let out = enforce_crisis_floor(
            "I hear you, and I'm not leaving. Please call 988 now.".to_string(),
            "I have a method and I've decided.",
        );
        assert!(out.contains("911") || out.to_lowercase().contains("emergency"));
    }

    #[test]
    fn kill_switch_env_contract() {
        // Default (unset) is ON; only the literal "0" disables. Not
        // testing via set_var (process-global); pin the parse rule.
        assert!("1" != "0");
        assert!(gate_enabled() || std::env::var(WELLBEING_GATE_ENV).as_deref() == Ok("0"));
    }
}
