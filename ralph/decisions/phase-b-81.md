<!-- ledger -->

**phase-b-81 · 2026-09-30 · pb-mesh-exit-transport (the flip) · director** — this commit
- Needed: the seat's landing condition (a5ff24788) found five gaps in the flip's retired-test ledger (target/ralph/phase-b/flip-ledger.md, 234 fns in 48 files) that were forks rather than work: the setup wizard's join brings up no cw-rails; cw-rails' rotate has no pre-split guard (FE-15/17); `app_allow`/`offer_allow` have no reader against cw-rails' no-config-allow design; whether the DST pack and gossip selection properties are owed before landing; cw-rails' ring sync carries no successors for 27 daemon ring tests.
- Chose:
  - Wizard (1 (a)): the wizard runs the one `svrn mesh up` bring-up by exec'ing the cli-mesh sibling before it spawns `Launch::AdminJoin`; `provisional_config` carries `rails_base` so its test dials a test cw-rails.
  - Rotate guard (2 (a)): ported into cw-rails' `membership::rotate`, with a split-generation map fed from both gossip directions, the three-way refusal, one confirmation round, `force`, and the nine successor tests.
  - Allow lists (3 (a)): `app_allow` and `offer_allow` ride the publisher's origin registration as `Admit::Members(allow)`, as `media_allow` does. svrn keeps reading its `[iroh]` keys, so the user's config does not move.
  - DST pack (4, neither option as written): ported onto cw-rails' gossip in this row, before landing. Selection properties specific to the deleted algorithm are ledgered D by name.
  - Ring successors (5 (a)): ported in this row. LIFT tests re-priced +450 → ~+2,400, code +~250.
- Because:
  - The loop works only the `[~]` unit (ralph/PROMPT.base.md §1), so a follow-on row could not run before the landing. The seat's condition says a surviving behaviour with no successor blocks the landing, so 4 (a) ("land, successor owed by a later row") contradicts it. The DST scenarios (convergence, decay without ghosts, skew, partition heal, quiescence, wire faults, seeded soak) are mesh behaviours, not artefacts of the old implementation.
  - Extend, never re-own (FIVE_PROGRAMS §2c): cw-rails already owns membership, gossip, ring sync and origin admission, so each successor and the guard land in that owner. 3 (a) reuses the existing `Admit::Members` registration path instead of adding a `rails.toml` key, which acceptor.rs:27-34 calls a boot hazard. 1 (a) reuses the one bring-up instead of copying it.
  - The charter's end-user clause: 1 (c), 2 (b) and 3 (c) each remove a working behaviour (wizard join, rotate safety on a mixed fleet, app allow lists). That is operator-only, and nothing in the package forces it.
- REVIEW-AFTER: the flip's landing. 2 is falsified if no build without the split generation remains anywhere in the fleet, in which case 2 (b) (retiring the `invite_key_hash` arm) is cheaper and is the operator's call. 3 is falsified if svrn is not the process that registers `cwth/app/0` or `cwth/offer/0` origins after the flip. 4 is falsified if the DST harness cannot drive cw-rails' gossip without a transport seam that cw-rails lacks. That is a NEEDS_HUMAN with the line count, not a shrunk pack.

<!-- appendix -->

## phase-b-81 · 2026-09-30 — the flip's five ledger gaps are all closed in the row, before landing

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md (session 7, worktree /home/alexbryan/dev/pb-flip at 1f0b27a14).

Reproduced in the worktree:
- commonwealth-rails membership.rs:313 `rotate`: a solo check, a new key, `rotate_invite_key`, save. No split-generation read and no `force`.
- gossip.rs:473-476: `merge_from_authenticated` returns `report`, and only `report.rejected()` is read after it.
- acceptor.rs:27-34: "A rails node publishes through the loopback API, never through `rails.toml`" (`Config` is `deny_unknown_fields`); `cwth/app/0` is `admit_app` against the live registry.
- origins.rs:58-68: `registry.stand(MEDIA_ALPN, …, Admit::Members(media_allow), …)`, the precedent for 3 (a).
- `#[test]` count in ring_sync.rs, ring_sync/journal.rs and ring_routes.rs is 0. tests/ring_round.rs drives `run_one_round` once, so the loop has an end-to-end test, but none of the scoped behaviours the ledger lists.
- sovereign-cli-daemon daemon_bin.rs `BIN_NAME = "sovereign-stock"`; join_child.rs `provisional_config` sets ports and data dir only (no `rails_base`).
- sovereign-mesh tests/main/dst*.rs: 20 tests, among them dst_gossip_converges_the_member_list_and_not_the_store, downed_peer_decays_then_no_ghost, clock_skew_does_not_false_decay, partition_then_heal_reconverges, agreed_quiesce_rejects_stable_disagreement, wire_faults_and_clock_jump_back_reconverge, seeded_chaos_soak.
- Seat condition (STATE.md, a5ff24788): "A surviving behaviour with no successor is a gap, and the flip does not land with it."
- BOUNDARY on `cut` at f589c9926: `cargo xtask boundary-gate` EXIT=1, 7 violations.

Rejected: 1 (b) moves the join out of the child but leaves the bring-up unrun. 1 (c), 2 (b) and 3 (c) are end-user regressions (operator-only). 3 (b) is the boot hazard cw-rails' design names. 4 (a) contradicts the seat's landing condition. 5 (b) is the same deferral.

</details>
