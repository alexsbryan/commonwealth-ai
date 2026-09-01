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
| `--data-dir <path>` | Override the default data root (`~/.svrnmesh`) |

### `svrn model`

See and change the models the daemon loads, without hand-editing `config.toml` and without the non-obvious restart. Changes are written to the `[models]` config and **hot-applied** to a running daemon via the admin reload endpoint (models swap with no restart); if the daemon isn't running, they take effect on its next start. A `<file>` is an absolute path or a bare filename resolved against the models dir (`~/.svrnmesh/models`, with or without a `.gguf` suffix) and is validated to exist before anything is written.

| Subcommand | Description |
|---|---|
| `list` (default) | Show the configured slots (primary / fast / embed / code / extras / context) and, when the daemon is running, which are currently `[loaded]` |
| `set <primary\|fast\|embed\|code> <file>` | Point a slot at a model file, then apply it live |
| `unset <fast\|code>` | Clear an optional slot (it falls back to the primary); `primary`/`embed` are required and cannot be cleared |
| `set-extra <name> <file>` | Add or replace a named always-resident extra slot |
| `rm-extra <name>` | Remove a named extra slot |
| `context <n\|auto>` | Set the context window (`n_ctx`) applied to all slots, or `auto` for the default |

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
| `rotate [--force]` | Mint a new invite key. Existing members stay connected — rotation changes only who may JOIN |
| `list [--json]` | Show every mesh this node has joined; the active one is marked |
| `switch <mesh>` | Park the active mesh and bring another joined one up |
| `forget <mesh>` | Drop a parked mesh from this node (refuses on the active one) |
| `status` | Show mesh members, hosted knowledge, loaded models |
| `balance` | Render the dimensional contribution ledger (inference / knowledge / network, never collapsed) |
| `leave` | Leave the current mesh |
| `logs` | Show mesh daemon logs |
| `fetch-model <name>` | Pull a GGUF from a mesh peer over the tailnet |
| `warm-cache <gguf>` | Pre-seed the RPC tensor cache from a local GGUF (offline) |
| `plan <gguf>` | Work out whether a model fits, and which machine holds what — before you commit |
| `bench` | Measure how fast the model you are running actually decodes, and record it |

`svrn mesh plan` answers the question you have before you download 80 GB: will this
run on the machines I have? It reads only the GGUF's header table, so it needs no
daemon, no GPU, and no model load, and it answers instantly even for a split
measured in hundreds of gigabytes.

```sh
svrn mesh plan the-big-one.gguf --devices 64,32,32   # a mesh you're considering
svrn mesh plan the-big-one.gguf --from-mesh          # the mesh you actually have
```

It lays the model's blocks across the machines by byte mass — not block count, which
matters on a mixture-of-experts model where one block can outweigh another many times
over — and reports what each machine would hold against what it has. `--host <idx>`
moves the output head; `--headroom <f>` explores a tighter or safer pack than the one
the load is configured for; `--json` emits the whole plan for scripting. Exit `0` if
it fits, `1` if it doesn't, `2` on bad arguments.

The report also says what it *cannot* tell you. Speed is reported only where a real
measurement exists for that exact model on that exact split; otherwise it says "not
measured" and names the command that would measure it, rather than offering an
estimate. A `--devices` plan describes hardware that isn't present, so it is never
eligible for a measurement at all.

`svrn mesh bench` is the command that produces those measurements, and it is the
counterpart to `plan` in one specific way: **it measures the configuration you are
running, and never loads the one it wants to measure.** There is no slot to select,
so there is no slot to get wrong — which matters because this daemon keeps a small
always-hot model beside the big one, and a request that lands on the small one comes
back fast, successful, and meaningless.

```sh
svrn mesh bench                       # measure whatever is loaded right now
svrn mesh bench the-big-one.gguf      # the same, but fail if that isn't what's loaded
svrn mesh bench --history             # what has this machine already measured?
```

Passing a GGUF is an *assertion*, not a selection: it is fingerprinted from its header
and compared against the resident model, and a mismatch exits `3` naming the config
line to change. The measurement itself fires real streaming completions at the real
HTTP surface and times the frames as they arrive, so the number includes the actual
split and the actual network path. Decode rate is steady state; time to first token is
reported beside it rather than smeared into it.

Nine validity guards run on every measurement — which slot served it, per-frame
timing, placement unchanged across the run, peer liveness before *and* after, a canary
first, host survival, a floor on frame count, inter-trial spread, and a complete finish
reason. Each one exists because it caught a real false result. A run that trips any of
them is still written down, so the failure can be inspected, but it is never served
back to `plan`: keeping failures is what stops the tool becoming retry-until-lucky.

The first of those deserves a note, because the obvious version of it does nothing.
The `model` field on a streaming response is an echo of what the client asked for, so
checking it proves only that the request was addressed correctly — a reply from a
different model carries the right name. What the guard actually checks is whether the
daemon reports the primary as *serving*, before and after the timed run. On the first
live run this caught exactly what it was written for: with the big model's process
still starting, requests to it came back at a rate that model cannot reach, correctly
labelled and entirely meaningless.

It is not instant. A cold load of a large model can take minutes before the first
trial starts. Exit `0` valid · `1` a guard tripped · `2` bad arguments · `3` assertion
failed · `4` nothing measurable · `5` no daemon.

### `svrn ring`

Deploy an app to a trust ring — a group of people, not a host. State is an
append-only log each member keeps a copy of, signed with their node key and
handed back in an order every node agrees on. The rail carries the log; what
an entry *means* is the app's.

| Subcommand | Description |
|---|---|
| `new <dir> [--name <title>]` | Scaffold a ring app — the page, its reducer, and the reducer's tests. Re-running it never overwrites |
| `roster add <person> (--self \| --key <hex>) --ring <ns>` | Bind a name to the node key that person signs with |
| `roster list --ring <ns>` | Who is in the ring, as the running daemon sees it |
| `dev <ns> [--dir <d>] [--port <n>]` | Serve the app at `127.0.0.1:4318`, holding a grant scoped to this ring alone |
| `log <ns> [--json]` | The acts on this journal, in the order every node applies them, and everything the rail could not account for |

A ring namespace is created by its first write; there is nothing to provision.
Start with `roster add`, because an op signed by a key no roster claims is a
gap rather than an act.

`ring log` always prints its **gaps**. A gap does not make the acts above it
wrong, it makes them partial — an op that has not arrived yet, a signature that
does not verify, a signer nobody claims. Sequence holes usually close on their
own: every node republishes what it holds to every online peer each minute.

There is no `ring balances`. A balance is an *expense app's* reading of a
journal, and the rail does not know what a payload means — a terminal that
printed one for whichever tenant happened to be in front of it would be the
money rules living in a second place. Open the app with `ring dev` to see them
rendered.

The grant `ring dev` mints reaches one namespace's rail and nothing else on the
daemon, lives in the dev server rather than in the browser tab, and dies with
the process. There is no rail route that can change a roster, so a deployed app
cannot add a key to the ring — including its own.

M0 is dev-server only. There is no `ring deploy`, and `window.ring` is injected
by `ring dev` rather than shipped in a bundle, so every member runs `ring dev`
against their own daemon from their own copy of the folder. The roster is a
per-node file (`~/.svrnmesh/rings/<ns>/roster.json`), not gossiped state — each
member adds the others locally. Journal ops themselves do sync, once a minute.

Walkthrough: [HOUSE_EXPENSES.md](../../docs/HOUSE_EXPENSES.md). Authoring
guide: [MESHAPP_AUTHORING.md](./MESHAPP_AUTHORING.md).

### `svrn corpus`

Manage knowledge corpora. See [KNOWLEDGE_BASES.md](KNOWLEDGE_BASES.md) for tier details.

| Subcommand | Description |
|---|---|
| `list` | List installed and available corpora |
| `install <id>` | Submit an install request. **Returns as soon as the daemon accepts it** — see below |
| `install <id> --wait[=SECS]` | Install and block until the index is actually usable; non-zero if it never is. Default budget 300s |
| `remove <id>` | Remove an installed corpus |
| `status [<id>]` | Show `state` (`ready` / `building` / `absent`) + shard status, for one corpus or all |
| `reconstruct-manifest <id>` | Rebuild source-file manifest before collaborative ingestion |

**`install` is asynchronous, and its exit code says so.** The command POSTs to
the daemon and returns the moment the request is *accepted*. The ingest then
runs in a background task writing `indexes/<id>-partition-<node>/`; the
canonical `indexes/<id>/` that `enrich init`, `chat --corpus` and search open
is materialised only by the finalise step at the end. So a bare
`svrn corpus install <id>` exiting 0 means **requested**, not **installed**,
and for a catalog corpus the gap is hours.

Use `--wait` in scripts and gates, where exit 0 has to mean the corpus is
there. It polls the same readiness rule `corpus status` reports and exits
non-zero, naming the state it observed, if the budget runs out:

```bash
svrn corpus install sep --wait=7200 && svrn enrich init sep --from-corpus sep
svrn corpus status sep        # ready | building | absent
```

`status` reports one row per **corpus**, not per directory: an in-flight
partition appears as its own corpus id in state `building`, never as a
separate corpus named `<id>-partition-node-…`.

### `svrn alignment`

Mesh-replicate the user's `~/.claude/` workspace state (plans, auto-memory entries, plan template) and `~/.svrnmesh/notes.db` between the user's own daemons. Newest mtime wins per logical key, so two machines that edit the same plan or note converge on the newer copy after a mesh tick. The post-merge projector materializes received chunks back to disk on the receiving daemon, so a fresh machine reaches parity in one ingest. See [PLAN_ALIGNMENT.md](PLAN_ALIGNMENT.md) for the design rationale.

| Subcommand | Description |
|---|---|
| `migrate` | Tar a backup to `~/.svrnmesh/backups/`, then submit the `alignment` corpus install. The daemon ingests in the background; peer convergence happens automatically via existing mesh hooks. |
| `migrate --dry-run` | Walk the alignment scope and print what would be exported (files + notes rows) without writing a backup or touching the daemon. |
| `status` | List paths in scope, count of files and notes that would be exported, and the local alignment corpus state if it has been ingested. |

**Operator flow** (run on both machines, order doesn't matter — reconciliation is symmetric):

```bash
sovereign alignment migrate --dry-run    # preview scope
sovereign alignment migrate              # backup + ingest
sovereign alignment status               # check progress
```

**Recovery.** The backup tar at `~/.svrnmesh/backups/alignment-pre-migrate-<ts>.tar` restores the original state with `tar -xf <path> -C $HOME` (the archive uses `~/`-relative paths). The migration is idempotent — re-running converges, doesn't compound.

**Sync mechanics.** This CLI lands the local state on the alignment corpus. The cross-machine merge happens via the daemon's existing hooks (`auto_recover` after a stranded-partition merge, `index_transfer` after a peer pull); the projector then writes received chunks back to `~/.claude/` and upserts `notes://...` rows into `~/.svrnmesh/notes.db` automatically.

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

### `svrn path`

Print a per-user directory as the toolchain resolves it, from the path SSOT. Answers "where does my data actually live?" without guessing.

| Subcommand | Description |
|---|---|
| `root` (default) | Per-user root — `~/.svrnmesh`, or a populated legacy `~/.sovereign` |
| `data` | As `root`, but honours `SVRNMESH_DATA_DIR` / `SOVEREIGN_DATA_DIR` |
| `mesh-data` | Platform-native data dir for the embedded mesh's shared storage |
| `config` | Platform-native config dir for GUI-owned settings (`desktop.toml`) |

| Flag | Description |
|---|---|
| `--explain` | Write the reason for the choice to stderr (stdout stays a bare path) |

Output is a bare path, so it substitutes directly: `root="$(svrn path root)"`.

**Scripts must use this rather than hard-coding `~/.svrnmesh`.** On a machine that still has a populated `~/.sovereign` and no `~/.svrnmesh`, creating the rebranded directory by hand makes every getter prefer it and silently orphans the real data root. `scripts/lib/svrn-root.sh` is the shell-side helper.

### `svrn status`

Top-level health rollup for the current project: code intelligence, daemon, watcher state, drift posture. Replaces `svrn project status` (old name still forwards here).

### `svrn notes` (alias: `svrn reflect`)

Durable working notes, plus the session-reflection view. The canonical name is `notes`; `reflect` still works and forwards here. Bare `svrn notes` renders the 30-day reflection *summary*; `svrn notes list` is the notes themselves — the same `NoteStore::read_notes` query the MCP `notes` tool runs, so the CLI and an agent cannot disagree about what is stored. The workflow these serve: [ground your agent — durable notes](../../docs/GROUND_YOUR_AGENT.md#durable-notes).

| Form | Description |
|---|---|
| `notes` | 30-day reflection summary; filters `--since <Nd\|Nh>`, `--tool <name>`, `--raw`, `--todos` |
| `notes add --kind <k> -m "..."` | Append a note (decision / invariant / todo / attempt); stdout is the new note's id |
| `notes list [--query <s>]` | List / search the notes; `--query` alone implies `list`; `--id <id>` reads one (8-char short ids work) |
| `notes promote <id> --to <s>` | Promote a note's scope |
| `notes rationalize` | Consolidation-candidate report (no writes); `--distill` previews LLM verdicts; `--apply --yes` writes |
| `notes gc [--days 30]` | TTL sweep of expired telemetry (the daemon runs this daily) |
| `notes migrate-from <path>` | Merge a stray local `notes.db` into `~/.svrnmesh/notes.db` |
| `--retire --tool <name> --reason <why>` | Retire matching reflections |

### `svrn backlog`

File work into the seat's ranked backlog. One call to the resident daemon model scores the item against the versioned value ruler (`quality/backlog-ruler.toml` — the same file `scripts/co-backlog.py` ranks with), drafting a Value with its axis named, an Approach derived only from the text you gave it, and a Cost that follows that Approach. The full map is [scripts/BACKLOG.md](../../scripts/BACKLOG.md).

A backlog item is a notes-store `todo` carrying `related_entity=backlog`; there is no separate backlog store, and ordering is derived at every read rather than maintained.

| Form | Description |
|---|---|
| `backlog add "<text>"` | Score one item on the resident model and file it, unvetted; stdout is the new item's id |
| `--objective <anchor>` | The standing objective, initiative or order id it serves |
| `--key <id>` | Producer identity — a repeat filing under the same key UPDATES that item instead of duplicating |
| `--no-score` | File it unscored for later triage. No model call, no daemon needed |
| `--db <path>` | The store. Defaults to `$CO_BACKLOG_NOTES_DB`, else `$SOVEREIGN_DATA_DIR/notes.db`, else `~/.sovereign/notes.db` — never discovered from the working directory |
| `--create` | Create the store if absent (off by default: a fresh store at a wrong path looks exactly like a working one) |
| `--ruler <path>` | The value ruler; defaults to `$CO_BACKLOG_RULER`, else the repo's `quality/backlog-ruler.toml` |
| `--daemon <url>` | Score against a specific daemon rather than the configured client port |
| `--json` | Print the result as JSON |

**Machine-scored items always land unvetted and cannot be pulled.** The item carries `Scored-by: <model>`, which the renderer treats as disqualifying however complete the rest of the header looks — a person reviewing it and clearing that line *is* the vetting. If the daemon is down or no chat model is resident, `add` refuses and files nothing rather than landing an unscored item as a scored one; a wrongly-scored item is worse than a missing one, because it gets ranked.

### `svrn journal`

Read, share, or switch off the **local journals** — per-feature, append-only, metadata-only records of how a feature behaved on your own work, under `~/.svrnmesh/journal/<stream>-<date>.jsonl`. 14-day retention, 8 MiB/day cap per stream. Offline: it touches no daemon, and **there is no send, submit, or upload form — no network path exists in the code.**

One stream ships today: **`next-edit`** (`sovereign/docs/NEXT_EDIT.md` §9d) — one line per `POST /v1/edit_predictions` plus one per outcome the editor reports, joined by `episode_id`. The command is written against a stream registry (`journal_cmd::VIEWS`), so a second feature's journal is a new file plus one row, and every form below covers it without change.

| Form | Description |
|---|---|
| `journal` | Stats for every journal: what each feature did, and what became of it (the default view) |
| `journal <stream>` | Stats for one — e.g. `journal next-edit` |
| `journal show [--last <N>]` | The raw records, oldest first (default 20) |
| `journal bundle [--out <path>]` | Write ONE file to hand back, then print exactly what is in it |
| `journal off` \| `journal on` | Stop / resume recording; effective on the next record, no restart |
| `journal clear [--yes]` | Delete every record |

A leading stream name scopes any form: `journal next-edit off` stops one journal (a `<stream>.disabled` marker), `journal off` stops them all including streams added later (the global `DISABLED` marker). `JournalStream::enabled` is the single decider over both markers and both env vars (`SOVEREIGN_JOURNAL`, `SOVEREIGN_NEXT_EDIT_JOURNAL`).

Journals record *why* a feature did what it did — for next-edit: whether the lane fired or stayed silent and why, which model answered, the region size, the timings. They do not record your code: not the document, the region, the file path, the matched text, or anything proposed. `path_ext` is the file extension alone and `region_bytes` is a length. `bundle` prints the complete field list of the file it writes, collected from the written bytes rather than from the records that went in, so the claim is checkable rather than assertable.

Reading the next-edit stats: the acceptance rate is computed over `accepted + dismissed` **only**, and prints *nothing judged yet* rather than 0% when that is empty. `diverged` (you kept typing) and `superseded` (a newer prediction replaced it) are not rejections and are never folded into dismissals, and episodes that never resolved are counted as `unknown` — that is the difference between a rate you can act on and a flattering one. Under 20 judged episodes the output says so instead of quoting a percentage as if it were a measurement.

### `svrn cache-audit`

Glassbox telemetry for the fleet's context spend. Parses the local Claude Code transcripts (`~/.claude/projects/<encoded-cwd>/*.jsonl`) and reports, per session, where the token/cache budget went and — the headline — the **raw-acquisition ratio**: how many raw file/grep tokens were pulled into context versus how many code-intelligence / RAG calls (`symbols`, `callers`, `code_search`, `notes`, …) were made. A session that acquired its codebase context entirely through `Read`/`cat`/`grep` (high left number, zero on the right) is the leak: each raw read then rides the cache-read tail for the rest of the session. Pricing is model-aware (Opus/Sonnet/Haiku/Fable). Read-only; no daemon or network.

| Flag | Description |
|---|---|
| `--project <path>` | Audit the project whose working dir is `<path>` (default: current dir) |
| `--dir <path>` | Audit a specific directory of `.jsonl` transcripts (overrides `--project`) |
| `--session <id>` | Detailed breakdown for one session (matches a filename/short-id prefix) |
| `--last <N>` | Show the N most recent sessions (default 10) |
| `--sort <key>` | `cost` \| `recent` \| `ratio` (default `cost`) |
| `--json` | Machine-readable output |

### `svrn session`

Session continuity (spec: `docs/specs/SESSION_CONTINUITY.md`). `session list` shows this project's Claude Code transcripts with a first-user-turn hint; `session distill <id>` extracts the deterministic **narrative spine** (real user turns + assistant texts + the edit working-set — tool results and hook payloads are dropped) and synthesizes a schema-v1 **session frame**: the ≤2k-token essential state (goal, position, next actions, decisions, invariants, dead ends, working set, verification) a successor agent needs to seamlessly continue a dead session's work. Frontmatter and the Working-set section are assembled deterministically by the CLI; the LLM writes only the narrative sections and is validated against the section contract (invalid frames are written but flagged, exit 1). Output: `~/.svrnmesh/sessions/<session_id>/{frame.md,spine.txt}`. Distillation quality is graded against `quality/session-frame.golden.md`.

| Flag | Description |
|---|---|
| `--project <path>` | Project working dir whose transcripts to read (default: current dir) |
| `--dir <path>` | Explicit transcript directory (overrides `--project`) |
| `--no-llm` | Stop after the spine (also the daemon-down fallback) |
| `--model <id>` | Chat model for synthesis (default `primary`) |
| `--max-tokens <n>` | Synthesis output budget (default 700) |

Beyond `list` and `distill`: `session frames` prints the index of live session frames (one pointer line each — what the boot hook injects), `session frames <id>` dereferences one whole, `session attach <id>` re-points the current terminal at that lineage, `session lineage` shows predecessor chains, and `session grade <id>` grades a frame against `quality/session-frame.golden.md` (exit 0 pass / 1 fail). The workflow they serve: [ground your agent — session continuity](../../docs/GROUND_YOUR_AGENT.md#session-continuity).
| `--stdout` | Print the frame as well as writing it |

### `svrn recipe`

Run and curate corpus ingestion recipes.

| Subcommand | Description |
|---|---|
| `list` | List all corpora available in the registry. `--offline` skips live registry refresh |
| `test <path>` | Run the full test harness against a recipe file. Flags: `--sample-size N`, `--output <path>`, `--params k=v[,...]`, `--params-file <json>` |
| `validate <path>` | Validate recipe fields without downloading data. `--offline` skips registry fetch |
| `publish <path>` | Add a recipe to `~/.svrnmesh/recipes/registry.toml`. `--submit-pr` also drafts a community-registry PR via `gh` |

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

Global flags: `--db <path>` (default `~/.svrnmesh/pipeline.db`), `--seed-only`, `--slugs <path>`, `--key <slug>` (repeatable). Failures bucket into `timeout` / `refused` / `vram_thrash` / `mismatch` / `model_missing` / `unknown` and retry up to `[dispatch].max_attempts` before landing in `failed`. Add an `[schedule]` block with `active_hours = "HH:MM-HH:MM"` to auto-pause outside that window.

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
| `--output <md>` | Write markdown digest here; JSON sidecar lands at `<output>.json`. Default: stdout for markdown, `~/.svrnmesh/indexes/<corpus>/atlas/git_archaeology.json` for JSON |
| `--threshold N` | Co-evolution jaccard threshold in `[0.0, 1.0]`. Default `0.5` |
| `--min-joint N` | Minimum joint-commit count for a co-evolution pair. Default `5` — drops scaffolding-era false positives |

Reads the structural atlas from `~/.svrnmesh/indexes/<corpus>/atlas/atoms.json` — build it first via `svrn enrich ingest <id> --source-corpus <id>`.

### `svrn archaeology-eval`

Re-verify the claims `git-archaeology` makes against git itself. Witness checks + baseline diff + curated regression cases (inquiries). See [ARCHAEOLOGY_EVAL.md](ARCHAEOLOGY_EVAL.md).

```
sovereign archaeology-eval <atlas-corpus-id> [--inquiry <toml>...] [--baseline <path>] [--output <md>] [--save-baseline]
```

| Flag | Description |
|---|---|
| `--inquiry <toml>` | Curated regression case (TOML). Repeatable. `file_globs` selects atoms; `keywords` / `authors` / `date_range` add inquiry-specific witnesses |
| `--baseline <path>` | Previous run's eval report (JSON). Default `~/.svrnmesh/eval/baselines/<atlas>.eval.json` |
| `--output <md>` | Markdown report path. Default `~/.svrnmesh/eval/<atlas>.eval.md` |
| `--save-baseline` | After running, save current report as new baseline |

Appends one CSV row per run to `~/.svrnmesh/eval/history.csv`. Exit code is non-zero on any inquiry failure or fabrication — CI-friendly.

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

Build, query, and audit v2 atlas enrichments of a corpus. Writes state under `~/.svrnmesh/enrichment/<corpus>/` (phase caches + run outputs) and `~/.svrnmesh/indexes/<corpus>/atlas/` (resolved atoms + edges + trajectories + configurations + schema-validation + cross-corpus edges).

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
| `seed <corpus>` | Assert every rule-shaped claim in the corpus's atlas as a governed rule (idempotent baseline) |
| `tensions <corpus> [--format json]` | List open tensions, ranked, with both rule texts and a copy-pasteable resolve command |
| `resolve <corpus> <tension-id> --keep <rule-id> [--rationale <s>]` | Pick a winner; the other rule is superseded — still history, no longer law |
| `accept <corpus> <tension-id> --rationale <s>` | Record the tension as known-and-tolerated; both rules stay in force |
| `ask <corpus> "<question>"` | Grounded, cite-or-abstain Q&A over current law (superseded rules' evidence excluded) |

The journey — what these are for and in what order: [govern a corpus](./GOVERN_A_CORPUS.md).

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
| `run-ab <feature-id> --brief <path> [--driver <name>]` | Run each driver against the same milestone, then diff |
| `probe-driver [--url <endpoint>]` | Trivial tool-use sanity check against an OpenAI-compatible server |
| `report <feature-id>` | Render milestone / red-team / epistemic / all reports |
| `teardown <feature-id> [--dry-run]` | Interactive note-classification pass; writes `epistemic-report.md` |
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

Logs: `~/.svrnmesh/logs/daemon.log`. Rotated in-process — copy-truncate, 10 MiB cap, 5 backups, 30-min sweep loop; preserves the inode for launchd-held FDs.

### `svrn install-service`

Register the daemon with the OS service manager — launchd on macOS, systemd on Linux — so it starts at login and stays running across logouts. Run once after `svrn setup`. Lives in the `sovereign-cli-daemon` sibling.

### `svrn update`

Check for and install a newer CLI release. Bare `svrn update` resolves the newest release on the public shelf, verifies its checksum through the same installer as `curl … | sh`, and replaces the running binary in place; `svrn update --check` only reports. Unix only (needs `sh` + `curl`).

### `svrn conformance`

Which requirements of `research/clean-room/REQUIREMENTS.md` are actually proven, in **four verdicts** — `passed` / `failed` / `could-not-judge` / `never-ran`. Source checkout only, and `--features dev-tools`.

It **runs nothing**. It joins five artifacts and owns no judgement of its own:

| Artifact | Answers |
|---|---|
| `quality/requirements.toml` | what the spec obliges — 625 in scope, generated from the spec and byte-gated against it |
| `quality/conformance/*.toml` | which TEST claims each requirement, generated from `covers:` doc tags |
| `target/nextest/*/junit.xml` | what that test actually did, on the last run |
| `sovereign/docs/cli-contract.toml` | which JOURNEY STEP claims each requirement |
| `~/.svrnmesh/journey-nightly/latest-steps.jsonl` | what that step did, on the last lane |

| Flag | Description |
|---|---|
| `--family <PREFIX>` | Restrict to one requirement family (`GR`, `X-EH`, `FE`, …) |
| `--scenarios` | `REQUIREMENTS.md §16`'s 19 acceptance scenarios instead. Ignores `--family` and says so |
| `--json` | Machine-readable |

**Two claim routes, because the requirement's own class decides which applies.**
`quality/requirements-enforceability.toml` classifies all 625: **260 `structural`**
(a type, a lint, a source-scanning test — a `#[test]` is the instrument) and
**311 `cli`** plus 11 `desktop` (a command and an assertion on its output, and
nothing else). The rest are 9 `model` and 34 `review`.

- **`structural`** — put `/// covers: GR-19` above a `#[test]` and regenerate that crate's manifest with `UPDATE_CONFORMANCE_TAGS=1 cargo test -p <crate> --test main conformance_tags`. A tag naming an unknown id, or over a body with no assertion, fails the generator rather than being counted.
- **`cli`** — put `requirements = ["UI-17"]` on the journey step whose `expect` block falsifies the clause. Gates refuse an id the spec does not state, a requirement no command can observe, a step asserting only an exit code, and a claim on a step no lane runs.

Offering only the first route is how 311 `cli`-class requirements got covered by
unit tests asserting something *adjacent* to the clause — 35 overclaims out of 74
audited. Pick the route the class names.

`--scenarios` reports §16 in **two columns**, demonstrated and cited, never one
number: §16.1's A-1 requires the demonstration to have been watched, so green
cites over a scenario nobody ran is the substitution §16 exists to prevent. A
scenario no journey declares reads `not declared`, which is not `never-ran`.

Two verdicts do the work the other two usually hide. A pass recorded **before** its guard's source file was last edited reads `could-not-judge`, never `passed`. A requirement with no claim, or whose test was absent from a filtered run, reads `never-ran` — so the denominator cannot be shrunk by omission. Exit is `1` only when a claimed requirement **failed**; `never-ran` is the honest starting state of nearly all 625 and is not an error. **No headline percentage is printed** — four numbers, never one.

### `svrn contract`

What this CLI promises, how much of it is proven, and when that was last checked against a running system. Reads `docs/cli-contract.toml` — a source checkout only, and `--features dev-tools`.

| Subcommand | Description |
|---|---|
| (bare) | Everything below, ending with the exact commands that re-derive it |
| `map` | The **experiences** — promises the product makes — and the journeys serving each, with how many steps of each assert output |
| `census` | How many declared steps can actually fail, split into the ones a lane RUNS and the ones nothing ever executes |
| `nightly` | The last journey-lane verdict on this host and its age, or a loud "no report here" |

The `census` split is the number to read. A step in a `skip_live` journey is a written intention, and a step with no `expect` block is an invocation — neither is a test, and both used to be counted in the same total as the steps that assert an answer. The census the report prints is the same one `cli_contract_journeys` enforces as a gate, so it cannot flatter the manifest.

See [TESTING_SURFACE.md](TESTING_SURFACE.md#the-cli-quality-surface) for the layers behind it and how to run each lane.

### `svrn posture`

One read-only table: artifact age + verdict for every posture-bearing quality subsystem — drift report, arch census, capability map, the CLI-contract nightly verdict, the watcher heartbeat (honoring a repo's `[watchers] enabled = false` opt-out as *off (by design)*, not a fault), the env-gate legacy baseline count, and the committed bench-baseline age range. Each row names the command that refreshes its artifact. `--features dev-tools`; repo-scoped rows degrade to `no repo context` outside a source checkout.

Added 2026-07-30 because the per-subsystem posture tools only answer when asked, and in practice none were: the drift and arch reports had both been weeks stale with nothing anywhere aggregating that fact. This verb is the aggregation — the one place a neglected corner shows up on its own.

### `svrn deep-research`

The thin local-only research loop (T1): ask a question, and the loop runs gated rounds over your local estate corpora plus web search — every search and fetch flows through one run-scoped, fail-closed budget decider, fetched pages are custody-stamped by code (public-web), and each round's draft is gap-audited (four verdicts: passed / failed / could-not-judge / never-ran) with the audit's gaps driving the next round's queries. The final report carries verdict stamps and `[Source: …]` citation handles per claim; invented citations are structurally impossible because the draft's URL allow-list is set by the loop.

```
svrn deep-research "<question>" [--run-dir DIR] [--max-rounds N]
    [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N]
```

| Flag | Meaning | Default |
|---|---|---|
| `--run-dir` | Parent dir for the run's artifact directory (`<run-dir>/<run-id>/`) | temp dir |
| `--max-rounds` | Round cap (each round searches, audits, and re-queries its gaps) | 3 |
| `--corpora` | Comma-separated estate corpus ids searched before the web | none |
| `--code-set-k` | Triage code-set size — the candidate set the ranker re-ranks (never excludes outright) | 3 |
| `--eps-quota` | Round-gap quota (epsilon) | 0.1 |
| `--search` | Web-search allowance in budget units | 4 |
| `--fetch` | Web-fetch allowance in budget units | 4 |

Needs the local daemon's embed + draft surface (loopback HTTP only — no egress beyond the DDG search + page fetches the budget allows). Dev-gated: `--features dev-tools`.

### `svrn seat`

The seat's notes-rail instrument: `svrn seat watch` polls the daemon's
notes rail for records carrying a seat anchor (`related_entity` in the
operational registry) and surfaces each new one as a `SEAT_WATCH` line —
one per stdout row, ready for a session-level background monitor. The
seat opt-in is `include_operational: true`; ordinary sessions never see
these rows. `--once` polls once and exits; flags `--every SECS`,
`--limit N`, `--anchors a,b,c`. `--features dev-tools`.

Added 2026-08-12 (order commons-fluency fix 8) because the drill had
run its cross-machine watchers by hand; the verb is the mechanism the
self-running F-drill (UC-F8) starts from.

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
