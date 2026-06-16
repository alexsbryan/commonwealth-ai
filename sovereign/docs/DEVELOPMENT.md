# Development

Building, testing, and extending Sovereign. For a high-level architectural map see [`ARCHITECTURE.md`](../ARCHITECTURE.md) and [`SYSTEM_OVERVIEW.md`](../SYSTEM_OVERVIEW.md).

← [back to README](../README.md)

## Building

```sh
cargo build --release              # all crates
cargo build -p sovereign-cli       # CLI only
cargo build -p sovereign-server    # server only
```

Desktop app requires [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) plus Node.js:

```sh
cd crates/sovereign-desktop
npm install
cargo tauri dev
```

## The CLI binaries

`sovereign` is a thin dispatcher. You always type `sovereign <verb>`, but the verb is handled by one of four binaries — the build is split this way so that editing one binary's code doesn't recompile the others.

| Binary | Handles | Kept separate because |
|---|---|---|
| `sovereign-cli` | the dispatcher, plus the light filesystem-and-SQLite verbs: `notes`, `status`, `drift`, `design`, `plan`, `init`, `reflect`, `serve` | no model, tree-sitter, or LanceDB dependency, so its edits rebuild in seconds |
| `sovereign-cli-daemon` | `daemon`, `setup`, `install-service`, `doctor` | the long-running host process and lifecycle setup; it rarely changes |
| `sovereign-cli-dev` | `project`, `code`, `tools`, `atos` | the local-dev workbench: project lifecycle, code intelligence, the MCP tool runner |
| `sovereign-cli-llm` | `chat`, `bench`, `eval`, `enrich`, `recipe`, `pipeline`, `mcp`, `mesh`, `corpus` | anything that holds a chat connection or runs a model loop; the heaviest to compile (llama.cpp bindings, LanceDB, every grammar) |

Each sibling is found next to the dispatcher, or at `$SOVEREIGN_CLI_DAEMON_BIN` / `$SOVEREIGN_CLI_DEV_BIN` / `$SOVEREIGN_CLI_LLM_BIN` if you set one. On Unix the dispatcher execs into the sibling, so it stays the same process.

The footgun to know about: edit a verb's code, rebuild only `sovereign-cli`, and the dispatcher execs the stale sibling — your change appears to do nothing. Rebuild the binary that owns the verb (`cargo build -p sovereign-cli-llm`, say), or build them all with `cargo build --release --bins`. The dispatcher compares sibling build times and prints a one-line warning when one looks stale, so you usually get a nudge rather than a silent miss; `SOVEREIGN_NO_STALE_WARN=1` mutes it.

## Testing

```sh
cargo test --workspace                              # all tests
cargo test -p sovereign-core --test functional      # functional tests (provenance, FTS5, KBs)
cargo test -p sovereign-tools --test smoke_tests    # smoke tests (Parquet ingestion, full pipeline)
```

Three layers:

- **Unit tests** — Mock-based, fast. Cover types, serialization, registry logic, plan parsing.
- **Functional tests** — `DeterministicInference` + real in-memory SQLite + real FTS5. No mocks on the store or search pipeline. Assert on provenance records, conversation state, corpus search results.
- **Smoke tests** — Real Parquet parsing → real SQLite → real Runtime → provenance assertions. End-to-end: ingest philosophy corpus, query about Bergson, verify provenance shows SEP chunks found.

No tests require a GPU, model file, or network access.

## Project structure

```
sovereign/
├── crates/
│   ├── sovereign-core/        # Traits, Runtime, Executor, Planner, Router, Memory, Skills
│   ├── sovereign-inference/   # llama.cpp, remote APIs, hybrid provider, backend selection
│   ├── sovereign-store/       # SQLite, PostgreSQL, in-memory StateStore
│   ├── sovereign-tools/       # Search, corpus parsers, web, shell, RAG, MCP
│   ├── sovereign-cli/         # Terminal REPL + subcommands (setup, project, mesh, …)
│   ├── sovereign-server/      # REST + WebSocket API
│   ├── sovereign-mesh/        # Embedded Commonwealth daemon + MCP router
│   └── sovereign-desktop/     # Tauri + Svelte desktop app
├── modes/                    # Bundled skills (each a skill.toml)
│   ├── inner-work/            # Reflective companion (local-only)
│   └── recipe-author/        # Corpus-recipe authoring workflow
├── docs/
│   ├── CLI_REFERENCE.md       # Flag + subcommand reference
│   ├── CODE_INTELLIGENCE.md   # Per-project code intelligence setup
│   ├── KNOWLEDGE_BASES.md     # Corpora, tiers, search pipeline
│   ├── FEATURES.md            # Routing, memory, skills, OICP
│   ├── DEVELOPMENT.md         # This file
│   ├── TROUBLESHOOTING.md     # Common issues + diagnostics
│   ├── FAQ.md                 # Quick answers
│   └── specs/oicp.md          # OICP protocol specification
├── contrib/
│   ├── launchd/               # macOS service template
│   └── systemd/               # Linux user-service template
├── models/                    # GGUF model files (not committed)
└── sovereign-server.toml      # Example server configuration
```

## Key traits

The system is built around five async trait boundaries (in `sovereign-core/src/traits.rs`):

| Trait | Purpose |
|---|---|
| `InferenceProvider` | `complete()`, `embed()`, `complete_stream()` — model inference |
| `Router` | `classify()` — intent classification |
| `Planner` | `plan()`, `replan()` — step DAG generation |
| `Tool` | `execute()`, `validate()`, `retry_config()` — tool execution |
| `StateStore` | 25+ methods for conversations, memories, documents, corpus state, permissions |

All database records carry a `version` (Lamport timestamp) and soft-deletable tables have a `deleted_at` field. Writes are append-only with soft deletes. This enables future multi-device sync without schema migration — two `StateStore` instances can merge by taking the union of records and resolving by timestamp.

## Adding a tool

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

## Adding a corpus

A corpus is a recipe — a TOML file declaring `acquire → extract → chunk → embed → index`, no code required. Author one with `sovereign recipe`, and to ship it add an entry to `sovereign-recipes/registry.toml`. The walkthrough is [sovereign-recipes/GETTING_STARTED.md](../../sovereign-recipes/GETTING_STARTED.md); every field is in [SCHEMA.md](../../sovereign-recipes/SCHEMA.md). The built-in extractors (Parquet, JSONL, HTML, email, Markdown, CSV, …) live in `corpus-engine/src/extractors/`; for a format they don't cover, add one there — or a `CorpusParser` in `sovereign-tools/src/corpus/` for the legacy built-in path.

## Adding a skill

Create `modes/my-skill/skill.toml` (bundled skills live in `sovereign/modes/`; the corpora are recipes in `sovereign-recipes/`, not under `sovereign/`):

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
