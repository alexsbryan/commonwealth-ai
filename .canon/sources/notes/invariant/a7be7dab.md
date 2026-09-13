# FLAKY UNDER LOAD: sovereign-compute supervisor::tests::a_proven_healthy_generation_resets_the_breaker (supervisor.rs:1251). Do not chase it…

FLAKY UNDER LOAD: `sovereign-compute` `supervisor::tests::a_proven_healthy_generation_resets_the_breaker` (supervisor.rs:1251). Do not chase it as a regression.

SYMPTOM: in a full `--workspace` nextest run (9357 tests, 6 jobs on 12 cores) it fails with
  `expected repeated restarts to exercise the reset, got 0 in [Starting, Healthy { pid: …, since_unix: … }]`
Observed 2026-08-07 in one full run; the immediately preceding full run of the SAME tree passed it and failed a different test instead.

EVIDENCE IT IS THE HARNESS, NOT THE CODE:
  - 5/5 pass in isolation (`cargo test -p sovereign-compute --lib -- supervisor::tests::a_proven_healthy_generation_resets_the_breaker --exact`).
  - `supervisor.rs` was untouched by all 55 commits rebased that day; its last change is upstream's `c7b82215 fmt`.
  - Failure time in the parallel run was 1.449s vs ~1.4s of real test work in isolation — i.e. it did not hang, it ran to completion having observed nothing.

WHY IT IS LOAD-SENSITIVE, from the test body: it spawns a real child process, then
`drain_states(&mut states, 40, Duration::from_millis(1200))` collects state transitions inside a
1200ms WALL-CLOCK budget and asserts `restarts >= 2`. `crash_loop_max = 1` and
`healthy_reset_after = 150ms`. Under CPU starvation the child cannot complete two spawn→crash→restart
cycles before the 1200ms window closes, so the drain returns `[Starting, Healthy]` and the count is 0.
Nothing in the assertion distinguishes "the breaker did not reset" from "the scheduler did not run us".

IF YOU WANT TO FIX IT (not done — it is upstream's test and no one has quantified how often it
bites): the durable shape is to drive the supervisor off an injected clock, or to wait on a
restart-count condition rather than a fixed wall-clock drain. Raising the 1200ms constant only
moves the load level at which it flakes.

RELATED: a full-suite fail of exactly one test, in a file your diff never touched, is the signature
to check for FIRST — re-run that test alone before assuming your change caused it.
