<!-- ledger -->

**phase-b-30 · 2026-09-27 · Phase B → the pre-flight sweep's forks answered: gate 0 kept with cli-llm's two halves pulled in, the composition root generalised, mesh-reach admitted · operator**
- Needed: phase-b-29 ordered REVIEW-pb-preflight-1..3 to census and trial every open row and end in ONE package of the remaining forks. The seat ran the three in one session with the operator present: three read-only census agents, one per cluster, and 43 compile trials in a detached worktree (target/ralph/phase-b/trials/; baseline 0 errors, BOUNDARY 49, LAYER pass). The census found 143 false premises across the 29 open rows, and re-priced them from about 43k stated lines to about 200k changed, about 140k of it moves or deletions. pb-cli-llm alone went from ~1,600 to ~112k: its 9 edges close only when the bench half (60.7k) and the ingest half (47.4k) leave the crate (its trial: 485 unresolved paths in 139 files).
- Chose (operator, the seat's recommendation in every group; package target/ralph/phase-b/preflight-forks.md):
  - Group 1. Keep gate 0. Pull pc-bench-dials' dial half into Phase B, then move cli-llm's bench half to bench's own CLI crate. Move its ingest half into the one ingest CLI crate, after the ingest dials. pc-bench-dials' judge and probe fixes stay in phase-c.
  - Group 2. The composition root generalises from phase-b-29 Q1. svrn's ingest-executing code calls an ingest port in sovereign-contracts, beside `CorpusReadPort`, and the stock binary hands it ingest's engine (F5 (b); HUMAN-fp7 (a) stands). The stock binary mounts code's MCP bundles on :9741 (F2 (a)). Phase B enrols only the stock binary as a `[[distribution]]`, and fp-25 closes by exec'ing it; the dispatcher and setup carve goes to phase-c. A distribution row carries a fixed `max_code_lines`, never ratcheted; stock = 300.
  - Group 3. Admit ONE new neutral contract-layer leaf, `mesh-reach`, holding `PeerTransport`, `PeerContact`, `TrafficClass`, `PeerEndpoint`, `RailsTransport` and the iroh guest dialer (leaf count 22 → 23). cw-rails runs the work donor and ProcessExecutor and forwards `ingest:v1` to an execute origin the daemon registers. sovereign-tdd, agent-tools and agent-bench move to `[code]`.
  - Group 4. notes.db stays code's; svrn's rows migrate to sovereign-store with a conservation test. Delete the legacy pull loop after a watched `ingest:v1` run (cw-lift 5g part 2). `router fit`, `router-cache` and `corpus extract-entities` dial serve and refuse by name on a model mismatch. Code writes its chunk index by exec'ing ingest's CLI. The mesh work-atlas test goes with pb-mesh-dissolve. Grants' merge half calls ingest's port. The judge fail-open fix goes to phase-c.
  - The queue is rewritten from the census: every work row carries a `finish:` bullet and a `trial:` bullet, and `dispatch_requires = ["finish:", "trial:"]` in queue.toml makes the planner refuse a row without them (principle 10).
- Because:
  - Principle 11 and phase-b-29's own falsifier: 9 of phase-b's 14 worker halts were found at execution; a compile trial lists every use site before a worker opens the row.
  - Principle 12 and §2c: the composition root outside every package is what makes a port legitimate between two programs. §12's Ports row called ports vacuous only because the root was [svrn].
  - Principle 8: one ingest executor, one job-execution drive, one tool-set build, one peer-transport vocabulary.
  - Boundary gate: 49 at f7238e6d3. No code is in this commit.

<!-- appendix -->

## phase-b-30 · 2026-09-27 — the pre-flight sweep's forks answered

<details><summary>reasoning, evidence, package</summary>

Run by the seat in session 8bc4e7be with the operator present, instead of as three serial loop sessions, because the loop had waited about 22 h on operator decisions in the previous round (phase-b-29). The loop was stopped for it (an empty ctl/STOP at 16:40Z, which also killed the in-flight REVIEW-audit-pb-auto-4 session; that row stays `[~]` with its uncommitted partial fixes in the tree).

Instruments:
- Census: three read-only agents, one per cluster; files in target/ralph/phase-b/census/ (one per row, plus three summaries and the FIVE_PROGRAMS reconciliation).
- Trials: target/ralph/phase-b/trials/. Each deletes the dependency edges a row closes, runs `cargo check --workspace --all-targets --keep-going` with the gate scripts' feature set, then boundary-gate and layer-gate, then reverts. The error count is unresolved paths (import sites), a lower bound on use sites. Validated against hand cites: the D6 trial finds exactly project_init/mod.rs's 2 sites, and the cli-dev → sovereign-tools trial exactly serve.rs's 1. A trial that strands an `[[exception]]` reads +1 per exception: the gate names the stale row.
- pb-stock-binary's trials: the gate ignores a `[[distribution]]` table today (T1: BOUNDARY 49, no mention); a crate linking sovereign-daemon and sovereign-serve compiles in 84 s with ONE llama-cpp-4 in its tree, and its only LAYER failure was the fan-in ratchet on sovereign-contracts (43 → 44) because the trial crate named contracts directly, so the stock crate reaches contracts only through the two faces.
- pb-serve-package's re-map: 49 → 54, not 55 (serving-host → commonwealth-core closed in pb-serve-sheds-core).

Rejected:
- Group 1 (B), two named exceptions: fails "no `package = "svrn"` exception remains".
- Group 2 F5 (a), dialing the ingest CLI: about 27k lines against about 3k, and it reverses HUMAN-fp7 (a).
- Group 3 (b), no leaf: reverses Q3's wording and grows `OriginKind`; a cw-rails guest mode changes `cw-rails run`'s refuse-without-mesh contract.

What would falsify this:
- The ingest write port needs more than ~30 methods in its first trial. Then the ingest-running code moves into ingest's library instead of behind a port.
- `mesh-reach` needs anything beyond kernel-types, iroh and workspace-hack. Then it is a mechanism, not vocabulary, and the leaf is wrong (§12 "The risk the worklist has to hold").
- A row after this commit halts at census on a premise its trial line would have caught. Then the trial rule is not applied, and the charter says why.

</details>
