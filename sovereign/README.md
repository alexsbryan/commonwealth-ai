# Sovereign

A self-hosted AI agent that runs entirely on your machine. One binary, no external services, no cloud dependency.

Sovereign combines local LLM inference, intelligent routing across model sizes, multi-step task planning and execution, RAG over your documents, web search, and tool use (email, calendar, shell, MCP) — all behind a single chat interface.

## What it does

- **Chat with a local LLM** — no API keys, no data leaving your machine
- **Automatic model routing** — simple questions use a fast small model, complex ones load a larger model on demand
- **Multi-step task execution** — plans and executes multi-tool workflows with approval gates for anything destructive
- **RAG** — point it at a folder of documents, ask questions grounded in their content
- **Web search** — multi-stage pipeline (query decomposition → search → content extraction → cited synthesis), works free with DuckDuckGo or better with a Brave/Tavily API key
- **Tool use** — email, calendar, files, shell, and any MCP server
- **Memory** — remembers facts about you across conversations

Ships as a desktop app (Tauri + Svelte) and an HTTP/WebSocket API server for programmatic use.

## Current status

Early development. Phases 0–1 of the [implementation plan](IMPLEMENTATION_PLAN.md) are complete:

- Cargo workspace with trait-based architecture (`sovereign-core`, `sovereign-inference`, `sovereign-store`, `sovereign-tools`, `sovereign-server`, `sovereign-desktop`)
- Single-slot local inference via llama.cpp (load a GGUF model, complete prompts, stream tokens)

## Quick start

### Prerequisites

- Rust toolchain (`rustup`)
- CMake (`brew install cmake` on macOS)
- A GGUF model file (any llama.cpp-compatible model)

### Download a model

```sh
pip install huggingface-hub  # if you don't have it
huggingface-cli download Qwen/Qwen2.5-0.5B-Instruct-GGUF qwen2.5-0.5b-instruct-q8_0.gguf --local-dir models
```

### Run inference

```sh
# Basic completion
cargo run --example complete -p sovereign-inference -- \
  --model models/qwen2.5-0.5b-instruct-q8_0.gguf \
  --prompt "What is the capital of France?"

# Streaming
cargo run --example complete -p sovereign-inference -- \
  --model models/qwen2.5-0.5b-instruct-q8_0.gguf \
  --prompt "Explain recursion in three sentences." \
  --stream --max-tokens 256 --temperature 0.7
```

### Options

| Flag | Default | Description |
|---|---|---|
| `--model <path>` | required | Path to a GGUF model file |
| `--prompt <text>` | required | Prompt text |
| `--stream` | off | Stream tokens as they generate |
| `--max-tokens <N>` | 256 | Max tokens to generate |
| `--temperature <T>` | 0.7 | Sampling temperature (0 = deterministic) |

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design. The short version:

Five trait boundaries (`InferenceProvider`, `Router`, `Planner`, `Tool`, `StateStore`) define the system's extension points. A `Runtime` struct assembles trait objects and dispatches messages. Any component can be swapped without touching the others.

```
sovereign-core          Traits, Runtime, Executor (no deps on UI/HTTP)
sovereign-inference     llama.cpp FFI, model slot management
sovereign-store         SQLite/Postgres persistence
sovereign-tools         Built-in tools, MCP adapter, RAG pipeline
sovereign-server        Axum HTTP/WebSocket API
sovereign-desktop       Tauri + Svelte desktop app
```
