# ralph-loop — a supervised agent loop

`scripts/ralph-loop.sh` runs an ordered queue of work units, one coding-agent
session per unit, until each is accepted. It is repo-agnostic: the repo supplies
a queue, a unit path, and an accept command; the loop supplies the supervision.

It exists because hand-rolled loops kept failing the same way, silently:

| failure | what the loop does now |
|---|---|
| a crash sat dead for hours | heartbeat + status file; `launchd` `KeepAlive={SuccessfulExit:false}` restarts a crash and stops on a clean finish; a `halt` marker stops a deliberate pause |
| a hung session ate hours | per-session wall-clock timeout **and** a staleness killer (no log growth for `--stale-after` seconds) |
| a SIGKILL lost the session's work | the prompt carries an incremental-commit contract: commit every distinct step, never hold >10 min uncommitted |
| "it looked fine" | `--self-test` runs a fake hung session and asserts the watchdog kills it |

## Use

```sh
ralph-loop.sh --workdir DIR --label NAME (--queue FILE | --queue-cmd CMD) \
  [--unit-path 'orders/{id}'] [--prompt-file order.md] [--done-file DONE.md] \
  [--accept-cmd CMD] [--max-attempts N] [--session-timeout S] \
  [--stale-after S] [--review-every N] [--principles PATH] \
  [--review-path 'orders/reviews/review-{n}'] [--notify] [--plan] [--self-test]
```

- **queue** — unit ids, one per line, in the order to run. `--queue-cmd` runs a
  command whose stdout is that list (e.g. a topological sort of a ring's
  orders). A unit whose `DONE.md` is already committed, with a clean tree and a
  green accept command, is **auto-accepted** — so a restart or a switch onto an
  in-progress queue never re-runs finished work, and the review cadence still
  counts it.
- **accept** — a unit is accepted when its `DONE.md` exists, `git status` is
  clean, and `--accept-cmd` exits 0. Omit `--accept-cmd` to accept on the first
  two only.
- **review** — every `--review-every` accepted units, a review unit runs over
  the code they landed against `--principles`, and fixes and consolidates what
  violates them. Behaviour-preserving: the accept command must stay green.
- **`--self-test`** — proves the watchdog fires. Run it before trusting a new
  queue; a loop you have not watched fail is not a loop.
- **`--plan`** — print the resolved queue and exit.

## Install as a launchd job

```sh
OPENCODE_CONFIG=/path/to/ralph.json ralph-loop.sh ... --install-launchd
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
```

`--install-launchd` writes a plist with `KeepAlive={SuccessfulExit:false}`
(restart on crash, stop on clean finish), `RunAtLoad`, and a 30 s throttle, and
prints the load/stop/watch commands. It does not load the job.

## State (outside the repo tree)

`~/.svrnmesh/ralph/<repo>-<label>/`:

- `status.json` — the live heartbeat: unit, attempt, phase, detail.
- `loop.log` — every transition.
- `accepted/<unit>` — one marker per accepted unit.
- `logs/<unit>-<n>.out` — each session's output.
- `done` / `halt` — the terminal markers. Remove `halt` to resume.
- `launchd.log` — the job's stdout/stderr.

## What the repo supplies

Ersilia's ring loop is the worked example:

- `scripts/ring-queue.sh <ring>` — the ring's orders in dependency order.
- `scripts/accept-no-failed.sh` — run the gate, fail only on a `failed` row.
- Invocation: `--queue-cmd 'scripts/ring-queue.sh 1' --unit-path 'orders/{id}'
  --accept-cmd 'scripts/accept-no-failed.sh' --review-every 3
  --principles .../ARCH_PRINCIPLES.md`.

A repo whose units are not `orders/<id>/order.md` sets `--unit-path` and
`--prompt-file` instead; nothing else changes.
