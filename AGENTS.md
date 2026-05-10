# Agent instructions — commonwealth-ai

## Quick facts

- **Multi-workspace monorepo** with three Cargo workspaces: `corpus-engine`, `sovereign`, `commonwealth`
- **No single root** `Cargo.toml` — each subdirectory is its own workspace
- **Shared MCP server** at `http://localhost:9741/mcp` — use before reading files
- **Build/test scripts** in `/scripts/` run across all workspaces, not raw cargo

## Required session start

```bash
read_notes(query: "active")
project_context("<task area>")
recent_changes(hours: 24)
```

## Builds and tests

```bash
# Lint all three workspaces (runs cargo check in parallel)
./scripts/sovereign-lint.sh

# Test all three workspaces
./scripts/sovereign-test.sh
```

- `corpus-engine`: test with `--features treesitter`
- `sovereign`, `commonwealth`: test without feature flags
- Do NOT run `cargo check/test` directly — it contends with the sovereign watcher

## Architecture

```
commonwealth-ai/
├── commonwealth/      # Mesh coordination daemon (runs at localhost:9741)
├── sovereign/      # Local AI + code intelligence server
├── corpus-engine/  # Knowledge base engine
├── oicp-types/    # Shared protocol types (used by both)
├── sovereign-recipes/  # Data recipes
└── scripts/       # Build/test wrappers
```

** commonwealth ≠ sovereign**. They are peer projects, not parent/child. The Commonwealth mesh daemon serves a local API that sovereign uses for inference routing.

## Session discipline

- Use `write_note(kind: "invariant")` when you discover never-violate constraints
- Use `write_note(kind: "decision")` when you choose an approach
- Use `session_reflection` at task end

## Pre-flight before editing

- Before changing a function signature: `find_callers("fn_name")`
- Before adding a trait method: `find_callers("TraitName")`
- Before non-trivial refactor: `blast_radius("symbol", max_depth: 2)`