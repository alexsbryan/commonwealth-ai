# Case study — Fernwood House, 25 residents

A worked walkthrough of the interfaces in
[LIVING_GOVERNANCE.md](./LIVING_GOVERNANCE.md), at a scale where the
statistics work and the politics are real. Fernwood is invented; every
interface, output, and failure below maps to something specified or
already built.

Twenty-five residents in a converted building. Three other co-living
houses in the same city, loosely connected, occasionally poaching each
other's ideas over beers.

---

## Month 0 — What is our law, actually?

Fernwood's governance lives in four places: a founding Charter written
in 2019, roughly forty sets of house-meeting minutes, a Notion page
called "How We Do Things", and a Slack channel with eleven pinned
messages.

**Nineteen of the twenty-five current residents never voted on the
Charter.** Six of the eight authors have moved out. This is the ordinary
condition of a community more than a few years old, and it is why "what
is our law" is a genuine question rather than a rhetorical one.

They add the Charter and the minutes through the **Rules & decisions**
folder template. Extraction runs against their own endpoint — one
resident already runs llama.cpp on a workstation in the basement, so
`base_url` points there and nothing leaves the building.

```
63 rules extracted    41 Charter · 22 Decisions
14 tensions surfaced  ranked by confidence
 9 rules superseded   by a later Decision, never marked as such
```

**First friction, and it is the important one.** The Slack pins are not
in the corpus, so three rules everyone actually follows do not exist as
far as the system is concerned. Garbage in. They add the pins as a dated
document and re-run. The lesson generalises: the system knows only what
was written down somewhere it can read, and the gap between that and
lived practice is itself a finding.

---

## Month 0, week 2 — The agreement diagnostic

Before adjudicating anything, they run the model-free diagnostic.
Eighteen of twenty-five residents label twenty proposals against their
own Charter. The report leads with coverage, because a 72% response rate
is part of the result and not a footnote.

```
Agreement, house-wide          71%   (18/25 responding)

By article — lowest first
  Art 9  "reasonable use of common space"    38%   ±9
  Art 4  guests and overnight visitors       41%   ±8
  Art 12 shared-food and fridge rules        63%   ±9
  ...
  Art 2  quiet hours                         96%   ±3
  Art 7  rent and dues                       94%   ±4
```

Before seeing this, residents polled informally guessed the house agreed
"about ninety percent of the time."

The gap is the product. Two phrases — *reasonable use* and what counts
as *a guest* — are carrying nearly all of the disagreement, and they
have been carrying it for three years as the recurring living-room
argument that everyone attributed to two particular housemates not
getting along.

**A resident objects that the survey is surveillance.** The objection is
legitimate and the answer is structural, not reassurance: no per-person
series is computed, results are reported per article with intervals, and
nothing in the pipeline can render a per-resident agreement score
because that column is never produced. They proceed. One resident still
declines, and the coverage line says 18 of 25 rather than quietly
normalising.

---

## Month 1 — The first session

The panel opens on the agenda, not on a score. Fourteen tensions, ranked,
each showing both rule texts.

They work through it in ninety minutes:

- **6 resolved** — a later Decision supersedes a Charter article.
  Rationale required and recorded.
- **3 dismissed** — detector noise. One paired a rule about guest
  *parking* with a rule about guest *stays*, on the shared word "guest".
- **2 accepted** — real contradictions the house chooses to live with,
  each with a written reason and a revisit date.
- **3 deferred** — genuinely hard, needs a longer conversation.

**The moment the thesis rests on.** Someone asks the house assistant a
guest question that evening. It answers with the current rule, notes
which Charter article it replaced and on what date, and cites both.
Before this, the Charter and the Notion page disagreed and residents
quoted whichever supported them.

That is `dead_law_sections()` doing its work within one cycle of the
decision that created it. It is small, and it is the thing people
actually feel.

---

## Months 2-4 — The reader runs standing

Fernwood is now on the paid tier for one reason: nobody in the house
wants the job of noticing things. The weekly digest is short and every
line is a call to action.

```
Fernwood — week of 12 May

CONFLICT ON ARRIVAL
  Thursday's decision on porch storage contradicts Art 9 (common space).
  Both texts attached. Decide before it becomes precedent.

MOOT — no longer needs your agenda
  3 open tensions closed themselves: Art 11 was superseded on 28 Apr,
  which resolved them. Removed from the agenda.

PAST REVISIT DATE
  The fridge-labelling contradiction you accepted on 3 Feb said
  "revisit in 90 days". That was 22 days ago.

QUIET
  No adjudication in 19 days. Nothing is wrong; noting it.
```

The *moot* line is the one residents mention. Three arguments they had
been dreading turned out not to need having, because a decision made for
another reason had already settled them.

---

## Month 5 — The stability warning, and what it found

The cross-cutting test fires:

```
STABILITY
  Your last 12 decisions split the house along the same line 9 times.
  That line is becoming a faction.
  (Partition geometry only — no resident is named or scored.)
```

This is the alarm before a fracture, and it says nothing about who.

Investigating the axis shows Articles 4, 9 and 11 all splitting the same
way. The system messages the four residents on the minority side of that
axis — privately, individually, and only them:

> Your reading of Articles 4, 9 and 11 differs consistently from the
> house's. Would you like to raise it?

Two of the four had assumed they were simply losing arguments. One
accepts.

**The position coheres.** All three articles quietly assume residents
are home in the evening — that is when common space is negotiated, when
guests arrive, when the house is "in session". The four are shift
workers: two nurses, a baker, a line cook. The Charter was written by
eight people who all worked nine to five.

That is not a disagreement. It is a constitutional gap, and it belongs
in the **unaddressed-and-wanted** quadrant: the Charter needs to *grow*
a new article, not amend an existing one.

They write Article 14, on the house's obligations to residents whose
hours differ from the majority's. It passes 23-2.

By month 7 the partitions have decorrelated and the stability warning
clears. The metric detected, the private channel diagnosed, a new
article resolved it, and no one was ever named to the group.

---

## Ongoing — the agent surface, doing dull work

Fernwood's chore bot and their Discord bot both speak MCP, and both call
the same four tools.

A resident proposes swapping a kitchen shift. The chore bot calls
`room_check`:

```
room_check("swap my Tuesday kitchen shift for Marco's Saturday")

  → SUPPORTED
    Art 5: a member unable to complete a chore must arrange a direct
    swap ahead of time. Cites Art 5 ¶3.
    Note: the cook exemption (accepted contradiction, 3 Feb) does not
    apply here.
```

A resident asks about installing a shelf in the hallway:

```
room_check("put up a shelf in the second-floor hallway")

  → UNADDRESSED
    No rule governs semi-private modification of hallways.
    3 other open items depend on the same gap.
    Added to open questions.
```

**The abstention is the useful answer.** It is also the most common one,
and over six months `room_open_questions()` accumulates into the agenda
for their annual Charter review — a list of what their law never covered,
assembled as a byproduct of people asking ordinary questions.

---

## Cross-pollination — four houses, one city

Fernwood peers with three other houses. The mesh model already answers
the hard question: edges are by invitation, out whenever, and nothing is
shared unless both sides say so.

**What is never shared: the oplog.** Their adjudication log is full of
rationales about specific incidents and specific people. It stays home.
This is the line, and it is not configurable.

**What is worth sharing turns out to be four things:**

| Shared | Why it works |
|---|---|
| **The ontology recipe** | How to extract governance from a charter. Generic, expensive to tune, identical across houses. |
| **The decoy library** | "Guest parking vs guest stays looks like a conflict and isn't." Every house rediscovers the same false positives; sharing them raises everyone's precision. |
| **Ambiguity findings** | Four houses independently measure ~40% agreement on the phrase *reasonable use of common space*. |
| **Article text, on request** | House 4 adopts Fernwood's Article 14 nearly verbatim, with attribution. |

The third is the surprising one. When four houses all measure low
agreement on the same phrase, that is a finding about **the phrase**, not
about any house — and it is shareable precisely because it names no one.
A city-wide library of *charter language that reliably fails* is a public
good that no single house could build, and a new house forming next year
can avoid four known traps before its first meeting.

**Nested rooms.** The city co-op holds bylaws binding all four houses.
That makes it an outer room, and `room_check` resolves against the
stack:

```
room_check("reserve the dining room for a private event")

  → CONFLICT (outer room)
    Fernwood Art 9 permits booking with 48h notice.
    Co-op bylaw 6 forbids exclusive use of shared ground-floor space.
    The outer rule governs. Escalate to the co-op, not the house.
```

Governance nests, and nobody's charter is the outermost one — a house
sits inside a co-op sits inside a city. The interface has to say *which
room* a conflict belongs to, or communities will keep trying to settle
questions at the wrong level.

---

## What this required, and what it did not

**Free, self-hosted, one room:** ingest, the panel, the CLI verbs, the
four `room_*` tools, the agreement diagnostic. Fernwood ran all of this
on a basement workstation. No cloud account, no data leaving the
building, and the oplog is a file they can walk away with.

**Paid:** the standing reader and the cross-house peering. Fernwood pays
for exactly one thing — nobody wanting the job of noticing.

**Never built:** a per-resident agreement score, a governance health
number, a ranked list of who dissents most. Fernwood asked for the last
one in month 6, after the shift-worker article, reasoning that it had
worked out well. The answer was no, and the reason is in
`LIVING_GOVERNANCE.md`: the finding they valued came from the *position*,
not the *person*, and the version that names people is an eviction
argument wearing a dashboard.
