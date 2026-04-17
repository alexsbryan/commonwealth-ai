# Agent instructions — sovereign

## Code intelligence (MCP)

A sovereign-server instance at `http://localhost:9741/mcp` exposes compiler-resolved
tools for this codebase. **Use MCP tools before reading files.**

```
symbol_lookup("TypeName")     → exact definition + file:line
find_callers("fn_name")       → all call sites, compiler-resolved
find_callees("fn_name")       → outbound calls
blast_radius("fn_name")       → transitive impact before editing
code_search("description")    → semantic search
read_notes(query: "topic")    → decisions from prior sessions
write_note(kind, title, body) → record decisions / invariants
```

## Required session start

1. `read_notes(query: "active todos")` — surface pending work
2. `project_context("task area")` — pull relevant conventions
3. `recent_changes(hours: 24)` — see what's been touched

## Pre-flight before editing

- **Before changing a function signature**: `find_callers("fn")` first.
- **Before adding a method to a trait**: `find_callers("TraitName")` to find all impls.
- **Before a non-trivial refactor**: `blast_radius("symbol", max_depth: 2)`.

## Architecture

This is the **sovereign** workspace — the local inference + code intelligence
layer. It does NOT contain the Commonwealth mesh (that lives in `/commonwealth`).

Key crates:
- `sovereign-server` — HTTP server exposing MCP + REST API
- `sovereign-inference` — multi-backend inference (embedded llama.cpp + remote)
- `sovereign-tools` — tool implementations (shell, search, corpus, SCIP graph)
- `sovereign-core` — shared traits and types
- `sovereign-mesh` — P2P gossip integration with Commonwealth

## Activity mesh

When you're actively editing files, sovereign-server signals the Commonwealth
mesh that this node is "hot" (`inference_availability = 0.20`). Peers route
inference requests to idle nodes automatically. You'll see this in the daemon
logs as "activity level transition".

## Compilation and test feedback

The sovereign watcher runs continuously. After editing:
- `lint_status` — check for errors (don't run `cargo check` via bash)
- `test_status` — check test results (don't run `cargo test` via bash)

Only fall back to `cargo` commands if `lint_status` returns `never_run`.
