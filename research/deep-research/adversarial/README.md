# Adversarial sub-bank + frozen longform-negative set — T1 pre-registration

Order `deep-research-t1a`, §18.6 discipline (gate-redesign.md §5):
**before the changed gate ships**, two frozen instruments are minted and
the frozen set is run against the changed gate. Both instruments are
authored NWCI — from operator/worker knowledge alone, before any gate
invocation of this slice; the windows are synthetic construction
material for the adversarial read (deliberately NOT retrieval results —
the read measures the gate's verdict behavior on controlled inputs).

| instrument | path | shape |
|---|---|---|
| adversarial sub-bank | `sub-bank.jsonl` | 13 negative (world-knowledge-lean) + 8 positive-control claim/window pairs |
| frozen longform-negative set | `longform-negative.jsonl` | 6 long-form answers with citation spans, specifics absent from their windows |
| pre-registration record | `pre-registration.md` | the declaration; the baseline run (judge alone) and the changed-gate run (judge+witness) recorded before/at ship; the witness-fix declaration and its re-read appended at execution |

The three witness-fix fixtures (ap-07 phantom-anchored, ap-08
case-shifted proper noun, an-13 negative claim contradicted by its window)
were added to the frozen set BEFORE the witness fix shipped (directive
6c25d88e discipline: the demo run's three defect classes become frozen
fixtures first). They are defect-anchored — authored from the demo flight
record, which predates the fix.

**NWCI record:** all claims and windows here were authored from
knowledge alone, before any gate, retrieval, or answer text of this
slice was consulted. No key here could only have been written by
consulting system output.

## Acceptance shape (declared here, before any run)

- Judge alone (baseline): most frozen negatives + longform negatives
  pass (marked `supported`) — the bias residual the measurement
  predicts.
- Changed gate (judge + containment witness): frozen negatives and
  longform negatives downgrade to `could-not-judge` (all extracted
  specifics absent); positive controls STAY `supported` (≥1 specific
  present). The witness never upgrades a verdict.
- The adversarial read ships beside the gate change in the same commit
  wave — a judge change with no adversarial read beside it is not a
  landed change.
