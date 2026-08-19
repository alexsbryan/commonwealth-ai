# Comaintainer channels — how a worker's concern reaches the operator

A worker that notices something has two ways to say so, and they are
deliberately different. Getting the difference right is what keeps the
operator's queue small without losing findings.

**The load-bearing asymmetry: banking is a SCRIPT, escalation is a
MESSAGE.** That single fact decides everything about portability.
Banking works in any harness, from a cron job, with no session alive.
Escalation is harness-level by design — it must work when the daemon and
every MCP tool are down — which means it exists only inside a live
session of a harness that has a message channel.

## The two channels, as they stand

Both are verbatim clauses the seat copies into every spawn prompt
(`.claude/skills/comaintainer/SKILL.md:192-217`).

| Concern | Channel | Lands in | Read by |
|---|---|---|---|
| Outside this order's Scope — a smell, a flaky test, a doc gap, a nearby refactor | **Banking**: `scripts/co-backlog-producer.sh --key <what went wrong>` | the heap (backlog notes), **unvetted and unpullable** | `co-backlog.py --open`; triaged at close-out |
| Blocked by something only the seat may do — daemon restart, config change, model swap, a seam to renegotiate | **Escalation**: a message to the seat session, then STOP and wait | the seat's judgement, then a directive pair | the seat; the operator, once it is a directive |

Banking is *deferred, not suppressed*, and the worker does **not** narrate
banked items in its report. `--key` names the essence — a lane name, a
check name, an invariant id — never a run id or timestamp (ARCH §7.5), so
a gate failing nightly leaves one item that keeps getting fresher rather
than thirty. Everything a producer files lands unvetted, so a noisy
worker costs the operator a scroll, never a wrong pull.

Escalation is an interrupt. `SKILL.md:222-225`: act directly if
seat-owned, **draft a steer if operator-owned**, and log every escalation
and its resolution as a directive pair.

## What the console closed, and what it did not

The console (`scripts/co-console.py`) is the approve step of the
escalation loop. A worker escalates, the seat drafts a steer
(`co-directive-log.sh --pending`), and that draft is what the *Waiting on
you* pane renders — approve, edit or reject it with one key. Before the
console, that approval was a command line.

`R5` is the banking channel's model-assisted twin, not a replacement. The
producer takes text from an automated signal; R5 takes an unstructured
prose finding — the kind that arrives inside a session report — and
returns an item the backlog ruler vets, keeping the worker's citation.
Producer items land unvetted; R5 items land vetted.

**What it did not close: the console cannot reach a worker.** It closes
the operator↔record loop, not the operator↔worker loop.

## The three seams, named

### 1. Approving a steer does not deliver it

**What breaks.** The operator approves a steer on the page. The parked
worker never hears it, because delivering a message is a harness
capability and the console is a Python process driving scripts. If no
seat session is running when a worker escalates, the escalation reaches a
session that is not there and the console shows nothing at all — no
directive row was ever written.

**What closes it.** A *seat inbox*: on boot, a seat session drains
resolved directives of `kind=steer` it has not yet delivered and sends
them. This keeps the message capability inside the harness, where it has
to be, and keeps the console a driver of records — the property that has
held through every other decision here.

**Cost.** A design conversation before code: it changes what a seat
session does at boot, and it needs a delivered/undelivered mark, which is
the first piece of state in this system that is neither a directive nor a
verdict. Get that shape wrong and it rots (see the closure-loop rule).

### 2. Banked items are invisible in the console

**What breaks.** *Standing* renders orders and verdicts. The heap is
still only `co-backlog.py --open`. So the channel workers use most often
is the one pane the console lacks, and an operator working from the
console alone will not see what the pool banked.

**What closes it.** A fourth pane reading `co-backlog.py`'s own store
reader by import — the same reuse `co-console.py` already does with
`co-closeout.py`'s `join_directives`. Do **not** reimplement the ruler or
the ranking; a second scorer would disagree with the heap page within a
week.

**Cost.** About an hour. This is the cheapest of the three and the one
with the clearest payoff.

### 3. `--pull` → R1 is unwired

**What breaks.** Nothing — it is a missing shortcut, not a defect.
`co-backlog.py --pull` already prints the top vetted item as an order
draft, which is exactly R1-shaped input. Today the operator copies it
across by hand.

**What closes it.** A route in R0's card, or a button on the heap pane:
pull the top item, run R1 on it, approve the resulting order. It turns
"the pool banked twelve things" into "here is the order for the top one".

**Cost.** Small, but it should follow seam 2 — there is nowhere to put
the button until the heap is on the page.

## Harness independence

Per `AGENTS.md`, assume the *capability*, not the tool that provides it.

| Capability | Claude Code | pi | Fallback that always works |
|---|---|---|---|
| Banking a finding | `co-backlog-producer.sh` | same script | same script — it is a script, so this row never varies |
| Worker pool | Agent tool | `pi-subagents` | one session, no delegation |
| Escalation (worker → seat) | `SendMessage` to `main` | not specified here — check the harness's own channel before assuming | park protocol + TTL |
| Approving a steer | the console, or `co-directive-log.sh --resolve` | same | same |
| **Delivering** an approved steer | the seat session, by hand | unspecified | **none — this is seam 1** |

The bottom row is the whole portability story. Every other capability is
either a script or already has a named fallback. Delivery is the one that
is harness-bound *and* has no fallback, which is why the fix is an inbox
the harness drains rather than a channel the console owns.

## Picking this up

Seam 2 first — it is an hour, it is pure reuse, and it makes the console
show the channel the pool actually uses. Seam 3 follows it naturally.
Seam 1 is the one that needs a decision before it needs code.

If you would rather these live in the heap than in a document, file them
with the system they describe:

```
scripts/co-role.py R5 --input "<the seam, with its citation>"
```

That is the honest test of whether this doc was necessary: a follow-up
that belongs in the backlog should be in the backlog.
