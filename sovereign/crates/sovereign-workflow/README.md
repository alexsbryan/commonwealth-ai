<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# sovereign-workflow

**Step · Artifact · Runner** — a typed dataflow substrate for local-model
workflows. P0+P1 of [`../../docs/specs/WORKFLOW_SUBSTRATE.md`](../../docs/specs/WORKFLOW_SUBSTRATE.md):
the smallest real instance, proving the abstraction on a user-authored workflow
before any unification (P2+).

A **Workflow** is a TOML graph. Nodes are **Step**s — `model:` (a local-model
call, routed to the daemon), `mcp:` / `tool:` (a tool call), or `transform:` (a
deterministic function). Edges are **auto-derived** from `{step.key}` references,
so you never write an edge list. The single-process **Runner** topologically
orders the steps and runs them per source item, threading **Artifact**s between
them. The crate is *core-only* — inference + tools are injected by the caller.

```
sovereign workflow run <file.toml> [--concurrency N] [--daemon <url>]
```

## Demo — [`examples/notes-digest.toml`](examples/notes-digest.toml)

For each note in a folder, read it via the reference MCP server and summarize +
extract action items with the local model:

```sh
# 1. the reference MCP server (provides read_memo) + register it
sovereign mcp demo-server &
sovereign mcp add demo --url http://127.0.0.1:4319/mcp

# 2. a folder of notes
mkdir -p notes
printf 'Standup: shipped the MCP work. TODO: spec the substrate, ping Vega.' > notes/standup.md

# 3. run (needs the daemon up for the model: step)
sovereign workflow run sovereign/crates/sovereign-workflow/examples/notes-digest.toml
```

One file mixes an ecosystem-shaped MCP tool, a local model, and data-authored
steps. Swap `mcp:demo:read_memo` → `mcp:whisper:transcribe_audio` and `*.md` →
`*.m4a` and it's audio transcription — no code change.

## Cache + resume (on by default)

Each `Read` step's output is content-addressed by its resolved inputs (incl. the
source file's mtime+size), persisted under `~/.sovereign/workflow-cache`. So a
re-run skips unchanged work; editing one file re-runs only that item; a
`Write`-effect step (e.g. `write_note`) is never cached. `--no-cache` forces a
full run; a per-step `cache = false` opts a volatile read out.

```
sovereign workflow run notes-digest.toml          # read·digest ×10   ~42s
sovereign workflow run notes-digest.toml          # 20 cached, 0 ran  ~0.3s
touch notes/standup.md && sovereign workflow run notes-digest.toml   # 1 item re-runs
```

## What's P2+ (not here)

Durable/distributed execution (the pipeline tool as an outer loop), the
inference-resource scheduler (`BackendSelector`), and corpus/enrichment/executor
convergence. See the spec.
