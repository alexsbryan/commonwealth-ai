# CO mesh drill — two-seat conformance for orders seat-durable-rail AND commons-fluency

Proves UC-D1..D4 of the seat-durable-rail order and UC-F1..F8 of the
commons-fluency order across **two machines** (RuggedFox and BeefyMac,
the two seats on the mesh). Prior art: the five-channel
`mesh-seat-coordination-drill` (closed as landed 2026-08-11) — per-case
entry points, four-verdict readings, and operator-relayed cross-machine
steps. Each case is a script verb; each verb prints
`DRILL_STEP <case> <step> <verdict> <evidence>` lines. The two sides'
logs are assembled into the final table by `co-mesh-drill.sh report` —
**for the D-cases**. The F-cases are relay-free: their verdicts are
written as anchored notes and the table is assembled from the notes
alone (UC-F8), so no operator acts between the start note and the
final table.

## Prerequisites — do these ONCE, in this order

1. **Daemon rebuild + restart on BOTH machines** with the merged
   seat-durable-rail binaries. This is SEAT-OWNED: the seat restarts
   the daemons, not the drill runner. The withholding lives in the
   daemon-served notes tool; a pre-registry daemon would read as
   could-not-judge on D4, not as a failure.
2. **The branch merged on both trees.** The drill runs the repo's real
   scripts (`co-order.sh`, `co-directive-log.sh`, `inject-notes.py`,
   `co_notes.py`) — from the repo root on each machine.
3. **The operator relays cross-machine steps — D-cases only.** Side A
   (reader) and side B (writer) are run on different machines; the
   operator runs each side's commands and copies the side's stdout
   into a log file for `report`. A relay unavailable at the moment a
   step needs it is a could-not-judge for that step, not a failure.
   The **F-cases need no relay** — the drill runs itself from a start
   note (UC-F8); the operator's only act is that one start note,
   written by hand the first time (the bootstrap honesty clause:
   the channel cannot carry an instruction to a session that is not
   yet watching).

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
silently defaulted). Pass bar (seat): with the comaintainer skill in
the session (detected from the transcript by the ambient hook;
`SOVEREIGN_SEAT=1` remains an explicit override only) the same ambient
path carries the anchored records and prints no withheld line.

```bash
scripts/co-mesh-drill.sh d4-check ordinary   # on either machine
scripts/co-mesh-drill.sh d4-check seat       # on either machine
```

Run D1 first — the seat-mode presence proof needs at least one
anchored drill record in the store (the order note's summary line).

---

## F-drill — commons-fluency (UC-F1..F8), the drill that runs itself

The F-cases prove the coordination rails answer with **identity,
receipts, and legible state transitions**, and that the drill itself
needs no operator relay: one start note (carrying the case list, the
side assignment and an epoch start time) is read by both sides' `seat
watch` loops; each side executes its steps on the epoch schedule,
writes every verdict as an anchored note, and `f-assemble` builds the
final table from those notes alone.

**Deploy prerequisite (SEAT-OWNED):** both daemons must run the
post-commons-fluency binaries. A pre-fix daemon reads as could-not-judge
on the affected steps (no `peer_requests` tally, no `convergence`
section, no stamps on notes), never as a failure — the drill names the
missing surface in its evidence.

### The run, end to end (one operator act)

```bash
# ONE side (any seat): write the start note. The peer node tag is the
# canonical node-<16hex> display form (e.g. node-b88252e4325bc377) —
# take it from the peer's /status node_id. WATCH OUT: `svrn mesh
# status` shows the 22-hex ROSTER fragment (e.g. b88252e4325bc377465f51),
# which is NOT the tag; the live run's f-exec refusal came from
# passing that form. Both sides normalize anyway (the f-start side-a
# and f-exec sides accept 16-hex bare, 22-hex, prefixed or not), so
# any of these forms reaches the same canonical tag — use the
# canonical one.
scripts/co-mesh-drill.sh f-start f-$(date +%s) <peer-node-tag>
```

```bash
# BOTH sides (their own sessions, at their own pace — the epoch clock
# inside the start note is the schedule):
scripts/co-mesh-drill.sh f-exec <run-id>
```

`f-exec` first probes that `svrn seat watch --once` surfaces the start
note (the fix-8 mechanism the drill runs from), then walks the schedule:
+5s writer acts (F1 long-TTL claim, F2 short-TTL claim, F3 marker note,
F4 addressed note), +45s reader acts (sighting, receipts, ambient),
+75/+105s the F4 reply round-trips, +135s the F2 tombstone window,
+180s origin receipts + negative + liveness arms + flood gate + wire
forms, +300s the verdict table. The schedule bracket (measured
2026-08-12, seat, under load): claim take ≤92s, attribution ≤252s,
notes batch ~5min (~300s) — so the per-case polls wait 150s (claims)
/ 300s (notes), and the assemble deadline is EPOCH+480s, past the
worst measured bound. A table assembled before the bound closes names
the deadline and the measured bounds in its evidence.

```bash
scripts/co-mesh-drill.sh f-assemble <run-id>   # either side, after EPOCH+480s
```

`f-assemble` reads the run's notes only (start + `DRILL_STEP` notes,
all anchored `order-seat`), applies the verdict rules per case, and
prints the table plus `UC-F8: escalations needed = N` and a
four-verdict summary (PASS / FAIL / could-not-judge / never-ran — a
case with no steps read counts as never-ran, never as done). The exit
code is the verdict: 0 only when every case PASSed; any FAIL,
could-not-judge or never-ran exits non-zero (2) — a caller gating on
the exit code never reads a non-pass as success. Nothing else is
required of the operator; a second run needs only a fresh run id and a
fresh start note.

### Pass bars

- **F1 — identity on claims.** B takes a claim on `drill:<run>:F1-<b8>`
  (TTL 600); A's first sighting of it reads `held` **with B's node id**
  inside ≤92s of starting to look (both directions). A sighting that
  reads `held` without a node is a FAIL; no sighting inside the bound
  is could-not-judge with the link evidence.
- **F2 — the expired state is legible AND durable.** A's F2 claim (TTL
  60) is abandoned by design. B's first read that stops being `held`
  must read `expired — … abandoned N minutes ago` (the tombstone
  render, distinct from `free`), and a second sample ≥60s later (a full
  GC sweep cadence) must STILL read expired+abandoned — the pre-fix
  eviction-collapse reads `free` and is a FAIL. Tombstone window:
  1 hour (`EXPIRED_TOMBSTONE_TTL_SECS`).
- **F3 — receipts.** The origin's marker-note row carries `sent_at`
  (the publish fired, on record) with `received_at` null; the peer's
  row carries `received_at ≥ created_at` (≥ `sent_at`), so the
  round-trip is a bracket computable from the stamps alone. Claims-rail
  receipts: the peer store stamps `received_at` when it applies the
  peer's claim. **Negative arm:** a session-scoped write never fires
  the publish sink, so its origin row reads `sent_at` null — the
  981dd6d8 failure mode is diagnosable from the origin alone. **Liveness
  arm:** `/status` exposes the publish-path convergence age as
  BRACKETS (0-30s / 30s-2m / 2-5m / 5-30m / >30m / never) plus a
  `stale` flag — a point age would be a FAIL.
- **F4 — addressed seat-to-seat coordination.** The addressed note
  (anchored `order-seat`) surfaces in the PEER's ambient when the peer
  session is in the seat (comaintainer skill marker in the transcript,
  detected by the ambient hook; `SOVEREIGN_SEAT=1` as explicit
  override); the reply round-trips with receipts at both ends; the
  round-trip is a measured bracket.
- **F5 (HARD GATE) — the flood guard still holds.** Ordinary ambient:
  zero seat records leaked, and the withheld line NAMES the anchors.
  Seat ambient: the rail records carried, no withheld line. Any
  anchored record in an ordinary session is a FAIL.
- **F6 — the reader can say what it was not shown.** The withheld
  line's anchor names are asserted, not just its presence.
- **F7 — wire forms.** A garbage `X-Node-Id` header lands the request's
  tally in the zero bucket, and the `/status` row names the rejected
  header value AND the expected 32-hex form. The zero-bucket row's
  `node_id` renders `node-0000000000000000`; a row that does not name
  the rejection is a FAIL; no row at all is could-not-judge (pre-fix
  daemon).
- **F8 — the drill ran itself.** `seat watch` surfaced the start note;
  the verdict table was assembled from the notes alone. `UC-F8:
  escalations needed = 0` on both sides is the done-when.

Every F-verdict is printed AND written as an anchored `order-seat`
note (kind decision, content `DRILL_STEP <case> <step> <verdict>
<evidence> run=<run-id>`), so `f-assemble` and any future observer read
the same record. Both sides use per-side label suffixes (first 8 hex of
the node tag) — note content-hash dedupe would collapse identical
markers from the two sides into one note.

### Cleanup and re-running

```bash
scripts/co-mesh-drill.sh cleanup <marker>   # retires the drill's notes, clears the drill log
```

`cleanup` is idempotent and safe to run at any time. The drill is
re-runnable: fresh markers each run (the verbs' examples use
`drill-$(date +%s)`), same pass bars. For an F-run, the marker is the
run id (`cleanup f-<ts>`), which matches the run's start note,
`DRILL_STEP` notes, markers and negative-arm notes.

## Assembling the verdict table

```bash
scripts/co-mesh-drill.sh report side-a.log side-b.log
```

`report` is daemon-free: it reads the relayed `DRILL_STEP` /
`DRILL_TABLE` lines and applies the verdict rules above, including the
D3 cross-machine ALL-row comparison (a machine that fell back to its
LOCAL tally marks the case could-not-judge — its banner is relayed as
evidence).
