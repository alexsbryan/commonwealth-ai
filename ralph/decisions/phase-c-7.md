<!-- ledger -->

**phase-c-7 · 2026-10-02 · pc-cmnwlth-lift-flake · seat, ruling the lane's PROOF fork** — this commit
- Needed: the lane built both halves of the outcome (b78101d1c keeps a red run's sandbox logs; 97a345a15 gives each commonwealth-rails `state()` fixture its own data dir, the race behind `the_founders_key_admits_a_joiner`'s ENOENT) and stopped at lift n6 of the PROOF's ten: the test phase was green (16 x "test result: ok"), the RUN smoke's self-heal step was not ("the founder's watchdog did not attempt an endpoint rebuild within 150s of losing the joiner"). The row names the test phase; the PROOF counted whole lifts.
- Chose: the PROOF is read on what the outcome names: ten consecutive lifts whose TEST phase is green, host load recorded; a RUN-smoke red is recorded with its kept logs and counts neither way. Six are in (n1-n6 at 97a345a15), so the lane resumes for n7-n10. The self-heal timing is pc-cmnwlth-lift-flake-selfheal, a split of this row and so in scope by the split rule (`out_of_scope`, ralph.py:328): the PROOF as written already required green RUN phases, so the split re-chunks this row's scope rather than adding to it. Moving it below the cut line is the operator's to make if they read it otherwise.
- Because: the PROOF's whole-lift count measured two defects with one number. The substitution is named here and on the row, not taken silently (principle 6), and the bar keeps its N (principle 7). The test-phase fix stands on a direct A/B: the lifted lib test binary looped 4 x 3 at host load ~3 went 12 runs / 4 red before 97a345a15 and 12 / 0 after, every red the one test. Retrying the smoke to green is not on offer.

<!-- appendix -->

## phase-c-7 · 2026-10-02 — pc-cmnwlth-lift-flake's PROOF counts test phases; the RUN smoke's self-heal timing is its own row

<details><summary>reasoning, evidence, package</summary>

The lane's reading of n6's kept founder.log (target/program-lift/cmnwlth/kept-20261001T215240-3494908): the joiner is killed at 04:50:10; gossip and ring sync keep dialing it every ~20s; gossip marks it Offline at 04:51:16; the iroh peer path is lost only at 04:52:37 (~147s), and the first escalation lands at 04:52:40, after the smoke's 150s poll (scripts/program-lift.toml:248-255). iroh 1.0.2 `ACTOR_MAX_IDLE_TIMEOUT` (60s, remote_state.rs:73) applies only with no active connection, and the founder's own re-dials keep one active. Observed: 1 red in 6 RUN phases here, 0 in the 6 pb-distribution's sweep ran at 327a8097b. Whether the watchdog or the smoke's budget is wrong is the new row's census.

</details>
