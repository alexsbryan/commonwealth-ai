# canon

A CLI that holds a body of commitments, records what was decided about
them, and answers whether a proposal sits with or against them.

Works for one person, one codebase, or one household. Standalone, open
source, no daemon required. A canon is an authoritative body of norms;
canon law is common law's direct ancestor.

---

## The model

**Two nouns.**

- **Commitment** — a normative statement. *"Mornings are protected."* ·
  *"One decider, one name."* · *"Quiet hours start at 11."*
- **Act** — assert, supersede, retract, accept, dismiss, revert.

**One fold.** Current state is a pure function of the acts — no IO, no
inference, no stored mutable state. What is live, what replaced what and
why, and which contradictions you are knowingly carrying all come from
replaying one file.

**One file.**

```
.canon/acts.jsonl
```

Append-only. Commitments live inside `assert` acts. Ids are content
hashes of `(prefix, timestamp, actor, body)`. Nothing else is state.

It diffs, so git gives it history for free. It greps. You own it — exit
is deleting a directory. And it is the same format the larger
Commonwealth tools read, so growing out of `canon` means pointing a
bigger tool at the same file, not migrating.

## The verbs

```
canon init [--profile personal|code|house]
canon draft --from <paths>       # cold start from loose notes
canon add "<text>"               # assert
canon list                       # what is live
canon why <id>                   # what this replaced, when, and why
canon supersede <id> "<text>" -m "<reason>"
canon retract <id> -m "<reason>"
canon check "<proposal>"         # the adjudication
canon tensions                   # where your commitments conflict
canon accept <a> <b> -m "<reason>"   # carry it knowingly; reason required
canon dismiss <a> <b>            # not actually a conflict
canon undo <act-id>              # revert; itself revertible
canon log                        # raw acts
canon share                      # pasteable snapshot
canon adopt <url>[@gen] | --paste
canon diff --upstream            # how you have diverged from your seed
canon upgrade <gen> | canon rebase --onto <url>@<gen>
canon mcp                        # agent surface
```

**Only `check`, `tensions` and `draft` need a model.** Everything else is
the fold. `canon` is useful on a plane.

```
canon config set endpoint http://localhost:8080/v1    # any llama.cpp
canon config set endpoint http://localhost:9741/v1    # a sovereign daemon
```

Exit codes: `0` supported · `1` conflicts · `2` unaddressed · `3` cannot
judge. `--json` puts data on stdout, logs on stderr.

## Three profiles, one engine

Identical primitives; `check` renders differently, and the difference is
not cosmetic.

**`code`** — verdict-shaped, because a codebase wants one.

```
$ canon check "add a GovernanceStore for the adjudication cache"
CONFLICT
  c-4f19  "Survey what exists and prove it cannot serve before building."
          asserted 2026-06-02, never superseded
exit 1
```

**`house`** — alignment against demand. Conflicting-and-wanted means
amend a rule; unaddressed-and-wanted means write one. The output is an
agenda, not a ruling.

**`personal`** — **never a verdict.**

```
$ canon check "take the on-call rotation next quarter"
  STAKE
    c-a81  "Mornings are protected."              ← pulls against
    c-3d2  "Be someone the team can rely on."     ← pulls toward
    accepted 2026-04-11: "reliability is how I earn the autonomy, for now"
```

### Why the personal profile fits IFS better than it should

The most distinctive thing here is that a tolerated contradiction is a
first-class state: `accept` requires a reason, and nothing is ever
force-resolved.

That is also the clinical stance of Internal Family Systems. Parts hold
contradictory commitments — *I want to be seen* against *I must not be
exposed* — and the work is never to eliminate a part but to understand
what it protects. Both are modelling systems where forcing consensus
destroys the information you came for.

So `accept -m` is the most valuable verb in the personal profile and
nearly the least valuable in `code`. And the hard constraint: this is a
structured journal, not a clinician. It does not diagnose or advise, it
says so in `--help`, and `check` reports stakes rather than rulings. A
tool that renders verdicts on someone's inner life would do harm the
codebase profile cannot.

## `canon draft` — the cold start

Nobody has written down how they like to be treated. No team has an
`ARCHITECTURE.md` matching what it enforces. No house has a charter until
its second bad argument. Blank pages lose.

But the normative content already exists, unextracted, in text everyone
already has.

```
canon draft --from ~/notes/**/*.md
canon draft --from-git --since 1y        # commit bodies + review threads
canon draft --from ./house-chat.txt
```

```
Candidate 3 of 11
  "Mornings are protected; I do not schedule before 11."

  from journal/2026-03-14.md:
    "...third week running where the 8am standup ate the only stretch
     I actually think well in. I keep saying I'll protect mornings."

  [a]ccept  [e]dit  [r]eject  [s]kip
```

**Every candidate carries its source passage or is not shown.** A drafted
commitment with no citation is the model inventing a value you never
held — cite-or-abstain applied to onboarding, and non-negotiable in the
personal profile.

**One at a time, and there is no `--accept-all`.** Each acceptance writes
an `assert` attributed to you, so onboarding *is* the first governance
session. A canon adopted wholesale is disengagement at t=0.

**Precision over recall.** A journal is mostly not normative. Eleven good
candidates beat forty mediocre ones; the failure mode is losing the user
in three minutes.

### The moment it has to produce

`draft` ends by running `tensions` over what was just accepted. Loose
accumulated text is nearly always self-contradictory, and nobody has seen
their own contradictions side by side.

```
  You already disagree with yourself:
  c-a81  "Mornings are protected."                 journal/2026-03-14
  c-3d2  "Be the person who shows up, always."     journal/2026-06-02 +4
  Carry it knowingly?  canon accept c-a81 c-3d2 -m "<what this protects>"
```

Same move in a codebase (*PR #440 established never do X; #612 does X and
nobody flagged it*) and in a house. **The first run must produce a
genuine "huh"** — something true the user did not know. A tidy list of
things they already knew gets deleted and there is no second session.

### How it works without a daemon, and where it stops

"Point it at an OpenAI endpoint" is not a sufficient claim. `draft` is
chunking, extraction, dedup, citation, and tension detection; a bare
`/v1/chat/completions` is text in, text out.

It works by staying in the regime where map-reduce over plain completions
suffices:

```
chunk                                   no model
map: extract per chunk, keep chunk id   N completions
reduce: dedupe                          1 completion, or /v1/embeddings
tensions over accepted commitments      1 completion, all in context
```

Two properties carry it, both consequences of smallness. **Provenance is
free in the map step** — a candidate was extracted *from* a chunk, so
citing it is bookkeeping, not retrieval; nothing is synthesised across
chunks, so the hard grounding problem never arises. And **tensions is one
call** — thirty commitments is under two thousand tokens.

Where it stops:

- **One corpus, not many.** Tensions *across* corpora is the atlas.
  Standalone cannot — not slower, cannot.
- **~100 commitments**, past which all-pairs-in-context degrades and you
  need retrieval plus a candidate enumerator.
- **Large source corpora.** Two hundred chunks is minutes; twenty
  thousand is a checkpointed pipeline.
- **Precision.** The full pipeline pre-filters candidate pairs on
  structural signals and carries tuned extraction guidance with
  hand-written carve-outs for recurring false positives. Standalone ships
  the guidance without the pre-filter, so expect more spurious tensions.

Standalone degrades on precision; it does not fail. That is the claim,
narrow on purpose.

### The bar that makes the claim honest

Maple House has an exhaustive `truth.json` — eleven planted tensions
across four types, seven labeled compatible pairs, with splits.

Run standalone `draft` against it, score with the same governance bench
lane the full pipeline uses, publish both numbers in the README. If
standalone reaches 0.72 precision where the pipeline reaches 0.91, that
is a fact users can act on. If it collapses on the decoys, `draft` ships
daemon-only or does not ship.

**No number goes in the README that was not produced this way.**

### What the daemon adds, specifically

Four capabilities, each matching a limit above: retrieval over corpora
too large to map exhaustively; deterministic candidate enumeration past a
hundred commitments; cross-corpus comparison; and a cite-or-abstain gate
that refuses an uncited claim rather than producing a plausible one.

**For personal corpora, local is not a preference.** `draft` defaults to
a local endpoint, refuses a remote one without an explicit flag, and
names which it used. *Your journal never left your machine* is worth more
than any quality delta a hosted model buys.

## The agent surface

The integration that matters. Everyone running agents has the same
problem: **the agent does not know the house rules.** The prevailing fix
is pasting them into the prompt, which saturates — this repository
injects ~14k tokens of standards into every session and its own text
records that the compass still gets lost.

```
canon mcp          # stdio MCP server
```

| Tool | Cost | Use |
|---|---|---|
| `canon_list` | free | orient at task start |
| `canon_why(id)` | free | explain a rule and its history |
| `canon_open` | free | what the canon does not cover |
| `canon_check(p)` | one call | adjudicate a consequential choice |

**The surface is read-only, and that is structural.** There is no MCP
tool that writes an act — not permission-gated, absent. Amending requires
the CLI, run by a person. An agent that thinks something should be
recorded says so in chat, as a command you can run. The canon is what
your agents are measured against; an agent that can edit it is grading
its own work.

**For a small canon, `canon_list` is the whole integration.** Twenty
commitments fit in context; the agent reads them once and reasons
directly. `check` earns its keep in two cases: **consistency** (agents
reasoning independently over a list vary; `check` gives the same answer
every time, and when the point is uniform application across a team, that
determinism *is* the product) and **scale**.

The gap between them is measurable and worth measuring — run N agents
over `list`, compare against `check`, and the spread tells you which
commitments are ambiguously worded.

Agent reasoning becomes **citable instead of plausible**, which finally
distinguishes *the agent misread the rule* from *the rule is wrong* — a
correction from an amendment.

### Nested canons

```
./.canon     # this codebase
~/.canon     # you
```

Both apply, neither silently wins, and conflict between them is one of
the most useful things the tool surfaces:

```
  CONFLICT ACROSS CANONS
    project  c-11a  "Ship the smallest thing that closes the issue."
    personal c-a81  "Mornings are protected."
    This task as scoped needs a 9am sync to land today.
```

Nothing resolves that and nothing should try. Naming it is the
contribution — the agent stops quietly optimising one canon at the
other's expense, which is what it does today, invisibly.

## Lineages

Lineages, generations, forking, rebasing, upstreaming: this is version
control. **A lineage is a git repository** holding `acts.jsonl` and a
`CARD.md`; generations are tags. Hosting, ancestry, review and
distribution come free.

Three things need building.

**1. A merge driver.** Two people append on two machines; git sees a
textual conflict where semantically there is none. The driver unions,
**dedupes by op id, and sorts by time** — exact rather than heuristic,
because ids are content hashes. Same act on both sides collapses;
different acts both survive.

**2. Ancestry in the log, not only in git.** A canon travels without its
repository. `adopt` is an *act* naming lineage, generation and source,
and inherited commitments carry `from:` the seed's op id. So `diff
--upstream` works on a file that arrived with no `.git` at all.

**3. `canon diff --upstream`** — the strategically important command.

```
  adopted  house-12-consensus@v3  (11 months ago)
  SUPERSEDED (2) · ADDED (4) · ACCEPTED (1) · UNTOUCHED (31)
```

**No model. Pure computation over two logs.** Aggregated across many
canons it is convergent divergence — *forty houses independently
superseded Art 9 in the same direction, so Art 9 is wrong* — which is the
evidence a lineage needs to earn its next generation, and it is
arithmetic rather than judgement.

**Upstream the shape, not the rationale.** `diff --upstream --propose`
emits what changed and in which direction. Rationales name incidents and
people; sending one is a separate, deliberate act.

**Rebase** is three-way — the seed you adopted, your law, the target.
Mapping supersessions onto a different base is semantic, so it costs one
call for a canon that fits in context. It emits a proposal with conflicts
marked, reports **how much of your law survives before you commit**, and
re-attributes every carried act to the person running it. Nothing
auto-resolves.

### Who sees git, and what it is for

| | Sees git | |
|---|---|---|
| Engineering team | **yes, a feature** | the canon commits with the code, so governance shows up in PR diffs |
| A house | **no** | `adopt` clones, `upgrade` fetches; most use paste and touch no repo |
| An individual | **no** | a file in a home directory |
| Lineage maintainer | **yes** | they run a repository and review PRs — a role held by few |

**Git is never load-bearing for correctness.** Delete `.git` and every
answer is identical; `list`, `why`, `tensions`, `check` and `diff
--upstream` all fold a file. Only `adopt`, `upgrade` and `rebase` *from a
URL* need the network, and each has a paste equivalent.

The oplog is the semantic truth — acts, reasons, attribution, the fold.
Git is transport. The mild redundancy is useful: git records when an act
*arrived on this machine*, the oplog when it *happened*, and offline
appends make those differ. The merge driver is the seam, and
content-addressed ids are what make the seam correct.

The larger system already works this way — the daemon reads
`governance_oplog.jsonl` from a directory, no git anywhere. `canon`
inherits the layering rather than inventing one.

## Sharing, and the registry that does not exist yet

**For most communities, paste-sharing is not a phase before something
better — it is how they will always share.** A house will never `git
clone` a lineage.

```
$ canon share
--- canon house-5-consensus · snapshot 2026-08-21 · 7f3a91
Quiet hours run 11pm-7am; headphones in common areas.        (c-4f19)
A guest may stay 2 nights in any 7.                          (c-8a02)
...
--- 12 live · adopt: canon adopt --paste
```

Readable by a person scrolling a thread, parseable back by the tool. No
attachment, no link, no auth, nothing to rot.

**A snapshot is not a log.** `share` exports derived current state and
drops supersession history, rationales, and the reasoning behind
tolerated contradictions — the parts naming incidents and people. Enough
to **adopt**, not enough to **audit**, which is the right trade for a
chat thread. Adopting records it honestly: *adopted from snapshot 7f3a91,
shared by @dana*.

### The thread is the registry

A Slack channel or Signal thread already has discovery, attribution,
discussion and recency. What it lacks is convention:

1. **Name consistently** — chat search is the index, so names are the schema.
2. **Pin what works** — a pinned message is a curated list.
3. **Say what you changed** — *adopted @dana's house-5, rewrote quiet
   hours for night shifts.*
4. **Post the card, not just the rules.**

Convention 3 carries the weight. **Those messages are convergent
divergence in prose** — when six people say they rewrote the same
article, the signal a registry would compute is already in the thread, in
English, timestamped. The ad hoc phase is a lossy human version of the
same loop, harvestable later.

### If a registry earns existing

A git repo containing a list of pointers. Entries by pull request; no
server, no accounts, no database — and **the index is forkable**, which
is what stops it becoming a gate.

Two rules, both against normal registry design:

**It must not rank.** No stars, downloads, or popularity sort. Ranking
manufactures the monoculture the lineage model exists to prevent: first
to trend becomes the default, variation collapses, discovery stops. List
alphabetically or shuffle.

**Require disclosure, not quality.** No editorial bar — that is
gatekeeping and someone must do the gating. The only requirement is a
complete card: assumptions, encoded politics in plain language,
contributors, and what is *not* known. Judge the disclosure, never the
governance.

**Adoption cannot be counted** without a registry or telemetry, neither
of which we want. So count what is verifiable — contributors, from PR
history — and label it: *Adopters: unknown and uncounted, by design.*
That undercounts, which is the safe direction for a number a new
community uses to decide whether a tradition is trustworthy.

## Boundaries

**Not built:** no daemon, server, or UI. No accounts, auth, or sync — it
is a file, use git. No mesh. No hosted canons; a lineage lives in its
authors' repository, and a registry that stores copies is one subpoena
from being the thing this was built to avoid. No score, ever.

**No integrations.** `--json` and honest exit codes make Slack bots, git
hooks and PR bots a shell script. MCP is the single exception, and it
earns it by being a standard socket rather than one vendor's API.

**It stands alone.** The README never requires knowing what Commonwealth
is — no platform banner, no diagram of a system the reader did not ask
about. It is a decision log with supersession, reasons, and a check
command; that description is complete. The relationship is one line:

> `canon` writes an append-only act log in an open format. Larger tools
> read the same file.

That is the distribution mechanism. It proves the fold and the format on
a surface small enough to read in a sitting. And a decision log competes
with nobody: it is not a category anyone is defending, which makes it far
easier to put in front of a skeptical reader than a system with opinions
about inference and mesh coordination.

**The strategy's risk is fragmentation.** Several standalone tools with
drifting formats is worse than one monolith. The only discipline that
prevents it is the format spec plus a conformance suite, versioned and
held seriously — the treatment OICP already got here. Without that, a
family is a pile.

## First cut

1. `init` · `add` · `list` · `log` — the fold, one file, no model.
2. `why` · `supersede` · `retract` · `undo` — history and reversal.
3. `check` · `tensions` — endpoint, `--json`, exit codes.
4. `canon mcp` — four read-only tools. The key integration.
5. `draft` — the cold start, and the reason anyone arrives.
6. Profile renderers.
7. `share` · `adopt` · `diff --upstream` · merge driver — once there is a
   second canon in the world to fork.

Stages 1-2 are a weekend and already useful: a decision log with
supersession and recorded reasons, which most teams do not have in any
form. Stage 3 makes it governance. Stage 4 is why anyone integrates.

**The leanness test:** someone clones it, reads the whole thing in one
sitting, and understands the fold.
