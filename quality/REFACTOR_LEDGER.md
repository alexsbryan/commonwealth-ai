# The refactor ledger — how work is stored, handed out, and proven closed

**This file is the execution model for the factory specified in
[`REFACTOR_FACTORY.md`](./REFACTOR_FACTORY.md).** That document specifies the
engine — the spec format, the six stages, the wire differ, the bar. This one
specifies how a series of agents actually pulls record after record until the
work is gone. The two are disjoint by construction: anything about *what a
refactor is* belongs there, anything about *how work is dispatched and proven*
belongs here. The factory's "What gets built, in order" section defers to the
build order at the bottom of this file, so there is one build order and not two.

**This spec dies when `svrn refactor status` prints its own burn-down.** The
tool is the artifact.

## Why this exists

Every instrument in this program is query-shaped. `svrn code converge noun
Verdict` computes a dossier live, prints it, and the dossier dies with the
session. `refactor plan` seeds an edit, runs `cargo check`, classifies, restores
the tree, prints. Nothing accumulates, so every session re-derives from zero and
adjudicates about ten things before it runs out of room.

That is the operator's own cost model, applied to the agents rather than to the
code:

> An agent writes new code because new is local, certain, and needs no
> discovery; reuse needs knowing something exists and trusting it fits.

Discovery is expensive and uncached, so it is paid again every session. **The
program has the disease it exists to cure.** Two consequences follow, and both
are why the target keeps receding:

- The unit of work is a *schedule* (rungs, waves, campaigns), not an
  *inventory*. A schedule says what is next; it can never say when it is done.
- The metrics exist *because* the inventory is missing. Miss-rate,
  redundancy count and residue-per-item are proxies you need only when the real
  number is uncountable. With a complete inventory the number is open sites, and
  it is exact.

## The end user

The far end is a person asking Sovereign a question and getting an answer that
carries its evidence. Today that rests on 147 correct decisions and an untyped
`metadata["provenance"]` channel; the refactor moves it onto `Evidence` having
one constructor. That is the *why* and it does not change.

The end user of *this* system is the worker agent pulling record N. Everything
below is designed backward from that agent's session.

### The terminal scene

`svrn refactor status` prints every destination converged and zero open
holdings, and `cargo xtask target-arch` regenerates `TARGET_ARCHITECTURE.md`
from the register and the graph with no `target` markers left in the migration
half. That is the target document's own terminal test. Nobody writes a report;
the tool prints one.

### A typical agent, mid-program

Two commands. This scene is the requirement; the rest of this document is what
must be true for it to be boring.

```
$ svrn refactor next --lease 2h
order written: .sovereign/features/rf-b0142/order.md
  destination  kernel_types::Origin
  holdings     23 (one class: metadata["provenance"] writer -> Origin field)
  files        6, none leased by a peer
  budget       ~0.6 session-chunk (basis: 38 sites/chunk, measured rf-4)
  detector     provenance-metadata-writer @ settings 4f21ab
  close with   svrn refactor close rf-b0142
```

It reads one file. The order is `work-order/v1` — the format already on disk
under `.sovereign/features/` — and it is **self-contained**: destination and its
totality rule, the sites freshly resolved with current text, the
`(expected, found) -> edit` rule for the class, three worked examples of that
same edit already applied with their real diffs, the close command, and one line
on what to do when unsure.

No order may say "see `REFACTOR_FACTORY.md` §4". Every doc reference is a
discovery cost the agent will either pay or silently skip, and both are failures.

It edits. Then:

```
$ svrn refactor close rf-b0142
  re-ran detector provenance-metadata-writer over 23 holdings
  negative control fired (grounding/mod.rs:412 control site) — detector is live
  closed      21  detector no longer fires
  still open   1  grounding/mod.rs:412 — still matches after edit
  escalated    1  streaming.rs:88 — no rule for (Origin, Cow<str>)
  wire diff    clean (json, sqlite)
  targeted     cargo test -p sovereign-core grounding:: — 47 passed
  lease released; 2 holdings returned to pool
```

Then it runs `next` again, or it dies, and neither outcome corrupts anything.

## What was deleted, and why each was a liability

An earlier draft of this design had five tables, a sqlite store, and a stored
work order. All of it came out.

- **The holdings database.** 3–5k holdings is ~1.2MB. Joining that in memory
  costs microseconds against a workload dominated by seconds of detector
  scanning. A schema buys nothing and adds migrations, corruption and a
  standing "is the ledger stale" question.
- **`work_order` as stored state.** An order that stores its own holding list
  can disagree with the holdings. That is a consistency bug with no upside. An
  order is now a *view* — the holdings under one lease — and the `order.md` on
  disk is a rendered artifact, disposable and regenerable.
- **`label_run`.** It was a join away from data that belongs on the row.
  Invalidating a bad labelling pass is `grep -v` plus a re-run.
- **The `detector` table.** Five detectors is a closed set, so it is an enum in
  code with its settings digest as a const (ARCH §2). A table lets the registry
  drift from the instruments it names.
- **Leases as new state.** `declare_scope` / `work_in_flight` already does
  TTL'd, file-scoped, cross-mesh claims (ARCH §19 — the inventory outranks the
  plan). Reuse it.
- **`closed` as a state.** See below. This is the important one.

## The two sources of truth

Both in git, both human-readable, both diffable in review.

**Destinations.** `quality/CONCEPTS.toml` for nouns — it already exists and is
already ratcheted by `cargo xtask concept-gate` — plus `quality/refactors/*.toml`
for the specs, which also already exist. Nothing new is minted here.

**Judgements.** `quality/refactors/labels/<detector>.jsonl`, append-only, last
line wins per key. One line per site, holding only what a detector cannot
re-derive:

```json
{"key":"provenance-metadata-writer/sovereign_core::grounding::assemble/provenance",
 "dest":"kernel_types::Origin","disp":"converge","why":"writes the untyped channel §7",
 "by":"seat","at":"2026-08-23"}
```

`disp` is the disposition set already defined by the register: `converge` ·
`distinct` · `idiom` · `external-mirror` · `layered` · `leave` · `UNSURE`.

Everything else is derived live. **Locations are never stored** — spans rot
within a day of anyone else committing, and a stale span sends an agent to edit
the wrong line. Symbols are stable; coordinates are not.

## Closure is an absence, not a record

**A holding is open if and only if its detector still fires on it.** When the
detector stops firing, the holding is simply not in today's join.

There is no verb that marks work done. Nothing writes progress. The burn-down is
a measurement taken fresh on every invocation, so it cannot be forged by an
agent that got tired, and there is no reconciliation step after a crash because
no progress was ever written.

This is ARCH principle 8 — one decider, one name. **The detector that opens a
holding is the only thing that may close it.** The five detectors already exist
and are already named in one place (`refactor_cmd/schedule.rs`): the string-field
census, `shape_census`, `converge::census`, `dry_report`, and the `hpr-cost.py`
arg-loop rule, at frozen settings.

### The way this fails, named before it happens

A detector can stop firing for the wrong reason. An agent renames a field, the
pattern no longer matches, the duplication is still there, and closure is fake
and green — the well-formed exit-0 wrong result ARCH §18 exists to catch.

**Every `close` run therefore carries a negative control**: it must also fire on
a known-unconverged site in the same pass. A detector that finds nothing
anywhere is `could-not-judge`, never `passed` (ARCH §18.1 — a check with no
failing input you can name is not a check). The control site is a const beside
the detector, so a detector without one does not compile.

## The seven interlocks

1. **Nothing writes progress.** No verb marks a holding done; the count can only
   be measured.
2. **Every detector run carries a negative control.** No control fired, no
   closures — `could-not-judge`, not `passed`.
3. **State is disposable.** Nothing irreplaceable lives outside git. There is no
   store to back up, rebuild, or distrust.
4. **A dead agent costs nothing.** No progress was written, so nothing unwinds.
   The claim TTLs out and the sites return to the pool.
5. **No two agents in one file.** The claim covers the order's resolved file
   set, not the destination name; `next` refuses if any file is held.
6. **`close` proves before it reports.** Detector re-run, negative control, wire
   differ across declared surfaces, targeted tests. Any failure exits non-zero
   and closes nothing.
7. **Detectors are frozen.** The settings digest is printed on every run and
   stamped into the order. Changing it is a visible diff that restarts the
   series — the same discipline the miss-rate bar already carries.

## Data structures — measured, not assumed

Three structures, and the measurements that chose them (taken 2026-08-23 against
`scip_graph.db`, 320,619 symbols / 1,646,038 refs).

**Judgements** — append-only jsonl, loaded into a hash map keyed by
`(detector, symbol, token)`. Small enough that indexing it optimises the wrong
end: 5k rows joined in memory is microseconds, against detector sweeps costing
seconds.

**Locations** — queried from the SCIP graph, never stored. The graph is already
indexed on `symbols(name)` (0.007s for a name lookup) but **was missing an index
on `refs(file_path)`**, which is exactly what a file-scoped `close` needs:

| query | plan | time |
|---|---|---|
| `refs where file_path = ?`, no index | `SCAN refs` | 3.4s warm / 8.0s cold |
| same, with `idx_refs_file` | `SEARCH refs USING INDEX` | 0.004s |
| index build, one time | — | 0.97s |

~850x. **It is not on the critical path after all** — the L1 falsification
above means detectors compute their whole population and never issue a
file-scoped ref query, so this gain is real but unclaimed by the ledger. It is
still worth the two lines (`refs_in_file` is a genuinely missing primitive —
`ScipGraph` has `symbols_in_file` and no refs equivalent), and it costs no
`SCHEMA_VERSION` bump because `CREATE INDEX IF NOT EXISTS` is idempotent. It
belongs beside the existing three at
`corpus-engine-scip/src/scip_graph.rs:656`. (An ad-hoc copy was created on this
host during measurement; the indexer rebuilds the db, so it vanishes until the
schema block carries it.)

**Detector output** — a content-hash cache keyed by `(detector_id, file,
file_hash)`, reusing the partition already implemented in
`enrichment/code_intel/mod.rs:353`: hash matches, reuse; hash differs,
recompute. Between runs only a handful of files change, so this turns `status`
from a ~5–15s workspace sweep into a pass over changed files. That matters
because a 15s status gets run once a day and a 0.2s status gets run every time
anyone wonders. The cache is **disposable** — delete it and everything still
works, slower — which is what keeps interlock 3 true.

`shape_census` (`corpus-engine-scip/src/shape.rs:355`) was checked and left
alone: it already builds an inverted posting list from field-key to types, caps
posting length at `MAX_SEED_POSTINGS`, and scores only cross-crate pairs sharing
a rare key. That is the right algorithm.

## The surface

Four verbs.

```
svrn code refactor status         # open holdings per destination. the one number.
svrn code refactor label ...      # append one judgement
svrn code refactor next           # join, lock files, render order.md
svrn code refactor close <order>  # prove, report, release
```

**The verb hangs off `code`.** Earlier drafts of this file wrote
`svrn refactor`; the dispatch is `code_cmd.rs` → `refactor_cmd`, so the real
spelling is `svrn code refactor`.

`status` and `next` are seconds for the graph-backed detectors. For rustc-backed
newtype destinations discovery is a `cargo check` — minutes, and it is the
truth-teller, so that is the right place to spend it.

Worked examples come from `git log --grep`, keyed off a
`Refactor-Rule: <detector>/<dest>` trailer on each landing commit. Nothing is
stored, and record N+1 gets more persuasive than record N for free — which is
the trust half of the cost model, paid down by work already done.

## Scale

The candidate population is not the 218,099 first-party production symbols in
the graph. It is what the five detectors surface, from the factory's work table:
~2,100 field declarations, 282 types in 112 shape groups, 247 duplicate names,
517 behaviour groups, ~144 arg loops. **Order 3,000–5,000 holdings.**
Deterministic bucketing handles most; the model sees the ambiguous remainder.

## Pre-registration

Written before the tool exists so the verdict cannot be fitted afterwards.

**L1 — DETECTORS SCOPE.** ~~Every one of the five can be re-run against a named
file set rather than the workspace.~~ **FALSIFIED 2026-08-24, during the build,
before a line of the ledger was written.** Two of the five cannot, and the
reason is not cost: `converge::census` computes "defined as a type in more than
one CRATE" and `shape_census` weights every match by IDF over the population
with an absolute `rare_df` gate. Narrowing their INPUT to six files does not
make them cheaper — it makes them WRONG, because six files in one crate yield
zero cross-crate collisions and zero cross-crate shape pairs. The detector
would stop firing for the wrong reason, which is the fake closure this whole
mechanism exists to prevent, arriving through the front door.

**Scoping is therefore a POST-FILTER, never a narrowed input.** Each detector
computes its whole population; the file set filters the RESULT. Exact, because
`TypeDef.file` and `ShapeSide.file` both carry the file.

**L1' — CLOSURE IS EXACT AND AFFORDABLE.** The sites reported for a file set
equal the whole-population run filtered to that set, and one `close` costs
under 30s.
*Falsified if:* a post-filtered result disagrees with the whole run, or close
exceeds the budget. Half-measured 2026-08-24: exactness held on the first live
close (4 of 6 sites converted, exactly 4 closed); affordability held for the
four cheap detectors and FAILED for the behaviour detector, whose near tier is
O(n²) over 24,823 reps and measured 156s. That detector is now opt-in behind
`--all` and renders as SKIPPED, never omitted.

**L2 — THE ORDER IS SUFFICIENT.** An agent burning a record makes no exploratory
tool calls outside the order's file set.
*Predict:* median exploratory reads per order = 0.
*Falsified if:* the median is above zero. **This is the whole thesis — it is the
cost model inverted, measured. If L2 fails the order is not carrying enough and
no amount of ledger tidiness will fix it.**

**L3 — THE CONTROL CATCHES FAKE CLOSURE.** Inject a known-unconverged site; the
negative control fires and the run refuses.
*Falsified if:* any detector reports closures with a silent control.

**L4 — THE CACHE EARNS ITS KEEP.** Warm `status` is at least 10x cold.
*Falsified if:* under 3x. Then per-file caching does not fit the detectors'
access pattern and the sweep should stay uncached rather than carry state for
nothing.

## Residual risk

Orphaned labels. A site edited but not converged can shift its key, and the
judgement stops joining. It is harmless — the site reappears as unlabelled — but
it is silent, so `status` reports "N labels no longer join" as a standing health
line. Absence is reported, never defaulted (ARCH §18.3).

## What this does not claim

The factory's own pre-registration puts roughly 40% of `TARGET_ARCHITECTURE.md`
within reach of migration; the rest is design work (minting abstractions that do
not exist) and generator work (xtask, registries). The ledger makes the
migration half mechanical. It does not invent `Capability<T>` or the measurement
fingerprint.

What it does add is that those show up as `destination` rows with `blocked_by`
set, so "someone must design this before anything burns" is a visible row in the
same view rather than a thing rediscovered in month three.

## Build order

0. **`idx_refs_file`** in the `init_schema` batch. Two lines, measured 850x —
   a real gain, but a cleanup rather than the foundation (see above).
1. **The label format, the five detectors behind the enum with their negative
   controls, and `status`.** Ships a populated burn-down to look at.
2. **`next` / `close`** — the lease via `declare_scope`, the order renderer, the
   proof chain. Gated behind the wire differ (rf-2), which must exist before
   anything is applied unproven.
3. **`label`** — the labelling pass on the code-intel driver, `UNSURE` routed to
   the seat, then a hand-adjudicated precision sample before the ledger is
   trusted. That sample is the one number kept, once, as an entry gate.
