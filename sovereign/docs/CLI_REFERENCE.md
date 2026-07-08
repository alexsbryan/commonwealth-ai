# CLI Reference

Full flag and subcommand reference for the `sovereign` CLI. For a walk-through of the three-command workflow (`setup` / `project init` / `mesh create`), see the [README](../README.md). Every subcommand also accepts `--help` for per-command detail.

← [back to README](../README.md)

## Top-level invocation

```sh
sovereign <subcommand> [flags]
```

Run `svrn --help` for the live list of subcommands. There is no interactive REPL — bare `sovereign` prints usage and exits; use `svrn chat` for an interactive shell.

Under the hood, `sovereign` is a thin dispatcher over four binaries; that split only matters when you're building from source — see [DEVELOPMENT.md](DEVELOPMENT.md#the-cli-binaries).

## Subcommand reference

### `svrn setup`

First-run onboarding. Detects hardware, downloads models, starts the daemon.

| Flag | Description |
|---|---|
| `--yes`, `-y` | Non-interactive; accept recommended choices |
| `--reset` | Wipe config and re-run (uninstalls service first) |
| `--data-dir <path>` | Override the default data root (`~/.sovereign`) |

### `svrn project`

Per-project code intelligence **and** the project-layer half of ATOS (charter + phases). See [CODE_INTELLIGENCE.md](CODE_INTELLIGENCE.md) for the indexing flow and [ATOS.md](ATOS.md) for the charter flow.

| Subcommand | Description |
|---|---|
| `init [--name <id>] [--no-scip] [--no-hooks] [--no-claude-config] [--workspace-root <dir>] [--port <port>]` | Set up code intelligence for the current workspace: symbol index, SCIP call graph, generated `.sovereign/`, `.claude`/`.opencode` wiring, daemon registration. Also available as `svrn init`. See [CODE_INTELLIGENCE.md](CODE_INTELLIGENCE.md). |
| `design [--import <path>] [--via <agent>] [--solo\|--stopgap] [--port <port>]` | Agent-collaborative `DESIGN.md` session against the Commonwealth daemon. Default launches opencode with the session brief primed; `--solo` drives structural-parser CLI prompts and writes `OPEN_QUESTIONS.md`; `--stopgap` is a provisional in-terminal chat (always flagged as such); `--import <path>` copies an existing doc into `<repo>/DESIGN.md` with diff-confirm |
| `plan [--allow-open]` | Compose `IMPLEMENTATION_PLAN.md` from `DESIGN.md` + `OPEN_QUESTIONS.md`; upsert rows into `.sovereign/plan.db` (`plan_items` table); defer stale rows from prior generations. Unanswered `OPEN_QUESTIONS.md` entries block unless `--allow-open` (then they surface as `Open risks` on the matching phase) |
| `charter [--print]` | Create or edit `.sovereign/CHARTER.md` — the team's free-form governance/onboarding doc. First invocation writes a minimal skeleton and opens `$EDITOR`; subsequent invocations just open the existing file. `--print` outputs the current file without spawning the editor |
| `status` | Show the status of code intelligence + ATOS scaffold (founded? current phase?) |
| `refresh [--rebuild-index]` | Re-export the SCIP call graph. Auto-rebuilds the LanceDB corpus index when the on-disk meta is stale (missing `_corpus_meta.json`, or `embedding_dimensions == 768` from the legacy zero-vector code-index path); otherwise keeps LanceDB work fast by skipping it. `--rebuild-index` forces a full LanceDB rebuild even when the meta looks current. |
| `serve` | Start a lightweight MCP server (no model required) |
| `install-hooks` | Upgrade (or install) the post-commit hook |
| `found` | **Retired** — founding is implicit now: `svrn init` plus a committed spec is sufficient |
| `amend [charter\|design]` | `amend charter` (default): diff `CHARTER.md` section-by-section on save, run adversarial Q&A for changed sections, write amendment log + new hash. `amend design`: track edits to `DESIGN.md`'s curated sections (`Anchors`, `Data & interfaces`, `Open questions`), ask targeted adversarial questions, append the Q&A to `DESIGN.md`'s inline `## Amendment log` (newest on top; does NOT bump `charter_version`) |
| `phase status` | Show founding state + current phase |
| `phase pass [N]` | Run phase N's stop condition from `PHASES.md`; write `phase-N.md` on green |
| `audit` | One-page reviewer rollup: founding state, phases passed, notes by kind, open questions, drift status |
| `register` | Register the project with the sovereign daemon for FS-watch + auto-refresh |
| `unregister` | Remove a project from the daemon's watch list |
| `list` | List every project the daemon is watching |
| `watch status\|restart\|logs` | Inspect or control the daemon's watcher for this project |
| `install-hooks` | **Deprecated** — daemon now owns freshness. Still installs the post-commit hook for legacy workflows |

> `svrn project status` now forwards to top-level `svrn status` (old name still works; set `SOVEREIGN_QUIET_DEPRECATIONS=1` to silence the hint).

### `svrn mesh`

Manage the local Commonwealth mesh.

| Subcommand | Description |
|---|---|
| `create [--name <name>]` | Promote the solo mesh to a joinable mesh; print invite |
| `join <arg>` | Join an existing mesh (bare key, https url, or sovereign://) |
| `rotate` | Generate a new shareable join key (invalidates previous) |
| `status` | Show mesh members, hosted knowledge, loaded models |
| `balance` | Render the dimensional contribution ledger (inference / knowledge / network, never collapsed) |
| `leave` | Leave the current mesh |
| `logs` | Show mesh daemon logs |
| `fetch-model <name>` | Pull a GGUF from a mesh peer over the tailnet |
| `warm-cache <gguf>` | Pre-seed the RPC tensor cache from a local GGUF (offline) |

### `svrn corpus`

Manage knowledge corpora. See [KNOWLEDGE_BASES.md](KNOWLEDGE_BASES.md) for tier details.

| Subcommand | Description |
|---|---|
| `list` | List installed and available corpora |
| `install <id>` | Install a corpus (e.g. `wikipedia`) |
| `remove <id>` | Remove an installed corpus |
| `status` | Show shard status for all corpora |
| `reconstruct-manifest <id>` | Rebuild source-file manifest before collaborative ingestion |

### `svrn alignment`

Mesh-replicate the user's `~/.claude/` workspace state (plans, auto-memory entries, plan template) and `~/.sovereign/notes.db` between the user's own daemons. Newest mtime wins per logical key, so two machines that edit the same plan or note converge on the newer copy after a mesh tick. The post-merge projector materializes received chunks back to disk on the receiving daemon, so a fresh machine reaches parity in one ingest. See [PLAN_ALIGNMENT.md](PLAN_ALIGNMENT.md) for the design rationale.

| Subcommand | Description |
|---|---|
| `migrate` | Tar a backup to `~/.sovereign/backups/`, then submit the `alignment` corpus install. The daemon ingests in the background; peer convergence happens automatically via existing mesh hooks. |
| `migrate --dry-run` | Walk the alignment scope and print what would be exported (files + notes rows) without writing a backup or touching the daemon. |
| `status` | List paths in scope, count of files and notes that would be exported, and the local alignment corpus state if it has been ingested. |

**Operator flow** (run on both machines, order doesn't matter — reconciliation is symmetric):

```bash
sovereign alignment migrate --dry-run    # preview scope
sovereign alignment migrate              # backup + ingest
sovereign alignment status               # check progress
```

**Recovery.** The backup tar at `~/.sovereign/backups/alignment-pre-migrate-<ts>.tar` restores the original state with `tar -xf <path> -C $HOME` (the archive uses `~/`-relative paths). The migration is idempotent — re-running converges, doesn't compound.

**Sync mechanics.** This CLI lands the local state on the alignment corpus. The cross-machine merge happens via the daemon's existing hooks (`auto_recover` after a stranded-partition merge, `index_transfer` after a peer pull); the projector then writes received chunks back to `~/.claude/` and upserts `notes://...` rows into `~/.sovereign/notes.db` automatically.

### `svrn mobile`

Serve the phone-facing API, riding on the daemon's already-loaded models. The phone talks HTTP + WebSocket to this bridge; no separate model load.

| Subcommand | Description |
|---|---|
| `serve` | Start the phone-facing API server (HTTP + WS) backed by the daemon's models |
| `status` | Show the mobile bridge status |
| `pair` | Print the pairing string a phone uses to connect |

### `svrn code`

Lower-level code-intelligence primitives. `project init` wraps these for the typical flow.

| Subcommand | Description |
|---|---|
| `index <path>` | Index a local repository with tree-sitter |
| `watch <corpus-id>` | Run a filesystem watcher that re-indexes on save |
| `mcp-status` | Ping the local MCP server and list exposed tools |
| `search <query>` | (placeholder — use `svrn chat ask` or the MCP `code_search` tool for now) |

### `svrn doctor`

Diagnose setup and daemon health across Sovereign / Commonwealth / OmO layers.

| Flag | Description |
|---|---|
| `--fix` | Attempt automatic repair for failing checks |
| `--watch` | Re-run periodically (every 5s) |
| `--json` | Emit structured JSON for scripting |

### `svrn status`

Top-level health rollup for the current project: code intelligence, daemon, watcher state, drift posture. Replaces `svrn project status` (old name still forwards here).

### `svrn reflect` (alias: `svrn notes`)

Review session reflections and retire ones that are no longer relevant. The canonical name is now `notes`; `reflect` still works.

| Flag | Description |
|---|---|
| `--since <Nd\|Nh>` | Period to analyse (default: 30d) |
| `--tool <name>` | Filter signals to one tool |
| `--raw` | Print full reflection prose |
| `--todos` | List open todo notes only |
| `--retire --tool <name> --reason <why>` | Retire matching reflections |

### `svrn recipe`

Run and curate corpus ingestion recipes.

| Subcommand | Description |
|---|---|
| `list` | List all corpora available in the registry. `--offline` skips live registry refresh |
| `test <path>` | Run the full test harness against a recipe file. Flags: `--sample-size N`, `--output <path>`, `--offline`, `--verbose`, `--params k=v[,...]`, `--params-file <json>` |
| `validate <path>` | Validate recipe fields without downloading data. `--offline` skips registry fetch |
| `publish <path>` | Add a recipe to `~/.sovereign/recipes/registry.toml`. `--submit-pr` also drafts a community-registry PR via `gh` |

### `svrn pipeline`

Generic ingestion-pipeline driver — durable worklist + retry + pause-resume. Drives any recipe whose `[enrich].command` is a `{key}`-templated shell command.

| Subcommand | Description |
|---|---|
| `run <recipe.toml>` | Seed + sweep + drive the recipe to completion. SIGINT/SIGTERM drains in-flight units, then exits cleanly. Re-running picks up where the previous run left off |
| `status <recipe-id>` | Print pending/done/failed counts, last-hour throughput, ETA, failure buckets |
| `list` | List every recipe-id known to the worklist DB |
| `pod up` | Launch a Vast.ai pod with the sovereign CUDA image, join the mesh, register in the cost ledger |
| `pod list` | Show every pod the ledger knows about with accrued cost |
| `pod down <vast-id>` | Destroy a Vast pod, close its ledger entry, print final cost |

Global flags: `--db <path>` (default `~/.sovereign/pipeline.db`), `--seed-only`, `--slugs <path>`, `--key <slug>` (repeatable). Failures bucket into `timeout` / `refused` / `vram_thrash` / `mismatch` / `model_missing` / `unknown` and retry up to `[dispatch].max_attempts` before landing in `failed`. Add an `[schedule]` block with `active_hours = "HH:MM-HH:MM"` to auto-pause outside that window.

### `svrn atlas`

Atlas-style structural enrichment of an installed corpus (Wikipedia today). Operates against an already-installed corpus index; install first via `svrn corpus install <id>` or `svrn recipe`.

| Subcommand | Description |
|---|---|
| `wikipedia` | Layer 0: build the link graph from Wikipedia extractor metadata |
| `budget` | Show or set the per-corpus Tier-2 enrichment budget (top-N articles) |
| `status` | Per-corpus atlas readiness — atom counts, Tier-2 progress, token spend |

The graph DB lives at `<data-dir>/indexes/<corpus-id>/wikipedia_graph.db`.

### `svrn bench`

Throughput + correctness benchmarks for enrichment LLM tasks. Operates against the running daemon at `localhost:9741`; the model under test is whichever `[models].primary` the daemon was started with.

| Subcommand | Description |
|---|---|
| `atlas` | Run atlas Phase 1 + short-call tasks against the loaded primary model |

See [BENCHMARKING.md](BENCHMARKING.md) for the broader embed-throughput runbook.

### `svrn search-gym`

Correctness harness for web-search-during-inference, scored against recorded mock-replay fixtures (no live network).

| Subcommand | Description |
|---|---|
| `run` | Run the search-gym bank against the configured model; score tool-use correctness |
| `calibrate-judge` | Calibrate the LLM judge against a labeled set before scoring |

### `svrn knowledge-gym`

Correctness harness for the unified `knowledge_lookup` tool (mock-replay).

| Subcommand | Description |
|---|---|
| `run` | Run the knowledge-gym bank and score `knowledge_lookup` tool-use correctness |

### `svrn eval`

Run a question bank against a corpus and measure retrieval quality. Retrieval-only — does not call the chat model.

| Subcommand | Description |
|---|---|
| `run` | Execute a bank and print per-question + rollup scores |

Bank format lives at `sovereign-recipes/<corpus>/eval/*.toml`. Daemon at `localhost:9741` required; override with `--daemon`.

### `svrn git-archaeology`

Walk a code corpus' git history and emit per-atom provenance + co-evolution edges. Standalone surface; also called from `svrn drift detect` to fold provenance into the unified drift digest. See [GIT_ARCHAEOLOGY.md](GIT_ARCHAEOLOGY.md).

```
sovereign git-archaeology <corpus-id> [--source-path <dir>] [--output <md>] [--threshold N] [--min-joint N]
```

| Flag | Description |
|---|---|
| `--source-path <dir>` | Override the source path stamped in `_corpus_meta.json`. Must live inside a git repository |
| `--output <md>` | Write markdown digest here; JSON sidecar lands at `<output>.json`. Default: stdout for markdown, `~/.sovereign/indexes/<corpus>/atlas/git_archaeology.json` for JSON |
| `--threshold N` | Co-evolution jaccard threshold in `[0.0, 1.0]`. Default `0.5` |
| `--min-joint N` | Minimum joint-commit count for a co-evolution pair. Default `5` — drops scaffolding-era false positives |

Reads the structural atlas from `~/.sovereign/indexes/<corpus>/atlas/atoms.json` — build it first via `svrn enrich ingest <id> --source-corpus <id>`.

### `svrn archaeology-eval`

Re-verify the claims `git-archaeology` makes against git itself. Witness checks + baseline diff + curated regression cases (inquiries). See [ARCHAEOLOGY_EVAL.md](ARCHAEOLOGY_EVAL.md).

```
sovereign archaeology-eval <atlas-corpus-id> [--inquiry <toml>...] [--baseline <path>] [--output <md>] [--save-baseline]
```

| Flag | Description |
|---|---|
| `--inquiry <toml>` | Curated regression case (TOML). Repeatable. `file_globs` selects atoms; `keywords` / `authors` / `date_range` add inquiry-specific witnesses |
| `--baseline <path>` | Previous run's eval report (JSON). Default `~/.sovereign/eval/baselines/<atlas>.eval.json` |
| `--output <md>` | Markdown report path. Default `~/.sovereign/eval/<atlas>.eval.md` |
| `--save-baseline` | After running, save current report as new baseline |

Appends one CSV row per run to `~/.sovereign/eval/history.csv`. Exit code is non-zero on any inquiry failure or fabrication — CI-friendly.

### `svrn drift`

Two surfaces under one verb:

- **`svrn drift <feature-id>`** / **`svrn drift accept <feature-id> --reason X`** — ATOS spec drift. Diff approved vs. on-disk `spec.md`; accept current spec as new approved content. Replaces `svrn atos spec diff` / `spec accept`.
- **`svrn drift detect --code <path> --narrative <doc>...`** — narrative-vs-code architectural drift. Produces a unified drift digest. See [DRIFT_DETECTION.md](DRIFT_DETECTION.md).

| `drift detect` flag | Description |
|---|---|
| `--code <path>` | **Required.** Path to codebase. Indexed if not cached |
| `--narrative <doc>` | **Required, repeatable.** Markdown narrative doc; each becomes its own atlas |
| `--output <md>` | Markdown report path |
| `--project-id <id>` | Override the project id (default: derived from `--code`) |
| `--chat-model <slot>` | Chat-slot probe. Default `fast` (scales without primary); pass `primary` for peak quality at ~5–10× wall time |

### `svrn mcp`

Inspect and test configured MCP servers.

| Subcommand | Description |
|---|---|
| `list` | List configured MCP servers with status |
| `test <server>` | Test connection to a named server |
| `tools [server]` | List available MCP tools |

### `svrn tools`

Invoke the 24 sovereign code-intelligence tools directly from the shell. Same `Tool::execute()` as the MCP path — use this when the daemon isn't running, when scripting, or for self-documenting `--help`. See [ARCH_PRINCIPLES.md](../ARCH_PRINCIPLES.md) §2 for the behavioural properties each tool declares.

| Subcommand | Description |
|---|---|
| `list` | Print the tool manifest, grouped by Effect × Scope (Read/Write × Session/Persistent/External) |
| `describe <id>` | Full descriptor for one tool: parameters JSON Schema, output schema (compose-able keys), examples with ready-to-copy `tools call` invocations |
| `call <id> [--key=value ...]` | Invoke the tool. Flags become the JSON params object; `--format text\|json` picks output shape; write-effectful tools print an `[audit]` banner |

Output is plain text by default, shaped for LLM consumption (fenced code blocks, markdown lists) — no JSON to parse. Agents running in a terminal can call these as primitives alongside `rg` / `cargo check`.

### `svrn chat`

Terminal mirror of the desktop chat flow. Streams through the same `Runtime::handle_message_stream` path the Tauri app uses — same intent classification, same multi-source retrieval (conversation-history + folder corpora + `sep` + web), same conversation persistence — so a flailing chat case in the GUI can be reproduced and diagnosed at the command line. Talks to the daemon over HTTP (no in-process model load).

Code corpora (`sovereign`, `commonwealth-ai`, `corpus-engine`, …) are filtered out of chat retrieval by default — they're served by the dedicated MCP code-intelligence tools. See `CorpusKind` in `corpus-engine/src/types.rs` for the classification.

| Subcommand | Description |
|---|---|
| `ask "<question>" [--conversation <id>] [--format text\|json] [--show-reasoning]` | One-shot turn. Streams the answer to stdout; writes the provenance footer (searched corpora · latency · intent · backend) and numbered source list to stderr. `--format json` dumps the full message + metadata payload. |
| `session [--conversation <id>] [--show-reasoning]` | Interactive REPL over a single persistent conversation id. Type `quit` / `exit` / Ctrl-D to end; blank lines are ignored. Follow-up turns inherit the conversation context. |
| `inspect "<question>" [--limit <N>] [--corpus <id>] [--snippet <N>] [--format text\|json]` | **Diagnostic.** Runs the retrieval stage *without* the LLM. Prints the query embedding dims, every installed corpus with its kind/dims/model, and top-N hits per corpus with scores + snippets. Code corpora are annotated `[omitted from chat by default]` so you can see the potential hit without it polluting actual retrieval. Use when the model is quoting sources that don't match the question. |
| `list [--limit <N>] [--offset <N>]` | List recent conversations from the state store. |
| `show <conversation-id> [--show-reasoning]` | Dump a conversation's turns + persisted provenance + retrieved-chunks metadata. |

#### Global flags

Accepted by every subcommand:

| Flag | Default | Description |
|---|---|---|
| `--daemon <url>` | `http://localhost:<SetupConfig.daemon.client_port>` | Override the daemon base URL |
| `--data-dir <path>` | `SetupConfig.data.dir` | State-store root (`sovereign.db` lives here) |
| `--chat-model <id>` | `SetupConfig.models.primary` stem | Chat model id sent on every request |
| `--embed-model <id>` | `SetupConfig.models.embed` stem | Embedding model id used for retrieval |

Model ids default to the filename stems of the files the daemon actually loaded — `qwen-embedding-0.6b.gguf` → `qwen-embedding-0.6b`. This sidesteps a historical race where `/v1/models` advertised both a locally-loaded and a mesh-peer version of the same model under different ids and the CLI's first-match heuristic picked non-deterministically.

#### Output conventions

- **Answer text** streams to **stdout** chunk-by-chunk as the model produces it. `--format json` buffers the whole turn and prints one structured payload on completion.
- **Provenance chrome** (the `─── conversation ───` banner, `Searched …` header, `--- sources (N) ---` footer, reasoning disclosure) writes to **stderr**. `chat ask "…" > answer.txt` captures just the answer.
- `<think>…</think>` blocks are split out client-side (the desktop does the same split in `parse-message.ts`). Collapsed by default into a `▶ reasoning (N blocks, M chars)` handle; `--show-reasoning` prints each block as quoted lines.

#### Daemon requirement

The daemon at `localhost:9741` must be reachable — `chat` probes `/v1/models` on bootstrap and exits with a remediation hint (`Start it with sovereign daemon run, or pass --daemon <url>`) if the probe fails. Chat + embed + MCP tool calls all flow through the daemon's OpenAI-compatible surface; no model is loaded in-process.

### `svrn solve`

Give the daemon a coding goal, get a green tree back (spec: [`docs/specs/SOLVE_UX.md`](../../docs/specs/SOLVE_UX.md), guide: [`SOLVER_FOR_PI_USERS.md`](./SOLVER_FOR_PI_USERS.md)). The daemon makes the goal test-shaped — driving your failing tests green if you have them, writing the one failing test that pins the goal if you don't — then iterates until the tests pass. Review the result with `git diff` in the workdir.

```
svrn solve <workdir> "<goal>" [--watch] [options]
svrn solve --status <job_id>
svrn solve --cancel <job_id>
```

Submits to `POST /v1/solve/jobs` on the daemon and prints the job id plus what was detected (framework, test command, model). `--watch` streams the SSE round events — one line per round with the winning candidate and the passing/failing counts — until the job ends.

| Flag | Description |
|---|---|
| `--watch` | Stream rounds live until the job finishes |
| `--verb <fix\|pin\|split>` | Path override when the default inference isn't what you meant: `fix` = only drive existing failing tests green; `pin` = only write the failing test; `split` = shrink oversized files |
| `--max-lines <n>` | With `--verb split`: the per-file line budget |
| `--suite <unit\|e2e>` | Steer to the browser (Playwright) suite when the project has both — unit stays the default |
| `--test-command <cmd>` | Override the auto-detected test command |
| `--model <id>` | Override the daemon's primary model |
| `--force` | Solve on a dirty tree (uncommitted changes) |
| `--daemon <url>` | Daemon base URL (default from setup config, `http://localhost:9741`) |
| `--status <job_id>` | Print a job's state + rounds + result as JSON |
| `--cancel <job_id>` | Cancel a running job |

The workdir must be a git repository with a clean tree (or `--force`); the daemon refuses system paths outright, allows one running job per workdir and two globally, and never edits outside the workdir. Exit code: 0 on `reached`/`improved`, 1 on `stalled`/`no_baseline`/`errored`, 130 on cancel.

Web apps work through the same two fields (spec: [`docs/specs/SOLVE_PLAYWRIGHT.md`](../../docs/specs/SOLVE_PLAYWRIGHT.md)): a `playwright.config.{ts,js}` detects as the `playwright` framework with default command `CI=1 npx playwright test --reporter=line --retries=0 --workers=1` — retries off so flake reads as failing, `CI=1` so every candidate gets a fresh `webServer` instead of silently reusing a running dev server. Browser trials sample 3 candidates serially with a 300s per-run budget. Failure feedback includes Playwright's aria snapshot of the page (`error-context.md`) so the model reads the UI as text. When a project has both a unit framework and Playwright, unit stays the default and the job's `detected` says so — steer with `--suite e2e`.

The same engine is exposed to agents as the `solve` / `solve_status` / `solve_cancel` MCP tools on the daemon's `/mcp` surface, and raw over HTTP: `POST /v1/solve/jobs`, `GET /v1/solve/jobs/{id}`, `GET /v1/solve/jobs/{id}/events` (SSE), `DELETE /v1/solve/jobs/{id}`.

### `svrn enrich`

Build, query, and audit v2 atlas enrichments of a corpus. Writes state under `~/.sovereign/enrichment/<corpus>/` (phase caches + run outputs) and `~/.sovereign/indexes/<corpus>/atlas/` (resolved atoms + edges + trajectories + configurations + schema-validation + cross-corpus edges).

The full architecture — seven atom types, seven edge types, deterministic resolver, LLM-driven Phase 8 configurations, cross-corpus bridges, §12 schema-revision protocol — lives in [`corpus-engine/ENRICHMENT_V2.md`](../../corpus-engine/ENRICHMENT_V2.md). This section documents only the command-line surface.

#### Primary flow

The normal loop is `init` once per corpus, then `build` to run the whole pipeline, then `query` / `report` / `review` / `bridge` to consume the result.

| Subcommand | Description |
|---|---|
| `init <corpus-id> --source <path> [--pipeline <id>] [--chapter-regex <pat>] [--chat-model <id>] [--embed-model <id>] [--dry-run] [--force]` | Scaffold the enrichment tree. Detects sections via `SectionedChunker`, writes `chapters.json` + `config.json` + `exemplars/` + `cache/` + `runs/`. `--pipeline` accepts any registered id (`literary_atlas`, `philosophy_atlas`, …); default `literary`. `--dry-run` prints detected sections and exits. |
| `build <corpus-id> [--chapters <ids> \| --full] [--skip <step>...] [--dry-run]` | One-shot: run every atlas phase in sequence — **seed → extract → cluster → name → resolve → tensions → gaps → configure → report**. Subset runs are promoted to cache so downstream phases have inputs. `--skip <step>` is repeatable (valid steps: `seed`, `extract`, `cluster`, `name`, `resolve`, `tensions`, `gaps`, `configure`, `report`). `--dry-run` prints the planned step sequence. |
| `query <corpus-id> "<text>" [--json]` | Classify + traverse a natural-language question against the resolved atlas; print an assembled brief. Zero LLM calls — the classifier is a keyword + known-entity-name matcher, the traversal is deterministic. `--json` emits the raw `TraversalResult`. |
| `report <corpus-id> [--json]` | Print the §12 schema validation report for one corpus across 8 dimensions (coverage, depth distribution, confidence histogram, atom-type utilisation, orphans, discourse distribution, cross-corpus connectivity, deterministic gap counts). Writes `atlas/schema_validation.json`. |
| `review <corpus-a> <corpus-b> [<corpus-c>...]` | Compare N corpora's schema-validation reports. Gap signatures present in ≥ 2 corpora surface as **schema-revision candidates** with targeted recommendations per kind; signatures present in exactly one corpus surface as **prompt-tuning candidates**. |
| `bridge <local> <peer> [--explain <edge-id>]` | Detect `Grounding` edges between two resolved atlases. Prints the glass-box `CrossCorpusReport` (candidates scanned, matches accepted, rejections grouped by reason, sample rejections with folded forms). `--explain <edge-id>` dumps the full `MatchTrace` for one accepted edge — signal path, confidence, alternatives considered. |

#### Individual phases

For debugging, partial re-runs, and iterating on a single prompt. `build` orchestrates these in order; calling them one-at-a-time lets you pause between phases to inspect outputs or tune exemplars.

| Subcommand | Description |
|---|---|
| `seed <corpus-id>` | Stage 1a: extract the canonical seed entity list from the first section. Writes `cache/seed.json`. Threaded into every subsequent Phase 1 prompt to prevent entity-name drift across sections. |
| `extract <corpus-id> [--chapters <ids> \| --full] [--terse]` | Phase 1: per-section atlas extraction (six facets — entities, entity-states, relations, relation-states, events, claims, questions). Subset runs write to `runs/` only; `--full` updates `cache/questions.json`. `--terse` uses the schema-only retry variant for chapters whose default pass hit a think-truncation. |
| `cluster <corpus-id>` | Phase 2: facet-typed clustering over the Phase 1 sketches (question / claim / entity-state / relation-state / event). Writes `cache/atlas-clusters.json`. |
| `name <corpus-id>` | Phase 3: one LLM call per cluster to name it with facet-specific vocabulary (thematic inquiry / position / conceptual arc / dialectical dynamic / argumentative thread). Writes `cache/atlas-named-clusters.json`. |
| `resolve <corpus-id> --phase <3a\|3b\|all>` | Phase 3a/3b: resolve atoms + edges + trajectories from the Phase 1 sketches. `--phase all` runs entity/event resolution (3a) + state/relation/claim/question resolution (3b) in one pass. Writes `atlas/atoms.json`, `atlas/edges.json`, `atlas/trajectories.json`. |
| `tensions <corpus-id>` | Phase 6 (deterministic): select tension candidates via intra-cluster + claim/claim entity-overlap + claim/state entity-overlap. Writes `atlas/tension_candidates.json`. The LLM classifier that promotes candidates into real `Tension` edges is a follow-up. |
| `gaps <corpus-id>` | Phase 7 (deterministic): detect structural gaps across 3 kinds — `transition_without_trigger`, `ungrounded_claim`, `open_question`. Writes `atlas/gaps.json`. |
| `configure <corpus-id>` | Phase 8 (LLM, opt-in per pipeline): 0–3 interpretive `Configuration` atoms over the atlas structure. Every configuration must carry an `interpretive_note` articulating alternative readings (the Ricoeur constraint per spec §1.2). Writes `atlas/configurations.json` and merges atoms into `atlas/atoms.json`. |

#### Utilities

| Subcommand | Description |
|---|---|
| `status <corpus-id>` | Per-phase cache-freshness table (fresh / stale / never-run). |
| `show <corpus-id> <target> [--chapter <id>] [--concern <id>]` | Formatted view of any cached phase output. |
| `exemplars <corpus-id>` | Report per-phase exemplar-bank counts + lint findings. |
| `reset <corpus-id> [--from <phase>] [--full] [--include-exemplars] [--dry-run] [--yes]` | Clear phase caches + run outputs to re-iterate. Default clears phases 2-7 and keeps Phase 1 + exemplars + config + manifest. `--full` wipes the whole enrichment tree (source text preserved). `--include-exemplars` opts into clearing hand-curated banks. Always prompts unless `--yes`. |

#### Daemon requirement

The Commonwealth daemon at `localhost:9741` is required for LLM phases (`seed`, `extract`, `name`, `configure`) and for `init` (which resolves chat + embed model ids via `/v1/models`). Pure-Rust phases — `cluster`, `resolve`, `tensions`, `gaps`, `query`, `report`, `review`, `bridge` — run offline once the atlas is resolved.

#### Legacy v1 surface

The pre-atlas command set (`cluster-questions`, `name-concerns`, `cluster-chunks`, `extract-positions`, `detect-tensions`, `detect-gaps`, `cascade`, `legacy-query`, `validate`, `promote`, `diff`) remains callable by exact name for corpora mid-flight on the v1 questions/concerns/positions path. It is hidden from the default `--help` and scheduled to retire once no active corpus depends on it.

### `svrn govern`

Common-law governance over a corpus — an event-sourced oplog of tensions and resolutions, with grounded Q&A over the active (non-superseded) rule set. Daemon at `localhost:9741` required for `ask`.

| Subcommand | Description |
|---|---|
| `seed` | Seed the governance oplog from a corpus's atlas atoms |
| `tensions` | Surface candidate tensions (conflicting rules / positions) |
| `resolve` | Record a resolution that supersedes or reconciles rules |
| `accept` | Accept a resolution into the active rule set |
| `ask "<question>"` | Grounded Q&A over the active rule set (dead/superseded law excluded) |

### `svrn atos`

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

Related project-layer commands (under `svrn project`) for the charter-level flow: `found`, `amend`, `phase pass N`, `audit`.

### `svrn daemon`

Long-running service, managed by launchd (macOS) or systemd (Linux). Lives in the `sovereign-cli-daemon` sibling binary; `svrn install-service` registers it with the OS service manager. You don't normally invoke this directly.

| Subcommand | Description |
|---|---|
| `run` (or bare `daemon`) | Run in the foreground; exits on SIGINT/SIGTERM |
| `start` / `stop` / `status` / `restart` | Lifecycle management against the installed service |
| `reload` | Apply config changes without a restart |
| `--setup-only` | Run the first-boot wizard and exit (what `svrn setup` aliases to) |

Logs: `~/.sovereign/logs/daemon.log`. Rotated in-process — copy-truncate, 10 MiB cap, 5 backups, 30-min sweep loop; preserves the inode for launchd-held FDs.

### `svrn install-service`

Register the daemon with the OS service manager — launchd on macOS, systemd on Linux — so it starts at login and stays running across logouts. Run once after `svrn setup`. Lives in the `sovereign-cli-daemon` sibling.

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
