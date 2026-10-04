// SPDX-License-Identifier: AGPL-3.0-or-later
// Paired A/B analysis for two chaos passes over the SAME question bank (e.g.
// quote-first ON vs OFF, via SOVEREIGN_CHAOS_REPLAY). Pairs by question text and
// runs McNemar on the discordant pairs — a within-subject test that cancels the
// question-mix variance which made single-run A/Bs undecidable (±10% at n=40).
//
//   paired-ab.mjs <passA.jsonl> <passB.jsonl> [labelA] [labelB]
import fs from "node:fs";

const [fA, fB, labelA = "A", labelB = "B"] = process.argv.slice(2);
if (!fA || !fB) {
  console.error("usage: paired-ab.mjs <passA.jsonl> <passB.jsonl> [labelA] [labelB]");
  process.exit(2);
}

function load(f) {
  const m = new Map();
  for (const l of fs.readFileSync(f, "utf8").split("\n").filter(Boolean)) {
    try {
      const r = JSON.parse(l);
      if (r.cmd !== "send_message_stream" || !r.aligned) continue;
      const q = JSON.parse(r.args ?? "{}").message;
      if (q && !m.has(q)) {
        m.set(q, {
          broke: r.userJudge?.broken === true,
          verdict: r.aligned.verdict,
          quoted: /Grounded in the source/.test(r.answer ?? ""),
        });
      }
    } catch {
      /* skip */
    }
  }
  return m;
}

// Abramowitz-Stegun erf for the McNemar normal approximation.
function erf(x) {
  const t = 1 / (1 + 0.3275911 * Math.abs(x));
  const y =
    1 -
    ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t +
      0.254829592) *
      t *
      Math.exp(-x * x);
  return x >= 0 ? y : -y;
}

const A = load(fA);
const B = load(fB);
const paired = [...A.keys()].filter((q) => B.has(q));

let aBroke = 0,
  bBroke = 0,
  aOnly = 0,
  bOnly = 0,
  both = 0,
  neither = 0;
for (const q of paired) {
  const a = A.get(q).broke;
  const b = B.get(q).broke;
  if (a) aBroke++;
  if (b) bBroke++;
  if (a && !b) aOnly++;
  else if (b && !a) bOnly++;
  else if (a && b) both++;
  else neither++;
}

console.log(`\n══ PAIRED A/B — ${labelA} vs ${labelB} ══`);
console.log(`paired questions: ${paired.length}   (only in ${labelA}: ${A.size - paired.length}, only in ${labelB}: ${B.size - paired.length})`);
const pct = (x) => (paired.length ? `${((100 * x) / paired.length).toFixed(0)}%` : "—");
console.log(`broke-rate:  ${labelA} ${aBroke}/${paired.length} (${pct(aBroke)})   ${labelB} ${bBroke}/${paired.length} (${pct(bBroke)})`);
console.log(`discordant:  ${labelA}-worse(broke,${labelB}-ok)=${aOnly}   ${labelB}-worse(broke,${labelA}-ok)=${bOnly}   (both-broke=${both}, neither=${neither})`);

const disc = aOnly + bOnly;
if (disc === 0) {
  console.log(`McNemar: 0 discordant pairs — identical on the paired set (no effect detectable).`);
} else {
  // Continuity-corrected McNemar z on the discordant split (null: 50/50).
  const z = (Math.abs(aOnly - bOnly) - 1) / Math.sqrt(disc);
  const p = 2 * (1 - 0.5 * (1 + erf(Math.max(0, z) / Math.SQRT2)));
  const better = aOnly > bOnly ? labelB : bOnly > aOnly ? labelA : "neither";
  console.log(
    `McNemar: z=${z.toFixed(2)}  ~p=${p.toFixed(3)}  → ${better === "neither" ? "tie" : `${better} fewer broke`}  ${p < 0.05 ? "[SIGNIFICANT]" : "[not significant — need more paired questions]"}`,
  );
}
console.log("");
