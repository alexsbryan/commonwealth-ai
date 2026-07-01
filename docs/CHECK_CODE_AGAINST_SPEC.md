# Check your code against its spec

Every project starts with intent — a spec, a design doc, a handful of "it must do
X" promises. Then the code gets written, and then it *lives*: commits pile up,
behavior shifts, and slowly the thing you built stops matching the thing you wrote
down. Nobody decides to let them drift apart; it just happens. And finding *where*
they've drifted — by hand, function by function — is the audit everyone knows they
should do and almost nobody does, because it's slow and easy to get wrong.

Sovereign does that audit for you. Point it at a codebase and a spec, and it reads
what the code *actually does* now, lines it up against what the spec *says* it
should do, and hands you a report where every disagreement is pinned to an exact
file and line. Then it gets out of the way. It will not tell you whether a
disagreement is a bug or a deliberate improvement — that call is yours, and it
should be. Its job is narrower and more useful: make every divergence cheap to see
and quick to confirm, so you can go through them one at a time and decide — *fix
the code, or update the spec.*

## Two structured models, kept apart

The reason this works where a plain text search wouldn't is that it doesn't treat
either side as text. It turns your code and your spec each into a *structured
model* — a map of the things that exist and how they relate, rather than a wall of
prose you keyword-search. The technical name for such a model is an **ontology**,
and the whole approach rests on building one for each side.

The code's model is the things in your system and how they connect: capabilities,
the functions inside them, and the real calls between them — read the way the
compiler sees them, not guessed from names. The spec's model is the claims it
makes: each requirement, the smaller conditions it rests on, and how binding it is
— a "must" versus a "should." The two are shaped nothing alike. Code is built
bottom-up, where capabilities emerge from how functions actually wire together; a
spec is written top-down, from intentions. One requirement can touch a dozen
capabilities, and one capability can answer to a dozen requirements.

The tempting move is to mash both into a single shared format so they line up
neatly — and that's the mistake. Flattening them throws away the very detail that
makes a finding worth acting on; you'd end up comparing summaries of summaries. So
Sovereign keeps each model in its own native shape and instead draws *links between
them*: for every claim in the spec and every capability in the code, does the code
satisfy the claim, contradict it, or say nothing about it? The result is a labeled
set of connections between two structures — and the places that don't connect are
the findings.

Keeping both sides structured is also what lets every result carry a receipt.
Because a claim is a real object with named conditions and a capability is a real
object with real functions at real lines, a verdict can say *this condition,*
satisfied by *that function,* at *that line* — not "these two seem related." The
structure is what turns a vague resemblance into checkable evidence.

## Why you can trust the hard findings

That evidence bar — every finding has to point at the specific code that backs it
up, and a topical near-match that can't cite a real line never gets reported as
"done" — matters most for the one that bites. The valuable, dangerous finding is
**drift**: the spec says X, the code does Y. It's the most important thing to
surface and the most damaging to get wrong. One false accusation, you click
through, see it's nonsense, and stop trusting the whole report. So Sovereign errs
hard toward being sure — five real drifts are worth more than fifty with three
phantoms — and every drift clicks straight through to the line you can check in
seconds. Evidence, or it doesn't ship.

## What you get back

Five kinds of finding, each with a plain decision that stays yours:

- **Agreed** — the spec and the code say the same thing. Nothing to do; that's your
  confidence.
- **Drift** — the spec says one thing, the code does another. Fix the code, or
  update the spec.
- **Gap** — the spec asks for something the code doesn't do yet. Build it, or drop
  the requirement.
- **Unfinished** — started, but part of the requirement isn't there. Finish it, or
  narrow the spec.
- **Undocumented** — the code does something the spec never mentions. Write it down,
  or mark it internal.

What makes these actionable instead of vague is that it checks requirements *in
pieces*. Instead of "requirement 7 is 80% there" — useless — it tells you "the
first half of requirement 7 is handled at this exact line; the second half has no
code behind it at all." That's a sentence you act on in ten seconds.

## You don't need a spec to start

The same machinery answers a simpler question on its own: *what does this code
actually do?* Run it against a codebase with no spec at all and you get a clear,
cited map of the system — every capability described in plain English, organized by
what the code is really for, each claim backed by a line you can open. It's the
fastest honest way to understand a codebase you didn't write, or to see your own
with fresh eyes. The spec check is just that same map, held up against your intent.

## What it costs

The first pass over a large codebase is the slow part — it has to read everything
once. After that it's nearly free. Change a few functions and commit, and it
re-reads only what you touched, re-checks only the parts of the spec that could be
affected, and updates in seconds. The version that never goes stale — that
re-checks on every commit — is actually *cheaper* to keep running than that
one-time first scan. The hard part is over once you've done it.

## How to run it

One command:

```bash
sovereign code map /path/to/your/repo --spec design.md
```

It handles the rest — reading the code, mapping it, comparing it to the spec — and
prints the report with its receipts. Drop the `--spec` and you get the plain-English
map of what the code does, on its own. It runs entirely on your own machine; your
code and your spec never leave it.

---

A reasonable spec and an honest look at the code, and you can finally see the
distance between them — every divergence located, every claim something you can
check yourself.

*The capability map and per-function summaries underneath this are the mature core;
the spec comparison is the newest layer built on them. The mechanics, the other
verbs (`enrich code-intel`, `code capability-map`, `enrich capability-doc`), and the
tuning live in [CODE_INTELLIGENCE.md](../sovereign/docs/CODE_INTELLIGENCE.md).*
