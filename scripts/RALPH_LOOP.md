# ralph — a commit-driven campaign loop

`scripts/ralph.py` runs a coding-agent campaign: it dispatches the ready rows
of a queue to agent sessions and lands what they finish, until every row is
done or only the operator can move the rest. It is a closed state machine. The
machine is a table (`TRANSITIONS`, with every pair it leaves out in
`IMPOSSIBLE` and a reason), every event it consumes comes from something the
loop owns, and a session ends by reporting exactly one result. Why it is built
that way: `docs/RALPH_STATE_MACHINE.md`.

## Run it

```sh
python3 scripts/ralph.py run --workdir . --queue <name> --install-launchd
python3 scripts/ralph.py start --workdir . --queue <name>
python3 scripts/ralph.py status --workdir . --queue <name>
python3 scripts/ralph.py follow --workdir . --queue <name> [--unit <row>]
```

`follow` prints the status, then pipes the output of every unit the loop
holds as it grows: a session's transcript while it runs, its background run's
log while it awaits. It switches log when a unit moves on, and marks each
switch and each change of the loop's state with a `==` line. `--no-follow`
prints a snapshot and exits.

`run` is the serial loop: one unit at a time, in the main tree. `pool` runs
units in worktree lanes beside the main tree (`--lanes N`) and reviews in the
main tree, alone. `--install-launchd` writes a job the host restarts on any
exit but 0 (launchd `KeepAlive`, systemd `Restart=on-failure`), to
`~/.config/ralph/jobs/`, never `~/Library/LaunchAgents`. The loop exits 0 only
when the queue is done or the operator stopped it. Its log is
`~/.svrnmesh/ralph/<repo>-<label>/launchd.log`, with one line per transition:
`unit <id>: <state> --<event>--> <state> · <detail>`.

## How a session ends

Every session is told this, and has `ralph-result` on its PATH:

- `ralph-result done`: the unit is finished and committed. A dirty tree is
  refused at the call, so the session can still commit.
- `ralph-result continue [note]`: progress committed, more remains. The note
  goes to the next session.
- `ralph-result await <budget> -- <cmd>`: a long run. The loop starts the
  command in the unit's worktree, in a process group of its own, logs it to
  `<log dir>/sessions/<unit>-<n>.await.log`, and resumes the unit with the
  exit code, how long it ran and the log when it ends however it ends (pass,
  failure, crash, OOM kill, reboot), or kills it when the budget passes.
- `ralph-result needs-human [--operator] [--package FILE] <why>`: a decision
  the row and the design do not make.

The log dir is `<control_dir>/log/` (`ralph/log/` on a legacy launch line):
session transcripts, run logs, a lane's kept evidence and `ralph-check.sh`'s
logs, kept out of git by the loop's excludes. The loop keeps none of its
records under `target/`, which a host may purge under disk pressure;
`target/ralph/lane.env` stays there because every dispatch rewrites it.

Nothing else a session writes is read as loop state. A session that ends
without a result is judged by its commits alone: commits mean it continues,
none mean a strike.

## Escalation

There is one path: retry, then strikes, then the director, then held. A unit
strikes when a session ends with no result and no commit, when it continues
more than `MAX_LANE_CONTINUATIONS` times in a row, when a run passes its
budget, or when its merge conflicts. At `--max-stall` (serial) or
`--max-lane-failures` (pool) strikes, a queue with a charter
(`ralph/CHARTER.md`, or the manifest's) sends the director: a session on
`RESOLVE_MODEL` that decides the forks the charter covers and records each
decision (`scripts/ralph-decisions.py`). A worker's `needs-human` goes to the
director first too, unless it says `--operator`. With no charter, or after
`DIRECTOR_MAX` director sessions, the unit is held: its package is written to
`<control>/parked/<row>.md`, the operator is notified once, and every row that
does not depend on it keeps running. `ralph.py unpark <row>` (or deleting the
package) releases it with its counters reset. HUMAN- rows and rows outside a
frozen scope (`scope_file`) are held the same way.

## The precondition guard

Before every dispatch the loop checks, in order: the queue parses (a
dependency cycle among open rows is refused by name), the worker prompt names
`ralph-result`, free disk on the lane root is over `DISK_FLOOR_GB` (the pool
first reclaims `target/debug` and `target/release` from lanes whose units hold
no process), free memory admits the cargo budget (pool), and the unit's model
answers a probe. The probe sends one minimal chat call per roster model,
through the same client lanes use, and passes the first that answers as the
session's single `--model`. A failing precondition blocks dispatch and
notifies after its limit (`PRECONDITION_NOTIFY_AFTER`), never a strike. It
covers dead and rate-limited providers: 10 of the 29 sub-two-minute ersilia
session deaths from 2026-09-17 to 10-04. Permission refusals, which end an
opencode session at once, still strike, and the strike names the count and
the settings file to extend.

## Stop, drain, start

`ralph.py stop` ends running sessions now, and their worktrees resume later
with no strike. `ralph.py stop --drain` lets them finish and starts nothing
new. Either way a background run keeps running, and the next start watches it
again. `ralph.py start` clears the stop and starts the job. `--hard` boots the
job out and ends its sessions and runs.

## The process

The loop's state is the queue (rows, dependencies, `[x]`), the parked dir, and
its ledger, `~/.svrnmesh/ralph/<repo>-<label>/loop.json`, which holds what it
started (each session and run by pid and start time, so a reused pid is never
mistaken for one) and its counters. Sessions and runs therefore survive the
loop's own restart. The host restarts the loop, and the restarted loop finds
the same processes again. The loop deploys its own committed fixes: at any
tick it re-execs onto `ralph.py` as committed, if the file changed and
compiles. It writes its heartbeat every tick in every state, so the watchdog
(`ralph.py watch --install-launchd`) has one job, noticing the loop is down or
hung. An error inside a tick is logged, notified once, and the loop keeps
ticking.

A loop that starts with no ledger adopts what the file-protocol loop left
(`adopt_legacy`): a waiting file whose first line names a marker becomes a
loop-owned watcher on it, budget 12h; a halt package parks the row it names;
the pool's counters carry over. `ralph.py status` previews it.

## Parallel lanes

Each lane is a git worktree on `ralph/<unit>`, beside the main tree (never
inside it, or cargo reads the main tree's config twice). A new lane's
`target/` is a reflink clone of the main tree's with every tracked file
touched, so only workspace crates rebuild. A lane kept across sessions
fast-forwards onto the base, or merges the base in where its session can
resolve a conflict. When a lane reports done, the loop renumbers its decision
ids against the base, merges `--no-ff`, regenerates `ralph/DECISIONS.md`,
marks the row `[x]` with the lane's tip and commits `ralph: <id> done` (what
`audit_every` counts), copies the lane's log dir and its `target/ralph/` into
the main tree's `<log dir>/<unit>/`, and removes the worktree. A conflict
aborts the merge and strikes the unit; its next session gets the base merged
in. Merges and main-tree commits wait while a main-tree unit's session or run
holds the tree.

Rows in `conflicts.txt` never run at once, `heavy.txt` rows run one at a
time, and `<id> *` runs alone. A ready review lets the lanes drain, then runs
alone in the main tree. Each lane gets its share of one cargo budget
(`lib/cargo-jobs.sh cargo_jobs_share`, written to `target/ralph/lane.env`) and
a cargo lock of its own.

## Queues, prompts, models

`--queue <name>` runs `ralph/next/<name>/queue.toml`: its state, prompt
(`ralph/PROMPT.base.md` plus the queue's addendum, `{{result}}` among the
vars), charter, models, checks (`scripts/ralph-check.sh` reads them through
`check-argv`), `audit_every`, `dispatch_requires` (a row lacking a marker is
held at dispatch), `scope_file`, and control dir. `ralph.py models` shows or
sets the models, `plan` prints the queue head and its routing, `report` the
director's decisions and their commit ranges, `prompt` the exact worker prompt.

## Build hygiene

A campaign that builds accumulates: measured 2026-09-15, this workspace's
`target/` reached 100G, and the disk ceiling took the loop down twice. A unit
opens with `scripts/dev-build.sh --clean`, which cleans the debug profile only
past 50G (`RALPH_CLEAN_MB`), and builds go through that entry or the check
scripts, never bare `cargo`.

## Tests

`python3 scripts/tests/ralph.py` (the machine: the totality test, the result
verb, real-process liveness, a real-process end-to-end run, and every past
incident as a row in `INCIDENTS`) and `python3 scripts/tests/ralph_parts.py`
(the carried-over mechanics). No model calls.
