//! Repair text: the corrective material and notes a failed claim carries
//! into a retry, a rewrite, or a marked answer.

use super::*;

/// One audit-failed claim plus the claim-conditioned passages its
/// targeted search returned — the rewrite's correction material.
pub(crate) struct FailedClaim {
    pub(crate) claim: String,
    pub(crate) evidence: Vec<String>,
}

/// The grounded abstention released when both drafts fail the gate.
///
/// Deliberately does NOT restate the rejected claim's value. The old wording
/// ("The draft answer asserted that Heat's first name is Vernon …") re-uttered
/// the fabrication even while disclaiming it: a strict judge reads the named
/// value as an answer (measured — the primary judge scored these as "answered",
/// so the gate's abstentions didn't count), and a skimming user sees the
/// fabricated specific anyway. The failed claim is preserved in the gate's
/// glassbox `meta` / trace, not in the user-facing text — observability without
/// leakage.
///
/// Wording is a SELF-SCOPED epistemic hedge ("I couldn't confirm …"), NOT a
/// universal claim about the sources ("none of them cover it"). Measured
/// 2026-07-08 (8h chaos run, class-A "evidence-denial"): the gate's short
/// citation path abstains far more often than the evidence warrants (single-digit
/// answers filtered, verbatim quote-match misses), so the abstention frequently
/// fires when the answer IS in the passages. A universal negative is then a FALSE
/// statement about the sources — the trust rubric scores it as confabulation, and
/// it reads to the user as the app denying its own evidence. An assistant-scoped
/// "I couldn't verify this against them" is honest in BOTH the true-miss and the
/// mis-abstain case (it claims only the assistant's confidence, never the
/// sources' content), and the calibrated judge's decline-shape override already
/// treats it as an honest limitation rather than a fabrication.
pub(crate) fn grounded_abstention(_claim: &str, chunks_checked: usize) -> String {
    format!(
        "I couldn't confirm an answer to this against the {chunks_checked} passages \
         your sources turned up — so rather than guess at something I can't verify \
         from them, I'd flag that instead. If you think it's there, try rephrasing \
         with the specific names or terms involved and I'll take another look."
    )
}

/// Remove a leading general-knowledge caveat ("Not in your sources — from
/// general knowledge: …") so the gate verifies the asserted CLAIM, not the
/// hedge. Applied ONLY on entity-anchored questions: there a GK caveat can never
/// legitimately answer an in-world question, so the value after it must be
/// grounded or dropped. For genuinely out-of-domain questions (not
/// entity-anchored) the caveat IS the honest move and is left intact — this is
/// why the strip is gated on `entity_anchored`, not applied unconditionally.
pub(crate) fn strip_gk_caveat(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(crate::runtime::prompts::GK_CAVEAT_PREFIX) {
        return rest.trim_start().to_string();
    }
    // Robustness: the marker may not sit at the very start.
    let low = text.to_lowercase();
    if let Some(p) = low.find("from general knowledge:") {
        if let Some(after) = text[p..].split_once(':').map(|x| x.1) {
            return after.trim().to_string();
        }
    }
    text.to_string()
}

/// System-message suffix for the single gated retry. Quotes the failed
/// claim back — the second draft knows exactly which assertion failed
/// verification and must either ground it or drop it.
pub(crate) fn retry_system_note(claim: &str, corrective: &[String]) -> String {
    const RETRY_EVIDENCE_PER_CLAIM: usize = 2;
    const RETRY_EVIDENCE_CHARS: usize = 700;
    let mut note = format!(
        "\n\nGROUNDING CHECK FAILED on your previous draft. It asserted: \"{claim}\" — \
         no retrieved passage supports that assertion."
    );
    if corrective.is_empty() {
        note.push_str(
            " Write a new answer using ONLY what the passages state. If the passages \
             do not contain the asked-for fact, say plainly that the sources do not \
             state it. Do not repeat the unsupported assertion.",
        );
    } else {
        // Parity with the long-form rewrite (measured v13c–v15): a
        // retry told only WHICH assertion failed, with no passages
        // stating the truth, can only delete and disclaim.
        note.push_str("\n  What the sources actually say on this point:");
        for p in corrective.iter().take(RETRY_EVIDENCE_PER_CLAIM) {
            let trimmed: String = p.chars().take(RETRY_EVIDENCE_CHARS).collect();
            note.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
        }
        note.push_str(
            "\nWrite a new answer using ONLY what the passages state — if the \
             passages above contain the asked-for fact, state it (with citations); \
             do not repeat the unsupported assertion.",
        );
    }
    note
}

/// Decode-committed opening for the long-form rewrite. Instruction-only
/// shape rules measured non-compliant (v14: the rewrite still led with
/// "I do not have access to passages detailing…" despite an explicit
/// "do not open with what the passages lack" rule — same ~60%
/// instruction-wall as the GK caveat). Committing the opening forces
/// the rewrite to continue into the supported account; the abstain
/// read of a disclaimer-led head disappears structurally. Like
/// GK_CAVEAT_PREFIX, assistant_prefix is decode-commit only — the
/// caller must prepend it to the returned text.
/// User-facing wording (grace audit 2026-07-11): the previous prefix
/// ("From the retrieved sources, here is what can be established:")
/// injected auditor-speak as the OPENING of every rewritten answer — a
/// structural jargon hit on the grace gate's `clean` component. The
/// prefix's decode-commit job (force continuation into the supported
/// account) needs no machinery reference.
pub const LONGFORM_REWRITE_PREFIX: &str = "Here's what I can say with confidence:\n\n";

/// Rewrite-request system note: every failed claim, each with the
/// passages its targeted corpus search returned (when any). The
/// correction material is the point — v13c/v14/v14b measured that a
/// rewrite told only WHICH assertions failed, with no passages
/// stating the truth, can only delete and disclaim.
pub(crate) fn rewrite_system_note(failed: &[FailedClaim]) -> String {
    const REWRITE_EVIDENCE_PER_CLAIM: usize = 2;
    const REWRITE_EVIDENCE_CHARS: usize = 700;
    let list = failed
        .iter()
        .map(|f| {
            let mut entry = format!("- \"{}\"", f.claim);
            if f.evidence.is_empty() {
                entry.push_str(
                    "\n  (no corpus passage states this — remove it, or say the \
                     sources do not establish it)",
                );
            } else {
                entry.push_str("\n  What the sources actually say on this point:");
                for p in f.evidence.iter().take(REWRITE_EVIDENCE_PER_CLAIM) {
                    let trimmed: String = p.chars().take(REWRITE_EVIDENCE_CHARS).collect();
                    entry.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
                }
            }
            entry
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\nGROUNDING AUDIT FAILED on your previous draft. These assertions did not \
         verify against the sources:\n{list}\n\
         Rewrite the answer: keep everything the sources support. For each failed \
         assertion that has corrective passages above, REPLACE it with what those \
         passages actually state, citing them — do not merely delete it. Never add \
         a NEW statement about what the sources say, cite, name, or omit unless a \
         passage above shows it. Structure \
         the rewrite as an ANSWER, not a disclaimer: open directly with the \
         supported account, organized to address the question. Do not open with \
         what the sources lack, and do not enumerate the removed assertions in the \
         body. If material gaps remain, note them briefly in a single short \
         paragraph at the end."
    )
}

/// The user-visible verification note. Items are answer spans / short claims
/// (`normalize_scan_item` reduces scan output toward answer wording); render
/// each one deduped and length-capped, in plain language — judge vocabulary
/// must never reach the user (observed 2026-07-01: raw scan chatter footnoted
/// a released answer with "… is a fabricated specific").
///
/// Items are deliberately UNQUOTED: the post-synthesis quote guardrail
/// (`quote_verification::verify_answer_against_turn_evidence`, streaming.rs) treats
/// any curly-quoted span as a quotation claim and demotes what it can't
/// verbatim-confirm — a quoted note item (a paraphrased claim, by nature not
/// verbatim) was rewritten to "[unverified excerpt: …]", turning the app's own
/// footer into a self-contradiction (probed 2026-07-01: the note trace showed
/// clean items; the released text showed them wrapped).
/// EXPERIMENT (`SOVEREIGN_NOTE_AS_METADATA=1`): keep the verification note
/// OUT of the answer text — the failed claims already ride
/// `GateOutcome.meta.failed_claims` → `metadata.grounding_gate`, and the
/// desktop renders them as a collapsible disclosure instead. Persona-QA
/// receipts (2026-07-11): the appended note owns the answer's final words
/// ("— The evidence states…", "[unverified excerpt:…]"), which zeroes the
/// grace gate's `agency`/`clean` components and buries the model's own
/// closing line — the honest audit trail read as auditor-speak in user
/// space. Default OFF: non-desktop surfaces (API/CLI) keep the in-text
/// note so a known-failed claim is never silently released without its
/// caveat (the never-silent invariant).
pub(crate) fn append_note(text: String, note: &str) -> String {
    let as_metadata = std::env::var("SOVEREIGN_NOTE_AS_METADATA")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if as_metadata {
        text
    } else {
        format!("{text}{note}")
    }
}

pub(crate) fn verification_note(failed_claims: &[String]) -> String {
    const NOTE_ITEM_CHARS: usize = 160;
    let mut seen = std::collections::HashSet::new();
    let items: Vec<String> = failed_claims
        .iter()
        .map(|c| {
            let c = unwrap_unverified_excerpts(c);
            let c = c.trim().trim_matches(['"', '“', '”']).trim();
            let mut item: String = c.chars().take(NOTE_ITEM_CHARS).collect();
            if c.chars().count() > NOTE_ITEM_CHARS {
                item.push('…');
            }
            item
        })
        .filter(|c| !c.is_empty() && seen.insert(c.to_lowercase()))
        .map(|c| format!("- {c}"))
        .collect();
    tracing::info!(
        target: "grounding_gate",
        n_claims = failed_claims.len(),
        n_items = items.len(),
        first_claim_head = %failed_claims.first().map(|c| c.chars().take(80).collect::<String>()).unwrap_or_default(),
        first_item_head = %items.first().map(|c| c.chars().take(80).collect::<String>()).unwrap_or_default(),
        "verification note rendered"
    );
    format!(
        "\n\n---\n*Verification note: these statements could not be confirmed \
         against your sources — treat them as unverified:*\n{}",
        items.join("\n")
    )
}
