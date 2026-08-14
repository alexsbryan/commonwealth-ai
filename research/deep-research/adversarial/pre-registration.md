# Pre-registration record — the changed gate's adversarial read

§18.6 discipline (gate-redesign.md §5): the frozen instruments were
minted **before** the changed gate ships; the frozen set is run against
the changed gate, and the read ships beside the change. This file is the
temporal record: declarations first, run results appended at execution,
nothing backdated.

## Declaration (minted 2026-08-14, before any gate invocation of this slice)

- Instruments frozen: `sub-bank.jsonl` (12 negative + 6 positive) and
  `longform-negative.jsonl` (6). Authored NWCI — no gate, retrieval, or
  answer text consulted.
- The changed gate = single-string judge (`claim_violation_joint`) +
  C-class containment witness (trigger: judge-supported claims with ≥1
  extracted specific; all specifics absent → downgrade to
  could-not-judge). Witness code: `deep_research/containment.rs`.
- Acceptance shape (declared above, in README.md):
  - baseline (judge alone): most negatives/longforms pass (supported);
  - changed (judge+witness): negatives/longforms downgrade to
    could-not-judge; positives stay supported;
  - the witness never upgrades a verdict.
- The gate change ships (loop audit wired) only with this read recorded.

## Baseline run — judge alone (frozen set)

*(appended at execution, timestamped)*

## Changed-gate run — judge + containment witness (frozen set)

*(appended at execution, timestamped — ships beside the gate change)*

## Read

*(verdict rows vs acceptance shape)*
