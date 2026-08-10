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

Two passes. Building the fact base is a single tree-sitter read of your code — fast,
and cheap to redo when the code changes. The facts live in a small per-file-keyed
SQLite store (`facts.db`), so a save doesn't rebuild the whole base: only the edited
file's facts are re-extracted and swapped in, and when the code watcher is running
they refresh live as you edit. Checking a spec is then a lookup per claim: the
deterministic checks are indexed reads, and the ones that need a call-graph trace are
near-instant against a graph loaded once into memory. The heaviest part is a small
model tagging each claim, and it runs on your own machine. There's no giant index to
keep warm.

## How to run it

Three steps, all local:

```bash
sovereign code facts /path/to/your/repo --corpus-id myrepo
sovereign enrich spec-intel design.md --corpus myrepo
sovereign code check-spec --corpus myrepo --claims ~/.svrnmesh/specs/myrepo/design/claims.json
```

The first builds the fact base from your code; the second turns your spec into a list
of claims; the third checks each claim and prints a verdict with its receipt. Your
code and your spec never leave your machine.

## Two kinds of answer, honestly labeled

Under the hood there are two layers, and the report tells you which one spoke. The
**deterministic** layer answers the claims it can pin to a fact — a config flag set a
certain way, a specific string present, a function that exists — and those verdicts
come with an exact file and line you can open in seconds. It is built to be *safe*:
when it isn't sure, it says nothing rather than guess. The **fuzzy** layer handles the
behavioral and conceptual claims the first layer can't pin down; add
`--fuzzy <spec_findings.json>` (from `enrich spec-reconcile`) and its verdicts fill the
gaps, clearly marked as the softer, review-me answers. Trust the deterministic ones;
scrutinize the fuzzy ones.

## Languages, and what's still uneven

The deterministic layer reads **Rust and Python** today. Adding a language really is
just adding a per-language pack, not new machinery: fact extraction is driven by a
small table (`lang_packs` in `corpus_engine::facts`) where each entry is a grammar
plus a few tree-sitter queries, and the call-graph side already ingests any standard
SCIP indexer (`rust-analyzer`, `scip-python`, `scip-typescript`, `scip-go`, …). So a
new language is a well-scoped afternoon, not a rewrite — the one judgment call is how
that language spells "a typed value built with named fields," the data-flow fact
behind config claims (Rust struct literals vs. Python constructor keywords, say).

Fidelity isn't identical across languages, and the report won't pretend otherwise.
The deterministic call-graph checks are only as precise as the underlying indexer:
`rust-analyzer` resolves calls the way the compiler does, while a dynamically-typed
language leaves more edges unknowable, so proportionally more of the work shifts to
the fuzzy layer. Which claims get a hard, cited answer also depends on how cleanly
each one names a checkable fact; the rest fall to the fuzzy layer or to you. And it
will never tell you whether a divergence is a bug or an improvement — that call is
yours, and the whole point is to make it cheap to make.

---

A reasonable spec and an honest look at the code, and you can finally see the
distance between them — every hard divergence located and cited, every soft one
flagged for you to weigh.

*Underneath: the deterministic fact base (`sovereign code facts` →
`corpus_engine::facts` / `facts_check`) plus the fuzzy capability-map and per-function
summaries. Mechanics and the other verbs (`enrich code-intel`, `code capability-map`,
`enrich spec-reconcile`) live in
[CODE_INTELLIGENCE.md](../sovereign/docs/CODE_INTELLIGENCE.md); the build plan is in
[internal/FACT_BASE_SCALE_OUT.md](internal/FACT_BASE_SCALE_OUT.md).*
