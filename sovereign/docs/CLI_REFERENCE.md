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
| `init [--no-git\|--yes-git]` | Set up code intelligence for the current workspace; prompt-and-offer `git init` when absent (respects `--no-git` / `--yes-git`, and remembers a prior declination via `lifecycle.git_declined_at_init`); soft-paths empty repos when `DESIGN.md` is present; installs the opencode ATOS plugin |
| `design [--import <path>] [--via <agent>] [--solo\|--stopgap] [--port <port>]` | Agent-collaborative `DESIGN.md` session against the Commonwealth daemon. Default launches opencode with the session brief primed; `--solo` drives structural-parser CLI prompts and writes `OPEN_QUESTIONS.md`; `--stopgap` is a provisional in-terminal chat (always flagged as such); `--import <path>` copies an existing doc into `<repo>/DESIGN.md` with diff-confirm |
| `plan [--allow-open]` | Compose `IMPLEMENTATION_PLAN.md` from `DESIGN.md` + `OPEN_QUESTIONS.md`; upsert rows into `.sovereign/plan.db` (`plan_items` table); defer stale rows from prior generations. Unanswered `OPEN_QUESTIONS.md` entries block unless `--allow-open` (then they surface as `Open risks` on the matching phase) |
| `charter [--print]` | Create or edit `.sovereign/CHARTER.md` — the team's free-form governance/onboarding doc. First invocation writes a minimal skeleton and opens `$EDITOR`; subsequent invocations just open the existing file. `--print` outputs the current file without spawning the editor |
| `status` | Show the status of code intelligence + ATOS scaffold (founded? current phase?) |
| `refresh` | Re-export the SCIP call graph |
| `serve` | Start a lightweight MCP server (no model required) |
| `install-hooks` | Upgrade (or install) the post-commit hook |
| `found [--design <path>] [--orchestrate]` | Default: four-stage founding conversation; writes `.sovereign/CHARTER.md` and `PHASES.md`, records answers as `decision` notes. Stage-1/Stage-2 predicates are signal-gated against `DesignSignals` extracted from `DESIGN.md`. `--orchestrate`: require `DESIGN.md` + answered `OPEN_QUESTIONS.md` + `IMPLEMENTATION_PLAN.md` + `CHARTER.md`, skip the questionnaire, elicit only the Phase-1 stop condition, then flip the lifecycle |
| `amend [charter\|design]` | `amend charter` (default): diff `CHARTER.md` section-by-section on save, run adversarial Q&A for changed sections, write amendment log + new hash. `amend design`: track edits to `DESIGN.md`'s curated sections (`Anchors`, `Data & interfaces`, `Open questions`), ask targeted adversarial questions, append the Q&A to `DESIGN.md`'s inline `## Amendment log` (newest on top; does NOT bump `charter_version`) |
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

### `sovereign enrich`

Admin harness for iterating on the v2 enrichment pipeline. **Provisional** — retires once v2 is promoted to production (see SYSTEM_OVERVIEW §12 Roadmap). Writes state under `~/.sovereign/enrichment/<corpus>/` and `~/.sovereign/indexes/<corpus>/`.

| Subcommand | Description |
|---|---|
| `init <corpus-id> --source <path> [--chapter-regex <pat>] [--pipeline <id>] [--chat-model <id>] [--embed-model <id>] [--dry-run] [--force]` | Scaffold the enrichment tree. Detects sections via `SectionedChunker`, writes `chapters.json` + `config.json` + `exemplars/` + `cache/` + `runs/`. `--dry-run` prints detected sections and exits. |
| `extract <corpus-id> [--chapters <a,b,c> \| --full]` | Phase 1: per-chapter question extraction. Subset runs write to `runs/` only; `--full` also updates `cache/questions.json`. |
| `cluster-questions <corpus-id>` | Phase 2: HDBSCAN over phase 1 question embeddings. Writes `cache/question-clusters.json`. |
| `name-concerns <corpus-id>` | Phase 3: name the canonical concern for each phase 2 cluster. Writes `cache/concerns.json`. |
| `cluster-chunks <corpus-id>` | Phase 4: embed + cluster paragraph chunks. Writes `cache/chunk-clusters.json`. |
| `extract-positions <corpus-id>` | Phase 5: grounded position extraction (aligns concerns ↔ chunk clusters by centroid cosine, top-3). Writes `cache/positions.json`. |
| `detect-tensions <corpus-id>` | Phase 6: pairwise tension detection between same-concern positions. Writes `cache/tensions.json`. |
| `detect-gaps <corpus-id>` | Phase 7: single-call gap identification across the atlas. Writes `cache/gaps.json`. |
| `cascade <corpus-id> --from <phase>` | Rerun `<phase>` and every downstream phase. `<phase>` is one of `questions`, `question-clusters`, `concerns`, `chunk-clusters`, `positions`, `tensions`, `gaps`. Phase 1 uses `--full` in cascades. |
| `query <corpus-id> "<text>" [--show-traversal] [--threshold <f>]` | Traverse the assembled atlas for a one-off query. Prints LOCATE (concern matches by cosine) / TRAVERSE (positions + tensions) / GROUNDING (passage ids). |
| `validate <corpus-id> --questions <path> [--threshold <f>] [--pass <f>]` | Run a `QueryBattery` (JSON list of questions) against the atlas. Prints a per-question score table and a headline pass-rate. |
| `promote <corpus-id> --phase <id> --run <path> --finding <id> --type <positive\|corrected\|negative> --rationale <text> [--selector <s>] [--model-output <json>]` | Append a curated finding from a run output into the per-phase exemplar bank. |
| `diff <corpus-id> <run-a.json> <run-b.json>` | Side-by-side compare of two phase 1 run outputs — added/removed questions per chapter, reveals + carriers changes. |
| `reset <corpus-id> [--from <phase>] [--full] [--include-exemplars] [--dry-run] [--yes]` | Clear phase caches + run outputs so you can re-iterate. Default: clears phases 2-7 and keeps phase 1 + exemplars + config + chapter manifest. `--from <phase>` customizes the starting point. `--full` wipes the entire tree + manifest (source text is preserved). `--include-exemplars` opts into clearing hand-crafted banks (default preserves them). Always prompts unless `--yes`; `--dry-run` previews without changes. |
| `show <corpus-id> <target> [--chapter <id>] [--concern <id>]` | Formatted view of any cached phase (targets: `phase1` … `phase7` or full names `questions`, `question-clusters`, `concerns`, `chunk-clusters`, `positions`, `tensions`, `gaps`). |
| `exemplars <corpus-id>` | Report per-phase bank counts and lint issues. |
| `status <corpus-id>` | Per-phase cache-freshness table (fresh / stale / never-run). |

Requires the Commonwealth daemon to be running at `localhost:9741`. `init` auto-resolves chat + embed model ids via `/v1/models` unless pinned explicitly.

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
