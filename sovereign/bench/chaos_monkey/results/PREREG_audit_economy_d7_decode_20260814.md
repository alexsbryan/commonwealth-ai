# PRE-REGISTRATION — D7: the generative registers become enumerate + forced-choice sweeps (order audit-economy, amended)

2026-08-14, registered BEFORE any candidate is built, per directive
6fdc5796 ("price the ceiling before building") and the amendment. Bars
below change only via the seat. Every cost claim carries (tokens_used /
prompt_chars, response_chars) beside its latency — the gap-analysis
procedural rule.

## The ceiling, priced from the D2-smoke census (scored turns, n=5)

| register | measured | fit | decode share |
|---|---|---|---|
| extract_claim_list | ms 3209-5705, prompt 4247-6252c, out 617-1160c | 327ms + 4.65 ms/out_char | ~75-90% |
| specifics_scan (A') | ms 4996-8332, prompt 36677-38682c, out 522-1235c | 2656ms floor + 4.32 ms/out_char | ~45-70% |
| claims_support_batched (the proof of the sweep shape) | ms 1125-1328, prompt 29271-29667c, out 29-44c | — | ~0 |

Ceiling if both emit ~3 chars per judged unit instead of prose:
claim_list 4.2s -> ~0.3s; scan 5.6s -> ~1.5s; combined -7.7s. The floor is
materially below cost, so building is justified — gated on everything
below. The initiative's own thesis is on record (NATIVE_GROUNDING.md:
"decode-rooted calibration to replace the judge stack"; H0-judge-free) —
D7 is that thesis applied to the two remaining decode-heavy registers.

## Inventory consulted before designing (principle 11)

- Sentence segmentation with byte ranges EXISTS: `segments_for_display`'s
  splitter (native_grounding/segments.rs). REUSED FOR ENUMERATION ONLY.
  The RESOLUTION half is measured at precision 0.7429 vs a 0.98 bar
  (resolver-precision FINDINGS 2026-08-09) and is structurally
  display-only — **no D7 candidate may source a verdict from
  span resolution**; verdicts come only from the calibrated forced-choice
  registers. This constraint is registered here so no candidate quietly
  "optimizes" the sweep away.
- Entity/specific extraction EXISTS: sovereign-gliner (NER), the
  deterministic in-world name veto (runs before any LLM verdict today),
  and the scan prompt's own specifics taxonomy (names, numbers, dates,
  quotations, section/version refs, code identifiers).
- The sweep register EXISTS: claims_support_batched (family prefix +
  CHUNK_JUDGE_SYSTEM + numbered lines, asymmetric trust, D1-recalibrated
  at catch 0.950 / clear 1.000, D2-wired).
- `extract_claim_list` has 9 call sites; ONLY `gate_longform:2836` is in
  scope. Bench harnesses (faithfulness, verifier) and summary_verify keep
  the generative register — it is not deleted.

## D7b — the scan, FIRST (self-contained: its output feeds only the annotate/repair path)

Candidate: deterministic candidate enumeration from the ANSWER (gliner
NER + numbers/dates + quoted strings + section/version/code refs +
attribution pairs "X said/argued Y"), then ONE batched forced-choice
sweep over the numbered candidates against the evidence (family leaf
prefix + CHUNK_JUDGE_SYSTEM; A=supported, B=unsupported; ~3 chars/line
decode). B-verdict candidates ride the existing scan-item path
(anchor_scan_item -> corrective search -> annotate). Replay-first; a
candidate is a build.

Bars:
- (a) Render fingerprints: all non-scan registers byte-identical to main
  (28 per_claim / 3 chunk_judge / 23 batched).
- (b) Labeled scan bank (9 cases / 10 items, thinness stated as always):
  should_not_flag <=2/6 flagged (A' parity — no FP regression);
  should_flag: the two catches A' holds (Chisholm-Pereboom, Keynes) BOTH
  held. The Kane-bridge stitch is read explicitly and reported whichever
  way it falls — it is already a priced, operator-accepted (c)-class;
  recovering it is upside, losing it again is status quo, and silence
  about it is a report defect.
- (c) Enumeration-recall read (the named structural risk): for every
  frozen-3 probe catch and every labeled should_flag item, state whether
  the enumeration LISTS a candidate covering it. A catch whose specific
  is structurally un-enumerable (relational classes: misattribution,
  false-claims-ABOUT-evidence, stitches) is a HEADLINE and a kill unless
  a deterministic enumerator for that class ships in the same candidate.
- (d) Frozen-3 live arm 3/3 in hardened form (trend row + dropped-catch
  read) at any live flip; CONFAB-LEAK NEW<=OLD on paired chaos.
- (e) Cost: scan term <=2.5s median on the live smoke shape, priced with
  (prompt_chars, out_chars) per call; item-level bit-stability on
  --repeat 2.

Kill: any (c)-bar structural miss without a same-candidate enumerator;
sweep false-"supported" on labeled scan negatives above the batched
register's measured 0.050; live floor >=4.0s (no material win vs A').

## D7a — claim units, SECOND (touches the audit's unit of account)

Candidate: `gate_longform` stops calling extract_claim_list; audit units
come from the segments SENTENCE SPLITTER (byte ranges into the released
draft) + a deterministic unit filter (self-referential-decline exemption
as today; minimum length; non-assertive filter derived from data, not
vibes). Units join the existing batched sweep; unsupported/gap units fall
through to the calibrated per-claim judge with rescue search (asymmetric
trust exactly as D2 wired it).

Bars:
- (a) Answer-level catch preservation on the pinned replay population:
  every claim main flags maps to >=1 flagged unit in the candidate,
  verified by hand claim-by-claim. One unmapped catch = kill.
- (b) Cost guards, both sides: claim_list term <=1.0s AND the
  fall-through rate keeps per-claim calls/turn <= today's median (~2.6
  post-D2) — sentence units include filler, and filler that reads
  "unsupported" would stampede the calibrated path at 1.8s/call. Priced
  on the replay population BEFORE any live arm.
- (c) Frozen-3 3/3 hardened; CONFAB-LEAK NEW<=OLD paired chaos.
- (d) Product surface: units are what the epistemic ledger displays as
  holdings. The change in holding granularity is shown to the operator
  (E-operator-holdout is terminal) before any default flip.

Kill: (a) fails on any pinned specimen; combined term (units + sweep +
fall-through) >= today's 4.2s + the per-claim delta it induces; operator
judges the holdings display worse.

## Sequencing and instrument

D7b then D7a; each lands replay-first through `svrn bench judge-replay`
(candidate = build; new sweep registers join the harness the way
batched_support did). ONE composed after-arm at the very end of the order
(after D5+D6+D7), under the D5 pre-flight (corpus optimize after any
daemon restart), against the 688f8eba baseline — 27.8s -> <=16.8s with
every quality gate held. Not one arm per deliverable.
