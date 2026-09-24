<!-- ledger -->

**five-programs-31 · 2026-09-24 · fp-11 · director** — this commit
- Needed: fp-11 halted because its premise failed: it says "DIAL code tools", and no code-program MCP server exists to dial.
- Chose: park fp-11 on a new operator row, HUMAN-fp11-code-mcp-host (the fp-10 and fp-25 shape). Recommendation to the operator: (b), an `[[exception]] package = "svrn"` for sovereign-daemon → sovereign-code, which is fp-10's answer applied to the MCP host.
- Because: TSV:15's `decision_needed` is `none`, but its own `missing_capability` cell ("the code program's MCP surface must exist") names new capability. Standing that up means moving mcp_router, the reindexer and the SCIP graph, adding a proxy, then dropping the dep, over 10+ files. That is principle 11's mint, not a row rewrite, and the only no-code close is an exception. Both are the operator's. Measured at boundary-gate 56.

<!-- appendix -->

## five-programs-31 · 2026-09-24 — fp-11 has nothing to dial: park on HUMAN-fp11-code-mcp-host

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp11-20260924.md. Reproduced at f03daf7ff:

- `scripts/ralph-check.sh boundary` reports 56 violations. `sovereign-daemon → sovereign-code` is on the list.
- `git grep -c sovereign_code -- sovereign/crates/sovereign-daemon` gives src/tool_registry.rs 34, tests/main/e2e_code_intel.rs 5, and e2e_code_intel/demo_auth.rs 9. The tests are dev-only, so the src file carries the edge by itself.
- tool_registry.rs:299-315 constructs SessionStateTool, WriteNoteTool and ReadNotesTool from sovereign_code. That is the notes/session_state MCP surface every harness session calls, so it is not just "code intel".
- tool_registry.rs:38 takes `sovereign_code::ScipGraphHandle`, and daemon_cmd/boot.rs:648 builds the one `corpus_engine_watchers::reindexer::ScipGraphHandle` that is shared with the freshness pipeline. Moving the tools without the reindexer brings back the frozen-snapshot bug.
- sovereign-cli-dev/src/project_cmd/serve.rs:666,675,696 builds project serve's MCP app from `sovereign_daemon::mcp_router`. That is the cli-dev → daemon edge fp-11 also owns (STATE appendix line for `sovereign-cli-dev → sovereign-daemon`). So "project serve dials the daemon" (§11) and "the daemon dials the code program" (TSV:15) run in opposite directions.
- TSV:15's behaviour_delta accepts that code tools "stop if the code process is not running". But no process today would be that code process, and the notes tools would go with them. That is a change to the default MCP surface, which is gated behind a decision that does not exist yet.

The HUMAN row gives three options. (a) project serve becomes the code server (REVIEW-mint, multi-row), with a sub-choice on where the notes/session tools live. (b) The exception, recommended: no code, 56 → 55, fp-11 narrows to the cli-dev → daemon mcp_router move. (c) Drop code tools from the daemon's /mcp, which changes the default surface and is not recommended.

Falsifier: a code-program MCP server already exists that the daemon can reach without supervising it, or mcp_router turns out to be movable into a shared leaf in a way that also carries the tool construction out of the daemon. In either case fp-11 is a worker row again and the park was wrong.

Gate at decision: boundary-gate 56 violation(s).

</details>
