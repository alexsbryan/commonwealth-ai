# Ground your agent

A coding agent's first minutes on a repo are usually spent guessing —
reading files to rediscover what somebody already knew, re-deciding what a
past session decided, colliding with work already in flight. svrnmesh's
daemon keeps that state so an agent can *read* it instead: exact code
answers, durable notes, peer coordination, and a handoff frame the next
session boots from.

This page is the session protocol — what an agent should do with these
tools, in the order it should do it. Wiring your harness to the daemon is
one config block pointing at `http://localhost:9741/mcp`, covered (with
the transport constraints) in [INTEROP §4](./INTEROP.md#4-mcp--give-your-existing-agent-our-code-intelligence).
The code-intelligence tools themselves — `symbols`, `callers`, `blast`,
compiler-resolved from a SCIP graph — are covered in
[code intelligence](../sovereign/docs/CODE_INTELLIGENCE.md).

**You need:** [a running daemon](./START_THE_DAEMON.md), and for the code
tools [an indexed repository](../sovereign/docs/CODE_INTELLIGENCE.md).

## Boot informed

At session start, before any file is opened, an agent can ask:

- `briefing` — the project brief: recent activity, active notes, posture.
- `recent_changes` — which subsystems moved in the last day.
- `project_context("<the task>")` — conventions and architecture docs
  relevant to what it's about to do.
- `notes(query: "<task area>")` — decisions and invariants prior sessions
  recorded (below).
- `work_in_flight(scope, match_mode)` — whether a peer agent or a human
  is already on those files (below).
- `drift_posture` — whether the narrative docs still match the code.

Each of those is one cheap read that replaces minutes of rediscovery.
This repo's own agent instructions
([`.claude/CLAUDE.md`](../.claude/CLAUDE.md)) are the working example of
the full protocol — written for agents working *on* svrnmesh, but the
shape transfers to any repo the daemon indexes.

## Durable notes

Notes are how a session leaves something behind on purpose: a `decision`
(chose X over Y, and why), an `invariant` (this must never be violated),
a `todo`, a failed `attempt` so nobody repeats it.

```sh
svrn notes add --kind decision -m "chose FTS5 over LanceDB: zero-vector embeddings"
svrn notes list --query "FTS5"
```

Agents usually write through the MCP tools instead — `note` to write,
`notes` to search — and both surfaces are the same store: `svrn notes
list` runs the identical query the MCP `notes` tool runs, so the CLI and
your agent cannot disagree about what was recorded. On a mesh, notes
propagate to your other machines.

## Don't collide: scopes and the work atlas

On a mesh, several agents (and humans in editors) may be in one codebase.
Before non-trivial work on a shared file or symbol:

- `work_in_flight(scope, match_mode: "file" | "symbol")` — live claims
  and recent edit observations, each naming the node and session. Someone
  active on your target is a conversation to have, not a race to win.
- `declare_scope(symbols, intent)` — publish what you're doing, in a
  sentence a colleague could read. Claims expire on a TTL.
- `release_scope(claim_id)` when done.

The machinery — observation grades, privacy, gossip — is in
[the work atlas](../sovereign/docs/WORK_ATLAS.md).

## Session continuity

Context windows end; work doesn't. The `session_state` tool upserts a
**frame** — objective, current state, next steps, decisions — as the
session works, and the next session starts from that frame instead of
re-reading the repo:

```sh
svrn session frames        # index of live frames, one line each
svrn session frames <id>   # read one whole
svrn session attach <id>   # re-point this terminal at that lineage
```

Write the frame at transitions (task start, step done, blocker hit), not
just at the end — a frame written while the state is in your hands is the
one a successor can actually resume from. The contract, budgets, and
grading live in the
[session-continuity spec](../sovereign/docs/specs/SESSION_CONTINUITY.md).

## Context spend

`svrn cache-audit` reads your local agent transcripts and shows where the
token budget went, per session: raw file reads versus distilled queries,
cache-tail costs, repeated reads.

```sh
svrn cache-audit --sort ratio    # worst raw-read offenders first
svrn cache-audit --session <id>  # deep-dive one session
svrn cache-audit --ramp          # startup cost: what a session read before its first edit
```

The pattern it exists to catch: hundreds of thousands of raw-read tokens
against zero `symbols`/`notes` calls — an agent paying full price to
rediscover what the daemon already knew.

## Where your agent's state lives

All of it is files under `~/.svrnmesh/` (legacy name `~/.sovereign/`),
owned by you: `notes.db`, session frames under `sessions/<id>/frame.md`,
the drift report, the code indexes. Nothing about your sessions leaves
your machine — on a mesh, notes and frames travel only to your own
peers. It also means a fresh HOME has none of this by construction:
state accumulates because you work, which is the point.
