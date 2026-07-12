// SPDX-License-Identifier: AGPL-3.0-or-later
// Outcome taxonomy (PERSONA_QA_DESIGN.md §6) — shared by personas.mjs (live)
// and persona-gap-atlas.mjs (which RECLASSIFIES journal rows so taxonomy
// fixes retroactively correct past runs' numbers).
//
// v2 (2026-07-10): answer QUALITY classifies; the gap card is ORTHOGONAL.
// v1 ranked "card fired" above everything, which made answered_grounded
// unreachable — the gap check fires liberally ("anything external would
// sharpen this"), so a good grounded answer + a helpful card classified as
// gap_admitted_offered. Measured: a receipt-good in-corpus answer (judge
// good, evidence-present, 40 chunks) counted as a gap outcome; whole runs
// showed grounded=0 for taxonomy reasons, not product reasons.
//
// Precedence (first match wins):
//   canceled_slow | turn_error | turn_timeout      — the turn never landed
//   rescued_by_web                                 — clicked, refined changed, judged good
//   search_blocked                                 — clicked, backend failed
//   search_futile                                  — clicked, no better answer came back
//   answered_ungrounded                            — confident + judged fine but bench says hallucination
//   answered_grounded                              — judged good and not a decline-shape
//   gap_admitted_offered / gap_admitted_no_offer   — decline-shape answer, card / no card
//   silent_gap                                     — judged unresponsive, no admission, no card
export const DECLINE_RE =
  /\b(don't have|do not have|no (specific|relevant)|not (in|covered by|available in) (your|the|my)|couldn't find|could not find|sources? (don't|do not)|cannot provide)\b/i;

export function classifyOutcome(t) {
  if (t.canceled) return "canceled_slow";
  if (t.error) return "turn_error";
  if (t.timeout) return "turn_timeout";
  const judgeGood = t.judge ? !t.judge.broken && t.judge.score < 6 : null;
  const declineShape =
    t.aligned?.verdict === "honest_abstention" ||
    t.aligned?.verdict === "caveated_ood" ||
    (t.aligned == null && DECLINE_RE.test(t.answer ?? ""));
  // The `aligned` scorer (bench score-answer → assess_asserted_value) is
  // VALUE-EXTRACTION-shaped: it returns honest_abstention / caveated_ood
  // whenever an answer asserts no single checkable atomic value. But that is
  // the NORMAL shape of a grounded process/explanation answer ("how does
  // photosynthesis work", a source-cited argumentative correction) — no single
  // "value", yet fully grounded and answered. Two INDEPENDENT oracles catch
  // this: the persona judge (did the persona get a satisfying answer) and the
  // careful-reader presence judge (does the evidence actually answer it). When
  // BOTH vouch, a substantive grounded answer must not be vetoed into the gap
  // family by the mis-applied extraction scorer. Receipts (2026-07-11): 3/12
  // gap-family turns were judge-good + evp-true explanations mis-bucketed as
  // gaps — grounded_rate was near-zero for TAXONOMY reasons, not product ones.
  // evp-false (genuine no-evidence decline) and judge-broken cases never
  // qualify, so honest graceful declines and real failures stay gaps.
  const groundedByOracles = judgeGood === true && t.evidencePresence === true;
  if (t.search?.clicked) {
    if (t.search?.error || t.search?.blocked) return "search_blocked";
    // The re-gate can revert (message-refined echoes the original) — never a
    // rescue. rescued requires CHANGED content the user-judge accepts.
    if (t.refinedChanged && t.refinedJudge && !t.refinedJudge.broken && t.refinedJudge.score < 6)
      return "rescued_by_web";
    return "search_futile";
  }
  if (t.aligned?.verdict === "hallucination" && judgeGood !== false) return "answered_ungrounded";
  if (judgeGood === true && (!declineShape || groundedByOracles)) return "answered_grounded";
  if (declineShape) return t.card ? "gap_admitted_offered" : "gap_admitted_no_offer";
  if (judgeGood === false) return t.card ? "gap_admitted_offered" : "silent_gap";
  // No judge verdict and no decline shape — count it as grounded-by-default
  // only when the bench says grounded; otherwise it's an unjudgeable gap.
  return t.aligned?.verdict === "grounded" ? "answered_grounded" : "silent_gap";
}

export const GAP_FAMILY = new Set([
  "gap_admitted_offered",
  "gap_admitted_no_offer",
  "silent_gap",
  "rescued_by_web",
  "search_futile",
  "search_blocked",
]);

// Rebuild the classifier input from a JOURNAL ROW (all fields are journaled),
// so the atlas can reclassify past runs under the current taxonomy.
export function reclassifyRow(row) {
  return classifyOutcome({
    canceled: row.outcome === "canceled_slow",
    error: row.error,
    timeout: row.outcome === "turn_timeout",
    answer: row.answer,
    judge: row.judge,
    aligned: row.aligned,
    evidencePresence: row.evidencePresence,
    card: row.card,
    search: row.search,
    refinedChanged: row.refinedChanged,
    refinedJudge: row.refinedJudge,
  });
}
