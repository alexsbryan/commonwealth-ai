# ralph-loop — a commit-driven campaign loop

`scripts/ralph-loop.sh` runs a coding-agent campaign over a repo until it is
done. It is the shape that actually moves — svrnmesh-cln's loop (49 build
orders in four days) — with the supervision hand-rolled loops kept failing to
include.

## The model

- **The queue is a file in the repo** (`ralph/STATE.md`): the plan is memory, so
  a fresh session knows where it is without a per-order DONE the loop waits on.
- **One small unit per iteration.** A session does one unit, commits, updates
  the queue. The unit is sized to one iteration (Ersilia: one lane).
- **Progress is a commit.** The loop advances when HEAD advances. `--max-stall`
  consecutive iterations with no commit halts it and notifies.
- **Reviews are keyed to commits.** Every `--review-every` commits, the review
  prompt runs (read-only, writes findings) and does not consume a work slot; the
  next work iteration resolves the MUST-FIX items first.

## The supervision (why a crash is never silent)

- heartbeat `status.json` + `loop.log`, written throughout;
- `launchd` `KeepAlive={SuccessfulExit:false}` — a crash auto-restarts, a clean
  finish stops;
- a per-session wall-clock timeout **and** a staleness killer (no output growth
  for `--stale-after` seconds);
- permission auto-rejections detected and reported;
- `--self-test` runs a fake hung session and asserts the watchdog kills it.

## Use

```sh
ralph-loop.sh --workdir DIR --label NAME \
  --prompt ralph/PROMPT.md --review-prompt ralph/REVIEW_PROMPT.md \
  [--review-every 3] [--max-stall 3] [--max-iter 200] \
  [--session-timeout 3600] [--stale-after 1800] \
  [--done-file ralph/DONE] [--stop-file ralph/STOP] \
  [--needs-human-file ralph/NEEDS_HUMAN.md] [--last-review ralph/.last_review] \
  [--notify] [--install-launchd] [--plan] [--self-test]
```

Markers are written where the repo asks (`ralph/DONE`, `ralph/STOP`,
`ralph/NEEDS_HUMAN.md`, `ralph/.last_review`) and added to `.git/info/exclude`
so they never dirty the tree the loop commits into. State and logs live outside
the tree in `~/.svrnmesh/ralph/<repo>-<label>/`.

- `ralph/DONE` — campaign complete; the loop stops.
- `ralph/STOP` — a halt (or a manual stop); remove it to resume.
- `ralph/NEEDS_HUMAN.md` — a decision package; the loop notifies and stops.

## Install as a launchd job

```sh
OPENCODE_CONFIG=/path/to/ralph.json ralph-loop.sh ... --install-launchd
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.ralph.<repo>-<label>.plist
```

`--install-launchd` writes the plist and prints load/stop/watch. It does not
load it.

## What the repo supplies

Ersilia is the worked example: `ralph/PROMPT.md` (the iteration work order),
`ralph/STATE.md` (the lane queue), `ralph/REVIEW_PROMPT.md` (the principles
audit against `commonwealth-ai/sovereign/ARCH_PRINCIPLES.md`). A new repo
writes those three files and points the loop at them; nothing else changes.
