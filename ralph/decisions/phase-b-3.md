<!-- ledger -->

**phase-b-3 · 2026-09-26 · launch + handover-first row · seat, operator go** — this commit
- Needed: five-programs drained to HUMAN-phase-b, and the operator asked why Phase B had not started. That was the seat's miss: it had held the launch as the operator's gate. While measuring before a daemon restart, the seat also found that HEAD's first boot on an existing install serves an empty store.
- Chose:
  - (1) Launch Phase B now.
  - (2) A new first row, pb-handover-first, restores rail_migration's own contract: the one-time journal handover runs before any cw-rails answers, from every caller that brings cw-rails up.
  - (3) The operator's daemon stays on its 2026-09-23 binary until that row lands. It is then restarted at HEAD and checked.
- Because:
  - Principle 6: an upgraded node reading an empty store would be a silent substitution.
  - Principle 8: the handover gets one call site, which moves; it is not duplicated.
  - Principle 12: the daemon hands its journals over before cw-rails serves, not after cw-rails has already loaded its store.
  - Boundary gate: 51, unchanged. There is no code in this commit.

<!-- appendix -->

## phase-b-3 · 2026-09-26 — Phase B launched; pb-handover-first leads it

<details><summary>reasoning, evidence, package</summary>

**Reproduced on a copy of the operator's `~/.svrnmesh/rings`, never the real one.** The copy held 10 namespaces and 14 MB (notes 5,797,083 B, work-atlas 3,707,102, contributions 1,650,528, and 7 more). The HEAD `sovereign-daemon run` booted terminal-class, with `rails_base` pinned and `CW_RAILS_DIR` set to a temp root.
- 02:50:31.675Z: cw-rails logged `kv: rebuilt the store from the journals on disk namespaces=0`.
- 02:50:31.906Z: `ensure_rails` reported BroughtUp.
- 02:50:32.052Z: the handover logged `the journal moved to the serving process's data root`, once per namespace, and every byte count matched.
- cw-rails never rebuilt, so every moved namespace read empty for that cw-rails' lifetime.
- The order comes from fp-solo-clients. Its `ensure_rails` sits in daemon_cmd/boot.rs:469, and `start_daemon` (daemon.rs:3156) runs the handover after it. rail_migration.rs:4-7 states the contract that broke: "runs once at daemon start, before any rail surface answers".

**Still open:** whether a seal inside that window could retire history the in-memory store never loaded. Seal and prune read the journal files from disk (commonwealth-rail journal.rs:284-352), so the seat expects not, but has not shown it. pb-handover-first's proof reads rows back on the first boot, so the window closes whichever way that question falls.

**Status at launch (measured 2026-09-26).** Boundary gate: 51 edges (42 svrn, 5 code, 3 cmnwlth), matching the handoff appendix. Lift sandboxes: cw-work value 1 on the host with `--image localhost/sovereign-work:latest`; cw-rails COULD-NOT-JUDGE, because its invite expired 2026-09-23; the other programs have no instrument until pb-lift-instrument. five-programs: 145/154 rows, and the 9 open rows are owned here.

</details>
