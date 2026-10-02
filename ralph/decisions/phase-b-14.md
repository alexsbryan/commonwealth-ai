<!-- ledger -->

**phase-b-14 · 2026-09-26 · pb-code-server size · seat** — this commit
- Needed: phase-b-13 (d9b9ce27a) deferred pb-mcp's HTTP+SSE framing, `McpNotifier` and the registry-backed tool-host port impl to pb-code-server, the first adopter. The row's LIFT stayed at ~1,400. The move alone is roughly 700 changed lines by recipe (the framing, plus the ToolRegistry half of `handle_tool_call`, mcp_router.rs:484-634), on a row that already carried the server, the SCIP loader collapse, the freshness path, the watcher runtime, SpecWatcher's 380-line move and two defects. That totals about 2,000 lines, and five-programs' fw-1 is what a row like that costs.
- Chose:
  - pb-code-server keeps the server running alone: pb-mcp's adopter work, the composition, connect-or-spawn, the fp-11 closure, the notes-fallback defect and the no-refusal delta. LIFT ~1,200, BOUNDARY −1.
  - pb-code-freshness takes the SCIP loader (and its lazy load), the Reindexer freshness path, the watcher runtime, SpecWatcher and the code_search defect. LIFT ~1,400, BOUNDARY −2.
  - pb-code-index depends on pb-code-freshness.
- Because:
  - The two halves prove out differently: code's RUN smoke for the server, and a fixture edit seen through the Reindexer plus a graph-never-opened test for freshness. The CHARTER splits only when proofs differ.
  - Coupling decides where SpecWatcher goes. The new server can take it from sovereign-tools for one more row, which keeps cli-dev → sovereign-tools red one row longer instead of pushing 760 changed lines into the server row.
  - Boundary gate: 51, unchanged. The histogram still sums to 51. No code in this commit.

<!-- appendix -->

## phase-b-14 · 2026-09-26 — pb-code-server splits: the server alone, then its freshness

<details><summary>reasoning, evidence, package</summary>

Dependents re-checked. pb-notes-split and pb-meshapp-rest need only the server, which is pb-code-server. pb-code-index moves `code_index` and its incremental indexer into the code program, next to the Reindexer, so it waits for pb-code-freshness. pb-code-clean depends on pb-code-index and so waits for both.

Falsifier: if pb-code-server's census shows the new server cannot run without the unified SCIP loader (for example, both loaders open one DB and conflict), fold pb-code-freshness back into it.

</details>
