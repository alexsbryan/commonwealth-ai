# Making your tests pass with the Commonwealth solver (for pi users)

You use [pi](https://github.com/earendil-works/pi-coding-agent) as your
coding agent, pointed at a local Commonwealth model. This guide shows
you how to hand the grindy part of coding — *make these failing tests
pass* — to the system's dedicated solver instead of asking pi to
iterate by hand.

## What the solver is (30 seconds)

The TDD Machine is a search loop that runs **on your machine, against
your local model**. You give it a folder and a test command. It
generates several candidate edits in parallel at different
temperatures, applies each to a scratch copy, runs your tests, and
keeps only candidates that make strictly more tests pass. It repeats
until everything is green or it honestly gives up. You get back a
report of what it tried and a working tree when it succeeds.

It is better than chat-driven iteration at exactly one thing: goals
that tests can measure. Fixing failing tests, fixing a bug you can
reproduce with a test, splitting an oversized module (it generates a
size-budget test ladder for you), or writing one failing test that
pins a behavior down.

## One-time setup

You already have the daemon if pi works. You need two more things:

```bash
# 1. Point pi at your local model (skip if you've done this).
bash scripts/setup-pi-provider.sh

# 2. Start the solver server (hosts POST /v1/solve on 127.0.0.1:8080).
sovereign serve
```

Requirements the solver enforces:

- **Your folder must be a git repository.** The loop uses git to
  snapshot and roll back. It refuses `/`, your home directory, and
  other system paths outright.
- **Commit or stash your work first**, or pass `"force": true` to
  acknowledge that the solver will be editing a dirty tree.

## Recipe 1 — make my failing tests pass

From any terminal (or from *inside* pi — ask it to run this with its
bash tool):

```bash
curl -s http://127.0.0.1:8080/v1/solve \
  -H 'Content-Type: application/json' \
  -d '{
    "workdir": "/path/to/your/repo",
    "model": "commonwealth/primary",
    "prompt": "Make the failing tests pass. The bug is somewhere in the parser.",
    "test_command": "python3 -m pytest -q",
    "polarity": { "kind": "maximize_passing" }
  }'
```

What you get back (abridged):

```json
{
  "status": "reached",
  "tests_before": { "passed": 3, "failed": 5 },
  "tests_after":  { "passed": 8, "failed": 0 },
  "rounds": 2,
  "trajectory": [
    { "round": 0, "winner": "rewrite parse_line@T0.4", "passing_after": 6 },
    { "round": 1, "winner": "txn[rewrite tokenize; write_file→helpers.py]@T0.2", "passing_after": 8 }
  ]
}
```

`status` values you'll see: `reached` (all green), `improved` (made
progress, ran out of rounds — call it again), `stalled` (honestly
stuck — the report shows what it tried), `no_baseline` (your
test_command found zero tests — the solver needs at least one failing
test as its compass).

Your working tree now contains the winning edits. `git diff` shows
you exactly what changed; commit it or throw it away.

## Recipe 2 — pin a behavior with a failing test first

Same call, different polarity. The solver writes ONE failing test
that captures the behavior you describe, without touching your source:

```json
"prompt": "Write a failing test proving that empty carts still get charged shipping.",
"polarity": { "kind": "generate_one_failing", "test_name_hint": "test_empty_cart_shipping" }
```

Then run Recipe 1 to drive it green. That's red-green as two calls.

## Recipe 3 — split an oversized file

The solver has a task wrapper that turns "every file ≤ N lines" into
a ladder of generated tests (at descending thresholds, so every
extraction step counts as progress), then drives them green. Today it
is exposed through the MCP tool surface (`tdd_solve` /
`tdd_bdd_cycle` on the server's `/mcp` route) and the Rust API
(`commonwealth_tdd::tasks::split_file`); an HTTP convenience wrapper
is planned. From an MCP-capable client, call `tdd_solve` with your
split goal in the prompt — the same engine runs.

## Using it from inside pi

Pi doesn't know about the solver, but pi has a bash tool. A workable
pattern is to keep a snippet in your project notes and tell pi:

> Run the solver on this repo: POST the JSON in `solve.json` to
> http://127.0.0.1:8080/v1/solve and summarize the trajectory for me.

Pi shells out, the solver grinds, pi reads you the result. You stay
in one conversation.

## What the solver does that a chat loop doesn't

- Samples **several candidates in parallel** per round instead of one
  attempt at a time — variance is the fuel, your tests are the judge.
- **Applies multi-file transactions**: coordinated edits across files
  land together or not at all.
- **Repairs its own mistakes**: syntax-rejected edits get one pointed
  fix-it turn; truncated model responses are detected by content and
  completed; a wedged model gets retried.
- **Refuses to regress**: an edit only lands if a previously-failing
  test flips. Your passing tests are a ratchet.

## Troubleshooting

| Symptom | Meaning |
|---|---|
| `workdir gate: not a git repo` | `git init && git add -A && git commit` first. |
| `workdir gate: uncommitted changes` | Commit/stash, or add `"force": true`. |
| `no_baseline` | Your `test_command` collected zero tests. Point it at a real suite; the solver steers by failing tests. |
| Long silence, then a big JSON | Normal — the call is synchronous and a hard problem takes minutes. Watch `git status` in the workdir if you're curious. |
| `stalled` with all candidates erring | Read `trajectory[].candidates` — the labels carry the failure class (`err:parse`, `err:apply`, …). |
