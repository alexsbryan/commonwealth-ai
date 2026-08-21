# Living governance — the agenda is the product

A design note, not a plan. It records the shape the governance stack is
being pulled toward: an agent that can read a room, score proposed work
against that room's law, and hand the community back an agenda.

Buildable-today integration is a different document —
[GOVERNANCE_INTEGRATION.md](./GOVERNANCE_INTEGRATION.md). Nothing here
is built. For these interfaces worked through end to end at the scale
where the statistics hold, see
[CASE_STUDY_FERNWOOD.md](./CASE_STUDY_FERNWOOD.md) — and for the same
machinery over a codebase, where most of the actors are agents,
[CASE_STUDY_ENGINEERING.md](./CASE_STUDY_ENGINEERING.md).

## The thesis

**This is a tool for living governance. It requires engagement to work,
and it repays engagement visibly.**

That is a positioning choice with consequences, and it runs against the
category. Compliance tools promise to remove work: upload the handbook,
receive a score, forget it. This one generates work — it hands a
community a list of decisions only they can make. The bet is that
governance is not overhead to be automated away but the activity itself,
and what has been missing is not the will to govern but a tractable
surface to govern *on*.

Two commitments follow, and they are the whole design:

1. When a community engages, the system must measurably improve, in a
   way members can feel within one cycle.
2. When a community disengages, the system must say so — loudly —
   rather than continuing to render a confident view of stale law.

The second is not a caveat on the first. It is what makes the first
honest.

## The output has two dimensions

A proposal carries an **alignment** relation to current law — supported,
unaddressed, or in conflict — and a **demand** from the community. Score
only the first and you have built something structurally conservative:
conflict with current law is how constitutions learn, so ranking by
alignment alone scores amendment pressure as low value and quietly
ossifies the charter.

Crossed, the two axes say what kind of act each proposal needs:

| | Low demand | High demand |
|---|---|---|
| **Supported** | dead letter — a rule nobody needs | just do it; the tool should get out of the way |
| **Unaddressed** | genuinely nothing | **write a new rule** |
| **In conflict** | drop it | **amend an existing rule** |

The two bold cells are the meeting agenda, and they are not the same
kind of work. Unaddressed-and-wanted needs the charter to *grow* — a new
article. Conflicting-and-wanted needs it to *change* — a supersession.
Different governance acts, different ceremony, and a system that
collapses them into one "issues" list has thrown away the most useful
thing it computed.

A community whose agenda is empty is not aligned. It is asleep, and the
tool should be able to tell the difference.

## Why engagement is felt — each act improves a different organ

The loop is mechanical, not aspirational. Every adjudication feeds a
distinct part of the system:

- **Resolve** writes a `Supersede`, which grows `dead_law_sections()`,
  which the retrieval filter reads on the *very next question*. The
  answer visibly stops citing the rule you just retired. This is the
  fastest felt improvement in the system and it lands in one cycle.
- **Dismiss** produces a labeled negative — "these two are not in
  conflict" — in the community's own vocabulary. That is precisely the
  `expected_non_tensions` shape the bench uses, generated as a byproduct
  of ordinary work.
- **Accept** requires a rationale, which forces an unwritten norm into
  text. "We know these contradict, and here is why we tolerate it" is
  where a community's implicit values get stated aloud, often for the
  first time.
- **Revert** keeps all of it safe to do: a mistaken adjudication is
  tomb-stoned by the fold, so engagement carries no permanent risk.

The design choice that looked like caution — refusing to auto-resolve —
turns out to be the data-collection strategy.

## Why disengagement must also be felt

Every instrument in this repo that depended on someone remembering to
look at it has rotted. The defaults ledger exists because flip
conditions withered. Drift was stale at boot. `notes rationalize` exists
and is nobody's job. A governance tool is the same species of artifact
and will fail the same way, except worse: a stale governance view does
not look stale. It looks authoritative while describing a house that no
longer exists.

So the view owes the community its own liveness, in the shape the repo
already uses elsewhere — the defaults ledger's rule that *a row past its
review-by date is not noise, it is the signal*:

- when the last adjudication was, not just what it was;
- how long each open tension has been open;
- which open tensions have gone **moot** because a rule was superseded
  elsewhere — decisions the community no longer needs to make;
- which accepted contradictions are past the date the community said it
  would revisit them.

None of that is a health score for its own sake. Each line is a
different call to action, and a tool that renders current law without
rendering its age is lying by omission.

## Reading the room

The agent-facing surface is small. MCP is the natural transport; four
questions cover it:

- `room_charter()` — what governs here, how current, how much settled
  versus open.
- `room_check(intent)` — supported / unaddressed / in conflict, with the
  rule text and citation. **Abstention is a first-class answer**, and in
  a real corpus it is the most common one.
- `room_open_questions()` — what the room has not decided that bears on
  the work in front of me.
- `room_precedent(situation)` — what was decided in situations like this
  before.

`room_precedent` is where unwritten norms surface. A charter holds the
written rules; rooms run on unwritten ones, and an agent reading only a
constitution will confidently misread a real room. That limit is
fundamental — no extraction pass fixes it. But when a community keeps
resolving in a direction the charter never states, an unwritten norm is
becoming legible in the log. **The adjudication log is the ethnography.**

## We are the first customer

An agent entering this codebase must determine the house rules before
acting. Today that is solved by injecting ~14k tokens of constitution
into every session and hoping — and `.claude/CLAUDE.md`'s own texture,
section after section opening "this exists because X kept happening", is
the evidence that it saturates.

`room_check("I'm about to add a new store")` returning principle 11 with
its citation is the structured alternative: query the constitution
against an intent rather than reciting it in full. That is
ARCH_PRINCIPLES §7 — make it structural, not remembered — finally
applied to the constitution itself.

It is also the cheapest honest pilot available, because we are the
domain expert on this corpus and can grade the output by hand.

## The first measurement is the first product

There is no ground truth for proposal alignment. Building it is the
work — and the first result it produces is worth more than the model it
was meant to validate.

**The bar, registered before any model runs:** measure human-human
agreement first. Ask N members to label M proposals against their own
charter, and compute agreement per article.

The instinct is to read a low number as a low ceiling — if members agree
only 60% of the time, what could a model possibly be worth? That reading
is backwards.

**A community that agrees with itself 60% of the time almost certainly
believes it agrees 90%.** That gap is not an abstraction: it is being
lived, right now, as recurring arguments, as the chore that never gets
done, as two people quietly opting out. It gets attributed to
personalities and temperaments, because nothing has ever made the real
cause visible. The cause is usually a handful of sentences that two
reasonable members read in two different ways.

So the number is not a gate to pass before the product. **It is the
product's first output, and it needs no model to produce.**

### Disagreement is local, and that is what makes it actionable

Agreement will not be uniformly 60%. It will be 95% on quiet hours and
30% on what counts as "cleaning the kitchen." The aggregate is a
curiosity; the per-article breakdown is a work order — it names the
sentences to rewrite, and it names them without anyone having to
volunteer that they find a rule unclear.

### What this makes the model's job

Not to be right. **To predict the split.**

Where members genuinely disagree, there is no gold label, and inventing
one is exactly the failure §18.3 names — an absence reported as a
value. The model's task is to find *which* proposals will divide the
house, and route those to a meeting.

That folds cleanly into the architecture already here. The cite-or-abstain
gate abstains when evidence is absent; this adds a second abstention
trigger — **abstain when the community does not agree with itself** —
and the abstention is not a failure to answer. It is the agenda item.

Where agreement is high, the tool can answer and no one needs a meeting.
Where it is low, the tool declines and says why. The metric that matters
is therefore calibration against the split, not accuracy against a gold
label that does not exist.

### Two honesties this measurement owes

**Small N.** A house has six members. Agreement statistics on N=6 are
noisy, so the proposal count has to carry the weight, and results belong
in per-article intervals rather than point estimates. One run is not a
measurement (§18.5).

**Attribute to the text, never to the people.** "You agree 60%" handed
to a community is destabilising and trivially weaponised — *see, nobody
agrees with you*. The finding must point at the rule, aggregated and
anonymised: *these three articles are read two ways*. Never per-person,
never attributable in either direction. A tool that makes disagreement
legible has an obligation not to make it personal.

Maple House already holds the harder half of such a corpus: seven
labeled `expected_non_tensions` with written reasons — additive rules,
different groups, different places, separate exemptions. Those are
exactly the distinctions a proposal scorer has to make, and they were
expensive to author.

## The persistent minority — the system answers to it

Consensus tooling that surfaces only majority alignment does not merely
miss the minority. It **mechanises the majority**: it gives the larger
group an instrument, and leaves the smaller one exactly where it was,
now with the appearance of arithmetic behind the outcome.

A member who is consistently orthogonal to consensus is carrying
information. Either they hold a value the charter never encoded, or they
occupy a position the charter's authors did not share — they cook every
night, they work nights, they are the only one without a car, they have
a condition nobody wrote a rule around. Both are constitutional gaps,
and both are invisible to any process that only counts.

So the obligation runs in a specific direction: **the system answers to
the dissenting voice.** Not the voice justifying itself to the system.

### Demand cannot be a mean

The two-dimensional output above is wrong if demand is an average. A
proposal wanted intensely by one member and mildly opposed by five is
not the same object as one nobody cares about, and a mean maps them to
the same point.

Dispersion is the signal, not central tendency. The minority-intensity
case is precisely what an average is built to erase.

### What is actually needed is the position, not the person

The useful finding is not *who* dissents. It is *what the dissent
articulates* — and that is recoverable without identity.

If Articles III, V and VIII all split five-to-one **along the same
axis**, that is a coherent minority position, and its content can be
read off the minority labels directly. The finding is: *there is a
consistent alternative reading of these three articles, it hangs
together, and here is what it holds.* A community can act on that
completely. Attaching a name to it adds nothing to the remedy.

Coherence across articles is the evidence. Identity is not.

### Tell the dissenter first, and give them the floor

The ethical channel inverts the usual information flow. When the system
detects a persistent minority pattern, the first party it tells is **the
person holding it** — who very often does not know their disagreement is
systematic rather than situational:

> Your reading of Articles III, V and VIII differs consistently from the
> house's. Would you like to raise it?

That is the system answering to them: it gives them the floor, and it
lets them choose disclosure. The position can then go to the agenda
carrying its argument and not their name.

### The line, and it is architectural

- To the community, as **a question about the rules** — permitted.
- To the dissenter, as **an offer** — permitted.
- To the community, as **a fact about a person** — never.

Three reasons this is a hard line rather than a policy preference.

**The misuse requires no effort.** "Sam disagrees with the house 70% of
the time" is a compliance score for a human being. In a shared house it
is an eviction argument, and the feature's stated purpose and its
easiest use point in opposite directions.

**Persistent dissent correlates with protected and minority positions.**
The person consistently orthogonal on kitchen rules may be the only one
with a dietary restriction, a chronic illness, or a night shift.
Surfacing "who is orthogonal" surfaces those people, with a very clean
mechanism and a very familiar disparate impact.

**Small N makes anonymity impossible once you are specific.** In a
six-person house, any sufficiently precise report resolves to an
individual regardless of intent. That is not solved by redaction; it is
solved by never computing the per-person series in the first place, and
by reporting patterns over rules rather than over people.

The system that makes disagreement legible carries the obligation not to
make it personal. Build the private channel and the position-level
finding; do not build the leaderboard, because once it exists it will be
used for what it wasn't built for.

## Issue-by-issue coalitions, and why they were unadministrable

The structure this whole design is reaching for has a name in political
science, and a reason it is rare.

**Coalitions that re-form per issue are stable because they rotate.**
When the same majority wins everything, the minority is permanent and
has no reason to stay — that is the destabilising case. When each issue
draws its own line, today's loser is next month's winner, everyone wins
sometimes, and members hold a stake in the *process* rather than in a
bloc. The stability condition is that cleavages **cross-cut** rather
than reinforce: different lines dividing different questions, instead of
one line dividing all of them.

Parties and blocs are not a better structure. They are a **compression
algorithm** for an administrative problem. Nobody can hold thirty issues
across six people, with intensities, updated continuously — so groups
compress preferences into a bloc that is cheap to administer and lossy
by construction. The bloc exists because issue-by-issue was impossible,
not because it was preferable.

What made it impossible is a specific, boring list:

1. know each member's position on each issue, and keep it current;
2. find the coalition per issue, which is combinatorial;
3. notice when cleavages have started to *reinforce* — the same split
   recurring — because that is a faction forming;
4. track intensity, not just direction, since the trades that make
   everyone better off depend on some caring more than others;
5. redo all of it as positions drift.

Every one of those falls out of machinery already described here. (1) is
the per-article agreement measurement. (4) is the dispersion requirement
from the demand axis. (3) is the persistent-minority detector — the same
computation, doing a second job.

### The same computation, read as a stability metric

The minority section asks whether Articles III, V and VIII split *along
the same axis*. Asked of the whole decision history rather than one
member, that is exactly the cross-cutting test.

Take the partition of members produced by each decision. If the
partitions keep coinciding, one axis is doing all the dividing:
reinforcing, faction-forming, unstable. If the partitions are largely
independent — this issue splits one way, the next another — the
cleavages cross-cut and the house is in the stable regime.

That is arithmetic over partitions. It needs no model, names no member,
and produces a warning a community can act on before it fractures:

> Your last twelve decisions split the house along the same line eleven
> times. That line is becoming a faction.

A property of the geometry of disagreement, reported without reference
to who stands where.

### What AI actually contributes, stated narrowly

"Too complex to administer before AI" is right, but it is worth being
exact about which part, because most of the above is old and cheap
arithmetic.

The model earns its place at the **edges**:

- turning unstructured human expression — thirty requests, meeting
  notes, a thread — into separable, comparable positions;
- predicting which issues will split the house *before* it spends a
  meeting finding out;
- reading a coalition's labels and saying what it holds in common, so a
  minority position arrives as an argument rather than a tally.

The governance mathematics in the middle stays pure. That is the same
shape the stack already has — model at extraction and at `ask`, a pure
fold in the core — and it is the same reason: the part that must be
auditable, replayable and beyond dispute is the part that must not
depend on a model.

### Two cautions this structure carries

**Reveal shared interest; never compute the winning bloc.** A system
that says "these four members care about kitchen access, for three
different reasons" informs. A system that says "you need two more votes,
offer Sam the parking item" manipulates, and turns adjudication into
logrolling with better tooling. The first output is a finding about
interests; the second is a strategy for capture, and the difference has
to be designed in rather than left to how a screen is worded.

**Resist bundling, because bundling is how blocs re-form.** Issue-by-issue
coalitions only exist while issues stay separable; package a proposal and
the coalition collapses back into a party. Usefully, the anti-bundling
mechanism is already load-bearing here for other reasons: decomposing a
document into atom-level claims is structurally the refusal to let five
questions be voted as one.

## Two limits to hold

**Scoring is proposing, never deciding.** INV-2 keeps non-human actors
out of adjudication, and a score that allocates chores, money, or
priority without a human act routes around it through the back door. The
oplog has no `score` act and must not gain one that binds anything.

**Scores become adversarial the moment they allocate.** Maple House's
decoy — guest parking against guest nights — is an accidental lexical
overlap. Once members know their requests are scored against the
charter, they will write requests in charter vocabulary deliberately.
The decoy rate stops being a detector-quality metric and becomes an
adversarial one, and that is a different and harder measurement problem.
