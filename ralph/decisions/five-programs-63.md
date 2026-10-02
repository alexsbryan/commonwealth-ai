<!-- ledger -->

**five-programs-63 · 2026-09-25 · standalone state: cw-rails solo mode · operator** — this commit
- Needed: `Rails::start_from_disk` refuses on a meshless node (commonwealth-rails/src/lib.rs:297-299, `Refusal::NoMesh`). Since fp-88 (d18f4514b) and fp-87 (3f57442a9), a local-only daemon therefore answers 503 on `/v1/models` and every store port is absent, and `svrn portfolio` and `svrn newsworthy` have nothing to dial. five-programs-58 ruled the 503 "§12 decision 2's named absence". The seat had asked the director to package it for the operator (seat note on fp-88's third package) and asked the operator directly, but the answer arrived after the rows had landed. The objective forbids changing standalone behaviour silently.
- Chose: cw-rails solo mode, the seat's recommendation. cw-rails runs with no mesh as well: it serves its store and doors, journals locally, and runs no ring sync. It owns its lifecycle by connect-or-spawn through its own `ensure` entry, the MCP rule from five-programs-39, and its clients call that entry, never holding cw-rails' lifecycle. This overrides five-programs-58 on standalone nodes. REVIEW-mint-fp-rails-solo (cap 5) mints the rows before `HUMAN-phase-b`. five-programs is not done until they land.
- Because: operator's word. The rejected arms were a backing chosen by mode (two backings in production, and portfolio needs its own meshless fallback) and accepting the change. Solo mode is the pure outcome under five-programs-54: one owner of the store in every mode, one backing in every client.

<!-- appendix -->

## five-programs-63 · 2026-09-25 — cw-rails solo mode restores standalone state

<details><summary>reasoning, evidence, package</summary>

The regression, measured by the fp-88 worker (ctl/NEEDS_HUMAN.resolved-fp88c-*): with no cw-rails listening, `local_only_boot.rs:278` got 503 on `/v1/models` where it had 200. five-programs-58 then pointed that test at a stand-in door through fp-112's `[daemon] rails_base`, so the suite is green while a production local-only install has no cw-rails at all. The minted rows must prove standalone behaviour against the real cw-rails binary. A process-level e2e test spawns the built binary, located the way fp-cond2-c locates sovereign-daemon, so no crate edge is added.

State at this decision: boundary gate 51, all owned (REVIEW-handoff-phase-b, d963105eb). The full suite was 13,404/0 at 64c3dd7d7 (REVIEW-audit-fp-auto-9), before the condition-2 rows. Condition 2 holds: outside sovereign-daemon, only the two census guards name `EmbeddedDaemon`.

This decision is falsified if cw-rails needs a mesh identity that no single-node form can supply without a charter change. It is also falsified if connect-or-spawn cannot be done without the daemon supervising cw-rails. Either finding is a NEEDS_HUMAN line with the site.

</details>
