<!-- ledger -->

**phase-b-111 · 2026-10-01 · pb-distribution-ship-gate · director, the escalation window** — this commit
- Needed: three escalations owed after F13 (knowledge-gym 05_noresults_honesty 0/3; chat-ask q2 audit 14 > 12 in run 3 of 3; throughput could-not-judge with e2e 24.6/29.2 s vs 13.4 s) could not run: the row forbade a second restart, a second 35B beside the deployed one does not fit safely, and the live data dir is a flock singleton that crossed the flip.
- Chose:
  - Option (a): one escalation window, the deployed daemon stopped (phase-b-34's restart grant) and restarted when it closes; C and B run alternately, one resident at a time, each on a fresh copy of one pristine seed holding the lanes' corpora, no node key, sandbox ports, mesh off. ABAB n=3 for all three lanes. Bars fixed in the row before any B data.
  - chat-ask's q2 is escalated, not ruled weather.
  - P3's pre-registered first-token re-read runs as written (74fad65d4's instrument, release, n=3); the worker's "answered by the idle run" is declined.
- Because: the method escalates both a bad-direction reading and a could-not-judge (ralph/PHASE_B_SHIP_GATE.md:21-24), and parity against B is the only reading that tells a Phase B regression from weather; ruling or filing without it would name a verdict no run produced (principles 5, 6, 7). REVIEW-AFTER: a 2-3 h stop of the deployed daemon takes this node's corpora off Meshsonics for the window; phase-b-34 grants a restart, and a window this long is a reading of that grant, not its letter.

<!-- appendix -->

## phase-b-111 · 2026-10-01 — ship gate escalations run in one window with the deployed daemon stopped; P3's first-token re-read runs as pre-registered

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md (2026-10-01 18:50), three forks.

Reproduced: target/quality-check/20261001-180435 holds `audit 14 calls > 12`; 20261001-181755 holds the knowledge-gym lane output with 05_noresults_honesty; 20261001-181538 holds `no baseline` and the e2e readings 24620 and 29238 ms. `free -g` at 19:10 reads 50 GB available of 125. target/ralph/idle-target/release holds sovereign-stock and cw-rails from phase-b-107. 74fad65d4's body names its bar as first-token p50 loopback <= 1.10 x in-process (the `serve_latency_bars` instrument, 0.8B chat), so the idle run's absolute 20.0-20.3 ms is a different quantity. `SOVEREIGN_DAEMON_URL` is the lanes' daemon knob (sovereign-cli-base/src/urls.rs:15).

Fork 1, how B runs. (a) stop the deployed node for a window: about 2-3 h (B build outside the window, 18 lane runs inside it); peers lose this node's corpora meanwhile. (b) rule on the readings: would declare knowledge-gym's honesty check and the e2e turn either regression or weather with no run behind it. (c) file to phase-c: the gate's own text says cut ships when every row has a verdict, and could-not-judge is owed, not passed. (a) is the only option that produces the verdict the method pre-registered, and it is inside the restart grant. Each run gets a fresh seed copy because the cut daemon migrates a main-era dir on boot (F8, F10), so a dir shared across C and B would make the second binary read the first one's layout.

Fork 2, chat-ask's q2. One run of three over the ceiling with no band. Ruling it weather now would set the band after seeing the data. It joins the ABAB at near-zero extra cost.

Fork 3, P3. The pre-registered escalation is the release first-token re-read; the method's general rule separately escalates the lane's could-not-judge to ABAB. Both run. The re-read cannot alarm on the e2e turn, which is why the e2e ABAB is the reading that decides P3; the re-read still runs because it is what was written before the data.

boundary-gate at 095a15c09: 0 violations, EXIT=0.

Falsified if: B reproduces the readings (knowledge-gym 0/3, e2e ~25-30 s) on the same seed, which makes them not Phase B's and this window the cheapest way to know it; or the window cannot run safely (memory, a seeded corpus B cannot read), in which case the node is restarted first and the package goes back with that fact.

</details>
