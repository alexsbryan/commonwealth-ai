# svrnmesh

svrnmesh is an AI assistant that runs on your own computer. Ask it to write, to search what you already know, or to think a problem through — the model that answers you lives on your machine, not someone's cloud. Nothing leaves your device unless you ask.

Its optional mesh, **cmnwlth**, lets you pool machines with people you trust to run models no single one of you could hold alone ([more below](#cmnwlth)).

Pre-release, source-available for audit under [AGPL-3.0-or-later](../LICENSE). Reading, building, and taking it apart is welcome; the contribution process is coming soon.

## Quick start

Set it up once — this finds models that fit your hardware, downloads them, and starts a background daemon — then talk to it:

```sh
svrn setup
svrn chat session
```

That's the whole loop. Answers come **cited** from sources you keep locally, your conversations and memory stay put, web search is off by default, and there's no telemetry. It also reads code as a codebase — real call graphs and symbol search that answer to a file and a line, not a plausible guess. ([What it can do](docs/FEATURES.md) is the full tour.)

Keep the daemon running across logouts with `svrn install-service`. It serves an OpenAI-compatible API and an MCP tool server on one port, so opencode, Claude Code, or any OpenAI-compatible client can point at `localhost:9741`:

```sh
curl http://localhost:9741/v1/models      # confirm it's alive
```

## Three ways in

- **Desktop** (Tauri + Svelte) — chat, knowledge bases, model setup, a guided first run. `npm install && cargo tauri dev` in `crates/sovereign-desktop`.
- **CLI** — everything the desktop does, plus `setup`, `mesh`, `corpus`, `doctor`, and `solve` (hand it a coding goal; it makes the goal test-shaped and iterates your repo to green — [solver guide](docs/SOLVER_FOR_PI_USERS.md)). Every command takes `--help`.
- **Server** — the same runtime over REST and WebSocket for your own frontends ([endpoints](docs/CLI_REFERENCE.md#http-endpoints)).

## For your code (developer build)

svrnmesh reads code as a codebase — tree-sitter symbols, a SCIP call graph, and MCP tools your AI harness can call, wired into `.claude/` or `.opencode/`:

```sh
svrn project init
```

`project`, `code`, and `tools` ship only in a dev build, not the prebuilt install below. Flags and multi-project setup: [code intelligence](docs/CODE_INTELLIGENCE.md) and the [development guide](docs/DEVELOPMENT.md).

## cmnwlth

Setup leaves you on a private mesh of one. To bring in company, promote it and share the key:

```sh
svrn mesh create        # prints a key like cwth-a1b2-c3d4-e5f6
```

A friend runs `svrn mesh join <key>`, and your machines answer as one endpoint — enough to run a model neither of you could alone, or to share a knowledge base. No central server; nothing leaves the group. [Run a model bigger than your machine](../docs/RUN_A_BIGGER_MODEL.md) walks it through.

## Install

Prebuilt binaries for macOS (Apple Silicon) and Linux (x86_64):

```sh
curl -fsSL https://svrnme.sh/install.sh | sh
svrn setup
```

That drops the `sovereign` CLI into `~/.local/bin`. You'll want 8 GB of RAM to start — 16 is comfortable, 32 runs the best open models.

### Or build from source

Needs a Rust toolchain and CMake. On macOS: `xcode-select --install`, then `export SDKROOT="$(xcrun --show-sdk-path)"` (bindgen needs the system headers). On Linux: `sudo apt install cmake build-essential protobuf-compiler`.

```sh
cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
ln -sf "$(pwd)/target/release/sovereign-cli" ~/.local/bin/svrn
svrn setup
```

AMD Strix Halo or a cloud-GPU peer take a little more — see the [toolbox](docs/TOOLBOX_SETUP.md) and [cloud-peer](docs/CLOUD_PEER_DEPLOY.md) guides.

## Where to go next

- Something broke? `svrn doctor`, then [troubleshooting](docs/TROUBLESHOOTING.md).
- [Command reference](docs/CLI_REFERENCE.md) · [knowledge bases](docs/KNOWLEDGE_BASES.md) · [features](docs/FEATURES.md) · [FAQ](docs/FAQ.md).
- Building on it? The [development guide](docs/DEVELOPMENT.md).
- Auditing? [SYSTEM_OVERVIEW.md](SYSTEM_OVERVIEW.md) and [ARCH_PRINCIPLES.md](ARCH_PRINCIPLES.md) first.

## License

AGPL-3.0-or-later, one license across the whole monorepo. The network-use clause applies mainly to the cmnwlth mesh daemon, which is a service other people connect to.
