# Case study — a 25-engineer team, all running agents

The companion to [CASE_STUDY_FERNWOOD.md](./CASE_STUDY_FERNWOOD.md).
Same machinery, a codebase instead of a house, and one difference that
changes the economics completely: **most of the actors are not people.**

This repository is already running the experiment. It just is not
measuring it.

---

## The team is already governed — in a different vocabulary

| Fernwood | This team |
|---|---|
| Charter | `AGENTS.md` + `ARCH_PRINCIPLES.md` — the eleven, the two ratified rules |
| Dated Decisions amending it | "Operator direction 2026-08-14"; "This paragraph said the opposite until 2026-08-20" |
| Rules | 276 invariant notes |
| Tensions | the 17-row smell table; `drift_findings` (narrative against code) |
| Adjudication | a directive with an `approve` / `revise` verdict |
| Accepted contradiction | a `DEFAULTS_LEDGER` row — flip condition, review-by date |
| The steward | the comaintainer seat |
| Charter authors gone | 170 transient sessions, none of which wrote it |

The correspondence is not a metaphor. `DEFAULTS_LEDGER.md` is
`accept_tension` with a required rationale and a revisit date, invented
independently for the same reason: a known contradiction the group
chooses to carry, which rots if nothing tracks it.

---

## What twenty-five engineers with agents changes

**Volume.** Fernwood produces forty decisions in three years. Twenty-five
engineers running agents produce that before lunch. Governance stops
being a monthly meeting and becomes a stream, which is precisely the
regime where human reading fails and the log stops being read at all.

**The membership inverts.** Twenty-five people running three agents each
is a hundred actors, seventy-five of them non-human. `INV-2` — every
adjudication attributed to `human:<name>`, anything else surfacing as
`UnattendedAct` — stops being a nicety and becomes the load-bearing
structural feature. Without it you get governance by whichever agent ran
last, and no way to see that it happened.

**The constitution is already being paid for, every session.** The
compensating move here was prompt-space: ~14k tokens of `CLAUDE.md`
injected into every session, whether or not the session touches anything
it governs. That is the cost line the alternative gets measured against.

---

## The kicker: inter-agent agreement is a spec-quality metric

At Fernwood the agreement diagnostic was limited by N=25 humans and a
72% response rate. Here that limit disappears.

**Two agents handed the same 14k-token constitution will make different
calls.** Run the same diff past N agent instances and ask each whether it
complies with principle 11, and the spread is not a fact about the
agents. It is a **direct measurement of how well-specified your
constitution is.**

```
Agreement across 40 agent labelings, 50 real diffs

  §11 inventory-outranks-the-plan          52%   ← ambiguous
  §18.4 validate the instrument            61%
  §10.6 one decider, one name              88%
  §2.1 match on string ids > 3 arms        97%   ← mechanical
  §1.1 doc changes land with the code      94%
```

A principle your own agents apply 52% of the time is not being enforced.
It is being sampled. Every session touching that area is a coin flip,
and the variance was invisible because nobody ever asked two agents the
same question and compared.

Three properties make this the strongest version of the diagnostic:

- **N is unbounded.** The small-sample honesty problem from Fernwood
  does not arise; run it a thousand times.
- **The corpus already exists.** 2,804 decision notes, 276 invariants,
  and a git history whose commit bodies state their own rationale.
- **It runs on the local daemon**, so it costs machine time rather than
  vendor tokens.

And the output is a work order: *these three sections are ambiguous;
rewrite them.* Not "the agents are bad."

---

## The worked example is in the constitution already

`AGENTS.md` on how principle 11 came to exist:

> Eleven was minted 2026-08-08 after the additive-bias pattern recurred
> a **third** documented time — each catch came from the operator, never
> from the builder's own process.

That is the entire product thesis in one sentence, written by the team
that needed it.

A recurring tension between what the constitution said and what sessions
kept doing took **three occurrences and a human** to become a rule,
because there was no reader. The first two occurrences were logged. They
were logged in a store with 3,417 notes and no one whose job is to read
it.

A standing reader raises that pattern at occurrence two. Not because it
is smarter — because reading the log is its only job.

---

## You do not need more instruments

This is the part that matters for a team that already bought tools.

The instruments here are extensive and good: notes, work atlas, session
frames, drift, defaults ledger, cache-audit, contract census, bench
baselines, arch report, posture. `COMAINTAINER.md` §2 audits them and
reaches one finding — **every row is an instrument with no staffed
operator.** `svrn notes rationalize` exists and is nobody's job. Drift
was stale at boot. Frames drifted off their own objectives in 21 of 63
cases.

So the standing reader is not another instrument. **It is the thing that
reads the instruments you already have**, and its weekly output is the
same shape as Fernwood's — every line a call to action:

```
CONFLICT ON ARRIVAL
  PR #2210 adds a second implementation of the retry threshold.
  §10.6, and smell-table row "two implementations of one threshold".
  The existing one: sovereign-core/src/runtime/backoff.rs:88.

MOOT
  4 open drift findings closed themselves — the narrative section was
  rewritten on 14 May. Removed.

PAST REVISIT DATE
  DEFAULTS_LEDGER row `cluster_weight=0.0` said "pending bench plan".
  That was 71 days ago.

PATTERN — second occurrence
  A session rebuilt a corpus harness that already existed (note 8def98d7
  was the first). §19 covers this. Two is when it becomes a rule.

QUIET
  No adjudication in 19 days.
```

---

## Cleavages in a codebase are Conway's law, measured

The cross-cutting test transfers directly, and finds something specific.

Take each contested architectural decision and the partition of
engineers it produced. If the partitions keep coinciding — the same line
dividing every question — that is a reinforcing cleavage, and in a
codebase it usually has one cause: **your team boundary and your module
boundary disagree.**

```
STABILITY
  Your last 15 contested decisions split the team along the same line
  11 times. That line runs between the crates, not between the people.
```

The persistent minority reads the same way it does at Fernwood, with a
detail that recurs: the engineer who keeps objecting to a pattern is
very often the one who maintains the thing it breaks. Their objection
cohering across three modules means there is a real constraint the
constitution never encoded — the on-call cost, the migration burden,
the customer who does the unusual thing.

Same private channel, same rule: to the team as a question about the
architecture, to the dissenter as an offer, never to the team as a fact
about a person.

---

## The agent surface: query the constitution, don't recite it

Every agent here already calls tools over MCP, so the interface adds
four and costs almost nothing — since nc-18, a new tool is a manifest
row plus an async function.

```
room_check("add a GovernanceStore for the new adjudication cache")

  → CONFLICT
    §19 / principle 11: survey what exists and prove it cannot serve.
    Existing surface: corpus-engine oplog + GovernanceView.
    Note 4c64171c: two nouns named `Gap` cost a refactor.
    This needs the inventory citation before it needs code.
```

```
room_check("bump the retry ceiling in the mesh client")

  → UNADDRESSED
    No principle governs retry policy.
    2 other open questions touch it. Added.
```

**And it is falsifiable against a baseline that already exists.** The
current design costs ~14k tokens per session, always. The queried
alternative either holds behaviour at lower spend or improves behaviour
at the same spend, and `cache-audit` measures it per session. That is a
bar, not a hope — and it is ARCH_PRINCIPLES §7, *structural not
remembered*, finally applied to the constitution itself.

---

## What the agenda looks like

The same two axes, and the quadrant that matters is unchanged:
**wanted and not permitted.**

| | Low demand | High demand |
|---|---|---|
| **Supported** | a principle nobody invokes — candidate for deletion | just do it |
| **Unaddressed** | nothing | **write the ADR** — the constitution must grow |
| **In conflict** | drop it | **amend the principle** — as §11 was, at occurrence three |

The bottom-right cell is how `ARCH_PRINCIPLES` grew to eleven. The
difference a reader makes is that the cell is populated continuously
instead of at whatever moment a human happens to notice for the third
time.

A team whose agenda is empty is not aligned. Its constitution has
stopped tracking what it actually does — which, given how fast
twenty-five agent-driven engineers move, is the more likely reading.

---

## Limits, held

**Scoring is proposing, never deciding.** An alignment score that gates
a merge without a human act has routed around `INV-2` through CI. The
score belongs in the PR as evidence, next to the reviewer, not in place
of one.

**Ambiguity findings are about the text.** "Your agents apply §11 half
the time" is a finding about §11. The version that ranks engineers by
compliance is the same eviction argument in a different building.

**A constitution that is never ambiguous is probably not doing much.**
The target is not 100% agreement everywhere; §2.1 hits 97% because it is
nearly mechanical, and a principle requiring judgement will never score
like that. The signal is a section scoring far below its own kind — and
the ceiling for each kind has to be measured before any number gets read
as a verdict.
