# Commonwealth AI

This is the monorepo behind Sovereign, an AI assistant that runs on your own computer, and Commonwealth, the optional mesh that lets a few machines you trust pool their capacity. To use it, start at [sovereign/README.md](./sovereign/README.md). This page is for people reading, building, or auditing the code.

The code is published source-available under [AGPL-3.0-or-later](./LICENSE) so it can be read and audited in the open. The network-use clause applies mainly to the Commonwealth mesh daemon, which is a service other people connect to. This is pre-release; external contributions aren't being solicited yet, so there's no contribution process, but reading and building the code is welcome.

## What's in here

It is one Cargo workspace — a single `cargo build --workspace`, a single `Cargo.lock`, cross-crate commits that land atomically.

```
oicp-types/        OICP wire-protocol types; the bottom of the dependency graph
corpus-engine/     The knowledge layer: acquire → extract → chunk → embed → index
                   over LanceDB and Tantivy, with carved-out corpus-engine-* siblings
sovereign/         The local assistant: runtime, CLI, desktop, server, tools
  SYSTEM_OVERVIEW.md   the system map; read this first
  ARCH_PRINCIPLES.md   the design rules, each tied to the incident that motivated it
  docs/                per-subsystem deep dives
commonwealth/      The symmetric mesh daemon Sovereign embeds; protocol in docs/oicp-v0.3.md
sovereign-recipes/ Corpus recipe definitions (Wikipedia, SEP, and others)
sovereign-mobile/  Thin Tauri mobile client (iOS and Android)
packages/chat-ui/  Shared Svelte chat surface for desktop and mobile
scripts/           bootstrap.sh, sovereign-lint.sh, sovereign-test.sh
```

Dependencies run one way. Sovereign embeds Commonwealth in-process through `sovereign-mesh`, which is the only place the two meet.

## Building it

The desktop app:
```bash
cargo install tauri-cli --version '^2'        # once
cd sovereign/crates/sovereign-desktop
npm install
cargo tauri dev                               # or cargo tauri build for a bundle
```

The CLI — the `sovereign-cli` dispatcher plus the `sovereign-cli-daemon` and `sovereign-cli-llm` siblings it `exec`s:
```bash
cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
./target/release/sovereign-cli --help
```

Code intelligence — symbol search, call graphs, and the MCP tools the assistant calls — is part of the default build. Two developer surfaces are opt-in and off by default, so the shipped binaries carry neither: **`--features dev-tools`** (git archaeology in the code brief, the agent-coding bench, and the `sovereign-cli-dev` workbench verbs — `project`, `code`, `tools`) and **`--features atos`** (the experimental agent feature-lifecycle subsystem). The default `sovereign` dispatcher refuses a workbench verb with a pointer to rebuild, rather than silently shipping it. The full setup, including platform build dependencies, is in [sovereign/README.md](./sovereign/README.md).

## Reading it

Start with [sovereign/SYSTEM_OVERVIEW.md](./sovereign/SYSTEM_OVERVIEW.md): every crate and subsystem, and where to look. Then [sovereign/ARCH_PRINCIPLES.md](./sovereign/ARCH_PRINCIPLES.md) for the rules the code is held to, each one tied to the incident that motivated it. The guiding idea is glassbox: someone running the system should be able to see why it did what it did from the logs, without reaching for a debugger.

## Working in the tree

```bash
./scripts/sovereign-lint.sh --human    # repo-wide cargo check
./scripts/sovereign-test.sh --human    # repo-wide cargo test
```

The tests run on a CI box with no GPU, no network, and no model weights; mocks stand in for inference and embeddings. Adapter logs land under `target/sovereign-test/latest/` for triage.

## Releasing

[RELEASING.md](./RELEASING.md) is the map: the CLI (`cli-v*` tags → `curl | sh` installer) and the desktop app (`desktop-v*` tags → installers) release independently from this one repo. Desktop specifics — the four-platform Tauri matrix, code signing, and the auto-updater — are in [sovereign/crates/sovereign-desktop/RELEASING.md](./sovereign/crates/sovereign-desktop/RELEASING.md).

## License

[AGPL-3.0-or-later](./LICENSE). Copyright © the Commonwealth AI authors.
