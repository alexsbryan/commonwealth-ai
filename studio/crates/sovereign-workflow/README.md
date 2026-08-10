<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# sovereign-workflow

**Step · Artifact · Runner** — a typed dataflow substrate for local-model
workflows. P0+P1 of [`../../docs/specs/WORKFLOW_SUBSTRATE.md`](../../docs/specs/WORKFLOW_SUBSTRATE.md):
the smallest real instance, proving the abstraction on a user-authored workflow
before any unification (P2+).

A **Workflow** is a TOML graph. Nodes are **Step**s — `model:<class>` (a
daemon-routed completion; the slot is an OICP latency class — `fast` / `normal` /
`extended`, with `thoughtful`/`slow` as aliases for `extended` — so the step
builds a protocol-native request, not a legacy `Speed` shim), `embed:` (a
daemon-routed embedding), `mcp:` / `tool:` (a tool call), or `transform:` (a
deterministic function). Each `uses` string is parsed once into a typed
`StepKind` (ARCH §2.1), so dispatch and "does this need the daemon?" are
compiler-checked, never re-grepped. Edges are **auto-derived** from `{step.key}`
references, so you never write an edge list.
The single-process **Runner** topologically orders the steps and runs them per
source item, threading **Artifact**s between them. The crate is *core-only* —
inference + tools are injected by the caller.

A `model:` step may declare **`structured_output`** (a JSON schema, as a TOML
table) or **`grammar`** (a lark grammar) — the daemon constrains the model's
output, and a structured step returns a parsed **`Json` artifact** (not a string)
so downstream steps compose on the structure. This is the general primitive that
lets an extraction (e.g. enrichment atoms) be authored as data — its output
*shape* in the workflow, not parsed in Rust. See `examples/extract-atoms.toml`.

A step may **`for_each`** another step's output. When that output is a JSON-array
*collection* — e.g. a chunker's `1→N` chunks — the step runs once per element
(read via `{element.key}` for an object field, `{element.value}` for a scalar),
and its own output is the array of the per-element results. Each element resolves and **caches independently**, so
editing one chunk re-runs only that chunk's downstream work. The `Artifact` never
had to change for this: a collection *is* a JSON array. Only the Runner grew the
map.

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
sovereign workflow run studio/crates/sovereign-workflow/examples/notes-digest.toml
```

One file mixes an ecosystem-shaped MCP tool, a local model, and data-authored
steps. Swap `mcp:demo:read_memo` → `mcp:whisper:transcribe_audio` and `*.md` →
`*.m4a` and it's audio transcription — no code change.

## Cache + resume (on by default)

Each `Read` step's output is content-addressed by its resolved inputs (incl. the
source file's mtime+size), persisted under `~/.svrnmesh/workflow-cache`. So a
re-run skips unchanged work; editing one file re-runs only that item; a
`Write`-effect step (e.g. `write_note`) is never cached. `--no-cache` forces a
full run; a per-step `cache = false` opts a volatile read out.

```
sovereign workflow run notes-digest.toml          # read·digest ×10   ~42s
sovereign workflow run notes-digest.toml          # 20 cached, 0 ran  ~0.3s
touch notes/standup.md && sovereign workflow run notes-digest.toml   # 1 item re-runs
```

## Generalization — diffed against the real pipeline

The substrate's claim is that the system's bespoke pipelines are *instances* of
one model. We test that directly: corpus ingest's `chunk → embed` stage,
re-expressed as a two-step Workflow (`tool:chunk` `for_each` → `embed:`), is
diffed byte-for-byte against the **real** `chunk_text` + embed run as an oracle
(`workflow_cmd::tests::chunk_then_embed_matches_the_real_corpus_pipeline`). It
matches — same chunks, same vectors, same order — and a second run is fully
served from the cache. The finding: the `Artifact` was *already* general enough
(JSON arrays are collections); the only gap was the **Runner**, which grew
`for_each`. A fifth previously-hand-rolled pipeline now falls out of the substrate
unchanged.

## What's P2+ (not here)

Durable/distributed execution (the pipeline tool as an outer loop), the
inference-resource scheduler (`BackendSelector`), and corpus/enrichment/executor
convergence. See the spec.
