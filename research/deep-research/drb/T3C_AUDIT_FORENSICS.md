# T3C — audit-forensics enforcement-gap map (order `deep-research-t3c`)

The verification arm's artifact-level forensics on the frozen t2b DRB
holdout. Scope: the 20 flights in `runs/{local,hybrid}/` (10 tasks × 2
arms, ids 56/58/59/62/65/69/78/83/90/95, hashes per `SHA256SUMS`,
verified OK, **never edited**), the demo9 score files, and the deep-research
audit gate (`sovereign-core/src/deep_research/audit.rs`,
`containment.rs`, `render.rs`) that produced the verdict-sets.

Method: artifact-level forensics. `SOVEREIGN_GATE_AUDIT_FORENSICS` was
**not set** on the t2b flights (pass site 5 below), so per-claim
judge-window records are not in the artifacts; every finding here is
re-derived from the frozen artifacts themselves (verdict-sets,
gap-lists, evidence windows) and checked against the gate's recorded
decisions. All counts in this document are mechanical — no model was
asked to classify anything.

Two instruments are named throughout, because they are different
instruments: the **audit gate** (the run's own claim checker — verdicts
failed / could-not-judge / passed) and the **DRB scorer** (the
vendored FACT pipeline — verdicts supported / unsupported / unknown).
The forensics map covers both; the pass sites live at their seams.

## The measured landscape (as-shipped, old instrument: 35B at :9741)

Across the 20 flights: **616 claims** — 99 failed ("refuted by the
evidence"), 512 could-not-judge, 5 passed. The could-not-judge mass
breaks into the three flags: **308 corroboration-floor**
(single-origin support, GAP-2/F22, downgrade-only — the honest one),
**182 extracted-specifics-absent** (the witness downgrade —
`all_absent`, "extracted specifics absent from the evidence"),
**22 not-judgeable**. The scorer side: 219 local pairs (85 judged: 11
supported / 74 unsupported / 134 unknown), 117 hybrid pairs (112
judged: 72 / 40 / 5 unknown).

The fabrication signal itself (the 99 failed claims; the scorer's
unsupported counts) is NOT in question here — the artifacts support
it. This map is about what could pass through *unmeasured or
mislabeled*: the 512-claim could-not-judge mass whose render a reader
cannot decode, and the 139-unknown abstention dimension the scorer
never scored.

## Pass site 1 — the render: 83% of claims pass through verbatim, citations unreadable

- **What passes through.** 512/616 claims (83%) render into the
  report's Open questions with their model-written `[Source: …]` tails
  intact and their figures intact (`render.rs` FinalClaim.text = raw
  claim text; flag → "open question: …" wording). A reader cannot
  distinguish *figure-verified* from *figure-untraced*: the "untraced:
  68" claim renders as "open question: extracted specifics absent from
  the evidence" with `[Source: ev-1]` printed beside it.
- **The guard that should have caught it — citation grounding.**
  The structured citation channel exists and is honest: `citations[]`
  and `evidence_ids` are populated from supporting chunks ONLY, and
  both sit on **5/616** claims (the 5 passed). The enforcement gap is
  that the structured channel and the rendered surface disagree: the
  tail a reader sees as a citation is the model's free text, not the
  structured field. The grounding guard enforces the channel, not the
  page.
- **Measured cost of the gap.** 511 of 512 could-not-judge claims
  carry no structured citation, and their rendered tails cannot be
  trusted as citations; a reader who counts `[Source: …]` occurrences
  over-counts grounding ~100×.

## Pass site 2 — the witness view: a figure present in the flight's own evidence recorded "untraced"

- **The pinned case (drb-56, local arm, claim c5).** Gap-list-2's
  witness record: `{"ran": true, "specifics": [], "all_absent": true,
  "reason": "claim figures absent from the evidence — untraced: 68"}`.
  The figure's only occurrence in the flight's own artifacts — the
  line "68 languages العربية" in `evidence-window-1.json` (round 1's
  Auction chunk) — is real. Three compounding view effects produced
  "untraced":
  1. **Round-window rotation.** Round-2's window
     (`evidence-window-2.json`: BBC_Television_Shakespeare#Behind_the_scenes,
     Lucky_7_(TV_series)#Abstract, The_Love_Boat#Abstract) no longer
     carries the Auction chunk. The claim persisted across rounds
     while its support left the window; rounds 2-3 audits genuinely
     could not see it.
  2. **Heading-shape exclusion.** The "68 languages العربية" line is
     heading-shaped by `is_heading_shaped` (short ≤80 chars, no
     sentence-final punctuation) → excluded from `appears_in_body` →
     `missing_claim_figures` reports the claim's own figure tokens
     untraced even in the round that had them.
  3. **Figure tokens, not extracted specifics.** The witness checks
     the claim's figure tokens, not its extracted specifics — a
     mismatch class the 95b82f97 incident documented on the other
     path.
- **The quantified view-mismatch (whole battery).** 161 claims carry
  a recorded "untraced: …" figure list. Against the union of each
  flight's OWN windows (all rounds): **127/161 (79%)** have ≥1
  untraced figure token present — 228 of 287 figure tokens. Stricter
  variant (multi-char "strong" figures only, so single-digit noise
  tokens like "4" can't inflate): 28 claims with strong figures
  untraced, **6/28 (21%)** have a strong figure present in the
  flight's own evidence — 66 of 107 strong tokens. The lower bound is
  the honest one to cite: at least one claim in five with a
  substantive untraced figure had that figure in its own flight's
  evidence. Separately, the recorded witness decisions are mostly
  self-consistent with their own round's window (177/182 of the
  recorded all_absent calls are consistent with the tokens in the
  round's window) — the failure is the view definition, not the
  ledger's honesty.
- **The guard that should have caught it.** The specifics scan
  (`witness_presence`) exists and ran on every claim; what it scans
  is the round's window through the heading-shape filter. The
  forensics ledger (`SOVEREIGN_GATE_AUDIT_FORENSICS`) exists and would
  have recorded each claim's judged window; it was not enabled.
  Either fix — union-window audit or recorded windows — closes the
  class.

## Pass site 3 — the measurement: the abstention dimension was never scored

- **What passes through.** 139 of the scorer's verdicts are
  "unknown" (134 local, 5 hybrid) — the 35B judge could not tell. The
  DRB measurement reports them as neither supported nor unsupported
  and moves on. The graded vocabulary that classifies exactly this
  dimension exists in the codebase
  (`sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs` `score_answer`:
  hallucination / grounded / caveated_ood / honest_abstention /
  answered_novalue, on the primitives assess_asserted_value /
  classify_extraction / classify_caveat) and was **never composed
  into the DRB measurement**.
- **The guard that should have caught it.** The abstention class
  itself — `honest_abstention` — is the guard; it exists as code and
  is absent from the DRB instrument. An unknown verdict is
  categorically different from a fabricated one, and the current
  report cannot name the honest-abstention share of 134 local
  unknowns.

## Pass site 4 — the decline class: a refusal can be counted as fabrication

- **What passes through.** 7 paired claims (6 local, 1 hybrid of the
  219/117 pairs) are decline-shaped (refusal / limitation wording).
  The vendored FACT validate prompt has **no decline class** — a
  model that honestly declines can be marked unsupported (fabrication)
  by the scorer.
- **The guard that should have caught it.** `DECLINE_RE` — the
  mechanical decline-shape override (→ honest_limitation) exists on
  the calibration side only. One decline shape, two instruments, one
  implementation — §10.6.

## Pass site 5 — forensics not recoverable: the "is this failure real?" question is artifact-blocked

- **What passes through.** `SOVEREIGN_GATE_AUDIT_FORENSICS` (the
  per-claim audit ledger, documented in
  `sovereign/docs/GROUNDING_GATE_ENV.md`) was not set on the t2b
  battery. The frozen artifacts contain the gate's decisions
  (gap-list witness records, verdict flags) but **not the window each
  claim was judged against** — the per-claim evidence view is not in
  the artifacts, so pass-site-2's view mismatch cannot be closed
  per-claim from the archive alone. The 95b82f97 defect class
  (mechanism blind to support outside its view) cannot be audited
  retroactively here.
- **The guard that should have caught it.** The env var — one line in
  the run driver, free, and the documented default posture for any
  gated battery. A battery without forensics is a battery without an
  audit trail; propose it become a driver default, not an opt-in.

## Pass site 6 — scorer traceability: per-fact judge verdicts are not persisted

- **What passes through.** `score-*.json` persist counts, not the
  per-fact rows: the validate step's per-(fact, reference) verdicts
  (the `per_url` dict) are dropped at stat time. Consequences
  measured: a re-measure cannot reuse old per-fact verdicts; the 139
  unknowns cannot be re-classified without re-running the judge; the
  old instrument's verdicts are unrecoverable except by re-judging.
- **The guard that should have caught it.** The per-fact row already
  exists inside the validate step — persisting it is the missing
  half of the same code.

## Timeline-only note (named, not asserted as artifact-confirmed)

The strip-3c anti-leak fix (commit 586c1839, 2026-08-17 01:02)
postdates the t2b flights (2026-08-16 22:00-23:26). The gap-list ICD
carries no `actionable_query` field (keys: charter_hash / claims /
empty_evidence_windows / gaps / icd / round / run_id /
strict_subset_of_prior / version), so the leak class the fix closes
cannot be confirmed or refuted in the t2b artifacts. It is noted as
timeline-only and excluded from every count above.

## Phase-2 recommendation (re-measure posture) — written per the order, with cost

**Posture.** Phase 2 is a RE-MEASURE with the new instrument, not a
fix-pass. The old-instrument P2 verdict (failed: hybrid CI
[0.2564, 0.4554] vs reference 0.1737) stands as measured; the bar text
is frozen. A re-judged number changes the bar's evidence only (see the
transition-note draft in pre-registration.md "T3c").

**1. The re-judge at the reserved window** (the seat declares the
122B window — ~90GB free, July-13 pattern; daemon restart and config
are seat-only), all pre-registered in pre-registration.md "T3c":

- (a) Calibration re-run: `calibrate-judge.mjs` with the 122B judge
  against the frozen calibration-bank.jsonl — does the house finding
  reproduce (35B fails its own gate sens 100% / spec 75%; 122B passes
  100/100)? If not, that finding IS the result.
- (b) FACT re-judge: `FACT_MODEL=Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003`
  `drb-score.py` — same scorer, same seed 4234932947, same frozen
  subset — judge-corrected pooled rates and CIs, side by side with the
  35B rows, both labeled.
- (c) Graded-vocabulary pass: `svrn bench chaos-monkey score-answer`
  per (fact, reference) pair with judge-model and critic-model = the
  122B stem at 127.0.0.1:9741 — the abstention dimension gets its
  honest_abstention share named (the 139 unknowns, never collapsed
  into fabrication); hallucination / grounded / caveated_ood /
  answered_novalue rows reported.
- (d) Per-fact verdicts persisted this time (pass site 6 closed at
  the measurement site).

**Cost of the re-measure.** ~0.5-2h daemon wall on the 122B (~530-670
short judge calls: ~197 FACT validate + ~336 score-answer judge+critic
for the 197 judged pairs + 139 unknowns), ~91GB RSS peak, zero external
tokens; plus ~10-15 min calibration. No new flights — the frozen
artifacts are the input.

**2. Instrument guards to compose (code, named, with the pass site
they close):**

| Guard | Closes | Est. cost |
|---|---|---|
| Render stamp: per-figure verification state on rendered claims (figure-verified / figure-unverified / figure-untraced) in the report | Pass site 1 | ~1 session-chunk |
| Provenance-aware heading exclusion: heading-shaped lines that carry the claim's figure tokens count for presence when they came from a chunk body | Pass site 2 | ~0.5-1 |
| Union-of-rounds window for the audit (or per-claim judged-window records) | Pass site 2 | ~0.5-1 |
| Structured citation channel on downgraded claims: strip model-written tails from downgraded renders, or populate the structured fields | Pass site 1 | ~0.5 |
| Decline class in the validate prompt composition (port DECLINE_RE — one implementation of the decline shape, §10.6) | Pass site 4 | ~0.5 |
| Forensics ledger as a run-driver default (`SOVEREIGN_GATE_AUDIT_FORENSICS` on every battery) | Pass site 5 | ~0.25 |
| Per-fact verdict persistence in `drb-score.py` output | Pass site 6 | ~0.5 |

Guard work total ≈ 4-5 session-chunks; the re-measure battery rides
the reserved window. The guards are cheap because every one of them
turns an existing, measured pass-through into a named state — none
requires a new mechanism.

**3. What a green phase 2 looks like.** Calibration reproduces the
house finding with the 122B; the re-judged P2 CI is reported beside
the old one with both instruments labeled; the abstention dimension is
reported as its own row; and every pass site in this map is closed by
either a named guard or a recorded-forensics audit trail. The P2
verdict is then the seat's to transition — on the evidence, never on
edited text.
