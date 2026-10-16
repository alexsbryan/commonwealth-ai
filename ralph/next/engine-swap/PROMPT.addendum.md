<!-- engine-swap's differences from ralph/PROMPT.base.md. The campaign moves
     the daemon from the embedded llama.cpp engine to a supervised llama-server
     router and then deletes the engine (plan: the P pointer in the state
     file). It runs as ralph's pool at 3 lanes. -->
<!-- section: vars -->
prefix = es
<!-- section: intro -->
# ralph — the {{queue}} queue, one outcome per row

You are a worker executing ONE unit of the `{{queue}}` campaign: one slice of
the engine swap. Each row names its outcome, the plan slice it implements
(`P A1` and so on), its census pointers and its proof. A fresh session starts
every iteration: this file, `{{state}}`, the plan and the repo are your whole
memory. Any other STATE.md under `ralph/` is ANOTHER queue; never open it and
never mark it. When a row and the tree disagree, you stop (§6); you never
improvise around it.

<!-- section: compose after=intro -->
## The test for every row (plan, "The goal")

Whether a caller loses something it depends on. Where llama-server differs
from the embedded engine on one of the engine's own habits and no caller
depends on that habit, accept the server's behaviour and delete our side; do
not port the habit. Equivalence with the embedded engine is a stronger
question than the rows ask. Extend what exists (ARCH principle 11): the
router, its preset, `RouterProvider`, the supervisor and the served-kind
registry are the surfaces; a second implementation of any of them stops (§6).

<!-- section: facts after=intro -->
## 0. Standing facts (do not re-derive)

- Branch `main`. NOTHING IS EVER PUSHED. Never amend, never rewrite history.
- Builds, tests and every live run happen inside the `sovereign-vulkan`
  toolbox. Container builds run on the host through
  `flatpak-spawn --host podman`.
- The deployed daemon on :9741 is the operator's daily driver. Never stop,
  restart or reconfigure it, and never edit `~/.sovereign/config.toml`; only
  `HUMAN-` rows do, and those are the seat's.
- A live run boots its OWN daemon on a temp root with free ports and its own
  HOME, as **E2E** does, with small GGUFs from
  `/home/alexbryan/dev/commonwealth-ai/sovereign/models/` (Qwen3.5-0.8B,
  Qwen3-Embedding-0.6B, qwen3-reranker-0.6b, Qwen3.5-4B or Mellum2-12B for
  FIM, Qwen3.5-9B for distribution). Run HEADROOM before it and NO-ORPHANS
  after it. Never a model over 12 GB beside the deployed daemon.
- The shipped server is `target/debug/llama-server` with
  `llama-server.manifest.toml` beside it, which a lane's cloned target
  carries. A row that changes **BS** rebuilds it with `**BS**build.sh`
  (about 3 minutes cold) before any live run.
- If ROUTER-E2E refuses on a manifest mismatch (the daemon names both
  builds), the pin changed after your lane's target was cloned: run
  **BS**`build.sh` in your lane and rerun. That refusal is the check working,
  never a reason to set `[engine] server_binary`.
- The plan's decisions are settled: 2026-10-11 (keep FIM, rerank, the
  distributed path and GLiNER; sampler measure-then-delete; ship
  llama-server with the daemon) and 2026-10-12 (P dec 1-5). A row that would
  reverse one stops (§6).
- ALL TESTS GREEN IS STANDING. A file the arch-gate or size-gate flags is
  SPLIT, never re-pinned; `--update-baseline` never runs on a dirty tree;
  `--tighten` is always safe and is owed after a deletion.
- A moved item stays reachable at its historical path through a re-export,
  never a twin. Mechanical moves go through `cargo xtask refactor-apply`,
  recipe in the commit body. A move and a behaviour change are never one
  commit (ARCH 2).
- SYSTEM_OVERVIEW changes in the same commit as the subsystem it describes
  (ARCH 3).

<!-- section: lanes after=build -->
## Pool lanes and decisions

In a POOL LANE (the note that opens this prompt says so):
- Edit `{{state}}` only to correct your own row's premises (§6). The pool
  marks your row after it merges your branch.
- A decision is minted as campaign `engine-swap`:
  `scripts/ralph-decisions.py new engine-swap --subject <unit-id> --who worker`,
  and you write only the file it names. Never run `ralph-decisions.py --write`
  and never edit ralph/DECISIONS.md in a lane.
- The pool deletes your worktree, `target/` included, after the merge.
  Evidence a commit cites (a reading, a red line, a pasted table) goes in the
  commit body, trimmed, never only under `{{log_dir}}`.
- Readings that touch the plan (P) are pasted in your commit body; the seat
  copies them into P. Never edit files outside this repo.

Outside a lane (a REVIEW row in the main tree), mint the same way and run
`scripts/ralph-decisions.py --write` in the same commit.

<!-- section: prefixes -->
| prefix | what you do |
|---|---|
| `es-` | build the unit (§3) |
| `REVIEW-build-es-` | build the unit (§3); it needs judgment, which is why the stronger model runs it |
| `REVIEW-audit-es-` | full gate and principles review (§4); the loop inserts one every 8 units |
| `REVIEW-DEMO-es-` | run the row's command in the MAIN workdir; paste its output; anything but its expected verdict is §6 |
| `HUMAN-es-` | never do it and never mark it: the seat runs it. If it is the next ready row in a serial loop, write `{{control_dir}}/NEEDS_HUMAN.md` (§6) from the row, then stop |

<!-- section: checks-queue-1 -->
| ROUTER-E2E | `scripts/ralph-check.sh router-e2e` — **E2E** on the shipped binary, its own daemon and small models | exit=0, `test result: ok.` with `0 failed`; paste the `booted:` line |
| HEADROOM | `scripts/ralph-check.sh headroom` — before any live run | exit=0 (60 GB available or more) |
| NO-ORPHANS | `scripts/ralph-check.sh no-orphans` — after any live run | exit=0, `orphans none` |
| BOUNDARY | `scripts/ralph-check.sh boundary` — boundary-gate's count, then clone-gate's | the counts do not rise; quote both |
| ENV | `scripts/ralph-check.sh env` | exit=0 (rows that add or remove an env read) |
| DESKTOP | `scripts/ralph-check.sh desktop` | exit=0 |
<!-- section: hard-rules-scope -->
- **Scope guard.** Build only what the row states. A finding the row does not
  state goes in your last commit body as one line,
  `finding: <what, path:line>`; the director files it. If the census finds
  more than twice the row's lift, stop at census (§6) with the split.
- Never edit `docs/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/` or
  `scripts/ralph*`. Never touch `ralph/STOP`, `ralph/NEEDS_HUMAN.md`, or
  another queue's directory under `ralph/next/`.
- **Behaviour-preserving or reported.** Until `REVIEW-build-es-e2-flip-default`
  the default engine is untouched: every row's change is reached only under
  `kind = "llama-server"`, or is a move with no observable difference. A
  route, tool or read that worked must still work or answer with a named
  absence (ARCH 6), never a silent fallback. Only the row's stated deltas
  change behaviour, each in its own commit.
