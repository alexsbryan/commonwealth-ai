# ralph-loop — a commit-driven campaign loop

`scripts/ralph-loop.sh` runs a coding-agent campaign until the repo says it is
done. It is svrnmesh-cln's shape — the one that rebuilt a 1M-line repo in four
days — with no daemon around it.

## Run it

```sh
nohup scripts/ralph-loop.sh --workdir . --label ring1 \
  --prompt ralph/PROMPT.md >> ralph/log.txt 2>&1 &
tail -f ralph/log.txt
```

One process, one append-only log. The agent's output streams into it live, so
progress is visible as it happens — not summarised after. Stop it with
`touch ralph/STOP` (or `kill`).

## The model

- **The queue is `ralph/STATE.md`** — the plan is memory, so a fresh session
  knows where it is. The unit is one order (or one `REVIEW-n`), not a
  micro-task; the order's lanes are its acceptance criteria, not an execution
  script.
- **Progress is a commit.** The loop advances when HEAD advances; `--max-stall`
  iterations with no commit halts it and notifies.
- **Commit as you go.** The prompt requires incremental commits, so a session
  killed at any moment costs at most the in-flight step.
- **Reviews are queue units** (`REVIEW-n`): audit the named orders against
  `ARCH_PRINCIPLES.md`, then fix and consolidate, behaviour-preserving.
  Optionally `--review-prompt FILE --review-every N` also runs a read-only
  review every N commits.

## The safety, in the loop

- a session past `--session-timeout` is killed; its uncommitted work stays in
  the tree and the next iteration resumes it (the loop tells the next session
  the tree is dirty);
- `--max-stall` consecutive no-commit iterations halt the loop and notify;
- permission auto-rejections are counted and reported;
- markers: `ralph/DONE` (complete), `ralph/STOP` (halt), `ralph/NEEDS_HUMAN.md`
  (a decision package — the loop notifies and stops).

## Options

```
--workdir DIR --prompt ralph/PROMPT.md [--label NAME]
[--review-prompt FILE] [--review-every N] [--max-stall N] [--max-iter N]
[--session-timeout S] [--done-file ralph/DONE] [--stop-file ralph/STOP]
[--needs-human-file ralph/NEEDS_HUMAN.md] [--last-review ralph/.last_review]
[--notify] [--plan]
```

## Parallel lanes

`ralph-loop.sh` is serial — one unit at a time. When a ring's frontier has
independent units (ring 1 opened with five; ring 2 has `transform-rung` beside
the registry chain), `ralph-pool.sh` runs them concurrently:

```sh
nohup scripts/ralph-pool.sh --workdir . --prompt ralph/PROMPT.md --lanes 2 \
  --review-model <model> >> ralph/log.txt 2>&1 &
```

It is wave-based: each wave takes up to `--lanes` ready units, runs each in its
own **git worktree** on its own branch, waits, then merges the finished ones
**serially** into the main tree. Safety:

- lanes never share a working tree, so no two sessions edit the same files;
- a merge conflict **aborts and halts** (`ralph/NEEDS_HUMAN.md` + `ralph/STOP`) —
  never auto-resolved;
- `REVIEW` units run serially in the main tree (a review must see its units);
- lanes do not edit `STATE.md`; the pool marks a unit `[x]` after merging, so the
  merge never fights over the queue file;
- a lane session runs in its own process group and is killed (group) past
  `--session-timeout`.

A lane session writes `ralph/done/<unit>` (committed) when the unit passes its
own tests; the pool merges a lane whose marker is present.

A repo supplies three files and points the loop at them: `ralph/PROMPT.md` (the
iteration work order), `ralph/STATE.md` (the queue), and — if it uses review
units — the principles it holds (`ARCH_PRINCIPLES.md`).
