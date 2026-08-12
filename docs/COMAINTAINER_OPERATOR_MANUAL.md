# Comaintainer Operator Manual

The one-page quick reference for the operator's own hands. The seat
runs most of these for you during a session; everything here is safe
to run yourself, any time, in any terminal at the repo root. Nothing
in this file is agent protocol — that lives in
`.claude/skills/comaintainer/SKILL.md`; the role's design is
`docs/COMAINTAINER.md`; the ruler the backlog is scored against is
`quality/backlog-ruler.toml`.

CLI note: `svrn` is the prod symlink. Hosts that predate it carry the
legacy `sovereign` symlink instead — every `svrn` command below works
with `sovereign` substituted.

## The three pages

| I want to see | Run | What renders |
|---|---|---|
| The morning glance — what changed in the architecture since I last looked | `svrn code fieldglass --open` | The fieldglass page (browser). Full render takes minutes (the duplication tier embeds); `--no-dup` is the quick pass. Evidence only, never verdicts — `docs/FIELDGLASS.md`. |
| Everything waiting on me | `scripts/co-closeout.py --open` | The closeout page: pending decisions first (each with its stated default if you say nothing), then resolved-in-window, open orders, recent verdicts. |
| The backlog, ranked | `scripts/co-backlog.py --open` | The heap as the ruler scores it today. Each card is headed by the item's NAME — `svrn backlog add` drafts it, a hand-written item falls back to its own first sentence — with the ref hash demoted to the metadata line under it. Unvetted items render greyed with the missing line named. Machine-scored items say who scored them. |

Rendered copies persist at `~/.sovereign/comaintainer/{closeout,backlog}.html`
and `~/.sovereign/arch/<corpus>/fieldglass.html` — re-openable without
re-rendering.

## The backlog

| I want to | Run |
|---|---|
| File a discovery (scored draft, you stay the vetter) | `svrn backlog add "<the discovery>" --objective "<what it serves>"` |
| File without a model score | add `--no-score` |
| Pull the top item as an order draft | `scripts/co-backlog.py --pull` |
| Check the backlog machinery itself | `scripts/co-backlog.py --self-test` |

A machine-scored item carries `Scored-by:` and cannot be pulled until
a person reviews it and clears that line — that review IS the vetting.
The same call also drafts the item's `Title:`, which is what you read on
the page; edit it like any other field if the model named it badly.
The full map (four artifacts, producer contract, why there is no heap)
is `scripts/BACKLOG.md`.

## The pool and the record

| I want to see | Run |
|---|---|
| Open orders — local files PLUS orders opened by a seat on any mesh machine, each with node attribution | `scripts/co-order.sh list` |
| One order's file | `.sovereign/features/<id>/order.md` (plain markdown, hand-editable) |
| Session frames (what each terminal's last session banked) | `svrn session frames` |
| Who is touching what right now (mesh-wide) | `svrn tools call work_in_flight --scope= --match_mode=file` |
| Is a SHARED resource (daemon, soak, snapshot) taken right now? | `svrn claim may-i daemon:<node>:<action>` — verdict `held` / `expired` / `free`, one call, no sockets |
| Take a shared resource for an operation | `svrn claim take daemon:<node>:<action>` (30-min TTL; `--ttl` to shorten), `svrn claim release <id>` when done |
| Which peer is my daemon serving right now? | `curl 127.0.0.1:9741/status` → `inference.peer_requests[]` (`active`, `served_total`, `name`) |
| The seat's edit-rate scoreboard — the MESH-WIDE number: the notes store is the denominator, so either seat reads the same count | `scripts/co-directive-log.sh --stats` |
| Directives carrying no edit verdict yet | `scripts/co-directive-log.sh --unclassified` |
| Recent landing verdicts | `tail ~/.sovereign/comaintainer/verdicts.jsonl` |

The scoreboard's `n` counts only rows where the seat STATED the verdict
(`--edited` / `--unedited` at resolve time, or a retrospective
annotation); `indet` / `nodec` / `unclass` sit outside that denominator
in their own columns so it is never quietly widened. Before 2026-08-10
the verdict was inferred from whether the final text differed from the
draft, which measured the seat's summary-writing habit and reported
97.5% edited against a true 13.3%.

Since 2026-08-11 (order seat-durable-rail) the tally is MESH-WIDE: the
denominator is the notes store — every directive the seat logs on ANY
machine writes through as a note anchored `directive-log` — so the
number reads the same from either seat, and the rows carry node
attribution. When the notes daemon is down, `--stats` falls back to
the local files and says so on its banner; the local number is not the
mesh number. Local rows that predate the write-through are excluded
from the mesh denominator and counted in that banner.

The raw machine logs live in `~/.sovereign/comaintainer/`:
`directives.jsonl` (every draft/final pair, plus
`directive-edit-verdicts.jsonl`, the append-only sidecar carrying the
edit verdict for rows logged before the flag existed), `verdicts.jsonl`
(every landing review, interactive and nightly).

## Health, gates, quality

| Question | Run |
|---|---|
| Is any quality subsystem stale? | `svrn posture` (each row names its refresh command) |
| Does the workspace compile? | `./scripts/sovereign-lint.sh --human --full` |
| Do tests pass? | `./scripts/sovereign-test.sh --human` |
| Did quality regress (retrieval/routing/synthesis)? | `./scripts/sovereign-ci-bench.sh --quick` (~35-40m; read lane KIND before the number) |
| Is the daemon healthy? | `svrn doctor` |

## Seat-internal (listed so nothing is mysterious — you rarely run these)

- `scripts/co-review.sh <ref>` — the landing-review helper behind every
  verdict draft.
- `scripts/co-directive-log.sh` (write forms) — how the seat logs
  draft/final pairs; you only ever see the drafts themselves.
- `scripts/co-field.py` — the seat's fieldglass reader: changed-file
  evidence and landing diffs for verdict records (`docs/FIELD_VERDICTS.md`).
- `scripts/co-sweep.sh` — the nightly shadow sweep (launchd, 03:30):
  reviews every commit since the last sweep into `verdicts.jsonl`.
  `--install` / `--uninstall` manage the launchd agent.
- `scripts/co-backlog-producer.sh` — the contract automated signals
  file through (e.g. a failed HARD bench lane); never run by hand.
- `scripts/co-order.sh new|check|close` — order lifecycle; the seat
  drives these, but the order file is always yours to edit directly.
  Since 2026-08-11 the mesh write-through is on by default and silent:
  `new` also opens a notes-store shadow (so any mesh seat lists it),
  `close` retires it — a daemon that is down is a named notice, never
  a failure.
- `scripts/co-mesh-drill.sh` — the two-seat conformance drill for the
  mesh-visible rail (UC-D1..D4 AND UC-F1..F8, four-verdict readings;
  procedure `scripts/CO_MESH_DRILL.md`). The seat runs it across the
  two machines when the rail changes. The D-cases need the operator to
  relay steps between machines; the F-cases run THEMSELVES — one
  hand-written start note, each side's `f-exec` on the epoch schedule,
  verdicts written as anchored notes, `f-assemble` builds the table
  from the notes alone (UC-F8; the operator's only act is the start
  note, and the deploy is seat-owned).
- `sovereign seat watch [--once]` — the notes-rail poller as a
  mechanism: surfaces new seat-addressed records (anchored
  `comaintainer-seat`, `order-seat`, `directive-log`) as `SEAT_WATCH`
  lines. This is the mechanism the F-drill runs from.
- `SOVEREIGN_SEAT=1` — the seat's session flag: the ambient notes read
  includes the coordination rail instead of withholding it (and the
  one-line withheld notice disappears from the prompt). Ordinary
  sessions never set it.

## Escape hatches (standing, operator direction 2026-08-06)

Everything is skippable. You can hand-run a worker in your own
terminal against an order file, ignore the briefing, or drop to plain
sessions any day — orders and the seat never make the simple path
harder. If a page looks wrong, the page's footer names the store it
read; the stores are the truth, the pages are views.
