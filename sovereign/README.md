# Sovereign

Sovereign is an AI assistant that runs on your own computer. Ask it to write, to search what you already know, or to think through a problem, and the model that answers you lives on your machine rather than in someone's cloud. Nothing leaves your device unless you ask it to.

Commonwealth, its optional mesh, lets you pool machines with a few people you trust so you can run models no single one of you could hold alone. There's more on that further down.

It's early — pre-release, and source-available for audit under [AGPL-3.0-or-later](../LICENSE). Reading, building, and taking the code apart is welcome; there's just no contribution process yet.

## What you get

Your conversations, documents, and memory stay on the machine in front of you. Web search is there if you want it, off by default and labelled plainly when it runs, and there's no telemetry because nothing was built to phone home in the first place.

Answers come grounded in sources you choose to keep locally — Wikipedia, the Stanford Encyclopedia of Philosophy, Stack Exchange, scholarly abstracts, your own files — searched before each reply and cited, so you can follow a claim back to where it came from. It remembers what mattered from earlier conversations instead of starting cold every time. And it reads code as a codebase: real call graphs and symbol search that answer to a file and a line, not a plausible guess.

## Quick start

Set it up once. This finds models that fit your hardware, downloads them, and starts a background daemon that survives a logout:

```sh
sovereign setup
```

Then talk to it:

```sh
sovereign chat
```

That's the whole loop. The daemon also serves an OpenAI-compatible API and an MCP tool server on a single port, so you can point opencode, Claude Code, or any OpenAI-compatible client at `localhost:9741` and it will just work:

```sh
curl http://localhost:9741/v1/models      # confirm it's alive
```

## Three ways in

The desktop app — Tauri and Svelte — gives you chat, knowledge bases, model setup, and a guided first run, built with `npm install && cargo tauri dev` in `crates/sovereign-desktop`. The CLI does everything the desktop does and adds `setup`, `project`, `mesh`, `corpus`, and `doctor`; every command takes `--help`, and the build is below. The server exposes the same runtime over REST and WebSocket for your own frontends — its [endpoints](docs/CLI_REFERENCE.md#http-endpoints) are in the CLI reference.

## For your code

```sh
sovereign project init
```

This indexes the current repository — tree-sitter symbols, a SCIP call graph, and MCP tools your AI harness can call — and wires up `.claude/` or `.opencode/` if it finds them. The full flow, the tools, and multi-project setups are in [Code intelligence](docs/CODE_INTELLIGENCE.md).

## Commonwealth

Setup leaves you on a private mesh of one. When you want company, promote it and share the key it prints:

```sh
sovereign mesh create        # prints a key like cwth-a1b2-c3d4-e5f6
```

A friend runs `sovereign mesh join cwth-a1b2-c3d4-e5f6`, and from then on your machines answer as one endpoint — enough, together, to run a model neither of you could run alone, or to share a knowledge base across the group. There's no central server, and nothing leaves the group. [Run a model bigger than your machine](../docs/RUN_A_BIGGER_MODEL.md) walks through the whole thing.

## Install

Pre-release means building from source. You'll need 8 GB of RAM to start — 16 is comfortable, 32 runs the best open models — along with a Rust toolchain and CMake. On macOS, run `xcode-select --install`, then export `SDKROOT="$(xcrun --show-sdk-path)"`, which bindgen needs to find the system headers. On Linux, `sudo apt install cmake build-essential` covers it.

```sh
cargo build --release -p sovereign-cli
ln -sf "$(pwd)/target/release/sovereign-cli" ~/.local/bin/sovereign
sovereign setup
```

Running on AMD Strix Halo, or adding a cloud-GPU peer, takes a little more — see the [toolbox](docs/TOOLBOX_SETUP.md) and [cloud-peer](docs/CLOUD_PEER_DEPLOY.md) guides.

## Where to go next

If something breaks, `sovereign doctor` walks the checks and the [troubleshooting guide](docs/TROUBLESHOOTING.md) covers the rest. The [full command reference](docs/CLI_REFERENCE.md), the [knowledge-base catalogue](docs/KNOWLEDGE_BASES.md) and its tiers, and a tour of [what it can do](docs/FEATURES.md) each have their own page, as does the [FAQ](docs/FAQ.md). If you mean to build on it or add a tool or corpus, start with the [development guide](docs/DEVELOPMENT.md). And if you came to audit, the [system overview](SYSTEM_OVERVIEW.md) and the [architecture principles](ARCH_PRINCIPLES.md) are the two documents to read first.

## License

AGPL-3.0-or-later, one license across the whole monorepo. The network-use clause applies mainly to the Commonwealth mesh daemon, which is a service other people connect to.
