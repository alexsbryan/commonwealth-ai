<!-- ledger -->

**five-programs-20 · 2026-09-24 · REVIEW-mint-fp-mesh-dial (residue still owned by open dials) · director** — this commit
- Needed: the mesh mint halted before minting anything. It found 12 rows of residue against a cap of 8, and 3 of them sit behind parked decisions (fp-42/D4, fp-9's membership bootstrap, fp-7).
- Chose: move the row behind fp-9, fp-10, fp-42 and fp-47, the same fix five-programs-17 made to the core mint. Name the owner of each overlapping class in the row text: iroh/join/persist go to fp-9, media Route types to fp-47, the D4 kv/outbox to fp-42, the test-tree inference fixtures to fp-10. When the re-mint runs, a SERVE+FLIP pair for a missing cw-rails surface counts as one row, and a surface that commonwealth-rails' docs disclaim goes to the appendix as NEEDS-OPERATOR. Cap stays 8. No code changed. Boundary gate FAILED at 62 violations (`RALPH_QUEUE=five-programs scripts/ralph-check.sh boundary`, at 01d6dbfe5).
- Because: the charter's false-premise rule, with -17 as precedent. The minted deps predate fp-9, fp-10, fp-42 and fp-47, so measuring the residue now mints their work twice, and the cap tripping is the symptom of that. The smaller reversible step is to re-sequence. Raising the cap would add scope (§11 "strictly necessary").

<!-- appendix -->

## five-programs-20 · 2026-09-24 — mesh mint re-sequenced behind the dial rows that own its residue

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpmesh-20260924.md. I reproduced it at 01d6dbfe5:

- `grep -rho "sovereign_mesh::"` over sovereign-daemon finds src 160 and tests 127. The per-module src split matches the package (iroh_access 27, ring_roster 15, peer_adapter 12, …, canonical_pull 1).
- commonwealth-rails/src/api.rs mounts only /v1/mesh/{status,media,app,offers,media/presence,fanout,publish,forget-member,roster-names}, /v1/rail/* and /v1/work/projection. It has no ring-sync, gossip, guest or measurements route.
- commonwealth-rails/src/lib.rs:26 says "It does not admit joiners." lib.rs:30 says "It does not join over LAN/mDNS." rail.rs:12-15 says the guest half is "deliberately NOT mirrored".
- fp-9's row already names "iroh" and carries the membership-bootstrap scout finding. fp-42 is parked (five-programs-8). fp-47 is open behind fp-46 [x].
- Boundary gate: 62.

Answers to the package's four questions. (1) Re-sequence, not a raised cap and not a partial mint. The REPOINT class (deep_link, MemberIdentity, InferenceVenue) closes nothing alone, and -17 keeps REPOINT inside the re-sequenced mint, so a lone REPOINT row now would be a second row where one will do. (2) A SERVE+FLIP pair counts as one mint row, because fw-1's wave already superseded the split halves. A surface that rails' docs disclaim is new capability and goes to the operator (principle 11), so the director does not mint it. (3) The test fixtures are fp-10's residue, measured after it. (4) MeshIrohAccess and the watchdog belong to fp-9. fp-47 owns only the media Route types.

The mint now depends on parked fp-42, as the core mint already does, so both wait on the operator's D4 answer. That is the honest state, and the loop continues on fp-9 and fp-10.

Falsified if the rows above land and the residue still needs more than 8 one-per-surface rows with no operator-parked class among them. Then the cap, not the sequencing, was the problem. Also falsified if fp-9 resolves by leaving iroh/membership in the daemon for good, in which case those sites are a keep and not residue.

</details>
