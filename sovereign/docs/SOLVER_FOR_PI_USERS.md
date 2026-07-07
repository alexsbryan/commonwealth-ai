# Solve: give the daemon a coding goal, get a green tree back

You use [pi](https://github.com/earendil-works/pi-coding-agent) as your
coding agent, pointed at a local Commonwealth model. That means the
daemon is already running — and the daemon is the solver. There is
nothing to install or start.

## The call

```bash
sovereign solve /path/to/your/repo "make the parser handle empty input" --watch
```

Two things: a folder (a git repo) and a goal in plain language. The
daemon makes the goal test-shaped — if you have failing tests it
drives them green; if you don't, it first writes the one failing test
that pins your goal down, then makes that pass. `--watch` shows each
round as it lands:

```
job 3f9c…
  detected: pytest · `pytest -q` · commonwealth/primary

[fix] round 0 — rewrite parse_line@T0.4 won · 6 passing / 2 failing
[fix] round 1 — patch tokenize@T0.2 won · 8 passing / 0 failing

✓ reached — 8 passing / 0 failing after 2 round(s)
  review: git -C /path/to/your/repo diff
```

Your working tree now holds the winning edits. `git diff` shows
exactly what changed; commit it or `git checkout .` to throw it away.

Without `--watch`, the job runs in the background and the command
prints how to check on it (`sovereign solve --status <job_id>`) or
stop it (`sovereign solve --cancel <job_id>`).

## When to reach for it

Almost every coding goal can be made test-shaped, so this is the
standard way to execute one — fixing a bug, adding a function,
changing a behavior, shrinking an oversized file. Hand-editing is the
fallback for the rare goal that resists a test.

Under the hood, each round samples several candidate edits in
parallel at different temperatures, applies each to a scratch copy,
runs your tests, and keeps a candidate only if it makes strictly more
tests pass. Multi-file edits land together or not at all; your
passing tests are a ratchet the solver cannot regress.

## How it ends

| Result | Meaning |
|---|---|
| `reached` | Everything green. Review with `git diff`. |
| `improved` | More tests pass than before, but not all. Progress is in the tree — call solve again to continue. |
| `stalled` | Honestly stuck. The report shows every round and what each candidate tried. |
| `no_baseline` | No tests found and the goal-pinning test couldn't be written — the one true failure. |

## Rules the solver enforces

- **The folder must be a git repo.** It refuses `/`, your home
  directory, and other system paths outright.
- **Commit or stash first**, or pass `--force` to acknowledge it will
  edit a dirty tree.
- It never edits anything outside the folder you gave it.

## Steering it (all optional)

```bash
sovereign solve <repo> "goal" --verb fix            # only drive existing failing tests green
sovereign solve <repo> "goal" --verb pin            # only write the failing test; don't fix it
sovereign solve <repo> "goal" --verb split --max-lines 300   # shrink oversized files
sovereign solve <repo> "goal" --test-command "pytest -q tests/"
sovereign solve <repo> "goal" --model commonwealth/fast
```

## From an agent (pi, Claude, anything MCP-capable)

The daemon's MCP surface (`http://127.0.0.1:9741/mcp`) exposes the
same engine as three tools: `solve` (submit — returns a `job_id`
immediately), `solve_status` (rounds so far + result), and
`solve_cancel`. Ask your agent to solve a goal in a repo and it has
everything it needs.

Raw HTTP, if you'd rather curl:

```bash
curl -s http://127.0.0.1:9741/v1/solve/jobs \
  -H 'Content-Type: application/json' \
  -d '{"workdir": "/path/to/your/repo", "goal": "add an is_palindrome function to utils.py"}'
# → 202 {"job_id": "...", "detected": {...}}

curl -s http://127.0.0.1:9741/v1/solve/jobs/<job_id>            # state + rounds + result
curl -sN http://127.0.0.1:9741/v1/solve/jobs/<job_id>/events    # live SSE rounds
curl -sX DELETE http://127.0.0.1:9741/v1/solve/jobs/<job_id>    # cancel
```

## Troubleshooting

| Symptom | Meaning |
|---|---|
| `refused (422): uncommitted changes` | Commit/stash, or add `--force`. |
| `refused (409): workdir_busy` | A solve job is already running in that folder — one at a time per repo. |
| `refused (429): at_capacity` | Two jobs are already running; retry when one finishes. |
| `no_baseline` with `--verb fix` | Your test command collected zero tests — the fix path steers by failing tests. Drop the verb and solve will pin the goal with a new test instead. |
| `stalled` with all candidates erring | Read the rounds in `--status` — the candidate labels carry the failure class (`err:parse`, `err:apply`, …). |
| `error: daemon call failed` | `sovereign daemon status`, then `sovereign daemon start`. |
