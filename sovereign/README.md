# Sovereign

Your own AI assistant that runs entirely on your computer. No cloud, no subscriptions, no data leaving your machine.

Sovereign loads open-source language models directly on your hardware and gives you a capable assistant with local knowledge bases, web search, document knowledge, multi-step task execution, and long-term memory — all without an internet connection to an AI provider.

## Why Sovereign?

- **Private by default.** Your conversations, documents, and memories never leave your computer.
- **Knowledge bases, not just chat.** Wikipedia, Stanford Encyclopedia of Philosophy, scholarly abstracts — indexed locally, searched before every answer, cited in responses.
- **No API keys required.** Runs open-source models locally via llama.cpp. Web search is an optional supplement, not a dependency.
- **Works offline.** Once you have a model and knowledge bases downloaded, everything works without internet.
- **Extensible with skills.** TOML-based skill manifests shape routing, planning, synthesis, and memory behavior. No code required.

## Quick start for coding (three commands)

One binary, three commands, a working local AI coding stack from zero:

```sh
sovereign setup         # once — detects hardware, downloads models, starts the daemon
sovereign project init  # per project — indexes the codebase, registers MCP tools
sovereign mesh create   # optional — promotes your local mesh to a joinable one
```

`localhost:9741` serves **both** the OpenAI-compatible completions endpoint (`/v1/chat/completions`, `/v1/models`) **and** the MCP tool server (`/mcp`). Point opencode, Claude Code, or any OpenAI-compatible client at it and everything just works.

### `sovereign setup`

First-run onboarding. Detects your hardware, curates a list of primary models that fit, downloads all three slots (primary / fast / embed) in parallel, writes `~/.config/sovereign/config.toml`, and registers the daemon with launchd (macOS) or systemd (Linux) so it survives logout.

```sh
sovereign setup              # interactive — pick your primary model
sovereign setup --yes        # non-interactive — accept recommended
sovereign setup --reset      # wipe config and re-run
sovereign setup --data-dir /path/to/override  # override ~/.sovereign
```

When it finishes you'll see:

```
✓ Models ready
    ✓ Wrote ~/.config/sovereign/config.toml
    ✓ Service registered
  Waiting for daemon to come up... ready

  ✓ Mesh running — 1 node (you)
  ✓ Endpoint: localhost:9741/v1
```

Your daemon is now running under launchd/systemd. Check it anytime with `curl http://localhost:9741/v1/models`.

### `sovereign project init`

Per-project code intelligence: tree-sitter symbol index + SCIP call graph + generated `SOVEREIGN.md`. Auto-detects which AI coding assistants you have installed (Claude Code, opencode) and offers to write their configs. See the [Code Intelligence](#code-intelligence) section below for details.

### `sovereign mesh create` / `join` / `rotate`

Share compute with trusted friends. `setup` leaves you on a silent single-node mesh; `mesh create` promotes it and prints a shareable invite:

```sh
$ sovereign mesh create

Mesh created.

  Join key:  cwth-a1b2-c3d4-e5f6

Share with a friend:
  App:  https://sovereign.dev/join/cwth-a1b2-c3d4-e5f6
  CLI:  sovereign mesh join cwth-a1b2-c3d4-e5f6
```

Your friend runs any of:

```sh
sovereign mesh join cwth-a1b2-c3d4-e5f6                          # bare key
sovereign mesh join https://sovereign.dev/join/cwth-a1b2-c3d4-e5f6   # https url
sovereign mesh join sovereign://join/cwth-a1b2-c3d4-e5f6         # deep link
```

Lost the key? The plaintext is never stored (only a BLAKE3 hash lives on disk). Run `sovereign mesh rotate` to generate a new one — existing members stay connected, only future joins need the new key.

### `sovereign daemon`

The daemon you never call directly. `sovereign setup` registers it with your service manager; launchd (macOS) or systemd (Linux) keeps it alive across logout. If you need to run it manually (e.g. to debug startup), `sovereign daemon run` blocks in the foreground.

Logs: `~/.sovereign/logs/daemon.log` (macOS) or `journalctl --user -u sovereign` (Linux).

---

## Getting Started

### Requirements

1. **8 GB RAM minimum.** 16 GB is comfortable; 32 GB lets you run the best open models.
2. **A `.gguf` model file.** Quantized open-source models from Hugging Face.
3. **Rust toolchain** and **CMake** for building from source.

### Install build tools

**macOS:**
```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
brew install cmake
```

**Linux (Ubuntu/Debian):**
```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt install cmake build-essential
```

### Download a model

| Your RAM | Model | Size | Command |
|---|---|---|---|
| 8 GB | Qwen2.5 0.5B Q8 | ~600 MB | `huggingface-cli download Qwen/Qwen2.5-0.5B-Instruct-GGUF qwen2.5-0.5b-instruct-q8_0.gguf --local-dir models` |
| 16 GB | Qwen2.5 3B Q4 | ~2 GB | `huggingface-cli download Qwen/Qwen2.5-3B-Instruct-GGUF qwen2.5-3b-instruct-q4_k_m.gguf --local-dir models` |
| 32 GB+ | Qwen2.5 7B Q5 | ~5 GB | `huggingface-cli download Qwen/Qwen2.5-7B-Instruct-GGUF qwen2.5-7b-instruct-q5_k_m.gguf --local-dir models` |

Install `huggingface-cli` with `pip install huggingface-hub`, or download `.gguf` files manually from [Hugging Face](https://huggingface.co).

### Clone and build

```sh
git clone https://github.com/anthropics/sovereign.git
cd sovereign
cargo build --release
```

### Run

**Desktop app** (recommended):
```sh
cargo install tauri-cli --version "^2"
cd crates/sovereign-desktop
npm install
cargo tauri dev
```

The setup wizard guides you through persona selection, model setup, and knowledge base installation.

**CLI:**
```sh
cargo run --release -p sovereign-cli -- --model models/your-model.gguf
```

**HTTP server:**
```sh
cargo run --release -p sovereign-server -- --config sovereign-server.toml
```

---

## Three Interfaces

### Desktop App

A native app with chat, conversation history, knowledge base management, and a setup wizard. On first launch, choose a persona:

- **Research & Analysis** — Activates the research-analyst skill. Searches local knowledge bases first, supplements with web when needed, cites findings.
- **Personal Assistant** — General-purpose helper for tasks, planning, and organization.
- **Developer** — Full control over model selection, inference settings, and search backends.

The setup wizard includes knowledge base tier selection (Essential through Full) and optional web search API key configuration. Knowledge bases download and index in the background — you can start chatting immediately.

Settings shows installed knowledge bases with real-time progress, per-corpus Install/Remove controls, and a tier quick-install for users who skipped onboarding.

### CLI

Interactive terminal REPL with all the same capabilities.

```sh
cargo run --release -p sovereign-cli -- --model models/fast.gguf --router
```

| Flag | Default | Description |
|---|---|---|
| `--model <path>` | *required* | Fast/default GGUF model |
| `--primary-model <path>` | same as --model | Larger model for deep reasoning |
| `--data-dir <path>` | `data` | Database and downloads directory |
| `--skills-dir <path>` | `~/.sovereign/skills` | User skills directory |
| `--router` | off | Enable LLM-based intent routing |
| `--ingest <path>` | — | Ingest documents from a directory |
| `--brave-api-key <key>` | — | Use Brave Search |
| `--tavily-api-key <key>` | — | Use Tavily Search |

Without `--router`, every message gets a direct response. With it, Sovereign classifies intent — simple questions use the fast model, complex requests trigger multi-step planning.

**Subcommands** (no model required):

| Command | Description |
|---|---|
| `sovereign project init` | Set up code intelligence for the current repo (see [Code Intelligence](#code-intelligence)) |
| `sovereign project serve` | Start a lightweight code-intelligence MCP server (no model required) |
| `sovereign project status` | Check health of index, call graph, MCP server |
| `sovereign project refresh` | Re-export the SCIP call graph |
| `sovereign code index <path>` | Index a repository with tree-sitter |
| `sovereign code watch <corpus>` | Filesystem watcher for incremental re-indexing |
| `sovereign code mcp-status` | Ping the MCP server and list tools |
| `sovereign mcp list` | List configured MCP servers |
| `sovereign recipe test <path>` | Validate a corpus recipe |

### HTTP Server

REST + WebSocket API for custom frontends and integrations.

```sh
cargo run --release -p sovereign-server -- --config sovereign-server.toml
```

| Method | Path | Description |
|---|---|---|
| POST | `/v1/conversations` | Create a conversation |
| POST | `/v1/conversations/{id}/messages` | Send a message |
| GET | `/v1/conversations/{id}` | Get conversation with history |
| GET | `/v1/conversations` | List conversations |
| DELETE | `/v1/conversations/{id}` | Delete a conversation |
| POST | `/v1/tasks/{id}/approve` | Approve a tool action |
| GET | `/v1/tools` | List available tools |
| POST | `/v1/search` | Search across messages |
| GET | `/v1/conversations/{id}/stream` | WebSocket for streaming |

---

## Knowledge Bases

Sovereign indexes curated reference sources locally. Every query searches these knowledge bases before generating a response — the model answers from verified sources, not hallucination. Web search supplements for current events and gaps.

### Available Corpora

| Corpus | Description | Size | License |
|---|---|---|---|
| **Wikipedia** | 6.8M English articles | 55 GB indexed | CC BY-SA 4.0 |
| **Stanford Encyclopedia of Philosophy** | Peer-reviewed philosophy articles (via HuggingFace dataset) | 0.5 GB indexed | CC BY-NC-ND 4.0 |
| **OpenAlex** | 250M+ scholarly abstracts with citations | 45 GB indexed | CC0 |
| **Stack Exchange** | Expert Q&A across 170+ communities (score ≥ 3) | 40 GB indexed | CC BY-SA 4.0 |
| **Project Gutenberg** | 70,000+ public domain books | 25 GB indexed | Public Domain |
| **CRS Reports** | US Congressional policy analysis | 4 GB indexed | Public Domain |

### Tiers

- **Essential** (55 GB) — Wikipedia only. Broad general knowledge.
- **Research** (105 GB) — Wikipedia + SEP + OpenAlex + CRS. Academic and policy research.
- **Technical** (95 GB) — Wikipedia + Stack Exchange. Programming and engineering.
- **Full** (170 GB) — All corpora.

### How It Works

Knowledge bases are defined in `data/corpora.toml`. The corpus manager downloads source files (Parquet, XML, JSONL), parses them with streaming parsers (never loading full corpora into memory), and indexes chunks via SQLite FTS5 full-text search.

Every query — regardless of how the router classifies it — searches the local knowledge base. Results are injected as context before the model generates a response. Provenance metadata records which corpora were consulted and how many chunks matched.

### Coverage-Aware Search Pipeline

The unified search tool (`search`) replaces separate knowledge and web search tools:

1. **Local search** — FTS5 text search across all indexed corpora
2. **Coverage assessment** — Heuristic + optional LLM evaluation of result quality
3. **Web fallback** — If local results are insufficient and web search is configured
4. **Synthesis** — Cited answer with source attribution and provenance

Budget tracking gates web search usage. The system is designed to work fully offline with local knowledge bases as the primary source.

---

## Code Intelligence

`sovereign project init` sets up the full code intelligence stack for a repository: tree-sitter symbol index, SCIP call graph, Claude Code integration, filesystem watcher hooks, and a generated `SOVEREIGN.md` with tool guidance and hybrid strategy. One command, no manual steps.

After init, Claude Code automatically uses a hybrid approach: Grep/Glob/Read for discovering what exists in a module, `symbol_lookup` for precise type definitions, `find_callers`/`find_callees` for compiler-resolved impact analysis, and `recent_changes` for session orientation. The strategy is documented in the generated `.sovereign/SOVEREIGN.md` and wired into `.claude/settings.json` so it works without any manual configuration.

### Global install

If `sovereign-cli` is on your PATH (e.g. via `cargo install`):

```sh
cd /path/to/your-project
sovereign project init
```

### From source (developer workflow)

If you've just cloned the workspace and want to build from source:

```sh
cd sovereign
cargo build --release -p sovereign-cli
```

Then run init from any project root:

```sh
./target/release/sovereign-cli project init
```

Or with `cargo run`:

```sh
cargo run --release -p sovereign-cli -- project init
```

### What it creates

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

### AI assistant configuration

During `init`, sovereign prompts for which AI coding assistant configs to generate:

```
Set up AI assistant configs: [A]ll / [C]laude Code / [O]pencode / [S]kip (default: A):
```

- **All** (default) — writes both `.claude/settings.json` and `.opencode/config.json`
- **Claude Code** — only `.claude/` config (the `--no-claude-config` flag still overrides)
- **Opencode** — only `.opencode/config.json` and `AGENTS.md`
- **Skip** — writes neither (useful when you want to configure assistants separately)

In non-interactive environments (CI, pipes) the prompt is skipped and all configs are generated.

#### Commonwealth mesh inference for opencode

When opencode is selected, `init` prompts for a Commonwealth URL:

```
Commonwealth inference URL (e.g. http://localhost:9741, blank to skip):
```

Alternatively, if `.sovereign/sovereign.toml` already contains a `[commonwealth]` section, `init` picks it up automatically without prompting:

```toml
# .sovereign/sovereign.toml
[commonwealth]
url = "http://localhost:9741"
```

When this is set, `init` calls `GET /oicp/v1/capabilities` on the Commonwealth daemon to enumerate available models. The generated `.opencode/config.json` looks like:

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

If Commonwealth is not reachable at init time, a single `"auto"` model entry is written as a placeholder — the mesh routes it at runtime. Re-run `sovereign project init` after starting the daemon to populate real model IDs.

Select the provider in opencode with `commonwealth/<model-id>` (e.g. `commonwealth/Qwen3-9B` or `commonwealth/auto`).

### Ongoing commands

```sh
# Check that everything is healthy
sovereign project status

# Re-export the SCIP call graph (happens automatically on commit if hooks are installed)
sovereign project refresh
```

### Flags

| Flag | Default | Description |
|---|---|---|
| `--name <id>` | directory name | Corpus identifier |
| `--port <port>` | `8080` | MCP server port written to settings |
| `--data-dir <dir>` | `~/.sovereign/indexes` | Where the symbol index is stored |
| `--no-scip` | off | Skip call graph export (if no SCIP exporter is installed) |
| `--no-hooks` | off | Skip git hook installation |
| `--no-claude-config` | off | Skip writing `.claude/settings.json` (overrides harness prompt) |

### SCIP exporters

The call graph (`find_callers`, `find_callees`) requires a language-specific SCIP exporter on PATH. Without one, `init` completes successfully but call graph tools return a `LanguageNotIndexed` caution instead of results.

| Language | Exporter | Install |
|---|---|---|
| Rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| Go | `scip-go` | `go install github.com/sourcegraph/scip-go@latest` |
| TypeScript | `scip-typescript` | `npm install -g @sourcegraph/scip-typescript` |
| Python | `scip-python` | `pip install scip-python` |
| Java | `scip-java` | [sourcegraph.github.io/scip-java](https://sourcegraph.github.io/scip-java/) |

### Multi-project ecosystems

This repository is part of a five-project ecosystem (`sovereign`, `corpus-engine`, `commonwealth`, `oicp-types`, `sovereign-recipes`), each with its own git repo. All five should be indexed so that `symbol_lookup` and `code_search` work across the entire codebase.

**How it works:** Every project gets its own corpus ID (defaulting to the directory name). All indexes live under the shared `~/.sovereign/indexes/` directory. The tools `symbol_lookup`, `code_search`, and `recent_changes` automatically query every installed index — so `symbol_lookup("InferenceProvider")` finds the Rust definition in `sovereign` and `symbol_lookup("EmbedFn")` finds it in `corpus-engine`, regardless of which project you're working in.

The call graph (`find_callers`, `find_callees`) is per-project — cross-project call edges aren't tracked. This matches reality: `corpus-engine` has no dependency on `sovereign`, so there are no cross-project call edges for the compiler to resolve.

**Index all five projects:**

```sh
# Build the CLI once
cd sovereign
cargo build --release -p sovereign-cli
SOVEREIGN=$PWD/target/release/sovereign-cli

# Init each project (run from the ecosystem root)
cd ..
for project in sovereign corpus-engine commonwealth oicp-types sovereign-recipes; do
  echo "=== $project ==="
  (cd "$project" && $SOVEREIGN project init)
done
```

With a global install this is simpler:

```sh
for project in sovereign corpus-engine commonwealth oicp-types sovereign-recipes; do
  (cd "$project" && sovereign project init)
done
```

After this, working in any one project gives you full symbol coverage of the entire ecosystem. Each project gets its own `.sovereign/SOVEREIGN.md` (for project-specific invariants), `.claude/settings.json`, and post-commit hook.

**Start the MCP server:**

```sh
sovereign project serve
```

This starts a lightweight, model-free MCP server that serves all five projects. It discovers every index under `~/.sovereign/indexes/`, merges their SCIP call graphs into a single in-memory view, and exposes all five tools (`symbol_lookup`, `code_search`, `recent_changes`, `find_callers`, `find_callees`) over JSON-RPC at `http://localhost:9741/mcp`.

No GGUF model, no config file, no auth. Localhost only.

From source:

```sh
$SOVEREIGN project serve
# or
cargo run --release -p sovereign-cli -- project serve --port 8080
```

The server prints what it found on startup:

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

Each project's `.claude/settings.json` (written by `init`) already points to this server — open Claude Code in any project directory and the tools are available immediately.

**Full workflow — init, serve, verify:**

```sh
# 1. Build
cd sovereign
cargo build --release -p sovereign-cli
SOVEREIGN=$PWD/target/release/sovereign-cli

# 2. Index all projects
cd ..
for project in sovereign corpus-engine commonwealth oicp-types sovereign-recipes; do
  (cd "$project" && $SOVEREIGN project init --no-scip)  # fast pass, symbols only
done

# 3. SCIP export (slower, requires rust-analyzer — do separately)
for project in sovereign corpus-engine commonwealth; do
  (cd "$project" && $SOVEREIGN project refresh)
done

# 4. Start the MCP server (runs in foreground)
$SOVEREIGN project serve
```

**Verify everything is indexed:**

```sh
for project in sovereign corpus-engine commonwealth oicp-types sovereign-recipes; do
  echo "=== $project ==="
  (cd "$project" && sovereign project status)
done
```

**Re-init a single project** after major structural changes (new crates, renamed modules):

```sh
cd corpus-engine
sovereign project init
```

This is safe to run repeatedly — existing settings are merged, hooks aren't duplicated, and the index is rebuilt from scratch. Restart `sovereign project serve` afterward to pick up the new data.

---

## Features

### Dual-Model Routing

A small, fast model handles simple questions in under a second. Complex requests route to a larger primary model that loads on demand and auto-unloads after 60 seconds of idle.

### Multi-Step Task Execution

Complex requests decompose into step DAGs. The executor runs steps in parallel where possible, handles branching, retries tool failures with backoff, and replans if a step fails.

Steps can be configured with:
- **Best-of-N sampling** — Generate multiple candidates and select the best via LLM judge, majority vote, or tool verification
- **Evaluation passes** — Closed-loop self-correction that checks output quality and retries with feedback
- **Adaptive compute** — Difficulty estimation adjusts token budgets, sampling, and evaluation per step

### Tools

| Tool | Description |
|---|---|
| **Search** | Searches local knowledge bases + optional web. FTS5 + coverage assessment. |
| **Web Fetch** | Downloads and extracts content from web pages |
| **Shell** | Runs commands on your machine (requires approval) |
| **Document** | Ingests and summarizes local documents (RAG) |

Tools retry on transient failures (timeout, rate limit) with configurable backoff.

### Memory

Working memory is compressed every message. Long-term memories are extracted when conversations end and retrieved via full-text search in future conversations. Skills can configure per-skill decay rates and prune thresholds.

### Response Provenance

Every response carries structured metadata about how it was produced:
- Which intent the router classified
- What knowledge bases were searched and how many chunks matched
- Which inference backend generated the response
- OICP capability match quality
- Token count and latency

The desktop app shows this as a collapsible provenance bar on each message. This helps users understand why an answer might be incomplete ("search found 0 chunks in SEP") and report problems effectively.

### Skills

Skills are TOML files that configure routing, planning, synthesis prompts, memory rules, and inference requirements.

**Bundled skills** (in `skills/`):

| Skill | Description |
|---|---|
| **Research & Analysis** | Multi-source research with citations. Knowledge-first planning. 5%/month memory decay. |
| **Code Review** | Structured code analysis. Privacy: local-only. |
| **Personal Assistant** | Task management and organization. |
| **Inner Work** | Reflective companion for personal psychological work. Always local-only. |

**Writing a skill:** Create a directory under `skills/` with a `skill.toml`. See the bundled skills for the format — routing triggers, planner templates (with optional `[sample:N:method]` and `[eval:name]` annotations), synthesis prompts, memory rules, and OICP inference requirements are all configurable.

Skills carry trust metadata: `signature` and `signed_by` fields enable distinguishing community-reviewed, author-signed, and unsigned skills.

### OICP (Open Inference Capabilities Protocol)

Skills declare capability requirements (code analysis proficiency, minimum context window, privacy constraints). When multiple backends are available, Sovereign routes to the best match. The inner-work skill declares `privacy = "local_only"` — its data never leaves your machine even if remote backends are configured.

See `docs/specs/oicp.md` for the full protocol specification.

---

## Architecture

Eight crates with five trait boundaries. Every component is swappable.

```
sovereign-core          Runtime, Executor, Planner, Router, Memory, Skills, Types, Traits
sovereign-inference     llama.cpp FFI, dual-slot model management, backend selection, health tracking
sovereign-store         SQLite + PostgreSQL StateStore, FTS5 search, soft deletes, sync-ready schema
sovereign-tools         Search pipeline, corpus parsers (Parquet, XML, JSONL, HTML), web, shell, RAG
sovereign-mesh          In-process Commonwealth daemon embed (sovereign:// deep links)
sovereign-cli           Interactive terminal REPL
sovereign-server        Axum REST API + WebSocket
sovereign-desktop       Tauri 2 + Svelte 5 native desktop app
```

OICP types are defined in the shared `oicp-types` crate at the workspace root and re-exported through `sovereign_core::oicp`.

### Data Flow

```
User message
  → Router (fast model): classifies intent
     → SimpleQuery / DeepQuery  → search local knowledge → synthesize with context
     → ComplexTask              → Planner generates step DAG
                                → Executor runs steps (parallel batches)
                                → Tool steps with retry + approval
                                → Adaptive compute adjusts per-step budget
                                → Best-of-N sampling on critical steps
                                → Evaluation passes with self-correction
                                → Synthesize final answer from step outputs
  → Provenance recorded in Message.metadata
  → Memory extraction on conversation end
```

### Sync Readiness

All database records carry a `version` (Lamport timestamp) and soft-deletable tables have a `deleted_at` field. Writes are append-only with soft deletes. This enables future multi-device sync without schema migration — two StateStore instances can merge by taking the union of records and resolving by timestamp.

### Corpus Integrity

Skills and corpus definitions carry optional `signature` and `signed_by` fields. The system distinguishes three trust levels: community-reviewed, author-signed, and unsigned. Corpus definitions include a `mesh_sharing` flag for license-aware index transfer control (e.g., SEP's CC-BY-NC-ND license sets `mesh_sharing = false`).

---

## Development

### Building

```sh
cargo build --release              # All crates
cargo build -p sovereign-cli       # CLI only
cargo build -p sovereign-server    # Server only
```

Desktop app requires [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) plus Node.js:

```sh
cd crates/sovereign-desktop
npm install
cargo tauri dev
```

### Testing

```sh
cargo test --workspace                              # All 309 tests
cargo test -p sovereign-core --test functional      # 18 functional tests (provenance, FTS5, knowledge bases)
cargo test -p sovereign-tools --test smoke_tests    # 7 smoke tests (Parquet ingestion, full pipeline)
```

The test suite includes three layers:

- **Unit tests** — Mock-based, fast. Cover types, serialization, registry logic, plan parsing.
- **Functional tests** — `DeterministicInference` + real in-memory SQLite + real FTS5. No mocks on the store or search pipeline. Assert on provenance records, conversation state, corpus search results.
- **Smoke tests** — Real Parquet parsing → real SQLite → real Runtime → provenance assertions. End-to-end: ingest philosophy corpus, query about Bergson, verify provenance shows SEP chunks found.

No tests require a GPU, model file, or network access.

### Project Structure

```
sovereign/
├── crates/
│   ├── sovereign-core/        # Traits, Runtime, Executor, Planner, Router, Memory, Skills
│   ├── sovereign-inference/   # llama.cpp, remote APIs, hybrid provider, backend selection
│   ├── sovereign-store/       # SQLite, PostgreSQL, in-memory StateStore
│   ├── sovereign-tools/       # Search, corpus parsers, web, shell, RAG, MCP
│   ├── sovereign-cli/         # Terminal REPL
│   ├── sovereign-server/      # REST + WebSocket API
│   └── sovereign-desktop/     # Tauri + Svelte desktop app
├── data/
│   └── corpora.toml           # Knowledge base manifest (compiled into desktop app)
├── skills/
│   ├── research-analyst/      # Multi-source research with citations
│   ├── code-review/           # Structured code analysis
│   ├── personal-assistant/    # Task management
│   └── inner-work/            # Reflective companion (local-only)
├── docs/
│   └── specs/oicp.md          # OICP protocol specification
├── models/                    # GGUF model files (not committed)
└── sovereign-server.toml      # Example server configuration
```

### Key Traits

The system is built around five async trait boundaries (in `sovereign-core/src/traits.rs`):

| Trait | Purpose |
|---|---|
| `InferenceProvider` | `complete()`, `embed()`, `complete_stream()` — model inference |
| `Router` | `classify()` — intent classification |
| `Planner` | `plan()`, `replan()` — step DAG generation |
| `Tool` | `execute()`, `validate()`, `retry_config()` — tool execution |
| `StateStore` | 25+ methods for conversations, memories, documents, corpus state, permissions |

### Adding a Tool

Implement the `Tool` trait:

```rust
#[async_trait]
impl Tool for MyTool {
    fn descriptor(&self) -> ToolDescriptor { /* id, name, description, JSON schema */ }
    fn required_permissions(&self) -> Vec<Permission> { vec![Permission::Network] }
    async fn execute(&self, params: &Value, ctx: &ToolContext) -> Result<StepOutput> { /* ... */ }

    // Optional: retry on transient failures
    fn retry_config(&self) -> Option<RetryConfig> {
        Some(RetryConfig { max_retries: 2, backoff_ms: vec![1000, 3000] })
    }
}
```

Register it in the CLI, server, or desktop `main.rs`:
```rust
tools.register(Box::new(MyTool::new()));
```

### Adding a Corpus

1. Add a parser implementing `CorpusParser` trait in `sovereign-tools/src/corpus/`
2. Register it in `registry.rs` `parser_for_corpus()`
3. Add the corpus definition to `data/corpora.toml`
4. Supported formats: Parquet, MediaWiki XML (bzip2), Stack Exchange XML, JSONL (gzip), HTML directory, plain text directory

### Adding a Skill

Create `skills/my-skill/skill.toml`:

```toml
[skill]
id = "my-skill"
name = "My Skill"
version = "0.1.0"
description = "What this skill does"

[routing]
trigger_phrases = ["relevant", "trigger", "phrases"]
default_intent = "ComplexTask"
min_confidence = 0.75

[[planner.templates]]
name = "my_template"
trigger = "When the user wants X"
steps = """
1. Search for information. [no_eval]
2. Analyze findings. [sample:3:llm_judge]
3. Synthesize answer. [eval:synthesis, max_retries:1]
"""

[tools]
required = ["search"]

[prompts]
synthesis = "You are a specialist in X. Cite sources."

[memory]
extract_prompt_addendum = "Extract facts about X that the user cares about."
confidence_decay_per_month = 0.05
prune_threshold = 0.1

[inference]
privacy = "LocalOnly"
min_context_tokens = 8192
```

## License

MIT
