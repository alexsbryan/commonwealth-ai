// SPDX-License-Identifier: AGPL-3.0-or-later
// FELT-QUALITY rubric — orthogonal to the trust rubric (rejudge-rubric.mjs,
// which asks "can I trust this"). This asks the harder, softer question a
// frontier-accustomed user actually feels: "was this a pleasure to receive,
// by the standard of the best assistants I use every day?"
//
// The judge is calibrated to FRONTIER expectations on purpose — we WANT it to
// notice when a grounded answer is correct-but-stiff, or declines something a
// top assistant would have answered. That gap is the honest cost of the
// grounded design, and hiding it would defeat the eval. This score is a PROXY;
// the ground-truth verdict is the human reading the surfaced answers.
//
// Five 0-2 dimensions + one overall satisfaction call. Kept parsimonious so a
// 1-token-attention judge weights each clause (feedback: parsimonious prompts).
export const FELT_SYSTEM = `You are a discerning user who relies on the best AI assistants (GPT-class, Claude-class) every day. Judge ONE answer to YOUR question by the exact standard you hold your primary assistant to. Do not grade on a curve for it being a smaller or local model.

Rate each 0, 1, or 2 (0 = poor, 1 = acceptable, 2 = excellent):
- responsive: answers what you actually asked, right away — no throat-clearing, no talking about its own process or "sources" before the substance.
- substantive: genuinely useful — right depth for the ask, real content, complete enough, not padded or thin.
- clear: well-organized and easy to read; structure that helps, not clutters.
- natural: reads like a capable assistant — confident, plain, warm. NOT robotic, NOT machinery-speak ("the retrieved sources", "the passages", "what can be established"), NOT groveling or lecturing.
- trustworthy: accurate and appropriately confident; backs claims (a citation when it helps); honest about what it can't cover WITHOUT hiding behind hedges.

A confident, correct, well-cited answer to a covered question is excellent. An answer that is correct but stiff/robotic loses "natural". A refusal or "your sources don't cover this" — even if honest — is what a frontier user feels as a MISS: low responsive/substantive, and overall at best "mixed".

Then ONE overall call — would you be satisfied receiving this from your primary assistant? satisfied | mixed | dissatisfied.

JSON only: {"responsive":0-2,"substantive":0-2,"clear":0-2,"natural":0-2,"trustworthy":0-2,"overall":"satisfied|mixed|dissatisfied","why":"<one line>"}`;

export function feltJudgeMessages(question, answer) {
  return [
    { role: "system", content: FELT_SYSTEM },
    {
      role: "user",
      content: `MY QUESTION:\n${question}\n\nTHE ANSWER:\n${String(answer).slice(0, 12000)}`,
    },
  ];
}

const DIMS = ["responsive", "substantive", "clear", "natural", "trustworthy"];

export function parseFelt(json) {
  if (!json || typeof json !== "object") return null;
  const clamp = (x) => Math.max(0, Math.min(2, Math.round(Number(x))));
  const dims = {};
  for (const d of DIMS) {
    if (typeof json[d] !== "number") return null;
    dims[d] = clamp(json[d]);
  }
  const overall = String(json.overall ?? "").toLowerCase();
  if (!["satisfied", "mixed", "dissatisfied"].includes(overall)) return null;
  const total = DIMS.reduce((a, d) => a + dims[d], 0); // 0..10
  return { ...dims, total, overall, why: String(json.why ?? "").slice(0, 160) };
}

export const FELT_DIMS = DIMS;
