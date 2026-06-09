# Commonwealth AI

**Sovereign** is a local-first AI assistant — a desktop app and a CLI that run
language models, retrieval, and agentic tools **entirely on your own machine**.
**Commonwealth** is the optional peer-to-peer mesh that lets a few trusted
machines pool model capacity and knowledge. Nothing leaves your device unless
you explicitly opt in (a web search, or joining a mesh).

This repository is **source-available for audit** under the GNU Affero General
Public License v3.0-or-later — see [`LICENSE`](./LICENSE). The AGPL's
network-use clause is deliberate: it matters most for the Commonwealth mesh
daemon, which is a network service.

> **Status: pre-release.** Published for transparency and review. External
> contributions are not being solicited yet, so there is no `CONTRIBUTING.md`
> or PR process — but reading, building, and auditing the code is encouraged.

---

## What's here

| Component | Path | What it is |
|-----------|------|------------|
| **Sovereign Desktop** | `sovereign/crates/sovereign-desktop/` | Tauri 2 + Svelte 5 GUI: chat with streaming + provenance, local knowledge bases, model setup, mesh UI. |
| **Sovereign CLI** | `sovereign/crates/sovereign-cli/` | `sovereign <verb>` — chat, corpus management, mesh, benchmarks, and more. A thin dispatcher that execs sibling binaries. |
| **Sovereign server** | `sovereign/crates/sovereign-server/` | Axum REST + WebSocket surface against the same runtime (powers the mobile client). |
| **corpus-engine** | `corpus-engine/` | The knowledge layer: acquire → extract → chunk → embed → index over LanceDB (vectors) + Tantivy (keyword). |
| **commonwealth** | `commonwealth/` | The symmetric mesh daemon: discovery, gossip, inference scheduling, knowledge sharing. |
| **oicp-types** / **sovereign-recipes** | `oicp-types/`, `sovereign-recipes/` | Wire-protocol types and the corpus recipe definitions. |

The whole tree is one Cargo workspace: one `cargo build --workspace`, one
`Cargo.lock`, atomic cross-crate commits.

---

## Quick start

### Prerequisites
- **Rust** (stable) — https://rustup.rs
- **Node.js 20+** — for the desktop frontend
- **Platform build deps** — a protobuf compiler and the usual native toolchain.
  `scripts/bootstrap.sh` wires up a fresh workstation; on Linux the system
  packages (protobuf, GTK/WebKit for the desktop, mold, etc.) are listed in
  `sovereign/scripts/bootstrap-linux.sh`. On macOS you need the Xcode command
  line tools (`xcode-select --install`) and `SDKROOT` exported.

### Build & run the desktop app
```bash
cargo install tauri-cli --version '^2'        # one-time
cd sovereign/crates/sovereign-desktop
npm install
cargo tauri dev                               # dev build with hot reload
# cargo tauri build                           # → a distributable bundle
```

### Build & run the CLI
The default build is the end-user surface (chat, corpus, mesh, daemon, …):
```bash
cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
./target/release/sovereign-cli --help         # the `sovereign` dispatcher
ln -sf "$(pwd)/target/release/sovereign-cli" ~/.local/bin/sovereign   # optional: onto PATH
sovereign chat                                # interactive chat through the daemon
```
The developer toolchain (project lifecycle, ATOS orchestration, code
intelligence, archaeology) is gated out of the default build. Enable it by
building the dev sibling and the dispatcher with `--features dev-tools`:
```bash
cargo build --release -p sovereign-cli-dev
cargo build --release -p sovereign-cli --features dev-tools
```

---

## Navigating the code (for auditors & reviewers)

Start with the two authoritative documents — they are kept as contracts, not
diaries:

- **[`sovereign/SYSTEM_OVERVIEW.md`](./sovereign/SYSTEM_OVERVIEW.md)** — the map.
  Every crate, every subsystem, and where to look. Read this first.
- **[`sovereign/ARCH_PRINCIPLES.md`](./sovereign/ARCH_PRINCIPLES.md)** — the
  design rules the code is held to (SOLID / SICP applied *here*), each rule
  citing the real incident that motivated it.

Per-subsystem deep dives live in [`sovereign/docs/`](./sovereign/docs/)
(inference, knowledge views, tiered retrieval, the mesh, and more). The
canonical mesh protocol spec is `commonwealth/docs/oicp-v0.3.md`.

Design ethos, in one line: **glassbox** — the person running the system should
be able to see *why* it did what it did from the logs, without a debugger.

---

## Repository layout

```
Cargo.toml              Workspace + shared dependencies (one version per dep)
LICENSE                 GNU AGPL-3.0-or-later
oicp-types/             OICP wire-protocol types (bottom of the dep graph)
corpus-engine/          Knowledge layer (+ carved-out corpus-engine-* siblings)
sovereign/
  crates/               Local agent runtime, CLI, desktop, server, tools, …
  SYSTEM_OVERVIEW.md    The authoritative system map
  ARCH_PRINCIPLES.md    The design rules
  docs/                 Per-subsystem deep dives
commonwealth/
  crates/               Symmetric mesh daemon
sovereign-recipes/      Corpus recipe definitions (Wikipedia, SEP, …)
packages/chat-ui/       Shared Svelte chat render surface (desktop + mobile)
scripts/                bootstrap.sh, sovereign-lint.sh, sovereign-test.sh
```

---

## Development

The workspace builds and tests as a unit:

```bash
./scripts/sovereign-lint.sh --human    # repo-wide `cargo check`
./scripts/sovereign-test.sh --human    # repo-wide `cargo test`
./scripts/sovereign-test.sh --human --package <name>   # one crate
```

Tests are designed to run on a CI box with no GPU, no network, and no model
weights on disk (mocks stand in for inference and embeddings). Adapter logs
persist under `target/sovereign-test/latest/` for triage.

---

## License

GNU Affero General Public License, version 3 or (at your option) any later
version. See [`LICENSE`](./LICENSE). Copyright © the Commonwealth AI authors.
