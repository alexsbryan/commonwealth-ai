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
| `lint_status` | Check for compile errors (do not run `cargo check` via shell) |
| `test_status` | Check test results (do not run `cargo test` via shell) |

## Required session start

1. `read_notes(query: "active")` — surface active invariants and todos
2. `project_context("<task area>")` — pull relevant conventions
3. `recent_changes(hours: 24)` — see what's been touched

## Pre-flight before editing

Call graph tools are not available (SCIP not enabled). Use `code_search` to find usage patterns.

## Build and test feedback

The sovereign watcher runs continuously in the background. **Do not run `cargo check`,
`cargo build`, `cargo test`, or `cargo clippy` directly** — they contend with the watcher
for the Cargo file lock and stall both processes.

- Check compile status: `lint_status` — response includes `age_seconds`, `watched_scope`,
and `watcher_active` so you can confirm the result covers your changes.
- Check test status: `test_status` — if stale, call `run_tests` then poll.
- Only fall back to `cargo` commands when `lint_status` returns `watcher_active: false`.

## Session discipline

- Call `write_note(kind: "invariant")` when you discover a constraint that must never be violated.
- Call `write_note(kind: "decision")` when you choose one approach over alternatives.
- Call `session_reflection` at the end of any significant task.

## Inference

Commonwealth mesh provider is configured at `http://localhost:9741`.
Use model `commonwealth/<model-id>` in opencode to route through the mesh.
Run `GET http://localhost:9741/v1/models` to list currently available models.
