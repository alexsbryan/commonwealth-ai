# Sovereign

Your own AI assistant that runs entirely on your computer. No cloud, no subscriptions, no data leaving your machine.

Sovereign loads open-source language models directly on your hardware and gives you a capable assistant with web search, document knowledge, multi-step task execution, and long-term memory — all without an internet connection to an AI provider.

## Why Sovereign?

- **Private by default.** Your conversations, documents, and memories never leave your computer.
- **No API keys required.** Sovereign runs open-source models locally via llama.cpp.
- **Works offline.** Once you have a model downloaded, everything works without internet (web search excluded, obviously).
- **Extensible with skills.** Bundled skills shape Sovereign into a research analyst, personal assistant, or reflective companion. Write your own in a simple TOML file.

## Getting Started

### What you'll need

1. **A computer with at least 8 GB of RAM.** More RAM = larger models = smarter responses. 16 GB is comfortable; 32 GB lets you run the best open models.
2. **A language model file.** These are `.gguf` files — quantized versions of open-source models. We'll show you how to download one below.
3. **Rust toolchain** and **CMake** for building from source (until we ship pre-built binaries).

### Step 1: Install build tools

**macOS:**
```sh
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install CMake (needed for llama.cpp)
brew install cmake
```

**Linux (Ubuntu/Debian):**
```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
sudo apt install cmake build-essential
```

### Step 2: Download a model

Pick a model based on your hardware. Smaller models are faster but less capable:

| Your RAM | Recommended model | Size | Download command |
|---|---|---|---|
| 8 GB | Qwen2.5 0.5B Q8 | ~600 MB | `huggingface-cli download Qwen/Qwen2.5-0.5B-Instruct-GGUF qwen2.5-0.5b-instruct-q8_0.gguf --local-dir models` |
| 16 GB | Qwen2.5 3B Q6 | ~2.5 GB | `huggingface-cli download Qwen/Qwen2.5-3B-Instruct-GGUF qwen2.5-3b-instruct-q6_k.gguf --local-dir models` |
| 32 GB+ | Qwen2.5 7B Q5 | ~5 GB | `huggingface-cli download Qwen/Qwen2.5-7B-Instruct-GGUF qwen2.5-7b-instruct-q5_k_m.gguf --local-dir models` |

You'll need `huggingface-cli` installed first:
```sh
pip install huggingface-hub
```

Or download the `.gguf` file manually from [Hugging Face](https://huggingface.co) and place it in a `models/` directory.

### Step 3: Clone and build

```sh
git clone https://github.com/anthropics/sovereign.git
cd sovereign
cargo build --release
```

### Step 4: Run

**Desktop app** (recommended for most people):
```sh
# One-time: install the Tauri CLI
cargo install tauri-cli --version "^2"

cd crates/sovereign-desktop
npm install
cargo tauri dev
```

A setup wizard will guide you through picking a persona (Research, Assistant, or Developer) and pointing Sovereign at your model file.

**Command line** (for terminal users):
```sh
cargo run --release -p sovereign-cli -- --model models/your-model.gguf
```

**HTTP server** (for developers building integrations):
```sh
cargo run --release -p sovereign-server -- --config sovereign-server.toml
```

---

## Three Ways to Use Sovereign

### Desktop App

A native app with a chat interface, conversation history, and a setup wizard. On first launch, you choose a persona:

- **Research & Analysis** — Activates the research-analyst skill. Sovereign will search the web, cross-reference sources, and cite its findings.
- **Personal Assistant** — A general-purpose helper for tasks, planning, and organization.
- **Developer** — Shows model details, config paths, and full control over inference settings.

The persona configures which skills and tools are active. After setup, it's just a chat window — type a message, get a response. Multi-step tasks show inline progress, and tool actions require your approval before executing.

### CLI

An interactive terminal REPL. Good for quick use and scripting.

```sh
cargo run -p sovereign-cli -- --model models/fast.gguf --router
```

| Flag | Default | Description |
|---|---|---|
| `--model <path>` | *required* | Fast/default GGUF model |
| `--primary-model <path>` | same as --model | Larger model for deep reasoning |
| `--data-dir <path>` | `data` | Database directory |
| `--skills-dir <path>` | `~/.sovereign/skills` | User skills directory |
| `--router` | off | Enable intent routing (simple vs. complex) |
| `--ingest <path>` | — | Ingest a directory of documents for RAG |
| `--brave-api-key <key>` | — | Use Brave Search |
| `--tavily-api-key <key>` | — | Use Tavily Search |

Without `--router`, every message goes to the same model. With it, Sovereign classifies intent first — simple questions get fast responses, complex requests trigger multi-step planning.

### HTTP Server

A REST + WebSocket API for building your own frontends or integrations.

```sh
cargo run -p sovereign-server -- --config sovereign-server.toml
```

Endpoints:

| Method | Path | Description |
|---|---|---|
| POST | `/v1/conversations` | Create a conversation |
| POST | `/v1/conversations/{id}/messages` | Send a message |
| GET | `/v1/conversations/{id}` | Get conversation history |
| GET | `/v1/conversations` | List conversations |
| DELETE | `/v1/conversations/{id}` | Delete a conversation |
| POST | `/v1/tasks/{id}/approve` | Approve a tool action |
| GET | `/v1/tools` | List available tools |
| POST | `/v1/search` | Search across messages |
| GET | `/v1/conversations/{id}/stream` | WebSocket streaming |

See `sovereign-server.toml` for configuration (model path, auth, database).

---

## Features

### Dual-model routing

A small, fast model handles simple questions in under a second. When a question needs deeper reasoning, a larger model loads on demand, then auto-unloads after 60 seconds of idle to free memory.

### Multi-step task execution

Complex requests are decomposed into step-by-step plans. The executor runs steps in parallel where possible, handles branching logic, and automatically replans if a step fails.

### Built-in tools

| Tool | What it does |
|---|---|
| **Web Search** | Searches DuckDuckGo (free), Brave, or Tavily |
| **Web Fetch** | Downloads and extracts content from web pages |
| **Shell** | Runs commands on your machine (requires approval) |
| **Knowledge** | Searches your stored memories and documents |
| **Document** | Ingests and queries your local documents (RAG) |

### Memory

Sovereign remembers across conversations. Working memory is compressed every message to stay within context limits. Long-term memories are extracted when conversations end — themes, facts, preferences — and retrieved via full-text search in future conversations.

### Skills

Skills are TOML files that shape how Sovereign behaves. They configure routing hints, planning templates, prompt overrides, memory extraction rules, and inference requirements.

**Bundled skills** (in `skills/`):

| Skill | Description |
|---|---|
| **Research & Analysis** | Multi-source research with citations and source evaluation |
| **Code Review** | Structured code review with security and performance analysis |
| **Personal Assistant** | Task management, scheduling, and organization |
| **Inner Work** | Reflective companion for personal psychological work (always local-only) |

**Writing your own skill:** Create a directory under `skills/` with a `skill.toml` file. See the bundled skills for the full format — routing triggers, planner templates, synthesis prompts, memory rules, and inference requirements are all configurable.

### OICP (Open Inference Capabilities Protocol)

Skills declare what they need from inference (capabilities like code analysis, minimum context window, privacy requirements). When multiple backends are available, Sovereign routes requests to the backend that best matches these requirements. The inner-work skill, for example, declares `privacy = "local_only"` — its data never leaves your machine even if remote backends are configured.

---

## How It Works

```
User message
  -> Router (fast model, <200ms): classifies intent
     -> SimpleQuery  -> fast model responds directly
     -> DeepQuery    -> primary model responds with depth
     -> ComplexTask  -> Planner generates step DAG
                    -> Executor runs steps (parallel batches)
                    -> Tool steps require approval
                    -> Branch steps skip non-taken paths
                    -> On failure: replan once, then surface error
                    -> Synthesize final answer from step outputs
```

Conversations, tasks, memories, and plan state persist in SQLite. If the process crashes mid-task, state is recoverable.

## Architecture

Six crates with five trait boundaries. Every component is swappable.

```
sovereign-core          Traits, Runtime, Executor, Planner, Router, Memory, Skills
sovereign-inference     llama.cpp FFI, dual-slot model management, backend selection
sovereign-store         SQLite (+ in-memory) StateStore, FTS5 search
sovereign-tools         Shell, web search/fetch, documents, knowledge
sovereign-cli           Interactive terminal REPL
sovereign-server        Axum REST API + WebSocket
sovereign-desktop       Tauri + Svelte native desktop app
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design and [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md) for the roadmap.

## Tests

```sh
cargo test --workspace  # 186 tests
```

## License

MIT
