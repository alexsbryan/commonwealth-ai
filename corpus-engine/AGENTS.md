# Agent instructions — corpus-engine

## Code intelligence (MCP)

A sovereign MCP server at `http://localhost:9741/mcp` exposes compiler-resolved
tools for this codebase. **Use MCP tools before reading files.**

| Tool | Purpose |
|---|---|
| `symbol_lookup` | Exact struct/trait/fn definition — use instead of reading files |
| `find_callers` | All call sites, compiler-resolved (catches trait dispatch) |
| `find_callees` | What a function calls |
| `blast_radius` | Transitive impact of changing a symbol |
| `code_search` | Semantic search across the codebase |
| `recent_changes` | Files changed in the last N hours |
| `project_context` | Architecture and conventions for a topic |
| `read_notes` | Decisions and invariants from prior sessions |
| `write_note` | Record a decision, invariant, todo, or failed attempt |
| `lint_status` | Dormant — watchers are off here; use `scripts/sovereign-lint.sh` |
| `test_status` | Dormant — watchers are off here; use `scripts/sovereign-test.sh` |

## Required session start

1. `read_notes(query: "active")` — surface active invariants and todos
2. `project_context("<task area>")` — pull relevant conventions
3. `recent_changes(hours: 24)` — see what's been touched

## Pre-flight before editing

Call graph tools are not available (SCIP not enabled). Use `code_search` to find usage patterns.

## Build and test feedback

The lint/test watchers are **off in this repo by design** — `.sovereign/sovereign.toml`
declares `[watchers] enabled = false` (they OOM'd the daemon under a resident model).
So `lint_status` / `test_status` have nothing to report and `sovereign doctor` treats
the opt-out as a pass. Do not try to repair them, and do not open a session by
diagnosing them.

The gate is the two scripts, run **inside the toolbox** (on the Fedora host
`llama-cpp-sys-4` cannot build — no clang):

```bash
toolbox run -c sovereign-vulkan ./scripts/sovereign-lint.sh --human --full
toolbox run -c sovereign-vulkan ./scripts/sovereign-test.sh --human
```

Gate on the **exit code**. Both scripts write the raw cargo output under `target/`
so a failure can be triaged without re-running cargo. Doctests are off by default
in the test script (CI runs them); the banner says so when they are skipped.

## Session discipline

- Call `write_note(kind: "invariant")` when you discover a constraint that must never be violated.
- Call `write_note(kind: "decision")` when you choose one approach over alternatives.
- Call `session_reflection` at the end of any significant task.

## Inference

Commonwealth mesh provider is configured at `http://localhost:9741`.
Use model `commonwealth/<model-id>` in opencode to route through the mesh.
Run `GET http://localhost:9741/v1/models` to list currently available models.
