# The test ledger — which failures are covered, and how that is proven

**This file is the execution model for a functional-coverage inventory drawn
from the working notes.** `quality/conformance-specs.toml` is its sibling and
its opposite: that registry starts from `research/clean-room/REQUIREMENTS.md`
and asks *does a test exist for this clause*. This one starts from the 402 live
invariants and 53 attempts in the notes store and asks *does a test exist for
this failure*. The two are disjoint by construction. Anything about what the
spec **promises** belongs there; anything about what the system **has actually
done wrong** belongs here.

**This spec dies when `svrn test ledger` prints its own burn-down.** The tool is
the artifact.

## Why this exists

The store at `~/.svrnmesh/notes.db` holds 10,614 notes spanning 2026-04-17 to
2026-09-02. After retirement and tombstones: 402 invariants, 53 attempts, 270
reflections. The invariants average 2,451 characters — they are not reminders,
they are postmortems, each with a dated headline, a mechanism, and a
measurement. 343 of them carry the `files` they concern and 299 carry `symbols`,
so the join from a failure to its site is already in the data.

Nothing in `scripts/` or `quality/` reads any of it.

Every conformance instrument in this program starts from the clean-room spec —
625 clauses written before most of these failures happened. So the program has
been enforcing what the system *should* do while holding a dated record of what
it *actually broke doing*, and never joining the two. ARCH §18.1 says a check
with no failing input you can name is not a check. **An invariant note is a
named failing input.** That is the whole argument for this ledger.

Two consequences follow, and both are why coverage keeps reading better than it
is:

- The unit of work has been a *clause*, which is a promise. A promise can be
  satisfied by a test that asserts the words and exercises nothing.
- A failure has a witness. It happened, on a date, to somebody, with a
  measurement attached. There is no arguing about whether it matters.

## The end user

The far end is a person asking Sovereign a question, ingesting a corpus, or
joining a mesh, and not hitting a thing this program has already hit once and
written down. Every record below traces to a specific incident in which that
person — usually the operator, sometimes a peer node — was misled, blocked, or
silently served a wrong result.

The end user of *this* system is the worker agent writing test N. Everything
below is designed backward from that agent's session.

### The terminal scene

`svrn test ledger` prints every record's mutation CAUGHT and zero survivors, and
`scripts/sabotage.py --ledger` re-derives that table from the tree in one pass.
Nobody writes a report; the tool prints one.

### A typical agent, mid-program

One command, one file read, then edits.

```
$ svrn test ledger next --lease 2h
order written: .sovereign/features/tl-GR-07/order.md
  failure      RAPTOR late-inject summaries never reach the deep prompt
  witnessed    note 3035f3a4, 2026-08-10, measured via retrieval_audit
  surface      rust-functional  sovereign-core
  target       runtime/retrieval/mod.rs::cap_and_reserve
  observable   a summary injected after step 13 appears in prompt_admission
  mutation     drop the post-cap reserve pass; this test must redden
  budget       ~0.3 session-chunk
  close with   svrn test ledger close tl-GR-07
```

The order is self-contained. **No record may say "see note 3035f3a4 for what to
assert."** The note is 2,400 characters and the agent will either pay that read
or skip it, and both are failures. `failure`, `observable` and `mutation`
together are the whole brief.

## Closure is an absence, not a record

**A record is open if and only if its mutation still survives.** Apply the named
edit, run the suite; if nothing goes red, the failure is uncovered and the
record is in today's join. When something goes red, the record is simply not.

There is no verb that marks a test written. Nothing writes progress. The
burn-down is measured fresh on every invocation, so it cannot be forged by an
agent that got tired, and there is no reconciliation step after a crash because
no progress was ever written.

This is the store's own hardest-won lesson, and it was learned three separate
times on the conformance side. Note `451ec2a8`: the GR family's zero-candidate
yield was an instrument defect, not absent coverage — same 250 pairs, generator
v1 to v2, one candidate became 62. Note `fc1b3443`: a CAUGHT verdict is not a
new requirement. Note `cf566968`: an adversarial re-adjudication of 13
hand-written claims returned 6 genuine and 6 overclaims. **A model's opinion
that coverage is absent is an opinion. Only a survived mutation is evidence.**

### The way this fails, named before it happens

A mutation can redden the suite for the wrong reason — it breaks the build, or
it trips an unrelated assertion three crates away, and the record closes while
the failure stays uncovered.

**Every close therefore names which test went red, and that test must be the one
the record names.** A mutation that reddens a test the record does not name is
`could-not-judge`, never `covered`. This is the same discipline the refactor
ledger's negative control carries, pointed the other way: there, a detector that
finds nothing anywhere proves nothing; here, a mutation that breaks everything
proves nothing.

Two known ways to get a false red, both already witnessed:

- **The build failed.** `scripts/sovereign-test.sh` reports a build-script
  failure as a failure since 2026-07-28, and disk exhaustion produced an exit-0
  `cargo build` with an 864-byte binary on 2026-08-20 (note `2fa8bcd5`). Read
  the pass/fail counts, not the exit alone.
- **Two runs collided.** Concurrent nextest overwrites the shared JUnit report
  and the counts are not yours — exit 5. Never mutate under a peer's run.

### And one way to get a false GREEN, which is worse

A false red costs you a re-run. A false green closes nothing and leaves the
record open, which is the answer you already expected — so nothing prompts you
to look.

**Cargo fingerprints a source file on MTIME.** A batch runner that does
restore → mutate → build inside one second lets cargo reuse the previous
build's artifact, so the suite runs WITHOUT the mutation, every test passes,
and the runner reads that as *survived* — i.e. "the failure is uncovered".
Measured 2026-09-02 on a 16-mutation sweep: FE-74 and FE-75 both read 465
pass / 0 fail, and both reddened the test their record names when re-run one at
a time (459/6 and 458/7). Two false survivors in sixteen. The tell was that
FE-74's run listed the seven failures belonging to FE-75's mutant — a red set
that belongs to the mutation before it.

The fix in a runner is one line after the write:
`os.utime(f, (time.time() + 2, time.time() + 2))`. Stamping the future is what
cargo cannot miss.

**The rule this generalises to** (§18.4, validate the instrument before the
result): a mutation harness's characteristic failure is a false SURVIVOR, and a
false survivor is invisible because it agrees with the null hypothesis. A sweep
reporting zero survivors needs no check. A sweep reporting survivors has not yet
distinguished *the test does not catch this* from *the test never saw this* —
re-run at least one survivor ALONE before believing it.

Two more instrument faults from that same sweep, both worth checking for in any
batch runner:

- `scripts/sovereign-test.sh` prints `pass: 0 fail: 0` when it cannot attribute
  the JUnit report to its own run — real `FAIL` lines on stdout, no counts.
  **Gate on the counts, not on the presence of FAIL lines.**
- An inline `python3 -c` inside the loop died on a macOS codesign policy error
  (`_posixsubprocess` load denied). The package variable came back empty and
  `--package ""` ran the entire workspace for that entry. Inside a loop whose
  failure mode is silent, use an explicit table in the shell, not a subprocess.

## The two sources of truth

Both in git, both human-readable, both diffable in review.

**Records.** `quality/tests/backlog.toml`. One entry per failure, holding only
what cannot be re-derived: which incident it came from, what to assert, and the
mutation that adjudicates it.

**Provenance.** The `note` field is a note id in `~/.svrnmesh/notes.db`, and it
is mandatory. **A record with no witnessed failure is not admissible here** —
that record belongs in `quality/conformance-specs.toml`, which is the registry
for promises. This ledger only encodes things that went wrong.

Nothing else is stored. In particular:

- **No `covered` column.** It rots the moment anyone edits a test, and asserting
  it is exactly the opinion-versus-mutation trap above.
- **No line numbers.** Symbols are stable; coordinates are not, and a stale span
  sends an agent to the wrong line. `target` names a file and a symbol.
- **No priority score.** `tier` and `class` are enough. A computed score invites
  tuning, and the operator's ranking is already stated: tier 1 is mesh,
  inference, setup, desktop, ingest, enrich, bench, plus grounding and
  reasoning.
- **No second test-to-requirement map.** If a record's test also settles a
  clause, the `covers:` tag on the test says so and
  `quality/conformance-specs.toml` picks it up. One decider, one name.

## The seven interlocks

1. **Nothing writes progress.** No verb marks a record done; the count can only
   be measured.
2. **Every record carries its mutation.** A record without one does not parse,
   because a test with no named failing input is not a check (ARCH §18.1).
3. **Provenance is mandatory.** `note` plus `found` date. No witnessed failure,
   no record.

   **Amended 2026-09-02.** A dated MEASUREMENT taken with a named instrument is
   also a witness. `UI-09`..`UI-18` record a command surface no automated path
   reaches, which is an absence of evidence rather than an observed failure —
   the rule as first written would have refused them. It refuses them for the
   right reason (a promise copied out of a spec is not damage) and for the
   wrong one (an empirical fact about this tree is not a promise). So: a record
   is admissible when its `note` carries either an incident or a measurement,
   and a measurement-witnessed record must name the instrument and the run in
   its `failure` field, so a reader can re-take it. Note `43199a50` is the
   first, and it also carries the caveat those ten records depend on — the
   synthetic tier measures FRONTEND attempt reach through a Tauri shim, not
   backend dispatch.
4. **The observable is user- or wire-visible.** A record asserting on an
   internal field the subject supplies or echoes back is refused — that is the
   guard-on-an-echo smell, and it passes while proving nothing.
5. **Close names the test that reddened.** A mutation that reddens something
   else is `could-not-judge`.
6. **One record, one failure.** A note describing three breaks yields three
   records. `5a952f09` yields three; `a2ab4a23` yields three.
7. **State is disposable.** Delete the ledger and re-derive it from the store;
   nothing irreplaceable lives outside git.

## The ten classes

Named from the data, not invented. The class is the reusable part — an agent
who has closed one `silent-substitute` record knows the shape of the next.

| class | the shape | why it survives testing |
|---|---|---|
| `silent-substitute` | an absence rendered as a result | the value is well-formed, so every assertion on shape passes (ARCH §18.3) |
| `inert-guard` | a guard that shipped unable to fire | the guard's own unit test constructs the state the guard never sees |
| `config-lie` | the surface reports something other than what ran | the reporter and the doer are two deciders and only one is tested |
| `command-seam` | every command works, the composition does not | unit tests test commands; nothing tests the join |
| `dark-capability` | code constructed by nothing outside its own tests | the tests pass; the call site does not exist |
| `instrument-defect` | the measuring device is wrong | the device reports a number, and a number reads as a measurement |
| `ambient-dependence` | behaviour turns on env or a cargo feature | the test environment is not the deployed one |
| `resource-ceiling` | correct until a bound is crossed, then wrong | the bound is never crossed in a test |
| `shared-state` | two writers, one store, no owner | each writer's test owns the store alone |
| `unreached-surface` | a shipped entry point no automated path invokes | nothing is known either way — the absence of a failing test reads exactly like the absence of a failure |

## The one queue

There are two registries and there is **one queue**. `quality/tests/backlog.toml`
holds what the system has broken; `quality/conformance-specs.toml` holds what
the spec promises. Both are settled by the same act — run the mutation, watch a
named test redden — so both belong in one burndown, and a worker should never
have to choose which list to pull from.

**The queue is a VIEW. It is not a third file.** This is the refactor ledger's
own deleted liability, applied here: a stored queue can disagree with the
registries it summarises, and that is a consistency bug with no upside.
`scripts/test-queue.py` renders it and writes nothing. Delete the script and the
work is still fully described by the two registries.

### Four kinds of work

| kind | what it is | cost | count |
|---|---|---|---|
| `tag` | a test already asserts this clause; add the `covers:` line, regenerate, then earn it with a mutation | minutes, plus the mutation | 75 |
| `write` | no test asserts this failure; write one | a session-chunk fraction | 111 |
| `blocked` | the test exists and passes, and there is no route for its claim to land | needs a route built first | 4 |
| `decide` | the spec and the code deliberately disagree | operator, not agent | 1 |

**191 open items across 126 files.**

`tag` is cheap and it is not free. Six of thirteen previously hand-written
claims came back OVERCLAIM under adversarial re-adjudication (note `cf566968`),
so a tag without its mutation is an assertion about coverage made by reading,
which is the one judgement this program has repeatedly got wrong. A tag is done
when its mutation has reddened the test it names.

The four `blocked` items are `UI-9` and `UI-10` (vitest — `conformance-tags.mjs`
scans Playwright specs only, and the desktop vitest config emits no JUnit for
`svrn conformance` to join), `EV-25` (python) and `EV-33` (shell). Their tests
exist and pass. Building the vitest route is a reporter plus roughly forty lines
of scanner; it is machinery, and it should be taken deliberately or not at all.

### The unit of work is a file

Not an item. Interlock 5 already forbids two agents in one file, so a file is
what an order can claim — and the join pays for itself: **18 of the 111 notes
records share a target file with an unsettled clause**, across 9 files. One
order in `sovereign-mesh/src/daemon.rs` closes four tags and two writes. One in
`commonwealth-core/src/mesh.rs` closes six tags and one write. That is the whole
reason the two registries are worth queueing together rather than separately.

### The order

1. **Lowest tier in the file.** Tier 1 is the operator's ranking — mesh,
   inference, setup, desktop, ingest, enrich, bench, grounding, reasoning. Only
   the `CI` family is tier 3, and it is the best-covered area already.
2. **Most items in the file.** A file carrying nine items is one claim, one
   context load, and nine closures.
3. **Tag-heavy before write-heavy.** Within an equal count, the file that
   converts existing work into proven coverage first.
4. **Id, so the queue is stable across runs.**

Nothing in that order is a coverage verdict. An item is open because nobody has
run its mutation, not because anyone judged it uncovered.

### What is deliberately not in the queue

- **40 requirements already carry a minted claim.** They are not work. They are
  also not proven — a claim is a tag plus an `asserts` count, and only the ones
  whose mutation has been watched are evidence. Re-adjudicating them is a
  separate pass and it belongs behind the open items, not in front of them.
- **33 requirements are `review`-class** — no automated check can settle them.
  They are never counted covered and never counted failing.
- **8 are `model`-class** and need live weights.

### The remainder, stated rather than padded

**505 in-scope requirements have never been surveyed** — not claimed, not in the
specs file, never looked at. 464 of them are mechanically settleable (258 cli,
202 structural, 4 desktop). By family the mass is `FE` 101, `RT` 69, `X` 60,
`CI` 48, `GR` 31.

Those are NOT queue rows, and putting them in as 505 unjudged records would be
the padding this ledger exists to avoid. A survey turns each one into a `tag` or
a `write`, and the surveying is its own unit of work with its own instrument
(`scripts/conformance-candidates.py`, whose v1-to-v2 repair is note `451ec2a8`).
The honest statement of the whole is:

```
625  in-scope requirements
 40  carry a claim
100  surveyed  →  75 tag · 24 written · 1 conflict
505  never surveyed  →  464 mechanically settleable
111  failures from the notes store, none of them derived from a clause
```

Render it with `scripts/test-queue.py`; `--counts` prints just the arithmetic,
`--all` prints every file, `--kind tag` and `--tier 1` narrow it.
