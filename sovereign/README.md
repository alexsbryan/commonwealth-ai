# Sovereign

A self-hosted AI agent that runs entirely on your machine. One binary, no external services, no cloud dependency.

## What it does

- **Local LLM inference** via llama.cpp — no API keys, no data leaving your machine
- **Dual-slot model routing** — a fast small model handles simple questions instantly; a larger model loads on demand for deep reasoning, then unloads after 60s idle to free VRAM
- **Plan-and-execute** — complex requests are decomposed into multi-step DAGs by the Planner, then executed by the Executor with parallel step batching, branch logic, and automatic replanning on failure
- **Persistent conversations** — SQLite with FTS5 full-text search; conversations survive restarts
- **Trait-based architecture** — five boundaries (`InferenceProvider`, `Router`, `Planner`, `Tool`, `StateStore`) make every component swappable

Planned but not yet built: RAG, web search, tool use (email, calendar, shell, MCP), memory system, desktop app (Tauri + Svelte), HTTP/WebSocket API server.

## Quick start

### Prerequisites

- Rust toolchain (`rustup`)
- CMake (`brew install cmake` on macOS)

### Download a model

```sh
pip install huggingface-hub
huggingface-cli download Qwen/Qwen2.5-0.5B-Instruct-GGUF qwen2.5-0.5b-instruct-q8_0.gguf --local-dir models
```

### Chat

```sh
cargo run -p sovereign-cli -- --model models/qwen2.5-0.5b-instruct-q8_0.gguf
```

This starts an interactive REPL with persistent conversations (stored in `data/sovereign.db`).

### With intent routing

```sh
cargo run -p sovereign-cli -- \
  --model models/qwen2.5-0.5b-instruct-q8_0.gguf \
  --router
```

The `--router` flag enables LLM-based two-pass intent classification. Simple questions stay on the fast model; complex requests trigger the Planner, which generates a multi-step execution plan.

### With separate fast and primary models

```sh
cargo run -p sovereign-cli -- \
  --model models/small-model.gguf \
  --primary-model models/large-model.gguf \
  --router
```

The fast model handles routing and simple queries. The primary model loads on demand for planning and deep reasoning, and auto-unloads after 60 seconds of inactivity.

### CLI options

| Flag | Default | Description |
|---|---|---|
| `--model <path>` | required | Fast/default GGUF model |
| `--primary-model <path>` | same as --model | Larger model for deep reasoning |
| `--data-dir <path>` | `data` | SQLite database directory |
| `--router` | off | Enable LLM-based intent classification |

### Raw inference (no runtime)

```sh
cargo run --example complete -p sovereign-inference -- \
  --model models/qwen2.5-0.5b-instruct-q8_0.gguf \
  --prompt "Explain recursion in three sentences." \
  --stream --max-tokens 256
```

## How it works

```
User message
  → Router (fast model, <200ms): classifies intent
    → SimpleQuery  → fast model responds directly
    → DeepQuery    → primary model responds
    → ComplexTask  → Planner generates step DAG
                   → Executor walks DAG in topological batches
                   → Each Reason step calls inference
                   → Branch steps evaluate conditions, skip non-taken paths
                   → On failure: replan once, then surface error
                   → Synthesize final answer from all step outputs
```

Conversations, tasks, and plan state persist in SQLite. The Executor saves progress after each batch — if the process crashes mid-task, the state is recoverable.

## Architecture

Five trait boundaries define the system's extension points. See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design.

```
sovereign-core          Traits, Runtime, Executor, Planner, Router
sovereign-inference     llama.cpp FFI, dual-slot model management
sovereign-store         SQLite (+ in-memory) StateStore
sovereign-tools         Built-in tools, MCP adapter (planned)
sovereign-cli           Interactive REPL
sovereign-server        Axum HTTP/WebSocket API (planned)
sovereign-desktop       Tauri + Svelte desktop app (planned)
```

## Tests

```sh
cargo test --workspace  # 97 tests
```

Covers serialization roundtrips, both StateStore implementations, intent parsing, plan generation/validation, executor step dispatch with branching and skip propagation, and full Runtime integration with mock inference.
