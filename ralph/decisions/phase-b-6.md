<!-- ledger -->

**phase-b-6 · 2026-09-25 · pb-rails-ready · director** — this commit
- Needed: the worker's MEASURE found that projecting before serving costs about 10 ms per journal line. At operator size that would blow `RAILS_BRING_UP_WINDOW` (10 s), so boot would report cw-rails as unreachable. The worker offered three forks: raise the window, add a prerequisite row that cuts the per-line cost, or change the outcome to a 503 "projecting".
- Chose: the second fork, folded into this row because it proves the same outcome. The dev profile builds `curve25519-dalek`, `ed25519-dalek` and `sha2` at opt-level 3. The window stays at 10 s, and its doc line now says a start includes projection.
- Because:
  - Principle 2: the cause was instrumented before any fork was picked. 100% of the time is in `journal.admit`; the read and the apply take microseconds.
  - Principle 12: ready still means projected. No client-side knob was widened to absorb a cost that cw-rails owns.
  - Nothing a user sees changes except that start is faster. The deployed daemons are dev builds, so they get the speedup too. Release builds are untouched.
  - Boundary gate: 51 violations, EXIT=1, delta 0.

<!-- appendix -->

## phase-b-6 · 2026-09-25 — pb-rails-ready lands under the existing window by optimizing the signature stack in dev

<details><summary>reasoning, evidence, package</summary>

Evidence, from a scratch test (deleted, not committed) that ran `project_all_on_disk` on a COPY of `~/.commonwealth-rails/rings` (11 namespaces, 12,863 journal lines; the real store was only read by `cp`), with per-stage `Instant` timings:

- Unoptimized dev profile: 117.9 s total. `admit` took 55.4 s on work-atlas (5,729 lines), 16.4 s on contributions and 16.0 s on notes. The journal read took 9-141 µs per namespace and `apply_projection` took under 25 µs.
- With the three crates at opt-level 3: 1.80 s, 1.74 s and 1.88 s over three runs.

Faster projection thinned the proof's margin. With 100 rows, the PLANT (the old spawn-before-project order) still went red, but only at start 1. `ready.rs` now seeds 1,000 rows. PLANT went red at start 0 on both runs, and the fix passed on both (19 s each).

Gates: CLEAN; LINT green; TEST(commonwealth-rails) 85/0; TEST(sovereign-daemon) 1288/0; LAYER green; BOUNDARY 51, delta 0.

Why not the other two forks. Raising the window to 60 s or more would size a client constant to a cost that is 65x avoidable, and every future start would pay it. The 503 "projecting" fork reverses the row's stated outcome, and it makes every client learn a second readiness signal (principle 12).

What would falsify this: an operator store that projects in more than about 5 s at the optimized speed (roughly 35k lines at ~0.14 ms per line) would put `ensure_rails` back inside half its window. At that point, answer the growth with seal/snapshot coverage for the journals, since `SEAL_AFTER_OWN_OPS` already exists for this, not with a longer window. The `elapsed_ms` field on "kv: rebuilt the store" is the reading to watch.

</details>
