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
| The backlog, ranked | `scripts/co-backlog.py --open` | The heap as the ruler scores it today. Unvetted items render greyed with the missing line named. Machine-scored items say who scored them. |

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
The full map (four artifacts, producer contract, why there is no heap)
is `scripts/BACKLOG.md`.

## The pool and the record

| I want to see | Run |
|---|---|
| Open orders | `scripts/co-order.sh list` |
| One order's file | `.sovereign/features/<id>/order.md` (plain markdown, hand-editable) |
| Session frames (what each terminal's last session banked) | `svrn session frames` |
| Who is touching what right now (mesh-wide) | `svrn tools call work_in_flight --scope= --match_mode=file` |
| The seat's edit-rate scoreboard (the M0 promotion metric) | `scripts/co-directive-log.sh --stats` |
| Recent landing verdicts | `tail ~/.sovereign/comaintainer/verdicts.jsonl` |

The raw machine logs live in `~/.sovereign/comaintainer/`:
`directives.jsonl` (every draft/final pair), `verdicts.jsonl` (every
landing review, interactive and nightly).

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

## Escape hatches (standing, operator direction 2026-08-06)

Everything is skippable. You can hand-run a worker in your own
terminal against an order file, ignore the briefing, or drop to plain
sessions any day — orders and the seat never make the simple path
harder. If a page looks wrong, the page's footer names the store it
read; the stores are the truth, the pages are views.
