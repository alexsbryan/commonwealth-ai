<!-- ledger -->

**phase-b-67 · 2026-09-30 · pb-serve-distributes-standalone → closed; the BAR is its own fenced row · seat (operator autonomy, 2026-09-30)** — this commit
- Needed: the supervisor parked -standalone as operator-only. Its baseline BAR trial ran `HOME=<sandbox> sovereign-cli mesh join`, which joined the operator's DEPLOYED node (node-44ae7614) to a test mesh and parked Meshsonics. The worker repaired it by switching back to Meshsonics and forgetting the test mesh. Everything else in the row was done at 7c934c014: the PROOF green, three PLANTs watched red, LIFT(serve) passed, BOUNDARY 19.
- Verified by the seat:
  - The deployed node is on Meshsonics alone, with 16 members and 2 online (RuggedFox and a Mac).
  - The cause reproduces read-only. With a one-line `[daemon] client_port = 19751` sandbox config, both the baseline and HEAD CLI read `mesh status` from :9741.
  - Three sites turn a `SetupConfig::load()` error into `unwrap_or(9741)` or the default port: setup_config.rs `client_daemon_base`, and mesh_cmd.rs:751-755 and :868-870.
- Chose:
  - -standalone is `[x]` at 7c934c014.
  - The BAR moves to the new row pb-serve-distributes-bar, fenced by construction: no CLI verb against a sandbox; HTTP joins on each sandbox's own port; mDNS off; the deployed node's id and mesh snapshot asserted after every step, with abort and NEEDS_HUMAN on drift; a PLANT that proves the fence.
  - The baseline topology is restated before any data. A local-only pair runs no gossip and cannot distribute, so the baseline is plaintext with iroh on and mDNS off.
  - The readings hold the cargo lock for their whole window.
  - pb-distribution depends on the BAR row.
  - The CLI defect is phase-c's pc-cli-config-load-silent-default. No CLI behaviour changes overnight.
- Because:
  - Principle 10: a fence the harness enforces holds; one it relies on the CLI to honour did not.
  - Principle 6 (the defect itself, and why it is recorded, not patched mid-queue).
  - Principle 7: the instrument must be able to measure the thing, so the topology is restated before the data, not after.
  - Principle 5: the BAR stays owed, never dropped.
  - Scope is frozen, and a CLI behaviour change is not a seat change overnight.
  - This commit changes no Rust.

<!-- appendix -->
