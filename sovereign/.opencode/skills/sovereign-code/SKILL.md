# Sovereign Code Intelligence

You have access to a local sovereign-server instance at `http://localhost:8080/mcp`
that provides compiler-resolved code intelligence for this repository.

## Tools available

| Tool | Purpose |
|---|---|
| `symbol_lookup` | Get exact struct/trait/fn definition — use this instead of grepping |
| `find_callers` | Compiler-resolved callers of a function (catches trait dispatch) |
| `find_callees` | What a function calls |
| `code_search` | Semantic search over the codebase |
| `recent_changes` | Files changed in the last N hours |
| `project_context` | Architecture and conventions for a topic |
| `read_notes` | Decisions and invariants from prior sessions |
| `write_note` | Record a decision, invariant, todo, or failed attempt |
| `blast_radius` | Transitive impact of changing a symbol |

## Mandatory pre-flight checks

Before modifying any function or trait:

1. `find_callers("symbol_name")` — know the blast radius before touching it
2. `symbol_lookup("TypeName")` — confirm the exact fields/signature before writing code
3. `read_notes(query: "topic")` — surface decisions from prior sessions

## Session discipline

- Call `read_notes` at session start to surface active TODOs and invariants
- Call `write_note` whenever you make a non-obvious decision or discover an invariant
- Call `project_context` before editing a module you haven't touched this session

## Activity signal

This server reports its coding activity to the Commonwealth inference mesh so
idle peers attract inference load and busy peers don't. The activity level is
visible in the daemon logs and via `GET /internal/node/activity`.
