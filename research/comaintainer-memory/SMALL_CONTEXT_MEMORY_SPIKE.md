# Small-context memory spike — the comaintainer surface

2026-08-13 · spike-level analysis · scope: the seat's memory loop only
(3 seat sessions, true ground truth from transcript tool results)

## BLUF

The question was: can the seat operate from a small context backed by the
notes rail, with recall delivered by the system instead of by hand? The
measured ceiling says **the rail is a fine memory but the recall loop is
entirely manual, and the automatic injection is nearly dead weight**:

- The hook injection (newest-20 decisions+invariants) carried **0–1 of the
  99 notes** the seat actually pulled in its most recent session. Across all
  three sessions replayed: **1/181**.
- Raw operator-turn-as-query (the "turn-driven recall" idea) retrieved
  **2–3 of 46** topical notes in the best session, **0** in the other two.
- The seat's own manual queries retrieved **everything it needed** — because
  that is what they are *for* — at a cost of **14.5k–51.7k tokens of note
  bodies per session** in tool round-trips.
- A full-body firehose hook (`inject-notes.sh`) is **still registered and
  firing**: ~**8.4k tokens on every user prompt**, no dedupe, and it hits
  ~0 of what the seat needs. The 2026-08-07 rewrite that replaced it with a
  one-line index never removed the registration.

The seat is already doing the cognitive-science thing humans do: a small
working set plus **cue-formulated manual recall**. The system's part is
broken twice — it injects tokens that don't hit, and it makes the seat pay
a round-trip tax for the recall that does. Neither failure is in the store
(5,485 notes, 5,480 embedded, entity graph live) or the engine. Both are in
the *policy*: what gets injected, and what the seat must fetch by hand.

## Method

Sessions replayed (all comaintainer seat sessions on RuggedFox):

| Session | Date | Operator turns | Manual notes calls |
|---|---|---|---|
| `c3a7b73f` | 08-12/13 | 7 | 10 |
| `40b5cc0b` | 08-11 | 9 | 12 |
| `5c8a3275` | 08-12 | 36 | 7 |

Ground truth was parsed from the transcripts' `toolUseResult` blocks — the
note ids the engine actually returned to the seat at the moment of each
query, not a re-query against today's store. Zero temporal skew for the
recall scoring. Each GT note was classified:

- **rail** (59% of GT, 107/181): looked up via `related_to`, a note id, a
  probe tag, or an anchor name (`order-seat`, `directive-log`, …). These are
  *reads of a known address*, not recall — the seat already knows what it
  wants. No retrieval engine can be scored on them.
- **topical** (41%, 74/181): keyword queries ("join key", "release bound",
  "deepseek v4 flash"). These are the recall-servable half, and the target
  of the turn-driven experiment.

For each operator turn with topical GT, two policies were tested against
the live engine (top-10, `include_operational`, `created_at ≤ turn time`
honesty filter): **verbatim** (first 500 chars of the turn) and **short**
(first 12 words). The hook baseline is the injection policy itself:
newest-20 decisions+invariants at session start (seat variant).

## Findings

### F1 — The firehose is live. ~8.4k tok/prompt, ~0% hit. (biggest single lever)

`settings.json` still registers `inject-notes.sh` as a UserPromptSubmit
hook. It prints the newest 20 decision+invariant note bodies **in full,
every prompt, no per-session dedupe** — measured live during this spike:
13 notes, 8,386 tokens per injection. `inject-notes.py` (the 2026-08-07
rewrite) already renders the same query as a one-line index with dedupe;
the `.sh` is the firehose the rewrite was written to kill, still wired.
Its payload carried **1 of 99** GT notes for the freshest session, **0/61
and 0/21** for the other two. In a 36-turn seat session that is ~300k
tokens of hook output for nothing — the context-management problem wearing
an efficiency costume, exactly as the operator's cache-primer warned:
rising injected volume, no attention value.

### F2 — Turn-driven recall does not work naively. 0–6% on topical GT.

Verbatim turn-as-query: union recall 2/46 (`c3a7b73f`), 0/22, 0/6. The
12-word compression did not rescue it (3/46, 0/22, 0/6). Failure mode
observed directly: the turn "let's pick up the frame … sandbox daemon,
join key rotate" surfaced the *frame/split-protocol* note family — because
the turn contains the word "frame" and BM25 term-overlap dominates long
queries. The notes the seat needed (handoff note `09622abd`, join-key
decisions) never ranked. The engine is a good cue-recall machine and a bad
reader of full turns; **something must formulate the cue.** Today that
something is the seat.

### F3 — The seat's manual queries are the recall engine. 10–12 round-trips/session, 14.5k–51.7k tok.

Every session scored pulled its GT through hand-written queries, at a cost
of 14.5k (7 calls), 32.2k (12 calls), 51.7k (10 calls) tokens of returned
note bodies — duplicates across queries included (distinct notes pulled:
21/61/99). The queries show a consistent three-part shape: boot-ritual
anchor reads by kind, identifier lookups (note ids, probe tags), and short
topical cues. The seat *is* the formulation layer, and it works — 100% of
GT by definition. The spike's real question is not "can retrieval replace
the seat" but "can the formulation loop be made structural and cheap."

### F4 — 59% of what the seat pulls is protocol, not recall. Make it a boot block, not a query.

107/181 GT notes are rail reads: anchor sweeps (`order-seat` ×30,
`directive-log` ×25, `related_to=comaintainer-seat`), id lookups, probe-tag
polls. The skill's boot step 1 already prescribes exactly these reads. They
are stable, seat-specific, and small when assembled once — a structural
"seat boot block" (anchor todos + recent decisions + open orders + ledger
stats, ~2–3k tokens, one script, cached per session) replaces N tool
round-trips with a constant-size injection. This is the working-memory-page
idea from the design conversation, concretized: the page exists in the
protocol, but the seat reassembles it by hand every session.

### F5 — One seat session ran 26 turns with zero manual recall.

`3bd86d20` (the 08-12 morning seat, two worker spawns, two landings) made
no notes calls at all — it rode the frame, the hooks, and worker traffic.
Demand for the rail is bursty, not uniform: recall should be demand-driven
and cheap when needed, near-silent when not. A policy that taxes every
prompt (F1) to serve a need that sometimes never arises is backwards.

## Interpretation

The cog-sci map from the design conversation holds up under measurement:

- **The store is a healthy semantic memory.** 5,485 notes (3,152 live),
  5,480 embedded (qwen-embedding-0.6b), 12,137 entities. The engine
  retrieves what it is cued with — the seat's cues hit.
- **The binding loop is manual.** Recall = the seat translating turns into
  cues (F3); consolidation = notes written at the moment (the skill already
  mandates this); the automatic side of the loop (injection) is either
  dead weight (F1) or the wrong shape (F2).
- **The missing layer is cue formulation, not a better index.** Naive
  injection of either turns or recency windows fails for the same reason:
  neither is a cue. The one mechanism that works today is a trained
  operator's short queries.

## Recommendations (ranked; none started — spike only)

1. **Remove the firehose** — drop the `inject-notes.sh` registration from
   `settings.json` (and the file). Pure win: ~8.4k tok/prompt of near-zero-
   hit payload gone, zero capability loss (the `.py` index remains, with
   frame-cited bodies intact). *Proven by F1; no further measurement
   needed.*
   **DONE 2026-08-13** (operator-approved, uncommitted): three
   registrations found and removed (root `settings.json`, plus sibling
   `sovereign/` and `commonwealth/` settings — all three `.sh` copies
   deleted), `SYSTEM_OVERVIEW.md` frame-injection sentence corrected
   (it described the deleted script as the frame injector), `session-boot.sh`
   comment updated. Propagation to peer machines is by commit + pull
   (repo-tracked); machine-local `~/.claude/settings.json` checked on this
   host — no registration. Two leftovers carried into the R2 order: the
   scaffold generator still emits the firehose for every new project
   (`sovereign/crates/sovereign-cli/src/project_init/scaffold.rs:432`), and
   `notes_retrieval_cmd.rs:4` still documents the 10–14KB block.
2. **Structural seat boot block** — one script assembling the rail reads
   the skill's boot step already prescribes (anchor todos first, then
   recent decisions, open orders, ledger stats), injected once per seat
   session, updated in place. Replaces 10–12 manual round-trips (F3/F4)
   with a constant ~2–3k token block. *Probe first: does a fresh seat
   holding only the boot block + frame still find what it needs?*
3. **Formulation-pass A/B (only if small-context becomes an active goal)** —
   a cheap fast-slot pass compressing the turn into 3–5 cues before the
   engine, injected as a one-line top-k index. Parity bar: match the
   seat's manual recall on these same transcripts (currently 0–6% without
   formulation; 100% by construction with the seat). If a 4B pass gets
   within striking distance, the seat's formulation tax is automatable;
   if not, the seat stays the formulation layer and R2 is the whole game.
4. **The real metric is decision parity, not injection size or hit rate** —
   a gate that replays seat sessions with the new policy and asks whether
   the same directives/verdicts emerge. That is the only measurement that
   would justify R3; without it, optimize cost (R1, R2) and stop.

## Honesty section

- The hook∩GT baseline for the two older sessions compares today's
  newest-20 against GT from two days ago — slightly unfair to the hook.
  `c3a7b73f` (ended ~8h before the spike) is near-fair and shows the same
  1/99 shape, so the conclusion does not rest on the skewed pair.
- GT is what the seat *actually pulled*, not everything relevant that
  existed. Turn-query recall of 0–6% is therefore a measure of **parity
  with the seat's manual recall**, not of absolute retrieval quality — the
  engine may well have returned useful notes the seat never asked for.
  (The T1 failure-mode print shows it returned *plausible but wrong-family*
  notes — plausible wrongness is this system's characteristic failure.)
- Three sessions, one fleet, one operator. Spike level: shapes, not
  distributions.
- The 8.4k-token firehose figure is a live measurement of this host's
  current payload (13 notes, non-seat query shape — the `.sh` never passes
  `include_operational`, so seat sessions pay the same dump).

## Repro

`python3 research/comaintainer-memory/spike_replay.py` — census, true-GT
extraction, per-turn replay, hook baseline, all numbers above.
