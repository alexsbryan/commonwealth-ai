# Bank v0 — README

**Bank v0 mint, 2026-08-14, order `deep-research-t0b`** — the instruments
every later tier measures against. Contents:

| Artifact | Path | Contents |
|---|---|---|
| Seed set | `seeds.md` | 12 questions + coverage keys (seeds 1-3 verbatim from the hand-run kill gate) |
| Adjacent set | `adjacent.md` | 12 drift-brother questions, one per seed, 2-4 keys each |
| Repeat set | `repeat.md` | 6 verbatim seed re-asks (seeds 1, 2, 4, 7, 8, 10) |
| Poisoned fixtures | `poisoned/` | 3 drill dirs: fabrication, prompt-injection, combined P5 shape |
| Labeled set | `labeled/` | 100 claims (60 supported / 40 unsupported) for FR-6 |

## The numeric bars — PROPOSALS, pre-registered in this mint commit

Per the order (§18.6): numeric bars are finalized in the mint commit,
before any arm runs. These are **proposals**: the worker proposes, the
operator approves at the landing review. No arm result existed when these
numbers were written; the mint commit is the pre-registration event.

**Bar 1 — dr-compass (R-12): `X = 10 of 12` seeds.** The gap-convergence
bar: the round-2 gap set is a strict subset of round-1's on **at least 10
of the 12 seed questions** (the hand-run's own criterion: strict shrinking,
gaps phrased as search-actionable queries). Rationale: the kill gate
passed 3/3; 10/12 keeps the standard nearly strict while leaving room for
a question whose estate context genuinely resists R1→R2 shrinking (a
legitimate signal, not a pass — it must be journaled per question).

**Bar 2 — P4 local floor (R-10, dr-local-loop): `K/N = 80% of total
coverage keys` restated absolute.** The coverage floor is stated as an
absolute count of the bank's own acceptance shapes, not a fraction of a
model-chosen denominator (per the PLAN's FR-4 refutation). Bank v0 carries
**93 coverage keys** (12 seeds: 6+6+7+6+6+6+6+5+6+6+6+6 = 72; 12 adjacent:
4+4+4+4+4+4+4+4+4+4+4+4 = 48 — see correction below). K/N restated: the
local-only arm must clear **80% of the seed keys (57.6 → 58 of 72)** by
the P4 checkpoint, with adjacent-set coverage reported alongside (not
gated). Rationale: the hand-run showed estate-only R1 scores 0 on frontier
seeds — the floor measures how much the loop, not the estate, recovers;
80% leaves honest headroom for evidence-corrected keys while proving the
loop earns its keep over the R1 baseline.

**Bar 3 — dr-instrument-validated (R-7, FR-6): decided ON THE NUMBER by
the FR-6 report, per PLAN §4 T0.** No numeric bar is pre-registered here
by design: the spec's posture (dual-string, disagreement → could-not-
judge) is exactly what the measurement tests. The report's agreement +
joint-miss numbers, and its keep/drop/redesign recommendation, are the
decision input.

> **Measured 2026-08-14** (report: `research/deep-research/notes/fr6.md`,
> raw detail: `labeled/fr6-report.json`): on the corrected bank the two
> strings agree 100/100 (0 joint-miss, 0 false alarms, 0 never-ran) — a
> perfectly correlated verdict structure; the disagreement→could-not-
> judge path never fires. The report recommends REDESIGN (the residual
> failure shape is shared world-knowledge bias, not disagreement);
> keep/drop/redesign is the operator's call on these numbers. The label
> corrections that precede the headline numbers are journaled in the
> report (the strings caught three bank authoring defects — two
> mislabeled supported claims, one rule-violating unsupported claim).

> **Correction (minted here, before any arm ran):** the adjacent set is 48
> keys (12 × 4), not 2-4 per question as sketched in the order — the
> order's own language ("2-4 keys each") permits this; 48 total. The P4
> floor's K/N therefore counts seed keys only (72), with adjacent keys
> reported as the generalization probe. This correction is made in the
> mint commit itself, timestamped before any measurement.

## Scoring rules (structured match, C — never a judge)

The PLAN's R-10 instrument: coverage is scored by **structured match**, a
deterministic rule, not an LLM judge:

- A key is **covered** when the round's answer names the key's subject and
  the round's evidence supports it (the hand-run's acceptance shape:
  *answered when we can name X, date Y, the causal link Z*).
- **Partial is a gap** under the all-of rule (hand-run seed 3, K2
  partial → gap): every element of the key must be present.
- **Evidence corrections** (hand-run seeds 1-K6, 3-K7): where the round's
  evidence corrects a key's hypothesis (date, attribution, figure), the
  key is covered when the CORRECTED fact is named and supported — the
  evidence is the arbiter. Corrections are journaled per key, never
  silently applied.
- The scorer is the structured-match checker (`fr6`-independent;
  implemented for T1 as a deterministic script over the answer+evidence
  artifacts), never a model.

## NWCI record

All 12 seeds + 12 adjacent + 6 repeats + labeled set were authored from
operator/agent knowledge alone, BEFORE any chat/retrieval/gate/search
invocation of this slice. No retrieval result, no gate verdict, no answer
text, no corpus listing was consulted. The full record is in `seeds.md`
(the test applies to all 12; adjacent/repeat/labeled follow the same
discipline). Any key that could only have been written by consulting
system output would be a kill-report, not a workaround — none exists; the
NWCI test passes for all 12 seeds.

## Lane sizing (recorded after the dry run)

The timed dry run (one seed through the 3-round recipe) and its lane
decision land HERE in the same commit as the run. If one run > ~20 min,
bank size and cadence are decided by arithmetic, not preference — the
honest home is the weekly tier, not `--quick`. (Placeholder: filled by the
dry-run commit.)

## Arms that may use this bank

- **dr-compass** (T1): the 3-round gap loop over the 12 seeds; bar 1.
- **dr-local-loop** (T1): the local-only loop over seeds + adjacent;
  bar 2. Repeats run at the cadence the dry run decides, comparing
  re-opened keys against the original run (instrument vs estate
  diagnosis).
- **dr-instrument-validated** (T0): the FR-6 measurement over `labeled/`;
  bar 3.
- **P5 drill** (T1): `poisoned/` fixtures through the loop; acceptance is
  the drill shape in each fixture's README (100%, no noise band).
