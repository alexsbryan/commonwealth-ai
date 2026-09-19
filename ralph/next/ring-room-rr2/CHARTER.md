# ralph director charter — ring-room

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
  when the order (`.sovereign/features/ring-room-week2/order.md`) already implies
  the fix. Cite the order step.
- Which of two options an order names; the smaller reversible step over the
  larger; the existing type or decider over a new one (principles 8, 11).
- Re-running a red gate to understand it, and fixing the code the gate names.

## Leave these for the operator (write the package and stop)

- Any `HUMAN-rr-` row, and anything that changes behaviour a user or peer can
  observe beyond what the row states (the roster default for a ring a person already uses, an offer admitted to someone the holder did not name,
  a wire field a client reads).
- A `REVIEW-mint-rr-` that needs more rows than its cap.
- Weakening a PLANT, adding an `[[exception]]`, or widening an `except` list.
- A stop condition from `.sovereign/features/ring-room/campaign.md` §Stop conditions firing, or any diff under `commonwealth/crates/commonwealth-rail*`.
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

## rr-2 additions (operator 2026-09-19)

- A guest keeps EXACTLY what `docs/THREAT_MODEL.md` grants guests today: bearer,
  TTL-clamped, `Scope` paths only, never in `Mesh.members`, never the invite key.
  A resolution that widens that, or reopens the plaintext client API on an
  encrypted mesh (sovereign-daemon/src/daemon.rs:100-125 rule 1), is the
  operator's, never yours.
- Membership is a member's `introduce`; a scan never mints a member.
- The holder decides when a library is offered (`offer` / `withdraw`) and a
  library in use is shown as in use and not contended; viewers are read-only.
  Rows that need a Jellyfin capability beyond a read-only user and `/Sessions`
  stop.
- The CPU-node ask (docs/RING_ROOM_DEMO.md part two) is the scheduler's row in
  another session; a row here that seems to need it stops and names it.
