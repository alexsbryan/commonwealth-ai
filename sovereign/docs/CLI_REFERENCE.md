# CLI Reference

Full flag and subcommand reference for `sovereign-cli`. For a walk-through of the three-command workflow (`setup` / `project init` / `mesh create`), see the [README](../README.md). Every subcommand also accepts `--help` for per-command detail.

← [back to README](../README.md)

## Top-level invocation

```sh
sovereign <subcommand> [flags]
sovereign --model <path.gguf> [options]   # legacy interactive REPL
```

Run `sovereign --help` for the live list of subcommands.

## REPL-mode flags

When invoked without a subcommand, `sovereign` starts an interactive terminal REPL. Every flag is optional except `--model`:

| Flag | Default | Description |
|---|---|---|
| `--model <path>` | *required* | Fast/default GGUF model |
| `--primary-model <path>` | same as `--model` | Larger model for deep reasoning |
| `--data-dir <path>` | `data` | Database and downloads directory |
| `--skills-dir <path>` | `~/.sovereign/skills` | User skills directory |
| `--router` | off | Enable LLM-based intent routing |
| `--ingest <path>` | — | Ingest documents from a directory before REPL |
| `--brave-api-key <key>` | — | Use Brave Search |
| `--tavily-api-key <key>` | — | Use Tavily Search |

Without `--router`, every message gets a direct response. With it, Sovereign classifies intent — simple questions use the fast model, complex requests trigger multi-step planning.

## Subcommand reference

### `sovereign setup`

First-run onboarding. Detects hardware, downloads models, starts the daemon.

| Flag | Description |
|---|---|
| `--yes`, `-y` | Non-interactive; accept recommended choices |
| `--reset` | Wipe config and re-run (uninstalls service first) |
| `--data-dir <path>` | Override the default data root (`~/.sovereign`) |

### `sovereign project`

Per-project code intelligence **and** the project-layer half of ATOS (charter + phases). See [CODE_INTELLIGENCE.md](CODE_INTELLIGENCE.md) for the indexing flow and [ATOS.md](ATOS.md) for the charter flow.

| Subcommand | Description |
|---|---|
| `init` | Set up code intelligence for the current workspace; also installs the opencode ATOS plugin |
| `status` | Show the status of code intelligence + ATOS scaffold (founded? current phase?) |
| `refresh` | Re-export the SCIP call graph |
| `serve` | Start a lightweight MCP server (no model required) |
| `install-hooks` | Upgrade (or install) the post-commit hook |
| `found [--design <path>]` | Four-stage founding conversation; writes `.sovereign/CHARTER.md` and `PHASES.md`, records answers as `decision` notes |
| `amend` | Open `CHARTER.md` in `$EDITOR`; on save, diff section-by-section and run adversarial Q&A for changed sections; write amendment log + new hash |
| `phase status` | Show founding state + current phase |
| `phase pass [N]` | Run phase N's stop condition from `PHASES.md`; write `phase-N.md` on green |
| `audit` | One-page reviewer rollup: founding state, phases passed, notes by kind, open questions, drift status |
| `register` | Register the project with the sovereign daemon for FS-watch + auto-refresh |
| `watch status` | Inspect the daemon's watcher state for this project |

### `sovereign mesh`

Manage the local Commonwealth mesh.

| Subcommand | Description |
|---|---|
| `create [--name <name>]` | Promote the solo mesh to a joinable mesh; print invite |
| `join <arg>` | Join an existing mesh (bare key, https url, or sovereign://) |
| `rotate` | Generate a new shareable join key (invalidates previous) |
| `status` | Show mesh members, hosted knowledge, loaded models |
| `balance` | Show your contribution to the mesh |
| `leave` | Leave the current mesh |

### `sovereign corpus`

Manage knowledge corpora. See [KNOWLEDGE_BASES.md](KNOWLEDGE_BASES.md) for tier details.

| Subcommand | Description |
|---|---|
| `list` | List installed and available corpora |
| `install <id>` | Install a corpus (e.g. `wikipedia`) |
| `remove <id>` | Remove an installed corpus |
| `status` | Show shard status for all corpora |
| `reconstruct-manifest <id>` | Rebuild source-file manifest before collaborative ingestion |

### `sovereign code`

Lower-level code-intelligence primitives. `project init` wraps these for the typical flow.

| Subcommand | Description |
|---|---|
| `index <path>` | Index a local repository with tree-sitter |
| `watch <corpus-id>` | Run a filesystem watcher that re-indexes on save |
| `mcp-status` | Ping the local MCP server and list exposed tools |
| `search <query>` | (placeholder — use Sovereign chat or MCP for now) |

### `sovereign doctor`

Diagnose setup and daemon health across Sovereign / Commonwealth / OmO layers.

| Flag | Description |
|---|---|
| `--fix` | Attempt automatic repair for failing checks |
| `--watch` | Re-run periodically (every 5s) |
| `--json` | Emit structured JSON for scripting |

### `sovereign reflect`

Review session reflections and retire ones that are no longer relevant.

| Flag | Description |
|---|---|
| `--since <Nd\|Nh>` | Period to analyse (default: 30d) |
| `--tool <name>` | Filter signals to one tool |
| `--raw` | Print full reflection prose |
| `--todos` | List open todo notes only |
| `--retire --tool <name> --reason <why>` | Retire matching reflections |

### `sovereign recipe`

Run corpus ingestion recipes.

| Subcommand | Description |
|---|---|
| `list` | List all corpora available in the registry |
| `test <path>` | Run the full test harness against a recipe file |
| `validate <path>` | Validate recipe fields without downloading data |

### `sovereign mcp`

Inspect and test configured MCP servers.

| Subcommand | Description |
|---|---|
| `list` | List configured MCP servers with status |
| `test <server>` | Test connection to a named server |
| `tools [server]` | List available MCP tools |

### `sovereign tools`

Invoke the 24 sovereign code-intelligence tools directly from the shell. Same `Tool::execute()` as the MCP path — use this when the daemon isn't running, when scripting, or for self-documenting `--help`. See [ARCH_PRINCIPLES.md](../ARCH_PRINCIPLES.md) §2 for the behavioural properties each tool declares.

| Subcommand | Description |
|---|---|
| `list` | Print the tool manifest, grouped by Effect × Scope (Read/Write × Session/Persistent/External) |
| `describe <id>` | Full descriptor for one tool: parameters JSON Schema, output schema (compose-able keys), examples with ready-to-copy `tools call` invocations |
| `call <id> [--key=value ...]` | Invoke the tool. Flags become the JSON params object; `--format text\|json` picks output shape; write-effectful tools print an `[audit]` banner |

Output is plain text by default, shaped for LLM consumption (fenced code blocks, markdown lists) — no JSON to parse. Agents running in a terminal can call these as primitives alongside `rg` / `cargo check`.

### `sovereign atos`

Feature-layer orchestration — the Agent Task Orchestration System CLI. See [ATOS.md](ATOS.md) for the full flow; this is the command reference only.

| Subcommand | Description |
|---|---|
| `provision <id> --charter <path>` | Parse a charter, seed the feature + milestones |
| `next [<feature-id>]` | Find the next unfinished milestone and hand off to a driver (`claude` / `opencode`) |
| `start-milestone <id> --brief <path>` | Open a run, spawn the driver; `--red-team` for red-team mode |
| `end-milestone <id>` | Run the stop condition, close the run, write `milestone-<N>.md` |
| `archive <id> --reason <text>` | Mark a feature archived |
| `status [<id>]` | Feature list, or detailed status + artifact checklist for one feature |
| `promote <note-id> --to feature\|global` | Lift a note to a wider scope |
| `diff <feature-id> [--ordinal N]` | Side-by-side per-tool activity across A/B driver runs |
| `run-ab <feature-id> --brief <path> [--drivers claude,opencode]` | Run each driver against the same milestone, then diff |
| `probe-driver [--url <endpoint>]` | Trivial tool-use sanity check against an OpenAI-compatible server |
| `report <feature-id> [--section ...] [--out <path>]` | Render milestone / red-team / epistemic / all reports |
| `teardown <feature-id> [--auto] [--dry-run]` | Interactive note-classification pass; writes `epistemic-report.md` |
| `feature approve <id>` | Commonwealth-native approval fallback (no git commit required) |
| `spec diff <id>` | Unified diff of current spec vs. approved content |
| `spec accept <id> [--reason <text>]` | Accept current spec as new approved content, log a `deviation` note |
| `doctor` | Health check: repo, `.sovereign/`, DB schemas, plugin freshness, per-feature approval + drift |
| `install-plugin` | (Re)install the opencode plugin at `.opencode/plugins/sovereign-atos.ts` |

Related project-layer commands (under `sovereign project`) for the charter-level flow: `found`, `amend`, `phase pass N`, `audit`.

### `sovereign daemon`

Internal long-running service, managed by launchd (macOS) or systemd (Linux). You don't normally invoke this directly — `sovereign setup` registers it.

| Subcommand | Description |
|---|---|
| `run` | Run in the foreground; exits on SIGINT/SIGTERM |

Logs: `~/.sovereign/logs/daemon.log`.

## HTTP endpoints

`localhost:9741` serves both the OpenAI-compatible `/v1/*` API and the MCP JSON-RPC server at `/mcp`. `sovereign-server` exposes additional conversation/task APIs when run standalone:

| Method | Path | Description |
|---|---|---|
| GET | `/v1/models` | List available inference models |
| POST | `/v1/chat/completions` | OpenAI-compatible completion |
| GET | `/oicp/v1/capabilities` | OICP capability manifest |
| POST | `/mcp` | MCP JSON-RPC 2.0 endpoint |
| GET | `/mcp/stats` | Tool call counts (localhost only) |
| POST | `/v1/conversations` | Create a conversation (sovereign-server) |
| POST | `/v1/conversations/{id}/messages` | Send a message |
| GET | `/v1/conversations/{id}` | Get conversation with history |
| POST | `/v1/tasks/{id}/approve` | Approve a tool action |
| GET | `/v1/tools` | List available tools |
| GET | `/v1/conversations/{id}/stream` | WebSocket streaming |
