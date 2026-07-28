# Negative controls — proving the suite can fail

## The problem

Every measurement this repo makes about its desktop tests describes **reach**:
how many specs, how many of the 251 Tauri commands got invoked
(`scripts/desktop-invoke-coverage.py`), whether the fixtures actually ingested
anything (the `FixtureExpectation` gate in `real/global-setup.ts`). Reach is
worth measuring and none of it answers the question an engineer actually has
before a release:

> If this broke, would we find out?

A green suite is equally consistent with "the product works" and "the
assertions are vacuous", and no coverage number separates those two worlds. An
assertion that can never fail is *reached* on every run and counted every time.
The number goes up; the protection does not.

The separator is the negative control: hand the instrument a sample you know is
bad and require it to say so. The repo already runs this discipline elsewhere —
`sovereign-eval`'s mechanism-fidelity harness keeps a blindfolded agent that
must score at chance (`mechanism_fidelity/mod.rs`, "validity: the negative
control fails while a sanity agent passes"), and CHAOS_QA's judge-calibration
gate holds sensitivity and specificity floors against a labelled bank
(`CHAOS_QA_METHODOLOGY.md` §judge-calibration). What was missing was the same
standard applied to the desktop suite itself.

## Two layers

Both are needed. Layer 2 is the point; layer 1 is what stops layer 2's
foundation from rotting.

### Layer 1 — is the assertion pack potent?

`specs/negative-controls.spec.ts` stages deliberately-broken turns and requires
`real/invariants.ts` to reject each one.

Fifteen mutations of a known-good turn (chunks that don't reconstruct the final
text, a duplicated `message-complete`, a missing intent, a citation that
dereferences to nothing, the runtime's own "not traceable" verdict being
ignored), each pinned to the specific error the pack is supposed to emit.

It runs in the **synthetic** suite, on purpose. `invariants.ts` executes only
under `playwright.real.config.ts`, which needs multi-GB GGUFs and a live
desktop and therefore runs nowhere in CI. Its assertions could be hollowed out
by a green PR. Putting the controls in the synthetic suite makes CI the guard
for a suite it never runs.

Three things make each control mean something:

1. **A positive control.** The unmutated baseline must pass first. Negative
   controls on an already-red baseline all "catch" a failure that has nothing
   to do with their mutation.
2. **One mutation each.** Blame stays attributable.
3. **The declared reason.** Every control pins the message it must fail with.
   Matching any error at all would let a typo in the staging code pass as a
   working control.

There are also five **tolerances** — odd-but-legitimate turns the pack must
*accept* (web citations with no chunk handle, attach-mode corpora, `length`
finishes under a token budget). Over-strictness is the other way an instrument
dies: not by asserting too little but by asserting so much that someone loosens
the whole pack to shut it up.

Fidelity note: controls push staged rows through `fixtures/tauri-shim-real.js`
itself with only `EventSource` stubbed, so `captured`, `chunksFor` and the
`__lagged__` synthesis are production code. A hand-rolled double would let the
shim's capture contract drift out from under the invariant pack while every
control still passed.

### Layer 2 — does the suite catch a real regression?

`scripts/sabotage.mjs` + `sabotage-bank.mjs`. Each mutant is a real, compiling
edit to real source; the runner applies it, runs **the specs that claim to
cover it**, and requires them to go red.

```
npm run sabotage            # the bank (synthetic suite)
npm run sabotage:list       # print it, run nothing
node tests/e2e/scripts/sabotage.mjs --only <id>
node tests/e2e/scripts/sabotage.mjs --json out.json
```

Three verdicts:

| Verdict | Meaning |
|---|---|
| `CAUGHT` | the declared specs failed — that invariant is genuinely defended |
| `SURVIVED` | the product was broken and the gate stayed green — **a bug report about the suite** |
| `STALE` | the mutation no longer applies; the bank is lying about what it covers |

`mustFail` names the spec that *claims* the coverage, not "the suite". A mutant
caught by some unrelated spec is weak evidence; a mutant caught by its own spec
is proof that the spec does what its name says.

Exit is non-zero on any `SURVIVED` or `STALE`, so it gates in CI.

#### The runner's own control

A perfect score is the least falsifiable result there is. A bug that made the
runner read every run as failing — a bad exit-code read, a spec path matching
nothing, a stray non-zero from `npx` — would print `17/17` forever, and nobody
questions `17/17`.

So the bank carries one entry, `self-control-unrelated-mutation`, whose declared
verdict is `SURVIVED`: it mutates the Library empty state and runs the chat
placeholder spec, which cannot observe it. If it ever reports `CAUGHT`, the
instrument is broken and **every other verdict in the run is worthless** — which
is what the script prints, instead of quietly scoring one higher.

It is scored separately from the mutants, so a passing control never pads the
ratio it exists to validate.

#### Blunt kills

A mutation that fails *every* test in its spec usually means a crash, not a
caught regression: it proves the page loaded, not that any assertion watches the
behaviour. Surgical kills leave sibling tests green.

Sometimes a whole spec file legitimately hangs off one behaviour (the corpus
filter strip renders from one chip list; every orphaned-turn path reads the
live-turn registry). That is allowed — but it must be **stated** in `bluntKill`,
never inferred from finding it, the same rule the fixture-liveness gate runs
under. An undeclared blunt kill warns; a `bluntKill` that stops being true also
warns, so the claim cannot rot into a rubber stamp.

## Reading a SURVIVED verdict

It is not a flake and not a test to be adjusted. It says: *this regression
reaches a user with every gate green.* The fix is to strengthen the named spec.
Deleting or weakening the mutant converts a known hole into an unknown one.

## Adding a mutant

```js
{
  id: "citation-chip-never-renders",
  suite: "synthetic",
  target: "src/lib/components/Foo.svelte",
  breaks: "the invariant, in the words its owner would use",
  userImpact: "what a person using the app would see",
  find: "a substring occurring EXACTLY ONCE in the target",
  replace: "must still compile and still pass `npm run check`",
  mustFail: ["tests/e2e/specs/foo.spec.ts"],
}
```

Two rules that are easy to get wrong:

- **`replace` must still build.** A mutant that breaks compilation fails every
  spec for the wrong reason and reports a cheerful `CAUGHT`.
- **`find` must be unique.** The runner reports `STALE` when it isn't, which is
  the entire anti-rot mechanism — a mutant that silently stops applying is
  worse than no mutant, because the bank keeps reporting `CAUGHT`.

## Safety

`sabotage.mjs` rewrites tracked source files. It:

- refuses to start when a target has uncommitted changes (`--allow-dirty` to
  override) — a crash mid-run must never destroy work git could not give back;
- takes an exclusive lock, because two concurrent runs is silent corruption
  rather than a clash: the second captures the first's *mutated* file as its
  "original" and restores a deliberate bug into the tree permanently;
- copies every original to `test-artifacts/.sabotage/` before the first edit;
- restores from a `finally` plus `SIGINT`/`SIGTERM`/`uncaughtException`
  handlers, then compares every file **byte-for-byte** against what it read.

Verification is against the captured bytes, not `git diff` — that would call a
legitimately-dirty target unrestored, and a restored one clean only by luck. On
any mismatch it exits 2. It never exits 0 with the tree modified.

## The tripwire

Controls guard the assertions that exist today. Nothing automatically notices
an assertion added *tomorrow* with no control behind it.

`negative-controls.spec.ts` therefore counts the assertions in `invariants.ts`
and fails when the number moves. It is crude on purpose: it cannot be satisfied
by accident, and the failure names the exact obligation — add a control and
raise the number, or confirm the deletion was intended and lower it. Adjusting
it without doing one of those is how a bank goes stale while staying green.

## What this does not cover

Stated so the gap is a known one:

- **The real-mode and faults suites still run nowhere automatically.** Layer 1
  protects the potency of their assertions; nothing yet proves those suites go
  red against a broken *backend*. Rust-side mutants (`commands/chat.rs` emits
  `message-chunk` at :161 and `message-complete` at :236) are the natural next
  bank, gated on real-mode running somewhere scheduled.
- **Mutants are hand-declared, not generated.** This is a bank of regressions
  we thought of. It is a floor, not a coverage measure.
- **`userImpact` is prose.** Nothing checks that a mutant's declared user
  impact is real; that judgement stays with whoever adds it.
