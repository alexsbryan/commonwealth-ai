# ralph director charter — ring-guest

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
  when the order (`.sovereign/features/ring-guest-substrate/order.md`) already implies
  the fix. Cite the order step.
- Which of two options an order names; the smaller reversible step over the
  larger; the existing type or decider over a new one (principles 8, 11).
- Re-running a red gate to understand it, and fixing the code the gate names.

## Leave these for the operator (write the package and stop)

- Any `HUMAN-rg-` row, and anything that changes behaviour a user or peer can
  observe beyond what the row states (the roster default for a ring a person already uses, an offer admitted to someone the holder did not name,
  a wire field a client reads).
- A `REVIEW-mint-rg-` that needs more rows than its cap.
- Weakening a PLANT, adding an `[[exception]]`, or widening an `except` list.
- The order's own "Not worth continuing if" firing (`.sovereign/features/ring-guest-substrate/order.md` §Objective), or any diff under `commonwealth/crates/commonwealth-rail*` outside the one row the additions below name.
- Pushing, rewriting history, `--no-verify`.

## Always

- Record the decision in `ralph/DECISIONS.md` in its two-part shape: ONE ledger
  entry (a bold line: id · date · unit · by · commit; then three bullets —
  Needed: what forced the decision; Chose: the choice; Because: the reason) and ONE
  appendix of the same number (`## A<n> · …`, body folded in `<details>`): the
  fork, the evidence (file:line or the run), what would falsify it, the
  worker's package inline. The ledger must read on its own.
- One decision, one commit, so `git revert <sha>` undoes exactly it.
- Tag `REVIEW-AFTER:` anything this charter did not clearly cover.

## ring-guest additions (operator 2026-09-20)

- DECIDED by the operator, not yours to reopen: D1 a guest's identity rides a
  signed `on_behalf_of` beside the payload in rail-core (ledger A52); D2 the
  name binds to a door-issued guest SESSION under the grant, asked once by the
  door's shim. A resolution that moves either back into the app, or into a
  reserved payload key, is the wrong resolution.
- The rail is open to ONE row, `rg-1-on-behalf-of`, for that one field. A
  second rail diff, a guest ROLE on any roster, or a new act kind is the
  operator's.
- The scaffold (`sovereign-cli-llm/src/ring_cmd/templates/`) is never edited.
  A row that seems to need it has found the campaign's answer: stop and say so.
- The guest posture in `docs/THREAT_MODEL.md` is not widened: bearer, TTL-clamped,
  `Scope` paths only, never in `Mesh.members`. The session is a NAME under a
  grant, never a second credential with its own scope.
- The six rr-2 bars and the rr-1 baseline are regression gates. A clause of
  `quality/campaigns/ring-room.toml` is never edited from this queue.
- What a guest's act MEANS to an app's arithmetic is the app's. Report what the
  unmodified reducer does; do not decide it.
