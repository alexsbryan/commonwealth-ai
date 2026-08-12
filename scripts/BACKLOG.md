# The backlog — how work gets in, and how it gets picked up

Four artifacts, and nothing else. If you are looking for a fifth — a
backlog database, an index, a priority table — it does not exist, and
the "Why there is no heap" section below is the argument for why.

| Artifact | What it is |
|---|---|
| `quality/backlog-ruler.toml` | The value ruler, as versioned data: the axes and their yardsticks, the 1-5 scale, the Blocks rule, the cost table, the item format's key list, and the liveness staleness window. The one copy. |
| `svrn backlog add` | The writer. Scores one item on the resident local model against the ruler, then writes the note. |
| `scripts/co-backlog.py` | The reader. Parses, ranks, renders, and decides what is pullable. Writes no ITEM back. |
| `scripts/co_liveness.py` | The verifier. Asks whether an item still reproduces at HEAD, on the local daemon. |
| `scripts/co-backlog-producer.sh` | The seam for automated producers. Its header is the producer contract. |

The store is the notes store. A backlog item IS a `kind=todo` note
carrying `related_entity=backlog` and a header block. There is no
separate backlog store, and nothing writes a second copy of an item
anywhere.

## Filing something

```
svrn backlog add "<the discovery, in its own words>" \
    --objective "<the standing objective or order it serves>" \
    [--key <producer-id>] [--no-score] [--create]
```

One call to the resident daemon model drafts a short human `Title`, a
Value with the axis named, an Approach derived only from the text you
gave it, and a Cost that follows that Approach. Then the note is written
at an explicitly named store path.

The Title is what the page's card headers say. They used to be 8-hex
note-id prefixes, which name an item without telling anyone what it is;
the ref hash is still on the card, demoted to the metadata line, because
it is what you type to talk about an item. An item with no Title — hand
written, or filed before the key existed — falls back to the first
sentence of its own discovery text, with this system's own provenance
preamble ("Scored against value ruler…", "MIGRATED from note…") stepped
over. Never to the hash.

Three behaviours worth knowing before you wire anything to it:

**A machine score never vets itself.** The item carries
`Scored-by: <model>`, and `co-backlog.py`'s `vet()` treats that line's
presence as disqualifying however complete the rest of the header looks.
An unvetted item renders greyed and cannot be pulled. A person reviewing
the item and clearing that line IS the vetting. This is what makes the
verb safe to point at automated producers: the worst a noisy producer
can do is cost the operator a scroll.

**It refuses rather than guesses.** Daemon down, no chat model resident,
or an unparseable answer, and the verb exits non-zero having filed
nothing. It never lands an unscored item as though it had been scored —
a wrongly-scored item is worse than a missing one, because a missing
item is absent while a wrongly-scored one gets RANKED. `--no-score` is
the deliberate way to file something unscored; it needs no daemon.

**It will not create a store.** A fresh store at the wrong path looks
exactly like a working one, so an absent store is refused by name.
`--create` is how you say you really are starting a new backlog here.

## Reading it

```
scripts/co-backlog.py --open       # the heap, ranked by ROI, unvetted greyed
scripts/co-backlog.py --pull       # the top pullable chunk as an order draft
scripts/co-backlog.py --self-test  # the lane
```

The rendered page prints the ruler it actually loaded, with the file
path and version, so an ordering can be argued with without opening a
file. The footer names the resolved store path and the row count, so a
render against the wrong store is visible rather than plausible.

`--self-test` runs a clean battery and then re-runs the whole battery
under four injected defects, requiring each to redden it. A gate nobody
has watched fail is not a gate, so this one watches itself fail on every
single run and cannot rot into a rubber stamp. It also writes an EDITED
copy of the ruler, re-renders, and requires the page to have followed
the edit — and requires the divergence check to go RED when compared
against the ruler the page was not rendered from.

## Writing a new producer

A producer is anything that notices work and is not a person: a failed
gate, a watcher, a nightly lane, a soak run. Producers do not score,
rank, or decide. They hand the verb text and an identity.

Call `scripts/co-backlog-producer.sh`; do not call the verb directly.
The script exists so every producer inherits the same four rules, whose
full statement is its file header:

1. **Identity is essence, not occurrence.** `--key` names WHAT went
   wrong — a lane name, a check name, an invariant id — never a run id,
   timestamp, PID or counter. A repeat filing under the same key updates
   the item that key already filed, so a gate that fails every night
   leaves one item that keeps getting fresher rather than thirty.
2. **The evidence is the producer's own output.** Pass the artifact you
   already have with `--evidence-file`. Do not summarize it — a producer
   that paraphrases its own log is a producer that can be wrong twice.
3. **Never break your caller.** The script always exits 0. A gate that
   files a backlog item is still a gate: it must not go red because the
   backlog was unreachable, and must not go green because it was.
4. **Say what you filed**, in the same log that recorded the failure.

Verify a new producer's wiring with `CO_BACKLOG_PRODUCER=dry`, which
prints exactly what would be filed and writes nothing.
`CO_BACKLOG_PRODUCER=0` disables every producer at once.

The worked example is `file_backlog_candidate()` in
`scripts/sovereign-ci-bench.sh`: a failed HARD lane files one candidate
keyed `ci-bench:<lane>`, with the lane's own output as the evidence. It
does not fire under `--update-baseline`, where "regressed" means "no
baseline yet" — a setup gap, not work.

## Why there is no heap

The backlog honours heap SEMANTICS — O(1) insert at any time, the top
item always current at read, pull removes from contention — and
implements them by DERIVING AT READ. Insert is a note append. Ordering
is computed fresh at every `--open` and `--pull` by sorting the live
items.

This is deliberate, and it buys three things:

- The notes store stays the single source of truth. There is nothing
  cached, so there is nothing to invalidate and nothing that can be
  stale in a way a reader cannot see.
- Editing the ruler re-scores the whole backlog for free. That is why
  the ruler can be data at all.
- Out-of-band priority mutations — an operator editing a Value, a
  reviewer clearing a `Scored-by:` stamp — are just fields at read time,
  rather than remembered sift operations somebody has to replay.

A maintained or materialized heap is REJECTED at this scale. If `n`
reaches thousands or reads go hot, the escalation is `ORDER BY` in the
store's own SQLite — the database becomes the priority queue, and the
verb and the item format do not change. Do not build that before the
numbers ask for it.

## The item format

The header block is the leading run of lines up to the first blank line.
Recognized keys are `quality/backlog-ruler.toml`'s `[format]
header_keys` and nothing else — an unrecognized key makes the item
malformed, and the page says so in the footer with the note id and the
offending text. The key list lives in the ruler rather than in either
program because the writer (Rust) and the reader (Python) are in
different languages, and a key list written twice drifts the moment
either side gains a field.

```
Objective:    what standing objective, initiative or order it serves
Value:        <1-5> — one falsifiable line, naming the axis
Cost:         <S|M|L> (session-chunks)
Approach:     1-3 sentences: what gets built, which EXISTING surface it
              builds on, and why that makes the Cost credible. Or
              "unknown — needs a design pass", which is a first-class
              answer and forces the item unvetted.
Chunks-with:  note ids, or none
Blocks:       order/step (optional) — the item inherits the value of
              what it blocks
Done-when:    the falsifiable completion condition (optional)
Evidence:     the citation that makes the above checkable (optional)
Producer:     what filed it (producers only)
Scored-by:    the model that drafted the score (producers only) — its
              presence keeps the item unvetted
Key:          producer identity (producers only) — a repeat filing
              under the same key updates this item
```

An item is VETTED, and therefore pullable, only when its header parses
clean AND it carries a non-empty `Done-when:` AND a non-empty
`Evidence:` AND an `Approach:` that is not "unknown" AND it carries no
`Scored-by:` stamp AND it is not already verified closed (below). Prose
is never sniffed for an implied done-when. Vetting is an act someone
performs, not a shape the parser infers.

## Liveness — vetted has never meant still true

Every condition in the vetting rule above is a property of the ITEM. None
of them is a property of the CODE. So an item stayed vetted, ranked and
pullable long after the defect it named was closed: on 2026-08-12 three of
the top four vetted items were already fixed, two of them by a commit that
landed three days BEFORE the migration filed them (seat finding
`14e2bcb3`). A worker spawned on any of them would have gone hunting a bug
that no longer exists.

```
scripts/co_liveness.py verify <id> [<id>...]   # judge named items
scripts/co_liveness.py verify --all            # the whole heap
scripts/co_liveness.py ledger                  # what is recorded
```

Each judgment is one call to the resident local model, given the item plus
current-state probes read from the item's own citations at HEAD — the
cited lines as they read today, whether the named symbols still exist,
whether the quoted strings are still there. Three outcomes: `alive`,
`dead` with a citation, `could-not-judge` naming what was missing.
`could-not-judge` is first class and is reported, never quietly counted
as alive.

**Nothing auto-retires.** A `dead` verdict is a proposal: the item stays
on the heap, greyed, saying it is proposed for retirement and citing why.
The seat retires it, with that citation as the pointer. An automated
retire is how a wrong judgment becomes invisible.

**Verification is level-triggered, and that is the whole design.** The
only question asked is "does this reproduce at HEAD?", whose answer
depends on nothing but the tree in front of you. There is no mark, no
cursor, and no commit range — so skipping this for a month costs exactly
one run to recover, and that run does the same work a run today would.
Compare `co-sweep.sh`, which keeps a high-water mark and a 20-commit cap
and has been unable to catch up for six nights: a catch-up cost that grows
while you sleep is a system that eventually gets skipped forever.

**The heap shows liveness age; it never blocks on it.** Every card carries
a liveness line — `Never verified against HEAD`, `Verified alive 3d ago`,
or `STALE, past the 14d window set in the ruler`. Staleness alone never
stops a pull. `--pull` re-verifies the handful of items it is about to
hand out, at the moment it hands them out, and states the result in the
draft ("re-verified just now: alive"). If the engine is down, the item is
still handed out with `could-not-judge` stated. A gate that stops the
operator because a nightly did not run is the failure mode this design
exists to avoid.

The cost of a pull is therefore bounded by what is being pulled, never by
how long nobody ran anything. `--self-test`'s resilience battery is the
proof: an identical pull against a ledger backdated 30 days and one
backdated 300 days costs the same number of verifications and reaches the
same verdicts, and a pull with the ledger DELETED still works.

Two writers, one accessor. A person who checks an item by hand stamps
`Verified-at: <date>` in its header; the machine pass appends to the
seat's existing judgment log (`~/.sovereign/comaintainer/verdicts.jsonl`,
`kind="liveness"`). `co-backlog.py`'s `liveness_of()` is the only function
that reads either, and it takes whichever is newer. There is no new store:
an absent or deleted log means "nothing has been verified", which is
honest and costs one pull to recover.

**The nightly sweep helps, and nothing depends on it.** `co-review.sh`
already model-reads every landed commit, so it additionally asks whether
that commit closes an open item and files candidates into the same log for
the seat to dispose of. A lexical prefilter caps this at three items per
commit, so the cost does not grow with the backlog. It is an accelerator:
it saves the level-triggered pass work when it runs and costs nothing when
it does not. Set `CO_CLOSURE_CANDIDATES=0` to turn it off; the heap stays
correct.
