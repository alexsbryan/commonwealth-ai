# Sovereign

AI that belongs to the people running it.

Sovereign runs open-source language models on your hardware and gives you a real assistant — one that understands code, reads documents, searches knowledge bases you install, remembers across sessions, and does multi-step work without asking anyone's permission.

## What it does well

**Understands code like a codebase.** Compiler-resolved call graphs, not fuzzy grep. Symbol-level search, watchers that stay fresh as you edit, answers cited to file and line. Session memory that persists — notes, reflections, the continuity of thought commercial tools throw away between chats.

**Grounds answers in knowledge worth trusting.** Wikipedia, Stanford Encyclopedia of Philosophy, Stack Exchange, scholarly abstracts — installed locally, searched before every answer, cited. Your own documents too.

**Private by construction. Sovereign.** Conversations, documents, and memories stay on your machine. Web search is optional and clearly labeled. No telemetry, no phoning home, no policy anyone could change — it's how the software is built.

**Shaped by skills, not code.** Skills are TOML manifests that configure routing, planning, synthesis, and memory for different kinds of work. Modify the ones we ship. Write your own. Just a file.

**Works offline.** Once models and corpora are downloaded, everything runs without a connection.

## The larger idea

The tools we use to think are becoming infrastructure, and right now that infrastructure is being built to belong to a few companies. We don't think that's the only shape it can take.

Commonwealth is a protocol for small trusted groups to pool machines into a shared mesh — friends, teams, research collectives, households. Run models no one machine could hold. Share knowledge across the group. Route heavy work to whoever's idle. No central server, no billing, no data leaving a ring of trust. A gift economy for compute, among people who already trust each other.

Sovereign works alone, and it works well alone. When you want to build something larger with people you trust, we're here.

## Quick start for coding (three commands)

```sh
sovereign setup         # once — detects hardware, downloads models, starts the daemon
sovereign project init  # per project — indexes the codebase, registers MCP tools
sovereign mesh create   # optional — promotes your local mesh to a joinable one
```

`localhost:9741` serves **both** the OpenAI-compatible completions endpoint (`/v1/chat/completions`, `/v1/models`) **and** the MCP tool server (`/mcp`). Point opencode, Claude Code, or any OpenAI-compatible client at it and everything just works.

### `sovereign setup`

First-run onboarding. Detects your hardware, curates a list of primary models that fit, downloads all three slots (primary / fast / embed) in parallel, writes a config, and registers the daemon with launchd (macOS) or systemd (Linux) so it survives logout.

```sh
sovereign setup              # interactive — pick your primary model
sovereign setup --yes        # non-interactive — accept recommended
sovereign setup --reset      # wipe config and re-run
```

When it finishes:

```
✓ Models ready
✓ Mesh running — 1 node (you)
✓ Endpoint: localhost:9741/v1
```

Run `curl http://localhost:9741/v1/models` to confirm it's alive.

### `sovereign project init`

Per-project code intelligence: tree-sitter symbol index + SCIP call graph + generated `SOVEREIGN.md` + auto-detected `.claude/` or `.opencode/` config. See [`docs/CODE_INTELLIGENCE.md`](docs/CODE_INTELLIGENCE.md) for the full flow and multi-project ecosystem recipes.

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

## Install

### Requirements

1. **8 GB RAM minimum.** 16 GB is comfortable; 32 GB lets you run the best open models.
2. **Rust toolchain** and **CMake** for building from source.
3. A `.gguf` model file is optional — `sovereign setup` downloads one for you.

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

### Clone and build

```sh
git clone https://github.com/anthropics/sovereign.git
cd sovereign
cargo build --release -p sovereign-cli
```

Put `./target/release/sovereign-cli` on your PATH (or `cargo install --path crates/sovereign-cli`) and you're ready for `sovereign setup`. If you prefer to manually pick a model, see [`docs/KNOWLEDGE_BASES.md`](docs/KNOWLEDGE_BASES.md) for model size recommendations by RAM tier.

## Three interfaces

### Desktop App

A native app with chat, conversation history, knowledge base management, and a setup wizard. On first launch, choose a persona:

- **Research & Analysis** — Activates the research-analyst skill. Searches local knowledge bases first, supplements with web when needed, cites findings.
- **Personal Assistant** — General-purpose helper for tasks, planning, and organization.
- **Developer** — Full control over model selection, inference settings, and search backends.

The setup wizard includes knowledge-base tier selection (Essential through Full — see [`docs/KNOWLEDGE_BASES.md`](docs/KNOWLEDGE_BASES.md)) and optional web search API key configuration. Knowledge bases download and index in the background — you can start chatting immediately.

```sh
cargo install tauri-cli --version "^2"
cd crates/sovereign-desktop
npm install
cargo tauri dev
```

### CLI

Interactive terminal REPL with all the same capabilities, plus the `setup` / `project` / `mesh` / `doctor` / `reflect` / `corpus` subcommands. Every command accepts `--help`. Full flag and subcommand reference: [`docs/CLI_REFERENCE.md`](docs/CLI_REFERENCE.md).

```sh
sovereign --model models/fast.gguf --router     # legacy REPL mode
sovereign setup                                 # first-run (recommended)
sovereign project init                          # per-project code intelligence
```

### HTTP Server

REST + WebSocket API for custom frontends. See [`docs/CLI_REFERENCE.md#http-endpoints`](docs/CLI_REFERENCE.md#http-endpoints) for the endpoint list.

```sh
cargo run --release -p sovereign-server -- --config sovereign-server.toml
```

## Troubleshooting

- **Daemon didn't come up after `sovereign setup`** → Check `~/.sovereign/logs/daemon.err`. Most common cause: a corrupt GGUF download (a sub-1 MB file). See [TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md#sovereign-setup-finishes-but-waiting-for-daemon-to-come-up-times-out).
- **`project serve` listens on `:8080` instead of `:9741`** → Stale binary; rebuild.
- **`mesh create` fails with "mesh already exists"** → Run `sovereign mesh rotate` instead.
- **Want to switch models post-setup** → Edit `~/.config/sovereign/config.toml` or `sovereign setup --reset`.
- **Anything else** → `sovereign doctor` walks through every check, or see [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md).

## Where to go next

| Doc | For |
|---|---|
| [`docs/CLI_REFERENCE.md`](docs/CLI_REFERENCE.md) | Full flag + subcommand reference; HTTP endpoint list |
| [`docs/CODE_INTELLIGENCE.md`](docs/CODE_INTELLIGENCE.md) | `project init`, SCIP exporters, multi-project ecosystems |
| [`docs/ATOS.md`](docs/ATOS.md) | Agent Task Orchestration System: charters, approvals, drift, auto red-team |
| [`docs/KNOWLEDGE_BASES.md`](docs/KNOWLEDGE_BASES.md) | Corpora, tier sizing, coverage-aware search pipeline |
| [`docs/FEATURES.md`](docs/FEATURES.md) | Routing, memory, skills, provenance, OICP |
| [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) | Setup/daemon issues, uninstall, port conflicts |
| [`docs/FAQ.md`](docs/FAQ.md) | Quick answers to common questions |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | Building, testing, adding tools/corpora/skills |
| [`docs/TOOLBOX_SETUP.md`](docs/TOOLBOX_SETUP.md) | Running on AMD Strix Halo via kyuz0 ROCm/Vulkan toolboxes |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Deep architectural design document |
| [`SYSTEM_OVERVIEW.md`](SYSTEM_OVERVIEW.md) | Cross-project (sovereign + commonwealth + corpus-engine) map |

## License

MIT
