# ralph director charter — threat-gaps

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
  when the order (`.sovereign/features/threat-gaps-close/order.md`) already implies
  the fix. Cite the order step.
- Which of two options an order names; the smaller reversible step over the
  larger; the existing type or decider over a new one (principles 8, 11).
- Re-running a red gate to understand it, and fixing the code the gate names.

## Leave these for the operator (write the package and stop)

- Any `HUMAN-tg-` row, and anything that changes behaviour a user or peer can
  observe beyond what the row states (a member refused on `:9742`, a worker that stops binding, a shared token that stops admitting,
  a first-party mesh-app that loses a command it calls).
- A `REVIEW-mint-tg-` that needs more rows than its cap.
- Weakening a PLANT, adding an `[[exception]]`, or widening an `except` list.
- The order's own "Not worth continuing if" firing (`.sovereign/features/threat-gaps-close/order.md` §Objective — one condition per gap: a member with no `mesh_secret`, the bridge landing on relay or a material tok/s drop, at most one distinct `RemoteClient` fingerprint), or any diff under `commonwealth/crates/commonwealth-rail*` or `sovereign/crates/sovereign-scheduler/`.
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

## threat-gaps additions (operator 2026-09-20, ledger A56, A58, A63)

- These are the OPERATOR's, accepted at A58 as order §Assumptions A1-A5, and
  not yours to reopen or to soften: the posture DEFAULTS (`[daemon]
  internal_auth = "member"`, the RPC worker bound `127.0.0.1:50052`, the shared
  client token still admitting); the KNOB NAMES (`internal_auth = "perimeter"`,
  `SOVEREIGN_RPC_ALLOW_PLAINTEXT_LAN` and its `[shared_model]` key,
  `client_tokens = "named-only"`; the mesh-app allowlist has no knob); the
  EXEMPT ROUTE SET on the internal port, which is exactly `/internal/join` and
  `/internal/gossip`; every bar CLAUSE, floor and goodhart line in
  `quality/campaigns/threat-gaps.toml`; every BASELINE; and any sentence in
  `docs/THREAT_MODEL.md` that says a gap is closed. A resolution that adds a
  third exempt route, ships a gate defaulted off, renames a knob, or strikes an
  entry the evidence only narrows is the wrong resolution: write the package.
- A row that finds a mesh member with no `mesh_secret`, or a first-party
  mesh-app calling a command outside the bridge list, STOPS. It does not
  default the gate off, exempt that member, or widen the allowlist. The order
  names both as "Not worth continuing if"; they are decisions, not defects.
- One resolver per port. `sovereign-daemon/src/internal_principal.rs` is the
  internal surface's (landed `8207660d3`) and the mesh-proof arm goes INSIDE
  it; the gate reads the `AttachedPrincipal` it attached. A second resolver, a
  second listener, a second principal type beside `Principal`, a new proof
  beside `Mesh::mesh_proof`, or a second token store shape is the wrong
  resolution (order §Predictions, §Less).
- `mesh_proof` proves the GROUP, not the member: any holder of the mesh secret
  can mint one naming any sender (`commonwealth-core/src/mesh/mod.rs:782-825`).
  So a proof ADMITS and never IDENTIFIES (ledger A64). The order's step 8
  once had a valid proof plus a typed node id resolve `Principal::Member`;
  that was the seat's drafting error, not the operator's word, and it is
  corrected in the row: a valid proof attaches the `ProvedMeshMember` marker
  and the principal stays `Anonymous`. A resolution that promotes the proof
  to an identity, reads `x-node-id` in `internal_principal.rs`, or widens
  `mesh_principal_gate`'s lists to make that possible is the wrong
  resolution. What stays open on a plaintext mesh — one member cannot be told
  from another, so a member off a ring's roster can still read it — is
  RECORDED for `docs/THREAT_MODEL.md` and is the operator's to close, because
  closing it stops file-rostered rings replicating on a plaintext mesh.
- Ledger A61 is OPEN and unapproved: the CLIENT plane (`client_principal::resolve`
  step 2; `forward_for`'s `CLIENT_ALPN` arm in `sovereign-mesh/src/iroh_access.rs`)
  still mints `Principal::Member` from a typed `x-node-id`. It is NOT in this
  queue's scope. `tg-3-tokens-have-names` edits the same resolver; a resolution
  that fixes A61 in passing, or that a worker's diff would need A61 fixed to
  pass, is the operator's.
- `HUMAN-tg-rpc-two-machines` holds clauses (c) and (d) of
  `tg-rpc-port-not-on-lan`. They read COULD-NOT-JUDGE on one host and that is
  the honest reading; never resolve a package by reading them PASSED from a
  one-host run, and never by blocking a machine row on them.
- Daemon restarts route through the seat. The deployed daemon is never stopped
  or restarted from this queue; a row that needs it writes the package.
- Never `git push`. Never `--update-baseline` on any ratchet. A file over its
  size ceiling is SPLIT by the unit that pushed it over and is NEVER taken to the
  operator (operator direction 2026-09-21, ledger A67: "The answer is never to
  repin"). A package that offers the operator "accept, trim or re-pin" for a
  file size is wrongly written; resolve it by splitting, or by adding a split
  row, and say so in the ledger. There is no such thing as an accepted raise.
- The rr-1 baseline, the six rr-2 bars, the five `rg-*` bars and the four
  `mp-*` bars are regression gates; their campaign files are never edited from
  this queue.
- Ledger entries go AFTER the previous one in `ralph/DECISIONS.md` (the last is
  A67, which sits above the older A44 block — insert below A67, not at the end
  of the ledger list), with the appendix of the same number at EOF. Next id:
  **A68**.
