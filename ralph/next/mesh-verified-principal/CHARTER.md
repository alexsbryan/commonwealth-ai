# ralph director charter — mesh-principal

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
  when the order (`.sovereign/features/mesh-verified-principal/order.md`) already implies
  the fix. Cite the order step.
- Which of two options an order names; the smaller reversible step over the
  larger; the existing type or decider over a new one (principles 8, 11).
- Re-running a red gate to understand it, and fixing the code the gate names.

## Leave these for the operator (write the package and stop)

- Any `HUMAN-mp-` row, and anything that changes behaviour a user or peer can
  observe beyond what the row states (the roster default for a ring a person already uses, an offer admitted to someone the holder did not name,
  a wire field a client reads).
- A `REVIEW-mint-mp-` that needs more rows than its cap.
- Weakening a PLANT, adding an `[[exception]]`, or widening an `except` list.
- The order's own "Not worth continuing if" firing (`.sovereign/features/mesh-verified-principal/order.md` §Objective), or any diff under `commonwealth/crates/commonwealth-rail*`.
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

## mesh-principal additions (operator 2026-09-20)

- The door exists: `commonwealth-transport/src/iroh_identity_forward.rs` already
  strips client-supplied `x-mesh-*` and appends the verified identity. A
  resolution that mints a second header scheme, a second principal type beside
  `Principal`, or a per-namespace ACL beside the roster is the wrong resolution.
- A request with NO verified key is `unverified`: served what needs no identity,
  refused by every decider that needs one. `mesh_proof` proves the group, never
  the member, and is never promoted to a caller identity. If a row finds that a
  DEPLOYED mesh's internal traffic arrives without a verified key, so that
  refusing it would break that mesh, that is the operator's: write the package.
- The rail crates and the scheduler's scoring are closed to this campaign.
- The six rr-2 bars, the five `rg-*` bars and the rr-1 baseline are regression
  gates; their campaign files are never edited from this queue.
- What row 1 measures about live-lane cursors, the tensor-split port and MCP's
  bind is RECORDED for `docs/THREAT_MODEL.md` and fixed by no row here.
