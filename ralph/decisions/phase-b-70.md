<!-- ledger -->

**phase-b-70 · 2026-09-30 · pb-cli-llm-ingest-move (worktree B) · seat as B's director (operator autonomy)** — this commit
- Needed: B's worker stopped the ingest move at census after landing three seams (4257ec3b9, 9388bd2cd, 684a1813d; BOUNDARY 17 → 17). It raised five findings:
  - (1) cli-llm's in-process Runtime builds a `CorpusEngine` (chat_cmd/bootstrap.rs:207; probe_cmd/vault_build.rs:445), and no composition root hands cli-llm ingest's ports;
  - (2) many ingest-group modules are clients of svrn's store, routes or tools;
  - (3) phase-b-51's RAPTOR move would leave standalone svrn without conversation RAPTOR;
  - (4) two premises in the row are false: serve_dial names the daemon's `serve_client`, and sovereign-cli links awareness in-process;
  - (5) the honest scope is ~12-15k lines of placements plus the 44k-line move.
- Chose:
  - (1) Package option (a), the daemon's precedent: cli-llm's lib takes `Option` ports, a second `[[bin]]` in sovereign-stock composes it through `HostedIngest`, the dispatcher execs that binary, and bare cli-llm names the absence.
  - (2) Principle 12's placement rule. A module that opens svrn's store, calls svrn's routes, or uses svrn's tools or atlas views is a svrn verb and stays in the remainder, reaching ingest through the ports. Only purely-ingest modules move.
  - (3) RAPTOR moves only if a module that moves still names it.
  - (4) serve_dial goes to the turn-client leaf; awareness is placed by rule (2) at census, and its `svrn awareness` spelling never changes.
  - (5) Split into -compose, -remainder, then the parent (the mechanical move, BOUNDARY −6).
  - A holds the two new rows in ctl/parked for B.
  - The census is archived at target/ralph/phase-b/pb-par-ingest-needs-human.phase-b-70.md.
- Because:
  - Principle 8: one composition pattern for svrn's processes, not a second.
  - Principle 12: what a module touches decides whose it is.
  - Principle 6: absence is named, on bare binaries only.
  - Principle 11: the stock distribution and `HostedIngest` exist; the turn-client leaf has serve's base since 8ba99165e.
  - Scope guard phase-b-29: split by proof.
  - Nothing a user sees changes on a default install.
  - This commit changes no Rust.

<!-- appendix -->
