# Code Intelligence

`sovereign project init` sets up the full code-intelligence stack for a repository in one command: tree-sitter symbol index, SCIP call graph, AI assistant integration, filesystem watcher hooks, and a generated `SOVEREIGN.md` with tool guidance. No manual steps.

After init, Claude Code and opencode automatically use a hybrid approach: Grep/Glob/Read for discovering what exists in a module, `symbol_lookup` for precise type definitions, `find_callers`/`find_callees` for compiler-resolved impact analysis, and `recent_changes` for session orientation. The strategy is documented in the generated `.sovereign/SOVEREIGN.md` and wired into `.claude/settings.json` so it works without any manual configuration.

← [back to README](../README.md)

## Quick start

Global install (`sovereign-cli` on PATH, e.g. via `cargo install`):

```sh
cd /path/to/your-project
sovereign project init
```

From source:

```sh
cd sovereign
cargo build --release -p sovereign-cli
./target/release/sovereign-cli project init    # run from your project root
```

## What it creates

| File | Purpose | Committed? |
|---|---|---|
| `.sovereign/SOVEREIGN.md` | Tool reference, session-start protocol, project invariants | Yes |
| `.sovereign/project.json` | Stores corpus ID, port, flags for `status`/`refresh` | No (gitignored) |
| `.claude/settings.json` | MCP server entry + system prompt (merged, not overwritten) | Your choice |
| `.claude/hooks/inject-notes.sh` | Injects active invariants/decisions before every Claude response | Your choice |
| `.opencode/config.json` | MCP server entry + Commonwealth inference provider (merged) | Your choice |
| `AGENTS.md` | AI assistant instructions: MCP tools, session start, inference routing | Yes (if absent) |
| `.git/hooks/post-commit` | Runs `sovereign project refresh --quiet &` after each commit | No (local) |
| `~/.sovereign/indexes/{corpus}/` | LanceDB symbol index | N/A (outside repo) |
| `~/.sovereign/indexes/{corpus}/scip_graph.db` | SCIP call graph (SQLite) | N/A (outside repo) |

## AI assistant auto-detection

`sovereign project init` auto-detects which AI coding assistants are installed (`.claude/`, `.opencode/`, `~/.claude/`, `~/.opencode/`, or the `claude`/`opencode` binaries on PATH). For each one detected, it prompts:

```
Detected Claude Code. Write config automatically? [Y/n]
```

In non-interactive environments (CI, pipes) the prompt defaults to yes. If neither harness is detected, `init` writes nothing extra — no clutter for non-AI-coding projects.

### Commonwealth mesh inference for opencode

After `sovereign setup` runs once, the local Commonwealth daemon lives at `http://localhost:9741`. The generated `.opencode/config.json` looks like:

```json
{
  "mcp": { "servers": { "sovereign": { "type": "http", "url": "http://localhost:9741/mcp" } } },
  "provider": {
    "commonwealth": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Commonwealth Mesh",
      "options": { "baseURL": "http://localhost:9741/v1" },
      "models": {
        "Qwen3-9B": { "name": "Qwen3-9B" }
      }
    }
  }
}
```

If Commonwealth isn't reachable at init time, a single `"auto"` model entry is written as a placeholder — the mesh routes it at runtime. Re-run `sovereign project init` after starting the daemon to populate real model IDs.

Select the provider in opencode with `commonwealth/<model-id>` (e.g. `commonwealth/Qwen3-9B` or `commonwealth/auto`).

## Flags

See [CLI_REFERENCE.md](CLI_REFERENCE.md#sovereign-project) for the full flag list. Quick reference:

| Flag | Description |
|---|---|
| `--name <id>` | Corpus identifier (default: directory name) |
| `--port <port>` | MCP server port (default: 9741) |
| `--data-dir <dir>` | Index directory (default: `~/.sovereign/indexes`) |
| `--workspace-root <p>` | Monorepo root; discover every Cargo/Go/etc. workspace under `<p>` |
| `--no-scip` | Skip SCIP call graph export |
| `--no-hooks` | Skip git hook installation |
| `--no-claude-config` | Skip writing `.claude/settings.json` |

## Ongoing commands

```sh
sovereign project status     # check that everything is healthy
sovereign project refresh    # re-export the call graph (auto-runs on commit with hooks)
```

## SCIP exporters

The call graph tools (`find_callers`, `find_callees`) require a language-specific SCIP exporter on PATH. Without one, `init` completes successfully but those tools return a `LanguageNotIndexed` caution instead of results.

| Language | Exporter | Install |
|---|---|---|
| Rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| Go | `scip-go` | `go install github.com/sourcegraph/scip-go@latest` |
| TypeScript | `scip-typescript` | `npm install -g @sourcegraph/scip-typescript` |
| Python | `scip-python` | `pip install scip-python` |
| Java | `scip-java` | [sourcegraph.github.io/scip-java](https://sourcegraph.github.io/scip-java/) |

## Multi-project ecosystems

The commonwealth-ai workspace contains five projects (`sovereign`, `corpus-engine`, `commonwealth`, `oicp-types`, `sovereign-recipes`), each with its own git repo. All five should be indexed so that `symbol_lookup` and `code_search` work across the entire codebase.

Every project gets its own corpus ID (default: directory name). All indexes live under `~/.sovereign/indexes/`. The tools `symbol_lookup`, `code_search`, and `recent_changes` automatically query every installed index, so `symbol_lookup("InferenceProvider")` finds the definition in `sovereign` and `symbol_lookup("EmbedFn")` finds it in `corpus-engine`, regardless of which project you're working in.

The call graph is per-project — cross-project call edges aren't tracked. `corpus-engine` has no dependency on `sovereign`, so there are no cross-project call edges for the compiler to resolve.

### Index all five projects

```sh
for project in sovereign corpus-engine commonwealth oicp-types sovereign-recipes; do
  (cd "$project" && sovereign project init)
done
```

### Serve the MCP server across all of them

```sh
sovereign project serve
```

This starts a lightweight, model-free MCP server that discovers every index under `~/.sovereign/indexes/`, merges SCIP graphs into a single in-memory view, and exposes all five tools over JSON-RPC at `http://localhost:9741/mcp`. Each project's `.claude/settings.json` (written by `init`) already points at it — open Claude Code in any project directory and the tools light up.

Typical startup banner:

```
  Sovereign Code Intelligence MCP Server
  ──────────────────────────────────────────────────────

  Corpora:
    ✓ sovereign (8,402 symbols)
    ✓ corpus-engine (3,291 symbols)
    ✓ commonwealth (2,104 symbols)
    ✓ oicp-types (312 symbols)
    ✓ sovereign-recipes (94 symbols)

  Call graph:
    ✓ sovereign: 8,402 symbols, 51,203 edges
    ✓ corpus-engine: 3,291 symbols, 18,447 edges
    ✓ commonwealth: 2,104 symbols, 12,801 edges
    Total: 13,797 symbols, 82,451 edges across 3 projects

  Tools: 5 registered
  Listening on http://127.0.0.1:9741/mcp
```

### Full workflow

```sh
# 1. Build
cd sovereign
cargo build --release -p sovereign-cli
SOVEREIGN=$PWD/target/release/sovereign-cli

# 2. Index all projects (symbols only — fast)
cd ..
for project in sovereign corpus-engine commonwealth oicp-types sovereign-recipes; do
  (cd "$project" && $SOVEREIGN project init --no-scip)
done

# 3. SCIP export (slower, requires rust-analyzer)
for project in sovereign corpus-engine commonwealth; do
  (cd "$project" && $SOVEREIGN project refresh)
done

# 4. Start the MCP server (runs in foreground)
$SOVEREIGN project serve
```

### Re-indexing

`sovereign project init` is safe to run repeatedly — existing settings are merged, hooks aren't duplicated, and the index is rebuilt from scratch. Restart `sovereign project serve` afterward to pick up the new data.
