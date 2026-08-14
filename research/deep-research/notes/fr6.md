# FR-6 — decorrelation measurement (order `deep-research-t0b`, red R-7)

**Date:** 2026-08-14. **Status:** measurement complete; posture
recommendation ESCALATED to the operator (keep/drop/redesign is the
operator's call per the order — the numbers below are the decision input,
not a decision).

## Red-first: at HEAD, decorrelation was unmeasured

Both strings were LLM calls living in the same module and same window —
`scan_unsupported_specifics` (judge.rs:491) and `claim_violation_joint`
(judge.rs:1409), privately imported at grounding/mod.rs:107 — and both
were `pub(super)`: no out-of-crate consumer existed, so no instrument
could measure them. The dual-string posture's premise — that two
independent strings decorrelate, with disagreement → could-not-judge —
was asserted, never measured.

Directives 13efc5dc + e39f87b2 opened the seam: the two visibility words
in judge.rs, `pub use judge::{claim_violation_joint,
scan_unsupported_specifics};` in the grounding/mod.rs import block
(line 94), and the runtime.rs re-export. **The strings measured here ARE
the production gate functions** — visibility bump and re-export cited
above; nothing re-implemented, nothing substituted.

## The instrument

- Driver: `sovereign/crates/sovereign-core/tests/fr6_decorrelation.rs`
  (`#[ignore]`d; runs against the live local daemon, `--ignored
  --nocapture`). Provider: `RemoteApiProvider` → the primary stem
  `Qwen3.6-35B-A3B-MTP-UD-Q6_K` (the same model class the hand-run chat
  used), `ShardingPrivacy::LocalOnly`, tau =
  `grounding_gate_threshold()` = 0.9.
- String A: `claim_violation_joint` — one call per claim; unsupported
  iff violation prob ≥ tau. Chunk window = the item's full evidence
  (4 chunks, under the production cap of 12), n_stable = 0.
- String B: `scan_unsupported_specifics` — one call per item over the
  answer (budget 6, production-floored at 3); flagged specifics
  containment-matched to claims. Claims are verbatim answer sentences,
  so the match is deterministic — no semantic matching anywhere.
- Bank: `research/deep-research/bank/labeled/claims.jsonl` — 20 items ×
  4 evidence chunks × 5 claims = 100 claims (60 supported / 40
  unsupported; kinds 14 claim-shaped / 13 specific-shaped / 13 overlap).
- Verdict classes: supported / unsupported / never-ran (a `None` from
  either string is a recorded class, never a default — §18.1/§18.3).
  Never-ran: 0 for both strings.
- Instrument validation (§18.4): raw outputs inspected before any
  number was trusted — string A's vp is a graded continuum
  (0.001–1.000 across the bank, not a degenerate 0/1); string B's
  flagged specifics are specific and semantically correct. The
  instrument also audited the bank itself (below) — the validation
  caught authoring defects before the result was taken.

## Label-correction journal (the instrument audited the bank first)

Three bank defects, all found by the strings on the first run — the
measurement validated the instrument AND the bank:

1. **item-10:2** — "…and the astrolabe…" is absent from every evidence
   chunk → the claim is NOT entailed → my "supported" label was wrong.
   Both strings flagged it (A vp=0.982, B flagged the sentence). Fixed:
   the astrolabe added to ev2.
2. **item-11:1** — "…and it is visible from space." absent from the
   evidence → same defect; both strings flagged it (A vp=0.964, B
   flagged the phrase). Fixed: added to ev0 (exact phrase).
3. **item-02:3** — "The safety bicycle's design was largely settled by
   1890." violates the minted authoring rule ("plausible-but-NOT-
   rescuable-from-world-knowledge"): the design WAS largely settled by
   1890 historically, so the claim is rescuable. Both strings missed it
   (A vp=0.331 — a weak "supported" lean; B silent). Re-crafted to a
   non-rescuable shape ("The safety bicycle's frame was typically made
   of cast iron.") — the pre-fix miss is retained below as the
   shared-bias observation.

First run (pre-correction): agreement 100/100; joint-miss 1 (item-02:3);
false alarms A 2 / B 2 — the SAME two claims from each string. Both runs
are recorded in `fr6-report.json` (the committed file is the corrected
run; the first run's numbers are journaled here). The report's headline
numbers are the corrected run.

## The measurement (corrected bank, 2026-08-14)

| metric | value |
|---|---|
| claims | 100 (60 supported / 40 unsupported) |
| string A ran / string B ran | 100 / 20 (never-ran 0 / 0) |
| **agreement** | **100 / 100 (100%)** |
| **joint-miss** (unsupported missed by both) | **0** |
| single-A-miss / single-B-miss | 0 / 0 |
| false alarms on supported | A 0 / B 0 |
| disagreements (would-be could-not-judge triggers) | **0** |

## Interpretation — correlated, not decorrelated

The two strings are verdict-identical on all 100 claims. The dual-string
posture's operational premise — disagreement between the strings triggers
could-not-judge — **never fires on this bank**: the strings agree
everywhere, including in their error structure (pre-correction, both
flagged the same two claims and both missed the same one). On bank v0 at
this model, string B is a redundant witness: it catches exactly what
string A catches, and neither catches anything the other misses.

Scoping caveat: bank v0's windows are small (4 chunks, ~500 chars) and
the unsupported claims are clearly absent — an easy regime for the 35B.
Decorrelation may emerge on harder banks (large windows, subtle
near-entailments). This conclusion is bank-scoped, as the instrument is.

The residual failure shape is NOT disagreement — it is **shared bias**:
both strings lean on world knowledge when evidence is ambiguous. The
pre-correction joint-miss is the specimen: a not-entailed but
historically-true claim read as "supported" (vp=0.331) by A and unflagged
by B. A second string of the same family cannot catch this shape, by
construction — it shares the same knowledge and the same prompt family.

## Posture recommendation — REDESIGN (escalated with numbers; operator decides)

Per the order's correlated-errors branch, the operator decides
keep/drop/redesign on these numbers. Recommendation: **redesign**.

- The disagreement → could-not-judge path is dead weight on this bank: a
  structurally-identical second LLM string (same module, same evidence,
  same model family) paid 20 extra LLM calls (one per item) and changed
  zero verdicts.
- The failure shape that matters — world-knowledge rescue on ambiguous
  evidence — needs a DIFFERENT instrument, not another string: a
  structural evidence-containment check (the string's charter is
  containment; make it structural — ARCH §7: never ask a model to
  guarantee what code can enforce), or a harder adversarial sub-bank that
  separates the strings before the could-not-judge path is trusted.
- If the operator keeps dual-string: its only earned role on this bank is
  belt-and-braces redundancy at a per-answer latency tax, with the
  could-not-judge path firing ~never. The honest home for the second
  string is gated on a harder bank that demonstrates disagreement.

## Provenance

Single commit naming directives 13efc5dc + e39f87b2: judge.rs visibility
words; grounding/mod.rs import-block `pub use`; runtime.rs re-export;
Cargo.toml dev-dep; `tests/fr6_decorrelation.rs`; the labeled-set
corrections journaled above; `fr6-report.json` (raw per-claim detail);
this report. All 240 LLM calls ran on the local daemon — zero external
model tokens.
