# CO mesh drill — two-seat conformance for order seat-durable-rail

Proves UC-D1..D4 of the seat-durable-rail order across **two machines**
(RuggedFox and BeefyMac, the two seats on the mesh). Prior art: the
five-channel `mesh-seat-coordination-drill` (closed as landed
2026-08-11) — per-case entry points, four-verdict readings, and
operator-relayed cross-machine steps. Each case is a script verb; each
verb prints `DRILL_STEP <case> <step> <verdict> <evidence>` lines. The
two sides' logs are assembled into the final table by
`co-mesh-drill.sh report`.

## Prerequisites — do these ONCE, in this order

1. **Daemon rebuild + restart on BOTH machines** with the merged
   seat-durable-rail binaries. This is SEAT-OWNED: the seat restarts
   the daemons, not the drill runner. The withholding lives in the
   daemon-served notes tool; a pre-registry daemon would read as
   could-not-judge on D4, not as a failure.
2. **The branch merged on both trees.** The drill runs the repo's real
   scripts (`co-order.sh`, `co-directive-log.sh`, `inject-notes.py`,
   `co_notes.py`) — from the repo root on each machine.
3. **The operator relays cross-machine steps.** Side A (reader) and
   side B (writer) are run on different machines; the operator runs
   each side's commands and copies the side's stdout into a log file
   for `report`. A relay unavailable at the moment a step needs it is
   a could-not-judge for that step, not a failure.

## Four verdicts, not two (ARCH §18.2)

- **passed** — every step of the case PASSed on both sides, evidence in hand.
- **failed** — at least one step FAILed (the observed state contradicted the pass bar).
- **could-not-judge** — a step could not tell: daemon down, gossip link silent inside the poll window, relay unavailable, or pre-registry daemon. The evidence is recorded with the verdict.
- **never-ran** — no step of the case was invoked on either side.

Time budget: **<5 minutes per case** (bounded polls only; the gossip
round is 10–30 s plus relay latency).

---

## D1 — an order opened on B is listed on A, and its close converges

Pass bar: A's `co-order.sh list` shows B's order **with node
attribution** within one gossip round + one prompt; after B closes, A's
`list` no longer shows it (the retire → tombstone → gossip path).

Side B (writer):

```bash
scripts/co-mesh-drill.sh open drill-$(date +%s) "drill order"   # note the id
```

Side A (reader), after the operator relays the id:

```bash
scripts/co-mesh-drill.sh list-check <id> BeefyMac    # or RuggedFox — the WRITER's node
```

Side B (writer):

```bash
scripts/co-mesh-drill.sh close <id>
```

Side A (reader):

```bash
scripts/co-mesh-drill.sh gone-check <id>
```

The open also creates a real order file in `.sovereign/features/<id>/`
on side B — `close` retires it; the file is the truth and the retire is
the mesh-visible hide.

## D2 — an addressed note is carried in B's ambient, and the reply round-trips

Pass bar: B's **prompt hook** (`inject-notes.py`, the real ambient
path) carries A's unanchored decision note; B's reply is visible to A
from an ordinary read. **The measured round-trip number** is the
elapsed time between the write and the ambient read, plus write-reply
to seen — the script prints the seconds in its step evidence.

Side A (writer):

```bash
scripts/co-mesh-drill.sh note drill-<ts>-d2 "checking the mesh rail"
```

Side B (reader):

```bash
scripts/co-mesh-drill.sh ambient-check drill-<ts>-d2
```

Side B (writer):

```bash
scripts/co-mesh-drill.sh reply drill-<ts>-d2 "reply from the peer seat"
```

Side A (reader):

```bash
scripts/co-mesh-drill.sh seen-check drill-<ts>-d2
```

**The D2 bootstrap problem, named:** a seat's ambient context is fed by
its own daemon's notes store, which only knows what has gossiped.
There is no out-of-band seed: a brand-new seat sees nothing of the
other machine until the first gossip completes (and a mesh link that is
down reads as "nothing to see", indistinguishable from an empty store
without the withheld/attribution reporting). D2's first hop therefore
depends on the link being up and converged; an ambient-check that
times out on a dead link is could-not-judge with that evidence, not a
code failure. The measured number to report is the write→ambient and
reply→seen latency on a **live** link.

## D3 — a directive resolved on A is counted on B, attributed to A's node

Pass bar: B's `co-directive-log.sh --stats` counts A's resolved
directive with **node attribution**, and the ALL-row (n, edited, edit
rate) reads the same on both machines — the same number from either
seat. The denominator is the notes store (mesh-wide), not one host's
file fragment.

Side A (writer):

```bash
scripts/co-mesh-drill.sh directive drill-<ts>-d3
```

Both sides:

```bash
scripts/co-mesh-drill.sh stats-check drill-<ts>-d3 A   # on A
scripts/co-mesh-drill.sh stats-check drill-<ts>-d3 B   # on B
```

Drill rows land in a **drill-specific local log**
(`~/.sovereign/comaintainer/directives.drill.jsonl`), so re-running the
drill never pollutes the real M0 edit-rate metric; the mesh
write-through goes to the real notes store either way — the store is
what the cross-machine comparison reads. Local rows in the real log are
excluded from the mesh denominator and reported as such by `--stats`.

## D4 — the flood guard: zero seat records in ordinary sessions, and the withholding is REPORTED

Pass bar (ordinary): an ordinary session's injected ambient contains
**zero** anchored seat records **and** the hook prints the withheld
line naming the anchors (ARCH §18.3 — absence is reported, never
silently defaulted). Pass bar (seat): with `SOVEREIGN_SEAT=1` the same
ambient path carries the anchored records and prints no withheld line.

```bash
scripts/co-mesh-drill.sh d4-check ordinary   # on either machine
scripts/co-mesh-drill.sh d4-check seat       # on either machine
```

Run D1 first — the seat-mode presence proof needs at least one
anchored drill record in the store (the order note's summary line).

## Cleanup and re-running

```bash
scripts/co-mesh-drill.sh cleanup <marker>   # retires the drill's notes, clears the drill log
```

`cleanup` is idempotent and safe to run at any time. The drill is
re-runnable: fresh markers each run (the verbs' examples use
`drill-$(date +%s)`), same pass bars.

## Assembling the verdict table

```bash
scripts/co-mesh-drill.sh report side-a.log side-b.log
```

`report` is daemon-free: it reads the relayed `DRILL_STEP` /
`DRILL_TABLE` lines and applies the verdict rules above, including the
D3 cross-machine ALL-row comparison (a machine that fell back to its
LOCAL tally marks the case could-not-judge — its banner is relayed as
evidence).
