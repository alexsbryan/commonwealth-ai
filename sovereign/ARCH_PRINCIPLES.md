# Architecture Principles — Commonwealth AI

Twelve principles, each earned from a failure in this workspace. **Hold all
twelve.** They are short enough to read whole, which is the point: a document
that has to be sampled gets ignored.

Every principle carries the incident that minted it and the smell that signals
it. Note ids are readable with `svrn notes list --id <id>`. Rules that a gate
already enforces are not here — they are in the gate, listed at the end.

**Budget: 8k tokens total, twelve principles, 600 per section. Fixed, not
baselined** (§How to add). A thirteenth principle displaces a twelfth.

---

## The twelve

1. **Glassbox, always.** A decision invisible at `tracing=debug` is not finished.
2. **Don't whack moles.** Instrument, reproduce, understand — *then* fix.
3. **Write for the next reader**, and land the doc change in the same commit.
4. **Cite, don't recall.** Verify before you claim it.
5. **A gate you have not watched fail is not a gate.** Four verdicts, not two.
6. **Never silently substitute.** Absence is reported, never defaulted.
7. **Validate the instrument before the result.** One run is not a measurement.
8. **One decider, one name.** One implementation per threshold, scorer, schema,
   key; one accessor per path; identity from essence.
9. **Closed sets are enums, open sets are registries, open text is a centroid.**
10. **Make it structural, not remembered.** Never ask a model to guarantee what
    code can enforce.
11. **The inventory outranks the plan.** Prove what exists cannot serve.
12. **Draw the line by what each side owns about itself.**

One through four are this workspace's operating ethos. Five through eight were
earned: they describe this system's characteristic failure — a plausible,
well-formed, exit-0 result that is wrong. Nine and ten prevent the most rework.
Eleven was minted 2026-08-08 and twelve 2026-09-11, both by operator directive,
both after the same pattern recurred a third time with every catch coming from
the operator rather than the builder's own process.

---

## 1. Glassbox, always

The user, the operator, and the next engineer must see *why* the program did
what it did without attaching a debugger. Every non-obvious decision — one the
operator cannot predict from inputs plus static analysis — emits a tracing
event named `module:short_action`, so `grep` goes straight to the site. Pick
the level deliberately: `error!` for bugs, `warn!` for degradation, `info!` for
lifecycle, `debug!` for routine decisions, `trace!` for per-item detail. A
system that only emits `info!` is not observable; it is a press release.
Redact deliberately — the drift detector logs 12 hex chars of each SHA-256,
enough to correlate and not enough to leak.

**A probe must not ride the resource it monitors.** `/v1/mesh/status` sampled
device memory through an FFI call that is a register read locally and a
synchronous network round-trip for an RPC device. Tested against an idle
worker it was ~2 ms; under a serving 122B it hung past 70 s, and `mesh bench`
died with "daemon not reachable" while the daemon was healthy. A status field
must never be produced by a call that can block — publish a timestamped
observation captured where the work already happens.

**Smell:** a branch of production code with no tracing event.

---

## 2. Don't whack moles

A failing test means something. Instrument, reproduce, understand, *then* fix.
Disabling a test to get green is a last resort that requires a `todo` note
saying what was deferred and why. When a deployed-path behaviour is wrong and
one signal cannot explain it, make the real decision visible first — tracing at
a captured target, or a trace file — and confirm the trace lands before forming
a fix. A detached daemon discards `eprintln!`.

The refactor corollary: **behaviour-preserving by default, one dimension at a
time.** A refactor is behaviour-preserving if existing tests pass unmodified,
`cargo check --workspace` is green, and no external caller can observe a
difference in interface, output shape or timing. If your change breaks any of
those it is a behaviour change that happens to involve restructuring, and it
lands as its own commit — especially if it improves something. Splitting a
1300-line file *and* changing its error semantics in one PR reviews badly and
reverts worse.

**Smell:** a refactor PR that also "just cleans up some nearby stuff".

---

## 3. Write for the next reader

You will not be at the keyboard when someone else modifies this code. Name the
constraint, surface the non-obvious, do not pun with variable names.

`SYSTEM_OVERVIEW.md` is a contract, not a diary: it describes what exists
today, everything derivable from it must still be true, and it is updated in
the same commit as the subsystem change. If you added a subsystem and did not
update the doc, you did not finish. One session found it silent on two
production subsystems, wrong about the MCP tool count, and missing five CLI
subcommands — a new engineer would have missed half the surface area.

Comments name the *why*, never the *what*. `// loop over the views` is what the
code says. Measured 2026-09-10: the workspace carries 287k comment lines
against 860k of code, comment-per-code on newly added lines rose 0.27 → 0.49 in
three months, and 170 paths cited in comments do not exist — 86% of them never
did. A comment that cannot be checked rots invisibly. Prefer a test (§10), a
citation, or nothing.

**Smell:** a comment asserting in English what a test could assert in code.

---

## 4. Cite, don't recall

Before claiming a function exists, a trait has a method, or a file says
something — verify it. From `grep`, from `symbols`, from a run you just did.
The codebase evolves faster than memory, and subagent reports are partially
wrong often enough to matter: one exploration claimed `MiddlewareRegistry` did
not exist (it did, `middleware/mod.rs:292`) and that `FeatureState` did not
exist (it did, re-exported at `sovereign-atos/src/local.rs:16`). Cost of
verifying: one call. Cost of acting on a false claim: a duplicate
implementation, found in review.

`cargo check` is a correctness witness and `cargo test` is a behaviour witness;
neither replaces the other. Before touching a signature, know the blast radius
— `callers` for the code side, `drift_findings` for narrative claims the change
would falsify.

The same rule binds prose: a claim in a commit body, a PR, or a comment names
something a reader can resolve, or it is not made.

**Smell:** a claim in commit or PR body that a function exists, without a citation.

---

## 5. A gate you have not watched fail is not a gate

Before you land a check, name the input that makes it red — then make it red
and watch. `doctor` asserted that an *unrelated* file existed and reported
Passed; nine test assertions pinned the wrong JSON shape, so for months the
suite defended the bug (`f6d4c770`). **Assert on something the subject cannot
author**: an SSE `model` field is a verbatim echo of the client's request, so
the wrong-slot guard passed cleanly on the exact failure it existed to catch
(`a6ca12aa`). **A zero count is not a positive control** — `canary_hits: 0` is
the expected good result and proves nothing (`72b3ab47`).

**Four verdicts, not two:** passed, failed, could-not-judge, never-ran. Give
each its own exit code; `sovereign-test.sh` exiting 4 on a zero-test run is
this already in force. An adapter gate reported an all-NaN diverged training
run as "not trained" because Python's `max()` keeps the running value against
NaN — diverged and never-started demand opposite responses (`edbfabb8`).

**The two that make no claim are owed, not free.** Abstention is always
available and always defensible, which makes it the dominant move for whoever
writes the check. An abstention you have not watched be necessary is not rigor.
A precondition may assert that the SUBJECT of a measurement exists; it may not
assert that the WORLD is convenient. `host-quiet:4` fired on most runs, taught
nothing across three of them, and read as careful — it is gone, replaced by
load recorded as a covariate that gates nothing. A repeated could-not-judge for
one reason is a defect in the instrument, not a fact about the world.

**Smell:** a check with no failing input you can name; an abstention branch with
no run that demanded it.

---

## 6. Never silently substitute

If you cannot do what was asked, refuse — or name the substitution in the
response. Degrading quietly is worse than failing, because it spends the
caller's trust instead of their attention.

With the distributed primary unavailable, the non-streaming path returned a
clean error while the streaming path, same model string, seconds apart,
returned 200 served by a 0.8B. Label, shape and finish reason were all correct,
so no client could tell (`d45489a3`). The desktop updater collapsed every `Err`
into `Ok(None)` = "up to date", and that mask hid two further update bugs for
weeks (`c17ba1ff`).

**Absence is reported, never defaulted.** A guessed rate is a fabricated fact
with a unit attached. `Unpredictable` is never collapsed into a number, and
`Unpredictable` and `Infeasible` point in opposite directions so they must not
collapse into one `Option` (`963a8d88`). Reading a metric under the wrong key
and defaulting to 0 returned 0 for every historical run and made the current
one look like a fresh regression from zero (`9345fb89`). Unknown fields are
errors, not no-ops: a wrong parameter shape produced `{created: true,
sections_updated: []}` and left three frames on disk as empty husks, one live
in the boot index (`30251dcf`).

Absence of a response is not evidence of absence. ggml's RPC server accepts one
connection at a time, so a busy worker is indistinguishable from a dead one —
distinguish "answered: no" from "did not answer".

**Smell:** an `Err` collapsed into a success-shaped value.

---

## 7. Validate the instrument before the result

When a new harness's first run reports a regression, suspect the instrument
before the system. An oracle judged answers against chunks truncated to 1500
characters while the gate under test grounded on the full set: a composite
reported at 60% was really ~90%, and 14 to 17 of 22 "broken" turns were correct
behaviours mis-scored (`8bfc177c`). Score against the same evidence the system
consumed, and resolve thresholds through the shipped resolver (§8). Validate
that the path you are measuring executed at all — four days of training runs
looked healthy while the gradient sum was exactly 0.0 (`3d9a9ce4`). If a run
cannot be re-scored without re-running the model, it is not instrumented.

**One run is not a measurement.** Establish the noise floor at your sample size
first. Judge verdicts flipped on 37% of facts (104/284) across trials on the
same transcript (`485f9f05`); at n=2 a two-node comparison read backwards and
at n=4 it inverted (`e39fa87d`). Short runs are liveness checks: a 5-step gate
passed one step before the identical config diverged to NaN at step 11
(`c4851203`).

**A judge is never tuned in one direction.** Any change to a judge, scorer,
veto, scan or threshold reports two numbers: what it stopped getting wrong, and
what it still catches — the second against known-bad cases frozen *before* the
change was measured. Widening an evidence window is strictly more permissive;
reported in the permissive direction only, a change is not failed, it is **not
judged** (`95b82f97`). A human holdout is terminal: a phase whose metrics
improved while the operator judges the output worse has failed.

**Smell:** a single-run delta reported as a result; a judge change reported only
in the direction it was meant to fix.

---

## 8. One decider, one name

A little duplicated code is tolerable; duplicated *deciding* is not. Duplicated
code diverges into a compile error or a red test. A duplicated threshold,
schema, scorer, policy table or measurement key diverges into a plausible
number with nothing red anywhere.

A third view of the slot-alias policy hand-spelled `vec![stem]` and omitted
`commonwealth/primary`: every peer request for the shared 122B was answered by
the 0.8B fast slot at HTTP 200, and every measurement filed for that model was
the small one under its name. Re-derive, never re-implement; if two
implementations must exist, share the body and add a golden equivalence test.
Conventional agreement decays, structural inseparability does not.

**Identity derives from essence.** Chunk ids allocated from `chunk_count()`
were reused after deletes — 31,432 duplicate Wikipedia rows, and a citation for
the 2026 Lebanon war opened an article about gold as a food additive. A
loopback port used as peer identity produced 14 rebuilds in 21 minutes for a
peer that had not moved. Key on content hash or a stable id; the address is a
mutable attribute, never the name. One accessor per path — hand-derived paths
in two processes present as "the write succeeded and the read found nothing".

**Collapse the drive before you split the file.** N sites running one sequence
share one bug: six copies of the turn drive each re-ran `handle_message` on a
non-streamable refusal, and all six wrote the user's message twice. Extract one
implementation and make the difference a parameter — a sink, a mode, a composed
bundle — never a copy, never a flag inside the drive. A refactor that cannot
name a count that went down has not been measured.

**Smell:** two implementations of one threshold, formula, or key; a key derived
from a row count, sequence number, or network address.

---

## 9. Closed sets are enums, open sets are registries, open text is a centroid

A `&str` constant repeated across files with a known set of valid values is a
type trying to escape — make it an enum with `const fn` accessors, and keep the
string form as an alias the enum computes when it is a wire contract. When the
set is open (third parties register handlers, the list grows with features),
dispatch through a registry: you never edit a match arm, you call `register`.
Unknown ids fail loudly at startup — a typo in a pipeline alias should fail the
request, not shave off a behaviour at runtime.

**Classifying open text is a centroid, not a keyword list.** The same error one
level up is a stringly-typed *decision procedure*. `needs_current_info`
substring-matched "today" inside "…from antiquity to today" and put a user's
phone into a refusal loop; a locator gated on nine literal substrings missed
"What was the first thing I asked?" on four runs in five. The replacement is a
calibrated centroid with both an abstain gate and a margin gate, proven on
held-out inputs of both classes. Two cautions that cost real measurements:
similarity is topic-dominated, not shape-dominated, so adding exemplars buys
the topics you add and not the shape; and after re-filing exemplars, re-check
the abstain cases, because margin is relative and "same winner ⇒ same verdict"
is false for any gate with a margin term. Stripping a fixed protocol token is
not this trap — that is mechanical, keep it narrow and in code.

The same axis governs data: prompts, templates and tunable defaults are data,
loaded by `include_str!` beside their consumer. If it can change without a code
change in the same commit, it is not program.

**Smell:** a `match` on string ids with more than 3 arms; a large const string
literal in a `.rs` file.

---

## 10. Make it structural, not remembered

Encode the invariant so it cannot be forgotten. KnowledgeView promises its
corpora never leave the machine, and that promise is three hardcoded fields no
caller can flip via config, CLI flag or remote request — the violation does not
compile. Back it with a test that pins the invariant, and layer the
enforcement: recipe, acquirer SQL, and splice all three exclude local-only
data, so no single layer slipping compromises it. Runtime asserts are fine for
bugs; threats want to be impossible to express, not merely observable.

**A model's behaviour is a threat, not a bug.** It cannot be fixed by asking
more firmly, and a prompt imperative relied on for correctness is a gamble
re-run on every request. Instruction-caveat compliance measured ~60% on the 4B
this repo runs. The structural fixes worked where four rounds of prompt
language had not: forcing the output shape to `{claim, contradicts_item}` and
dropping uncited entries took hallucinations 11 → 2; committing the constraint
at decode time removed the honesty *variance*, which was the actual defect;
dropping uncited bullets and re-asking once took frame recall 17% → 88%; and on
zero atlas matches the append round is skipped entirely, so there is nothing to
fabricate over. Remove the fuel rather than forbidding the use. A bright line
is also cheaper than a fuzzy one — one unambiguous clause took hand-confirmed
breaches 2 → 0 *and* moved unrelated quality, filler questions 19.5% → 7.2%.

Corollary for agents: more imperative language will not beat a model that has
discovered it can no-op. Reach for a stronger verify gate (§5).

**Smell:** an assertion in English prose rather than in a test; a guarantee
asked of a model that code could enforce.

---

## 11. The inventory outranks the plan

Bias toward reuse. Build new only after surveying what exists and saying, with
a citation, why it cannot serve.

A drafted order funded enrichment runs to mint ~2,000 calibration claims while
59,100 already-enriched Claim atoms sat in 1,665 installed corpora; the plan
inherited a spec's substrate list and nobody ran `atlas list-corpora`
(`3836ec2d`). An agent began replicating the CLI's enrich pipeline inside the
desktop; the fix was ~3 lines reusing `state.corpus_engine` (`400649c9`). In
every case the capability existed, the builder never looked, and the catch came
from the operator — never from the builder's own process.

The survey is one command per resource class, not a research project: corpora
via `corpus status` / `atlas list-corpora`; seams via `symbols`, `code_search`,
`callers`; tools via `tools list` and `ls scripts/`; prior art via `notes`. New
nouns go through `svrn code converge noun <Name>` first — a name already owned
elsewhere is a convergence decision, not a free choice.

**The review rule, falsifiable:** a plan, order or PR introducing a new store,
pass, corpus, harness or subsystem must name the existing surface it checked
and why that surface cannot serve. No citation, no funding. The tell: when a
design starts feeling complicated, you missed the reuse.

**Smell:** new capability added without citing the existing surface that was checked.

---

## 12. Draw the line by what each side owns about itself

Before deciding who calls whom, write down what each component owns about
*itself* — its data, its lifetime, its recovery. Everything else only asks.
sv-surface spent a day on "who should own the daemon process?" before the
operator dissolved it: a daemon owns its data root, its weights, when it idles,
when it restarts; a client owns what it shows and what it asks for.

**Is anyone doing something *for* another component** — supervising, restarting,
deciding when it stops? The line is wrong. `sovereign-desktop` held a restart
policy, a shutdown budget and an exit handler for a process that owns all
three: 1,990 lines, ~271 of which survive the question.

**A gap in one thing is not a job for another.** A daemon holding ~28 GB after
the window closed was written up as a lifetime trade-off between app and
daemon; it was a missing idle policy *in the daemon*. Hand a component's gap to
a neighbour and that neighbour knows its internals forever.

**Calling something is not owning it.** One call granting no new ability is a
fine seam; holding a handle, a retry or a dependency is ownership. Argue about
what each side *can become*, never what it happened to call. The same rule
sizes interfaces: trait bounds narrow to what a function touches, and pipeline
stages take chunks and handles, not a source aggregate.

**You drew it wrong when the uses go to zero and the ability stays.** Three
instruments in a row hit their targets — forks 15 → 0, attach reads 55 → 33,
constructions pinned at 12 — while `main.rs` still re-entered the desktop
binary as the daemon and `Cargo.toml` still linked 21 crates: a 661 MB "client"
beside a 614 MB daemon. Look where the ability is *granted* — the dependency,
the import, the feature flag — not at the sites that use it. `cargo tree` is
the check that works.

Do not gate on who is running it. "Phones cannot run a backend" is a fact about
this year; "this build ships no backend it can start" is the same guard without
deciding the future.

**Smell:** a component holding another's lifecycle; a count that could reach
zero with the ability fully intact.

---

## Rules that live in their gates, not here

A rule with a ratchet behind it belongs in the ratchet's error text, where it
arrives at the moment of violation instead of needing to be remembered (§10).
Each gate names its own fix command.

| Rule | Enforced by |
|---|---|
| Doc paths resolve; `SYSTEM_OVERVIEW` claims verify | `cargo xtask docs-gate` |
| File ceilings (400/800/1200) and crate size | `cargo xtask size-gate`, `layout-gate` |
| Dependency direction, layer map, `[[forbid]]`, fan-in caps | `cargo xtask layer-gate`, `svrn code arch-report` |
| One version per third-party crate; feature-set skew | `cargo xtask lock-gate` |
| Wire-API constants match their enum | `cargo xtask api-gate` |
| New concepts converge on an existing owner | `cargo xtask concept-gate` |
| Env vars declared before they are read | `cargo xtask env-gate` |
| Zero-test runs, unattributable runs, build failures | `scripts/sovereign-test.sh` (exit 4/5) |
| §15 smells in the staged diff | `.claude/hooks/commit-smells.py` |
| Hook commands anchored, never doubled | `.claude/hooks/tests/settings-wiring.sh` |

`cargo xtask quality` runs every local gate with one summary table; baselines
under `quality/baselines/` are machine-written, `--tighten` banks improvements
and never raises. Tool choice, PR shape, note discipline and session protocol
live in `AGENTS.md`, which every harness reads.

---

## Where the old section numbers went

~3,000 citations in code comments and commit bodies name the pre-2026-09-11
numbering (§18.3 alone appears 812 times). They resolve through this table
rather than by rewriting every file.

| Old | Now |
|---|---|
| §0 ethos | 1, 2, 3 |
| §1 documentation | 3 (+ `docs-gate`) |
| §2 stringly-typed, §4 registry, §6 data-vs-program | 9 |
| §3 file size | `size-gate` (+ 2 for refactor scope) |
| §5 interface segregation | 12 |
| §7 structural invariants | 10 |
| §7.5 identity from essence | 8 |
| §8 dependency hygiene | `layer-gate`, `lock-gate` |
| §9 observability, §9.5 probes | 1 |
| §10.1–10.5 refactor discipline | 2 |
| §10.6 one decider, §10.7 collapse the drive | 8 |
| §11 verify before claiming | 4 |
| §12 testing | 10 (+ `sovereign-test.sh`) |
| §13 tools, §14 process | `AGENTS.md` |
| §15 smell table | `AGENTS.md` |
| §18, §18.1, §18.2 | 5 |
| §18.3 | 6 |
| §18.4, §18.5, §18.6 | 7 |
| §19 inventory | 11 |
| §20 ownership | 12 |

New citations name the principle number, not a `§`.

---

## How to add to this document

1. It must come from a **real incident here** — a bug, a refactor, a drift you
   caught. Cite the commit, note id, or file.
2. It must be **specific** and **actionable by someone who did not see the
   incident**.
3. **If a gate can enforce it, it goes in the gate**, not here.
4. **Principles replace prose, not the reverse.** A new rule that obsoletes an
   old one deletes it; they do not coexist.
5. **Budget: 8k tokens, twelve principles, 600 per section. Fixed — it never
   rises.** A thirteenth principle displaces a twelfth. This file reached 80
   sections and 19k tokens because nothing ever forced that trade, and at that
   size it stopped being read.
