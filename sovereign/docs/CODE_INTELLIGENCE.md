# Code intelligence

Sovereign can index a codebase so an AI assistant works from a real model of it instead of guessing — exact symbol lookup, compiler-resolved call graphs, and semantic search, every answer pointing at a file and a line, kept current as you edit. Once a project is indexed, the tools are available to Claude Code or opencode over MCP, and to you on the command line.

> **Developer build required.** The `project`, `code`, and `tools` CLI verbs are part of the developer toolchain — they ship only in a build with `--features dev-tools` (plus the `sovereign-cli-dev` sibling), not in the prebuilt install from the [README](../README.md). See the [development guide](DEVELOPMENT.md) for the build flags. The code-intelligence MCP tools themselves are served by the standard daemon once a repository is indexed.

## Set it up

From inside a repository, with the daemon running:

```sh
sovereign project init
```

That does several things in one pass:

- Detects the languages in the repo and builds a symbol index from a tree-sitter parse, stored as a LanceDB index under `~/.sovereign/indexes/<name>/`.
- Exports a SCIP call graph, if the language's exporter is installed — for Rust that's `rust-analyzer scip`. The graph is a SQLite database (`scip_graph.db`) next to the symbol index, and it's what makes `callers`, `callees`, and `blast` exact rather than grep-shaped. A missing exporter is a warning, not a failure: you still get symbols and search, just no call graph for that language.
- Writes a generated `.sovereign/SOVEREIGN.md` (a short map of the repo), the `.sovereign/` config directory, and an `AGENTS.md`.
- Wires up your AI harness — a `.mcp.json` or `.opencode/opencode.json` pointing at the local MCP server — so the tools appear without you configuring anything.
- Registers the project with the daemon so it stays fresh, which is the next section.

The common flags are `--name <id>` to set the index name (it defaults to the folder), `--no-scip` to skip the call graph, `--workspace-root <dir>` for a monorepo (it finds the workspaces under that path), and `--port` if your MCP server isn't on 9741. The full list is in [CLI_REFERENCE](CLI_REFERENCE.md#sovereign-project).

`sovereign project init` also founds the ATOS side of a project — charters, phases, design docs — which is a separate concern covered in [ATOS.md](ATOS.md). This page is only the code-intelligence half.

## The tools you get

In your AI harness, over MCP at `localhost:9741/mcp`, four tools navigate the code:

- `symbols` — exact lookup of a named function, struct, trait, or type, returning its file and line.
- `callers` — every call site of a symbol, resolved through the SCIP graph, so it catches trait dispatch that grep can't see.
- `callees` — what a given symbol calls.
- `blast` — the transitive impact of changing a symbol: its callers at every depth.

A few more are CLI-only — registered in the tool surface but not exposed over MCP. Reach them with `sovereign tools call <id>`:

- `code_search` — semantic search over the indexed code, by meaning rather than text match.
- `recent_changes` — symbols in files modified in the last N hours.
- `project_context` — search the project's own docs and conventions (`*.md`, `.sovereign/conventions/`).

`sovereign tools list` shows everything available, grouped by effect and scope, and `sovereign tools describe <id>` prints a tool's parameters and output shape. A CLI call is the same code path as the MCP call — `sovereign tools call symbols --name=Foo` is exactly `symbols({"name": "Foo"})` underneath.

The tools were renamed in a CLI refactor, and the old names still work as aliases: `symbol_lookup` → `symbols`, `find_callers` → `callers`, `find_callees` → `callees`, `blast_radius` → `blast`. `tools/list` advertises the old names as deprecated mirrors, and a call under an old name is rewritten before lookup. New code should use the short names.

## Staying fresh

You don't re-index by hand. The daemon watches every registered project and rebuilds when it needs to — on a file save, when git HEAD moves under it (a branch switch or a pull), and once on startup. A rebuild runs the exporters into a `.new` file and swaps it in atomically, so a query in flight always sees a complete graph rather than a half-built one.

To check on it or push it:

```sh
sovereign project list             # every project the daemon watches
sovereign project status           # this project's index and graph state
sovereign project refresh          # rebuild the graph now
sovereign project watch status     # watcher health and graph age
```

If `symbols` reports "no symbol named X" for something you know exists, the index for that project is usually missing or stale. `sovereign project status` shows its state, and `refresh` or a re-index fixes it.

## The lower-level command

`sovereign project init` is the full setup. When you want only the symbol index — no call graph, no harness config, no registration — `sovereign code index <path>` is the primitive:

```sh
sovereign code index . --corpus-id myrepo
```

It builds the LanceDB symbol index (with embeddings, so it needs the daemon for the embedding model) and nothing else. It's for scripting a narrow pipeline; most of the time `project init` is what you want. Alongside it, `sovereign code watch <id>` runs a standalone re-indexing watcher, `sovereign code finalize <id>` promotes a stranded partition into place, and `sovereign code mcp-status` pings the MCP server and lists what it exposes.

## Where it lives

Per repo, `.sovereign/` holds the generated `SOVEREIGN.md`, the project config, and the watcher settings in `sovereign.toml`. The indexes themselves are global, under `~/.sovereign/indexes/<name>/`: the LanceDB symbol chunks, and `scip_graph.db` for the call graph. The daemon's registry of watched projects is in `~/.sovereign/projects/`.

## More than one project

Register as many repos as you like — each gets its own watcher and its own index, and the daemon merges their call graphs so a query can span them, with every result tagged by the project it came from. For a single monorepo, `sovereign project init --workspace-root <dir>` discovers the workspaces under that root and indexes them as one project. There's no separate "ecosystem" command; multiple projects are just several registrations.

---

The internals — how the SCIP graph is built and queried, the reindexer's freshness gates, the chunk schema — live in `corpus-engine-scip` and `sovereign-tools`, and are mapped in [SYSTEM_OVERVIEW](../SYSTEM_OVERVIEW.md). If something's wrong after setup, [TROUBLESHOOTING](TROUBLESHOOTING.md) covers the common cases.
