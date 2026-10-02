<!-- ledger -->

**phase-b-31 · 2026-09-27 · Phase B → svrn starts no other process: cw-rails' bring-up goes to one opt-in mesh verb, its idle cost gets a row and two bars, and a blocked row no longer stops the loop · operator**
- Needed: the seat found the daemon-started cw-rails burning ~68% of a core (the operator saw 93%) for about 40 h, unattributed. svrn's boot brings cw-rails up (sovereign-daemon daemon_cmd/boot.rs:396, `ensure_rails`), and so does cli-llm's `rails_kv()` (legacy_store.rs:36). Nothing else starts it. pb-distribution had planned to move that call into the stock binary's main, which would have kept every stock start tethered. Separately, the loop stopped outright at every HUMAN- row and every operator-only package: HUMAN-pb-lanes-dials-serve held about 40 independent rows from 07:29Z to 16:19Z.
- Chose (operator):
  - "We can't make starting the daemon tethered to a process eating 93% of the CPU — that makes no sense for end user generalized library experience (the daemon has to be as performant as llama server)." New row pb-rails-untether, after pb-stock-binary and before HUMAN-pb-lanes-dials-serve, whose AFTER reading is the untethered binary. svrn's boot and `rails_kv()` only dial cw-rails and name its absence. The fork on where the bring-up goes: ONE opt-in verb of the mesh program (`svrn mesh up` is the working name) owns `ensure_rails` and the rings handover. The seat had recommended cw-rails starting itself, with the handover staying at svrn's boot; the operator chose the verb. Nothing starts cw-rails by default, the distribution included.
  - cw-rails' idle burn is a Phase B row (pb-rails-idle), by the operator's explicit exception to phase-b-29's scope guard, together with two bars pre-registered before any data: cw-rails idle ≤ 2% of one core, and the stock binary's idle CPU ≤ llama-server's + 2% of one core with first-token p50 at most 10% slower (release profile, n = 3, 5-minute windows). Attribution under a profiler comes before any fix.
  - "Let's unblock and if we get blocked pull in rows that aren't blocked … rather than using every roadblock as a total stop." "The goal is to get the current runs done on this machine as quickly as possible." The planner now skips HUMAN- rows and parked rows. The supervisor parks a row whose package is operator-only or that directors could not clear, and runs on (d607b9102). The loop stops only when nothing else is ready.
- Because:
  - FIVE_PROGRAMS §4 rule 8: the mesh is a layer added from outside. A program passes its journeys with cw-rails absent, and the absence costs reach, never an answer.
  - Principle 12: a component holding another's lifecycle. svrn's boot owned cw-rails' start, and every svrn lift smoke and NER live test leaked a detached cw-rails.
  - Principle 7: the idle bars exist before the reading.
  - Principle 11: pb-distribution already carried the bring-up move, and its PROOF and PLANT move to the new row rather than being written twice.
  - Boundary gate: 49 at 8b06e3c70, delta 0 for both new rows.

<!-- appendix -->

## phase-b-31 · 2026-09-27 — untether, idle cost, and a loop that runs past a block

<details><summary>reasoning, evidence, package</summary>

Census (seat, at d436419a8): `ensure_rails` (rails_client/bring_up.rs:38) is the only `CW_RAILS_BIN` reader outside commonwealth-rails. rail_migration.rs names only commonwealth-media, toml and std, so the handover can leave svrn with no new red edge. bring_up.rs:154 names svrn's `SetupConfig::default_path()`; the verb's caller passes the config path in instead. On this host the handover already ran: `~/.svrnmesh/rings` is empty, and `~/.commonwealth-rails/rings` holds 10 namespaces, among them `work-atlas`, `work` and `notes`. So until the operator runs the verb here, the work atlas answers the named absence. The seat's `declare_scope` at ~20:25Z already did ("cannot reach the mesh's serving process at http://127.0.0.1:9747").

pb-distribution: its Layering bullet, its "svrn boots and answers with cw-rails absent" PROOF and its `ensure_rails`-at-boot PLANT are struck and point here. 18 of boundary.log:66's 27 lines leave sovereign-daemon in pb-rails-untether, and :66 itself still closes in pb-distribution after the meshapp rows. pb-distribution now depends on pb-rails-untether.

The loop: pb-stock-binary halted at 20:17Z on a one-line `.config/nextest.toml` clause. The worker and the director were both refused `git add .config/nextest.toml` by `Bash(git add .*)` in ralph/claude-settings.json, a rule meant for `git add .` that matched every dot-path as a prefix. The seat committed the clause (8b06e3c70, the worker's prepared message) and narrowed the rule to the exact `git add .` and `git add . *`.

The option the operator did not take, a new `svrn mesh up` verb vs. cw-rails starting itself: the verb costs more code (the ~580-line move plus the verb), and it keeps a svrn-side entry point to cw-rails' lifecycle. It gives the upgrade path one command that hands the rings over and brings cw-rails up together.

</details>
