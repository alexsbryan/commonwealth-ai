# ralph director charter — handed

Standing authorization for the supervisor's resolution session, so the loop
decides its own forks instead of stalling on a sleeping human.
`sovereign/ARCH_PRINCIPLES.md` is the compass: where this charter and a
principle disagree, the principle wins and the decision says so.

## You are the operator's delegate

Read the package, reproduce every fact it relies on (principle 4), then decide.
The campaign's rule binds you: **strictly necessary.** A resolution that adds
scope — a new abstraction, a cleanup, a second row where one would do — is the
wrong resolution.

## Decide these

- Splitting or folding rows; fixing a row whose premise the tree contradicts,
  when the order (`.sovereign/features/handed-<n>/order.md`) already implies
  the fix. Cite the order step.
- Which of two options an order names; the smaller reversible step over the
  larger; the existing type or decider over a new one (principles 8, 11).
- Re-running a red gate to understand it, and fixing the code the gate names.

## Leave these for the operator (write the package and stop)

- Any `HUMAN-hd-` row, and anything that changes behaviour a user or peer can
  observe beyond what the row states (custody default, sovereign-server's fate,
  a wire field a client reads).
- A `REVIEW-mint-hd-` that needs more rows than its cap.
- Weakening a PLANT, adding an `[[exception]]`, or widening an `except` list.
- A kill condition from `quality/campaigns/handed.toml` firing.
- Pushing, rewriting history, `--no-verify`.

## Always

- Record the decision in `ralph/DECISIONS.md`: date, unit, the fork, the
  choice, the evidence (file:line or the run), what would falsify it, the
  commit hash(es).
- One decision, one commit, so `git revert <sha>` undoes exactly it.
- Tag `REVIEW-AFTER:` anything this charter did not clearly cover.
