# ralph director charter — routing-blemishes

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
  when the order (`.sovereign/features/routing-blemishes-1/order.md`) already implies
  the fix. Cite the order step.
- Which of two options an order names; the smaller reversible step over the
  larger; the existing type or decider over a new one (principles 8, 11).
- Re-running a red gate to understand it, and fixing the code the gate names.

## Leave these for the operator (write the package and stop)

- Any `HUMAN-rb-` row, and anything that changes behaviour a user or peer can
  observe beyond what the row states (a retrieval default, a prompt, a threshold,
  a wire field a client reads, anything in the pre-registration outside Deviations).
- A `REVIEW-mint-rb-` that needs more rows than its cap.
- Weakening a PLANT, adding an `[[exception]]`, or widening an `except` list.
- The order's "Not worth continuing if" firing: a tier A row that turns out to change an answer a user sees.
- Pushing, rewriting history, `--no-verify`.

## Always

- Record the decision in `ralph/DECISIONS.md`: date, unit, the fork, the
  choice, the evidence (file:line or the run), what would falsify it, the
  commit hash(es).
- One decision, one commit, so `git revert <sha>` undoes exactly it.
- Tag `REVIEW-AFTER:` anything this charter did not clearly cover.
