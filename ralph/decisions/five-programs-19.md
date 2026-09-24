<!-- ledger -->

**five-programs-19 · 2026-09-24 · fp-8 (premise false three ways) · director** — this commit
- Needed: fp-8 halted before editing anything. Its pump half was already done, its TSV pointers named pairs it cannot close, and cw-rails has no guest door to dial.
- Chose: strike the pump half, which fw-1/fp-54 satisfied at 24070aeb8; its store half is fp-42's. Strike the TSV pointers. Rescope what is left to D6's serve half, the package's option (a): one loopback daemon door that opens or reuses the stored link's tunnel through `StoredGuestLink` and returns its base URL. fp-26 stays its client. No code changed. Boundary gate unchanged at 63 (`scripts/ralph-check.sh boundary`, at 9a6c5a80b).
- Because: §12 D6 reads "cli-llm dials the daemon's guest surface", so the charter's "option §12 already names" rule decides it. (b) is a crate move plus a new cw-rails door that rail.rs:12-15 disclaims, and it moves the gate 0. The smaller step over the existing decider wins (principles 8, 11). REVIEW-AFTER: a guest with a stored link and no local daemon can no longer chat. D6 implies this, but no row priced it.

<!-- appendix -->

## five-programs-19 · 2026-09-24 — fp-8: pump half done by fw-1, guest half becomes one daemon door (D6 option a)

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp8-20260924.md. I reproduced it at 9a6c5a80b:

- Boundary gate FAILED with 63 violations.
- daemon.rs:3101-3103 constructs `rails_client::RailsRingRail` as the `RingRailPort`, and `spawn_rail_kv_pump` is at daemon.rs:3972. rail_kv_pump.rs:241 still drains `store.outbox_take`, the daemon's in-memory MeshStore, and moving that is §12 D4's flip (fp-42, parked by five-programs-8).
- `git grep sovereign_mesh::` in sovereign-cli-llm/src finds 12 refs: ingest 1, partitions 3, pipeline_cmd 7, portfolio 1. `guest_route` appears in cli-llm only at chat_cmd/config.rs:315, via `sovereign_cli_mesh`. The `commonwealth_state` refs are `MeshStore` at newsworthy_cmd.rs:31 and portfolio_cmd/mod.rs:19.
- commonwealth-rails/src/rail.rs:12-15 says the guest half is "deliberately NOT mirrored".
- `StoredGuestLink` at sovereign-serving-host/src/guest_lender.rs:250-262 caches the tunnel keyed by dial string, and reads the holder's link file through `GuestLinkReader`. So the door needs no link on the wire.
- guest_route.rs:30-44 opens `GuestTunnel` in-process and refuses with no plaintext fallback. config.rs:311-314 calls a silent fallback to the local daemon a §18.3 substitution, and the door must keep that refusal.

The behaviour cost is why this carries REVIEW-AFTER. Today `open_route` runs in the CLI process, so a guest machine needs no daemon. Once fp-26 dials the door, it does. D6 is the operator's answer ("Operator answers 1-6"), and fp-26's row already records "with no daemon the verb says so". So the charter covers the decision, but the operator has not seen the price stated this plainly.

Falsified if the daemon process does not hold a `StoredGuestLink` for the holder's link: if bootstrap.rs:2335 / provider.rs:154 build it only for a lender-side path, the door has nothing to reuse and would need a new tunnel owner. Also falsified if the operator reads D6's "the mesh owns the tunnel" as the cw-rails process rather than the daemon, which turns this into option (b).

</details>
