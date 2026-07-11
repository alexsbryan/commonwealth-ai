// SPDX-License-Identifier: AGPL-3.0-or-later
// Persona-QA judge prompts + parsers, shared by personas.mjs (live runs) and
// calibrate-persona-judge.mjs (the calibration gate). One definition so the
// calibrated prompt IS the deployed prompt — the chaos methodology's rule:
// no judge scores runs without passing the calibration bank.
//
// V1 (numeric 0-10, broken iff >=6): the original judgeAsUser-derived
// wording. Calibration found intermittent SCALE INVERSION on the 35B — a
// praising `why` with score=10 ("accurately and comprehensively…" → 10/10
// broken) in ~7/22 receipt-verified cases: the model sometimes reads
// "score" as quality-out-of-10 despite the instruction.
//
// V2 (categorical): verdict ∈ good | partial | broken — no numeric scale to
// invert. Same evolution the offline re-judge rubric went through
// (numeric → categories) for the same reason.

export function personaJudgeV1Messages(question, answer, goal) {
  return [
    {
      role: "system",
      content:
        "You are a demanding, knowledgeable user judging the assistant's answer to YOUR question. " +
        "A correct, coherent, complete, responsive answer scores LOW (good). Wrong, empty, cut off, " +
        "incoherent, or hallucinated scores HIGH (broken). " +
        "If the app genuinely lacks the sources, an honest decline is correct — but ONLY if GRACEFUL: " +
        "brief, warm, pointing to a path forward. A graceful honest decline scores LOW. An abrasive, " +
        "cold, or lecturing refusal scores HIGH even though honest. " +
        'JSON only: {"broken":true|false,"score":0-10,"why":"<one line>"}. broken=true iff score>=6.',
    },
    {
      role: "user",
      content: `My goal: ${goal}\nMy question:\n${String(question).slice(0, 600)}\n\nThe app's answer:\n${String(
        answer,
      ).slice(0, 12000)}\n\nJudge it.`,
    },
  ];
}

export function parseV1(json) {
  if (!json || typeof json.score !== "number") return null;
  const score = Math.max(0, Math.min(10, json.score));
  return { broken: !!json.broken || score >= 6, score, why: String(json.why ?? "").slice(0, 140) };
}

export function personaJudgeV2Messages(question, answer, goal) {
  return [
    {
      role: "system",
      content:
        "You are the user judging the assistant's answer to YOUR question. Pick ONE verdict, first match wins:\n" +
        "broken: wrong, incoherent, cut off, irrelevant, a cold lecturing refusal, or a non-answer after you already re-asked.\n" +
        "partial: addresses it but incomplete or too terse to act on.\n" +
        "good: correct, coherent, addresses the question — OR honestly says the sources don't cover it, briefly and warmly, with a path forward.\n" +
        'JSON only: {"verdict":"broken|partial|good","why":"<one line>"}',
    },
    {
      role: "user",
      content: `My goal: ${goal}\nMy question:\n${String(question).slice(0, 600)}\n\nThe app's answer:\n${String(
        answer,
      ).slice(0, 12000)}\n\nJudge it.`,
    },
  ];
}

// V2 maps onto the V1 shape so personas.mjs call sites don't change:
// good→2, partial→5 (below the satisfaction thresholds), broken→9.
export function parseV2(json) {
  const v = String(json?.verdict ?? "").toLowerCase();
  if (!["good", "partial", "broken"].includes(v)) return null;
  const score = v === "good" ? 2 : v === "partial" ? 5 : 9;
  return { broken: v === "broken", score, verdict: v, why: String(json?.why ?? "").slice(0, 140) };
}

export const VARIANTS = {
  v1: { messages: personaJudgeV1Messages, parse: parseV1 },
  v2: { messages: personaJudgeV2Messages, parse: parseV2 },
};
