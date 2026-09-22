// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decline detection — the predicates that recognise a released text
//! that asserts nothing: the honest abstention ("the sources don't
//! cover it") and the pure provenance-flagged NO_CLAIM decline. Split
//! out of `mod.rs` (file ceiling); the historical `grounding::` paths
//! hold via the re-exports there.

use super::strip_gk_caveat;

/// True when a released short answer is itself an honest abstention / decline
/// ("the sources don't cover it", "I'm not certain", the `grounded_abstention`
/// prose). Such an answer asserts no verifiable value, so the specifics scan has
/// nothing to fabricate-check — running it only surfaces kind-(3) noise (the
/// scan second-guessing a correct "not in sources" as a false claim ABOUT the
/// evidence). Skipping is a latency optimisation and errs fail-open: a false
/// skip just preserves prior behaviour. Measured 2026-07-01: 6/7 short-band scan
/// flags on GOOD answers were exactly these honest abstentions.
pub fn answer_declines(text: &str) -> bool {
    let h = text.trim_start().to_lowercase();
    const DECLINES: &[&str] = &[
        "i don't have reliable information",
        "i do not have reliable information",
        "i am not certain",
        "i'm not certain",
        "i do not have information",
        "i don't have information",
        "couldn't confirm an answer", // grounded_abstention prose (current)
        "could not confirm an answer", // grounded_abstention prose (current)
        "none of them actually cover it", // grounded_abstention prose (legacy, still in-the-wild)
        "i'd rather not guess",       // grounded_abstention prose (legacy)
        "do not contain",
        "does not contain",
        "not recorded there",
        "the sources do not",
        "the sources don't",
        "sources do not contain",
        "no passage",
        "not in your sources",
    ];
    DECLINES.iter().any(|d| h.contains(d))
}

/// True when a NO_CLAIM release is a pure provenance-flagged decline — the
/// model saying "I don't have reliable information in my knowledge base"
/// over retrieved-but-useless evidence. Such a turn asserts nothing, so
/// releasing it as an answer mis-states the turn's epistemic standing: the
/// ledger derives `Unverified` (evidence present, nothing audited), the
/// coverage probe never runs (`gap_turn=false`), and a genuine knowledge
/// gap defaults to `ClaimUncovered` (bench/gap_check/DECISION.md, bug 2).
/// A 0-holding decline IS an abstention — reclassify the ACTION, keep the
/// model's own (honest, already provenance-flagged) prose.
///
/// Deliberately narrower than [`answer_declines`]: a caveated parametric
/// answer ("Not in your sources — from general knowledge: Canberra…")
/// declines-then-ANSWERS, and must keep releasing — so the caveat is
/// stripped first and any remaining "from general knowledge" pivot vetoes
/// the reclassification.
/// Did a claim-free release actually abstain?
///
/// ONE decider, on both arms of the native-grounding flag: the incumbent
/// 17-phrase zoo, which recovers the decision the system made but never
/// carried.
///
/// **Why the typed verdict is not consulted here.** It used to be: when
/// H1 had run, its `decision` supplied the action directly and the zoo was
/// skipped. P1 retired that (`NATIVE_GROUNDING_PARITY_PLAN.md` §4.1 —
/// admission is telemetry, "decisions traced, never enforced"), because
/// letting it stand made the flag change a turn's *action* in both
/// directions: a prose decline under a typed `Answer` stayed `released`
/// on the flag-on arm while flag-off reclassified it, and the epistemic
/// ledger, the collaboration surface and the honesty scorer all read that
/// string. That divergence is exactly what A1's arm-identity check
/// forbids, and A1 is the plan's pre-registered kill for the whole phase.
/// The typed path returns at P3c, when a verdict is enforced again by a
/// signal that earned it.
///
/// Returns the legacy action string to reclassify to, or `None` to leave
/// the action alone. Pure — no model, no env, no clock.
pub(crate) fn abstention_action(text: &str) -> Option<&'static str> {
    released_pure_decline(text).then_some("abstained_decline")
}

pub fn released_pure_decline(text: &str) -> bool {
    let stripped = strip_gk_caveat(text);
    if stripped.to_lowercase().contains("from general knowledge") {
        return false;
    }
    answer_declines(&stripped)
}
