---
name: sovereign-code
description: Compiler-resolved code intelligence, durable cross-session notes, and peer-work awareness for this repository, served over the local Sovereign MCP server. Use when reading, navigating, or changing code rather than grepping and reading whole files.
---

# Sovereign Code Intelligence

## On skill load

Verify the sovereign tools are in your tool set. If `symbols`, `code_search`, or
`blast` are absent, report immediately:

> sovereign-code tools unavailable. The Sovereign MCP server is not responding at
> localhost:9741. Code intelligence is disabled for this session. Run
> `svrn doctor` to diagnose, `svrn daemon start` to restart.

Do not proceed silently without these tools on any task involving code changes.

---

You have a local Sovereign daemon at `http://localhost:9741/mcp` serving
compiler-resolved code intelligence for this repository.

## Tools

| Tool | Purpose |
|---|---|
| `symbols` | Exact struct/trait/fn definition — use instead of grepping |
| `callers` | Compiler-resolved callers of a function (catches trait dispatch) |
| `callees` | What a function calls |
| `blast` | Transitive impact of changing a symbol |
| `code_search` | Semantic search over the codebase |
| `facts` | Live-fresh extracted facts about a file or symbol |
| `notes` | Decisions and invariants from prior sessions |
| `note` | Record a decision, invariant, todo, or failed attempt |
| `briefing` | One assembled session-orientation brief for this repo |
| `work_in_flight` | Whether a peer or human is already editing this scope |
| `drift_findings` | What the narrative docs claim about a symbol |
| `lint_status` | Whether the workspace currently compiles |

Older names — `symbol_lookup`, `find_callers`, `find_callees`, `blast_radius`,
`read_notes`, `write_note` — still resolve as deprecated aliases. Use the short
names above.

## Mandatory pre-flight

Before modifying any function or trait:

1. `callers("symbol_name")` — know the blast radius before touching it
2. `symbols("TypeName")` — confirm exact fields and signature before writing code
3. `notes(query: "topic")` — surface decisions and invariants from prior sessions

## Session discipline

- Call `briefing` at session start instead of reading architecture docs end to end.
- Call `notes` before editing a module you haven't touched this session.
- Call `note` whenever you make a non-obvious decision or discover an invariant.
- Call `work_in_flight` before non-trivial edits to a hot file — someone else may
  be in there right now.

## Activity signal

The daemon reports coding activity to the Commonwealth inference mesh so idle
peers attract inference load and busy peers don't. Visible in the daemon logs and
via `GET /internal/node/activity`.
