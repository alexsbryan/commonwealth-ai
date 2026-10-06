# Make the ralph loop a closed state machine

Status: proposal, 2026-10-06, not ratified. It describes `scripts/ralph.py` as
of b9a2c07f0.

`scripts/ralph.py` runs a coding-agent campaign. It picks a ready row from a
queue, hands it to an agent session, and repeats until every row is done or a
human is needed. Its docstring promises that it "cannot resolve to quietly
stuck". Eighteen fixes to its behaviour landed between 2026-10-02 and
2026-10-06, and on the last of those days it sat quietly stuck for more than
four hours, with about 43 more to go before anything would have noticed. This
note explains why the fixes have not converged and names the property that would
make them converge. It then gives a target machine small enough to finish. The
operator's bar for the loop is a finite state expression, and a point where the
loop is done.

## The incident

The ersilia pool lane `r12-release-preview` ran its release cut as a background
run. It wrote `ralph/waiting` naming `ralph/r12-release-preview-close5.done`, with
a launcher that touched that marker only when the gate passed. The cut refused
within a minute, because the lane's own untracked files made the tree dirty, and
exited 1. No marker was written. The pool cannot tell a run that has not finished
from one that died. It held the lane, logged one line a minute, and would have
parked the row at 48 hours (`LANE_MAX_WAIT_SECS`). The ring's close (`DEMO-DAY-12`,
then `REVIEW-18`) depended on that lane. So the campaign's end sat behind a
failure that took seconds to happen and one session to fix.

## Eighteen fixes, sorted by cause

These are the fix commits to `scripts/ralph.py` from 2026-10-02 to 2026-10-06,
sorted by what each one patched. b9a2c07f0 (the permission bridge's paths)
landed in the same window but concerns a helper rather than loop state, so it
is not counted.

| Cause | Fixes | Commits |
|---|---|---|
| The loop's own process lifecycle | 7 | c6f375538 (a SIGKILL instead of a SIGTERM orphaned a lane session), 07bf3ebe1 (an unparseable `STATE.md` killed the supervisor, ~18 h down), adb515604 (the supervisor died of its own errors and of ENOSPC), 6bc367088 (a stop naming no row ended the night), a0df57d1f (fixes never reached the running pool), 24355ab77 (finished jobs re-ran at login), 0eec06b5e (`start` killed a running job) |
| The environment | 4 | 8e98859b3 (no `cp --reflink` on macOS), 0a4289609 (a 40 GB disk floor), db84fe84a (reclaim build output before waiting), d7ab91fc2 (the floor lowered to 5 GB) |
| One transition coded twice | 2 | a617796fe (the serial driver halted on an all-done queue that the pool ends), b1d992295 (a closing audit, fixed "in run and in the pool") |
| An outcome guessed from side effects | 2 | 83f56804b (a productive lane struck at the session cap), 1a6b5cef0 (sessions that died in 30–90 s struck lanes out) |
| A wait on a file that may never appear | 1 | e899ef1f4 (a run killed by a reboot; an overdue run halted the whole pool) |
| Control state written by the agent | 1 | f0b70b6c4 (prose on `waiting`'s first line; a lane's own done marker named as the run's) |
| A case with no transition, so it halted | 1 | 16223550c (a merge conflict halted the pool, 8 times) |

The sort is a judgement, and a few commits touch two classes. e899ef1f4 also
changed deployment. The shape does not depend on where those edges fall. Each
fix is a correct guard for one way things went wrong. The guards don't add up to
a finished machine, for the reasons below.

## Why the fixes do not converge

### The state lives in files that other actors write

The loop's state is a set of files: `STATE.md` rows, `ralph/waiting`, `*.done`
markers, `ralph/lanes/<unit>.done`, `NEEDS_HUMAN.md`, `ralph/parked/*.md`, `STOP`
and `DONE`. The loop writes some of them. Agent sessions write most of the rest,
following prose instructions (`PROMPT.md` and the lane note in `Pool.run_lane`).
Background runs write markers through shell one-liners the agent composes. The
operator edits files by hand.

A state machine can be made total, meaning every pair of state and input is
defined, only over a closed set of inputs. Whatever an agent writes to disk is
not a closed set. An agent can always produce a configuration nobody
anticipated:
- a sentence on the first line of `waiting` (f0b70b6c4);
- a lane's own done marker named as the run's (f0b70b6c4);
- a launcher that writes its marker only on success (2026-10-06);
- an untracked marker that dirties a tree a later step needs clean (2026-10-06).
  `ensure_excludes` keeps the loop's own markers out of `git status`, but it
  cannot know the names an agent invents.

Each guard enumerates one more mistake, and because the set of possible mistakes
is open, the guards never run out. Principle 10 in `docs/ARCH_PRINCIPLES.md`
already says so: a model's behaviour is a threat, not a bug, and a prompt
instruction relied on for correctness is a gamble re-run every session.

### Some states end only when something has been absent long enough

A lane waiting on a background run leaves that state when the marker appears. If
the run dies without writing it, nothing appears, and absence is not an event. The
only exit is a clock: 24 hours in the main tree (`DEFAULT_WAIT_LIMIT_S`) and 48 in
a lane (`LANE_MAX_WAIT_SECS`). Each clock has to be longer than the longest
legitimate run (r9-boundary-sweep ran about 23 hours), so every death costs the
full limit. The reboot check from e899ef1f4 recovers one cause of death. A run
that is OOM-killed, crashes, refuses, or skips its marker still waits out the
clock. Principle 6 names the trap: absence of a response is not evidence of
absence.

The same shape appears in three more places.
- The watchdog reads a stale heartbeat as "stalled", but `Pool.run` writes no
  heartbeat between waves. Only `Session.heartbeat`, `Campaign._beat` and the
  supervisor's cool-down write one, so an idle pool and a dead pool look the
  same.
- When `STATE.md` does not parse, the pool sleeps a minute and tries again,
  forever, without a log line.
- When nothing is ready while lanes are waiting, the pool logs "no ready unit …
  waiting" every minute and never alerts.

### Session outcomes are guessed

When a lane session ends, `Pool.run_wave` works out what happened by checking a
ladder of signs, in this order:
1. a non-empty `NEEDS_HUMAN.md` in the lane;
2. a `ralph/lanes/<unit>.done` that the lane's own commits added;
3. a `waiting` file naming a valid marker;
4. new commits since the session started, which counts as a continuation, at
   most 6 in a row;
5. an end within 120 seconds with no commits, which counts as "did not run", at
   most 3 in a row;
6. anything else, which is a strike (three strikes halt the pool).

Each rung is a heuristic over side effects. Two of the eighteen fixes were rungs
added after a real outcome was misread (83f56804b, 1a6b5cef0). The session knows
what happened, but the loop never asks it.

### The machine is written three times

`Campaign` (serial), `Pool` (parallel) and `Supervisor` (wrapped around either)
each implement overlapping transitions with their own limits:
- Waiting is limited to 24 hours in one driver and 48 in the other.
- The 120-second "did not run" rule is coded in the pool and again for the
  supervisor's resolver.
- The closing audit had to be fixed in both drivers (b1d992295).
- A lane's `NEEDS_HUMAN.md` stops the whole pool until the supervisor parks the
  row and relaunches it. The supervisor's own rule since phase-b-31 is to park
  that one row and keep running.

This breaks principle 8: one decider, one name.

### The lifecycle is a stack of watchers

The pool runs under a supervisor that cools down and relaunches it. The
supervisor runs as a launchd job without `KeepAlive`, and a watchdog job checks
the whole stack every two minutes. Each layer exists because the layer inside it
was not trusted to stay up, or to say why it stopped. Seven of the eighteen fixes
landed in this stack.

Principle 12 asks who owns the loop's lifetime. Restarting a crashed process
belongs to the operating system: launchd `KeepAlive` on macOS, systemd
`Restart=on-failure` on Linux. Deciding that a unit needs a human belongs to the
unit's state machine.

## What would converge

Four properties. When all four hold, the ways the loop can be wrong are bounded
by the size of its transition table. A new incident is then either a wrong cell,
fixed by changing the cell, or an environment condition, fixed by changing a
number. Neither adds a code path.

1. **The loop owns every input.** Every event the machine consumes comes from
   something the loop itself runs or does: a process's exit status (sessions and
   background runs alike), its own git operations, its own clock, or an operator
   command. No file an agent writes is read as loop state.
2. **A session reports one typed result.** A session ends by calling
   `ralph.py result` with exactly one of `done`, `continue`,
   `await <budget> -- <command>` or `needs-human <why>`. The command validates
   the result when it's called, so the session sees a refusal while it can still
   fix it, instead of the loop finding a bad state hours later. A session that
   ends without a result is the event `no_result`, and the loop infers nothing
   beyond that.
3. **Every waiting state has an exit that will arrive.** Either a process the
   loop owns will end, or the state carries a time budget set when it was
   entered. There is no global wait limit.
4. **The machine is a table, and a test proves it total.** The transition table
   is data. A test walks every (state, event) pair and asserts that each has a
   defined next state, or is marked impossible with a reason. It also asserts
   that every waiting state has an owned or budgeted exit. Each past incident
   becomes a row in that test.

Two things stay open. The environment (disk, memory, model providers, reboots)
is outside the loop and always will be; one precondition guard handles it, not a
set of states. And a session can report `done` when it isn't. That is a
verification question, which the order's PASS BARs and the review rows answer,
not a question of loop state. This design leaves it where it is.

## The target machine

### A unit

A unit is in one of these states:
- `pending`: its dependencies are not done;
- `ready`;
- `running`: a session the loop holds;
- `awaiting`: a background run the loop holds, with its budget;
- `merging`: pool only;
- `held`: waiting on the operator, because it is parked, a `HUMAN-` row, or
  outside the frozen scope;
- `done`.

| From | Event | To | Action |
|---|---|---|---|
| pending | dependencies done | ready | |
| ready | dispatched | running | start the session; record its base commit |
| running | result `done` | merging | |
| running | result `continue` | ready | continuation count +1; past K it counts as a strike |
| running | result `await b -- cmd` | awaiting | the loop starts `cmd` in its own process group, logging to a known path, and records the pid, its start time and the budget b |
| running | result `needs-human why` | held | write the package, notify once; the rest of the pool keeps running |
| running | `no_result`, new commits | ready | as `continue` |
| running | `no_result`, no new commits | ready | strike |
| running | operator stop | ready | the session is killed; its worktree resumes later |
| awaiting | the run's process ended with exit code c | ready | the next session is told c and the log path, whatever c is |
| awaiting | budget b passed, process still alive | ready | kill the run's process group; strike; the next session is told why |
| merging | merge clean | done | mark `[x]`, commit, keep evidence, remove the lane |
| merging | conflict | ready | merge strike; the lane merges the base in and resolves it |
| any | strikes reach N | held | package with the last session's evidence; notify once |
| held | operator unparks | ready | strikes reset |

"The run's process ended" covers every way a run can stop: success, failure, a
crash, an OOM kill or a reboot. The loop records the pid together with the
process's start time, so a pid that gets reused after a reboot is not mistaken
for the run. With this table, the incident above becomes `awaiting → ready`
within a minute of the cut exiting 1, and the lane's next session reads the
refusal in the log.

A charter's director, today the supervisor's resolution session, fits in as one
optional transition. When a charter covers the unit, `held` (by strikes) goes to
`running` with the resolver, before the operator is notified. It is a policy in
the table, not a separate process.

### The pool

| State | Meaning | Leaves when |
|---|---|---|
| `running` | at least one unit is running or awaiting | the units' own events |
| `blocked(p)` | a unit is ready but precondition p fails | p holds again; if p keeps failing past its limit, the pool notifies once and stays blocked |
| `idle` | nothing is ready, and some units are awaiting | their runs end or reach their budgets, both bounded |
| `stuck` | nothing is ready, running or awaiting, and some units are held | an operator act; entering it notifies once, naming the held units |
| `done` | every row is done, after a closing audit if one is owed | terminal |
| `stopped` | operator stop | operator start |

The preconditions are one list, checked before each dispatch:
- free disk on the lane root;
- free memory for the cargo budget;
- a healthy model in the roster;
- a queue that parses.

Each precondition has a limit after which it notifies, and none of them is a
state of its own. Probing the roster before every dispatch is also what makes
the 120-second "did not run" rule unnecessary: a dead provider blocks dispatch
instead of using up strikes.

### The process

The loop is one process. It writes its heartbeat every tick in every pool
state, so a stale heartbeat means dead and only dead. It exits 0 at `done` and
at `stopped`. The host restarts it on any other exit: `KeepAlive` with
`SuccessfulExit` false on macOS, `Restart=on-failure` on Linux. Deployment stays
as it is: between waves, and from committed code only. The watchdog keeps one
job, noticing that the loop is down or its heartbeat is stale, which is the one
thing the loop cannot report about itself.

## What this removes

The target is smaller than today's code. From `scripts/ralph.py`:

- `waiting_marker`, `wait_for_marker` and their guards, along with
  `ralph/waiting` and every agent-named `*.done` marker;
- `machine_boot_time` and both reboot checks;
- `DEFAULT_WAIT_LIMIT_S`, `WAIT_LIMIT_S` and `LANE_MAX_WAIT_SECS`;
- the outcome ladder in `Pool.run_wave`, and `NEVER_RAN_SECS`, `MAX_NEVER_RAN`
  and `NEVER_RAN_BACKOFF` with both of their uses;
- `Campaign` as a separate driver: serial becomes the pool with one lane, and a
  review becomes a unit whose worktree is the main tree;
- the supervisor's cool-down ladder and relaunch loop (`SUPERVISOR_COOLDOWNS`
  and the error net in `Supervisor.run`), replaced by the host's restart and the
  `blocked` and `stuck` states;
- the watchdog's disk-low condition, which becomes the pool's disk
  precondition;
- the background-launch recipes in each queue's `PROMPT.md`, and the `waiting`
  instructions in the lane note.

## How to get there

Each step lands on its own. A step is behaviour-preserving unless it says
otherwise (principle 2), and each names a count it lowers.

1. **Write today's machine down as a table**, describing it as it is, with the
   eighteen incidents as test rows. The totality test then reports the cells
   that are undefined or that only a clock can exit. This confirms or corrects
   the analysis above before anything changes. The count is undefined and
   clock-only cells; the later steps take it to zero.
2. **Add `ralph.py result` and the loop-owned `await`.** Switch the lane note
   and `PROMPT.md` to them in the same change, and delete the `waiting` parser
   once no queue uses it. This changes behaviour: a background run's end,
   success or failure, resumes its lane at once. The counts are the control
   files an agent writes (down to one result per session) and the wait limits
   (down to zero).
3. **Replace the outcome ladder** with the typed result and the dispatch-time
   precondition check. This changes behaviour: a session that ends without a
   result is `no_result`, and the "did not run" rule goes. Before landing it,
   check against the 2026-09-17 to 10-04 sessions cited in 1a6b5cef0 that a
   probe at dispatch would have blocked them.
4. **Fold `Campaign` into the pool as one lane.** Drivers go from three to one.
5. **Move restart to the host**, give the pool its heartbeat and its `stuck`
   notification, and reduce the supervisor to the optional director transition.
   Lifecycle layers go from four to two: the loop and the host's restart, plus
   the watchdog's single check.

## When it is done

The loop is finished when all of these hold:

- the transition tables are data, and the totality test passes;
- every event comes from something the loop owns: process exits, its own git
  operations, its clock and operator commands;
- every waiting state has an owned or budgeted exit;
- there is one driver, one escalation path (retry, strikes, held, notify) and
  one precondition guard;
- the only thing an agent writes for the loop is its result.

After that, changes are numbers in configuration, not code: budgets, strike
limits, and disk and memory floors. A new kind of event source would reopen the
design, and that should be a deliberate decision rather than a fix.

## Open questions for the operator

- When a run passes its budget, should the loop kill it and return the unit to
  the lane with a strike, as proposed here? Or should it hold the unit for the
  operator and leave the process running?
- Should serial `run` become the pool with one lane, or does a queue rely on
  main-tree serial behaviour beyond reviews?
- Should the director stay as an optional transition on `held`, or be retired?
- Before a public release, some policies are specific to this workspace and
  would need to become configuration:
  - the cargo budget (`scripts/lib/cargo-jobs.sh`);
  - decision renumbering (`scripts/ralph-decisions.py`);
  - copying `.sovereign/features` into lanes;
  - the audit row's text, which names `sovereign-cli`.

  The queue grammar and the `Host` abstraction already look general.

## Appendix: today's machine, as found

What each of today's waiting points can leave on, and what kind of signal that
is. Owned means an event the loop produces itself. Every other kind is a source
of future incidents.

| Situation today | Encoded as | Leaves on | Kind |
|---|---|---|---|
| a lane waits on a background run | `ralph/waiting` in the worktree | the marker appears; the file is older than the last boot; 48 h, then parked | file, boot time, clock |
| the main tree waits on a background run | `ralph/waiting` in the main tree | the marker appears; boot time; 24 h, then a halt | file, boot time, clock |
| a lane session has ended | files, commits and duration | the six-rung ladder in `Pool.run_wave` | inferred |
| nothing ready while lanes wait | nothing | only the lanes' clocks; a log line each minute | silent |
| `STATE.md` does not parse (pool) | nothing | the file is fixed; no log line | silent |
| free memory under the floor | nothing | memory returns; never alerts | environment, silent |
| free disk under the floor | nothing | space returns, after the pool reclaims idle lanes' build output; the watchdog alerts at the same 5 GB | environment, alerted |
| no model in the roster answers | the probe | a halt, then supervisor cool-downs and relaunch | owned |
| a lane writes `NEEDS_HUMAN.md` | a file in the worktree | the whole pool halts; the supervisor parks the row or resolves it | file |
| a merge conflicts | git | a lane strike; 3 strikes halt | owned |
| the supervisor has halted | `STOP` and `NEEDS_HUMAN.md` | up to 4 resolutions, a park, 6 cool-downs, then exit | file, clock |
| the loop process is down | the launchd job state | the watchdog notifies; a person restarts it | host |
