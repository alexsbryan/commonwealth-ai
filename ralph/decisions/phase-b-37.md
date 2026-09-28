<!-- ledger -->

**phase-b-37 · 2026-09-28 · pb-rails-membership · director** — this commit
- Needed: the row asked cw-rails to serve the daemon's PLAINTEXT-invite join (`perform_join`, sovereign-mesh join.rs:334) and proved it with "a plaintext invite admits a joiner". Every other door was built and green (f24b3b645..d3c6017eb); only `perform_join`'s two address paths (`relay=` host:port POSTed in plaintext, mDNS peers' plaintext port) were missing.
- Chose: drop the two address paths from the row. A plaintext mesh's `dial=` invite joining by key satisfies the proof; the address paths retire with the daemon's plaintext fallback at the flip, which pb-mesh-exit-transport now names. Row marked done at d3c6017eb.
- Because: phase-b-36 (operator, written after this row at phase-b-33) retires plaintext posture at the flip and refuses to migrate a plaintext mesh, so porting a plaintext address join into cw-rails would build a door the flip then closes; cw-rails' own tested scope refuses it (principle 10); and the lift cannot prove it, since cw-rails founds encrypted only (found.rs:7-9), so the port would be a gate nobody watched fail (principle 5).

<!-- appendix -->

## phase-b-37 · 2026-09-28 — pb-rails-membership: cw-rails joins by key only; plaintext address join retires at the flip

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md (removed in this commit), options (A) drop the address paths, (B) port them into cw-rails.

Evidence, reproduced by the director:
- commonwealth-rails join.rs:20-28 scopes out "a `relay=` host:port POSTed directly, and a daemon's plaintext mDNS port"; `an_invite_with_no_iroh_dial_is_refused_by_name` (join.rs:243) asserts "iroh only"; `a_plaintext_invite_carrying_a_dial_string_is_accepted` (join.rs:279) covers the key path.
- found.rs:7-9: cw-rails founds `require_encryption` meshes only, so no plaintext invite exists on the cmnwlth-only lift.
- ralph/decisions/phase-b-36.md: "The daemon's plaintext fallback retires with its endpoint", "Migrating a plaintext mesh is refused by name".
- `ralph-check.sh test commonwealth-rails`: pass 106 fail 0 (director re-run). Worker's LIFT(cmnwlth) PASSED and PLANT 105/1 (`an_expired_invite_is_401`) taken from the package, not re-run.

Chose (A). (B) reverses a tested scope, needs a plaintext-founding path in cw-rails only to prove it, and contradicts an operator decision. No end-user behaviour changes in this row: the daemon keeps handling plaintext joins until the flip, whose plaintext retirement the operator already accepted in phase-b-36. FIVE_PROGRAMS is unchanged: it names no plaintext join, and phase-b-36 carries the posture.

Falsified if: a live mesh the operator runs (or a shipped client) joins by `relay=`/mDNS address with no `dial=` string and must keep working across the flip — then the address paths need a home and the fork goes to the operator.

Found, not this row's (worker, recorded for phase-c): mDNS keeps advertising the mesh `run --mdns` started on after a live switch; `accept_join_with_identity` (commonwealth-discovery membership.rs:253-266) does not clear `removed_at` on a same-id rejoin.

</details>
