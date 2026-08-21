# Noun convergence — the program

**Status:** proposed, 2026-08-16. Not yet approved, nothing landed.

**The register is [`CONCEPTS.toml`](./CONCEPTS.toml)**, not this file. Nouns,
canonical owners, totality rules, phase assignment and current shape live
there; this document is the argument and the method. The destination
architecture is [`TARGET_ARCHITECTURE.md`](./TARGET_ARCHITECTURE.md).
[`CLEANUP.md`](./CLEANUP.md) stays as the per-item backlog this program
reorders.

**Contracts this answers to:**
[`ARCH_PRINCIPLES.md`](../sovereign/ARCH_PRINCIPLES.md) §10.6, §7, §18, §19.

---

## 1. BLUF

The workspace is 59 first-party crates, ~955k lines of Rust, ~657k of it
production. It is not too large for what it does. It is hard to reason about
for one measurable reason:

> **The system re-derives what it should carry.** 278 concepts are defined
> as a type in more than one crate. The wire message between desktop and
> daemon is declared seven times. The four-verdict vocabulary is declared
> ten times. Provenance crosses the project boundary as
> `metadata["peer_name"]`.

Every row in `SYSTEM_OVERVIEW.md` §10 — forty deferred file splits — is
downstream of that. A file grows past 1,200 lines because the thing it does
has no name; the arch-gate then ratchets on *line count*, which rewards
moving code between files and never rewards naming a concept.

**This program replaces the line-count ratchet with a concept-ownership
ratchet, and drives one number to zero — with the score and the discovery
feed kept separate** (amended 2026-08-17 after the census was adjudicated):

```
headline   register rows converged / rows in_program        →  all
feed       D1 census: names typed in >1 first-party
           production crate, idiom patterns excluded         →  0 undispositioned
```

The census DISCOVERS candidates; the register DISPOSITIONS them (converge /
distinct-rename / idiom-pattern / external-mirror / layered — see
`CONCEPTS.toml`); the headline is the register burn-down. This split exists
because adjudication (n=55 random sample plus every ≥3-crate cluster, read)
found **roughly half the census tail is name coincidence, not
duplication** — `Layer` the ARCH tier vs `Layer` the doctor probe. Driving
the *raw* count to zero would force ~100 renames of things that were never
duplicates, which is neither convergence nor honest.

Monotone. Ungameable by moving code — collapsing two definitions into one
is real work with a real diff. Ungameable by *renaming* because every
decrement must name its disposition in the landing verdict, and a `distinct`
rename requires the tape's rationale — renaming a true duplicate apart to
juke the number leaves a recorded lie, not a green tick.

**And the numbers are minted, not typed.** Every figure in this document is
a dated snapshot with a re-measure method; the register carries a `measure`
field per row; the frozen baseline is an `nc-t0b` artifact with its filters
declared inside it, never a constant in prose. The lesson was measured on
this very program: the 2026-08-16 register was verified against the code
one day later and six rows' prose had already decayed (the counts all
reproduced; the *qualifiers* had rotted). Part of the work is the live
measure-and-improve loop itself — teach the process to fish, or every
number in it is week-old fish.

**The outcome this program is accountable to** (operator direction,
2026-08-17): that the future-architecture document can be written
*honestly* — a system mostly accountable to a few abstractions
interacting, not a spaghetti architecture wearing confident documentation.
The metric is the input condition; the **generated document is the outcome
test**, because generation is impossible over spaghetti: a document
derived from the register and the graph can only claim structure that
actually exists, and what does not exist renders as a visible gap instead
of confident prose. That is `nc-doc`, the initiative's headline bar — a
converged system that still needed its architecture narrated by hand would
be a failure with excellent hygiene.

**And the refactor is not the product.** Performed with tools that record
what they did, it yields three things instead of one: a converged system, a
replay tape another agent can re-run, and a portable harness for any
codebase with the same disease (§6). A phase whose only output is cleaner
code has produced a cost centre.

---

## 2. The diagnosis, in evidence

Measured 2026-08-16 against the working tree and the live SCIP graph
(227,327 symbols, 1,265,301 refs). Per-noun detail is in
[`CONCEPTS.toml`](./CONCEPTS.toml); this section is the shape.

The disease has three mechanical forms.

**Re-derived identity** — the same concept gets a fresh type at each
boundary. `Args` ×33, `Result` ×19, `Verdict` ×10, `Plan` ×9, `Error` ×9,
`ChatMessage` ×7, `ChatChoice` ×7, `Evidence` ×5, `Provenance` ×4.
**278 total** (2026-08-16 snapshot; the raw count mixes classes — see the
BLUF — and the per-name adjudications live in the register rows). The
daemon-boundary DTOs are the densest cluster, which is the structural
confirmation of ~1,250 sites where a surface hand-assembles or hand-picks
apart a daemon message. Two sharpenings from the 2026-08-17 verification:
all seven `ChatMessage`/`ChatChoice` are partial private mirrors of the
*same external OpenAI schema* whose canonical is already `pub` and already
shared cross-crate — the cheapest convergence in the register; and the
purest specimen is the newest code — `deep_research/icd.rs`, minted this
week, re-derives five register nouns including a `Verdict` that implements
the register's exact target enum, privately, because no canonical home
existed to import. The disease is not carelessness; it is the absence of an
owner to reach for.

**Re-derived policy** — a decision that belongs to the data, recomputed at
each use. Variant-reference fan-out: `Error` 2,422 refs across 32 crates,
`AtomEnvelope` 739, `Intent` 486, `StepOutput` 450, `Role` 447,
`Verdict` 253 (2026-08-16; caveat added 2026-08-17: fan-out over a *closed*
enum measures exposure, not disease — matching on a closed set is what
enums are for (§2), so D3 needs a policy-vs-pattern-match discriminator
before its numbers are read as findings; `AtomEnvelope` is the worked
example). Corpus *sharing policy* is a pair of registry bools consulted at
~149 sites — about 5:1 plumbing to genuine guards — and one of the bools is
an `Option<bool>` resolved by an unnamed `unwrap_or` rule at index open.
The 2026-08-17 verification corrected v1's stronger claim here: a typed
chunk-level `Custody` *does* exist and *is* carried by the data; the two
axes had been fused under one name. The register now splits them
(`SharingPolicy` / `Custody`).

**Re-derived context** — what is wired, what mode, what is comparable.
`Runtime` has 32 fields of which **15 are optional capabilities** (9
`Option<Arc<dyn>>`, 4 `Option<Arc<Concrete>>`, a bare `Option<RerankFn>`,
one `RwLock<Option<…>>` — re-measured 2026-08-17), giving up to 2^15
undeclared configurations assembled by 18 `with_*` builders across three
real bootstraps. Absence degrades silently —
`runtime/retrieval_pipeline.rs:713` returns `Vec::new()` when no corpus
engine is wired, so "not configured" and "found nothing" are the same
value; the sharpest consequence found is `is_governance_turn`, which reads
unwired-engine as not-governance and silently selects the **wrong gate
calibration surface** for the turn. That is the shape
`ARCH_PRINCIPLES §18.3` forbids. Two reuse facts temper the build (§19):
`UnavailabilityReason` already exists in contracts with four variants —
the fix starts with adding the missing `NotConfigured` member — and
`handlers/commissive.rs` already implements the honest-refusal pattern this
phase universalizes.

### 2.1 The second-order cost

Because nothing is carried, everything must be *checked*. Code whose only
job is to detect that two hand-maintained descriptions have drifted:

| Subsystem | Lines |
|---|---:|
| drift toolchain | 5,155 |
| contract QA tests | 3,489 |
| docs-gate + arch-gate (xtask) | 2,670 |
| capability map / reconcile | 2,156 |
| CLI contract model | 1,468 |
| contract census + cmd | 711 |
| posture table | 503 |
| quality gates | 454 |
| **partial total** | **~16,600** |

Partial — excludes `arch_report`, spec↔code fact mining, and the MCP drift
surfaces. (2026-08-17 audit: every row reproduces to the line, but the
table is a **floor**, not a total — the named exclusions add ~4,900 lines,
the capability family has ~2,700 more in-scope lines the row missed, the
drift row's 5,155 in fact *includes* `atlas_drift_report`'s 1,765 despite
this footnote, and the whole Python reconciliation layer — `co-lineage.py`
at 865 lines and its siblings — is uncounted because the table is
Rust-only by construction. Honest first-party figure: **~21,500 lines**.
The exact per-row deltas and file lists live in the 2026-08-17 review; the
number that matters is minted at `nc-t0b` alongside the census, with its
inclusion rules declared in the artifact.)

**Every duplicated description is paid for twice: once to write it, once to
build the machine that checks it.** That machine is itself code that drifts,
needs tests, and has its own posture command. `drift_posture` currently
reports *stale, last run 45 days ago* — which is what happens to
compensating machinery: real work, no user-visible value, first to slip.

### 2.2 Checked and cleared

Stated so the program is not justified on things that are fine:

- **Panic surface is healthy.** 973 `unwrap`/`expect` in 657k lines of
  production code — one per 675 lines. (A raw grep says 10,357; ~90% sit
  inside colocated `#[cfg(test)]` modules.)
- **Test volume is healthy.** ~31% of first-party Rust is test code.

The *shape* is the issue, not the amount: 9,864 tests, of which 1,018 are
integration tests. Coverage of functions is excellent; coverage of the
configuration space is thin.

---

## 3. The loop

```
CENSUS ─────────► REGISTER ─────────► COALESCE ─────────► GENERATE
(SCIP, automated)  (CONCEPTS.toml)     (strangler fig)     (docs fall out)

ranked concept     one canonical       old type becomes    the register IS
register           owner declared;     a From/Into alias;  the architecture
                   ratchet freezes     shadow, then        document
                   the baseline        delete
```

### Census

Three detectors, all of which run against the graph **as it stands today**:

| Detector | Query | Output |
|---|---|---|
| **D1 duplicate identity** | same `name`, `#`-terminated descriptor, >1 crate | the 278 |
| **D2 god object** | methods per `impl#[Type]` × distinct files | `Runtime` 130×32, `SqliteStateStore` 125×15, `CorpusIndex` 112×7, `CorpusEngine` 102×6 |
| **D3 decision fan-out** | variant refs × crates spanned | §2 |

It ranks **concepts, not files.** That is the entire change.

#### Instrument defects — validate before the result (§18.4)

**Defects 1-3 are FIXED, 2026-08-19.** Each was re-measured against the live
graph first (313,741 symbols / 1,564,645 refs — the 227,327/1,265,301 snapshot
above was three days stale and 38% low), and two of the three descriptions
were wrong in ways that changed the fix.

1. **`symbols.kind` is unusable — CONFIRMED, worse than stated, FIXED.**
   278,233 of 313,741 (88.7%) are `unknown`. Where populated it is
   anti-correlated with the truth: of the 8,759 top-level type descriptors
   **not one** is labelled a type — 7,682 `unknown`, 946 `constructor`, 131
   `method`. `Intent` reproduces exactly as cited. Of 19,691 rows tagged
   `enum`, only 1,560 carry a variant descriptor.
   *Fix:* `corpus_engine_scip::descriptor` — `symbol_kind()` derives the
   kind from the descriptor. Derived, not stored; no re-index required.
2. **`refs.ref_kind` is 100% `"direct"` — CONFIRMED (1,564,645/1,564,645),
   but the stated CONSEQUENCE was wrong.** The open question ("whether
   trait-dispatch edges are present") is settled: **they are.** The measured
   `Arc<dyn InferenceProvider>` call at `runtime/streaming.rs:208` is
   recorded as an edge into `traits/InferenceProvider#…().`, the trait's own
   declaration, while concrete implementations carry the distinct
   `impl#[Concrete][Trait]…().` shape. So `CLAUDE.md`'s claim that `callers`
   "catches trait dispatch" is **true**, and the pre-flight blast check is
   not compromised. What is missing is only the ability to *filter* on it.
   *Fix:* `descriptor::dispatch_hint()`. Named `ThroughTrait`, not
   `Dynamic` — a generic `impl Trait` bound lands on the same declaration
   and is monomorphized, and the descriptor cannot separate the two.
3. **Negative spans — CONFIRMED but MIS-LOCALIZED, FIXED.** Not "some":
   175,489 of 313,741 rows (56%) had `line_end < line_start`. But split by
   class it is benign exactly where the census reads and catastrophic where
   it does not: top-level types 96.1% sane, functions 98.3% sane, enum
   variants 11.5% sane, **locals 83.6% inverted** (and locals are 62% of the
   table). The cited `StartupOutcome#Failed#` case is an enum variant:
   start 433, "end" 18 — a column.
   *Root cause:* SCIP encodes a range as `[start_line, start_char, end_line,
   end_char]` OR, when it begins and ends on one line, the 3-element
   `[line, start_char, end_char]`. The 2026-06-25 `enclosing_range` fix
   guarded on `len() >= 3`, so it corrected the 4-element case and left the
   3-element one reading an end COLUMN — which is the common shape for a
   single-line name occurrence. The second site (`def_scopes`) was never
   fixed at all, and it is not cosmetic: the scope end decides which
   definition a reference is attributed to.
   *Fix:* `scip_proto::range_lines()` owns the discrimination, both sites
   call it, and no decoded span is ever inverted.
4. **D1's own semantics need validation, not just its inputs** (added
   2026-08-17 — §18.4 applied to the program's own instrument). Three
   defects, each with a fix: *(a) contamination* — the raw query counts
   vendored crates, `examples/`, `tests/` and `benches/`; the census
   ships with first-party production filters, declared in its output
   artifact (the same correction §2.2 already made for the panic count).
   *(b) precision* — same-name ≠ same-concept; adjudication found ~half
   the two-crate tail is coincidence. `nc-t0b` therefore includes a
   **hand-adjudicated precision audit** of a random census sample, with
   precision reported as a number and the adjudicated baseline frozen —
   the H4 hand-adjudication move, aimed at ourselves. *(c) blind spots* —
   the `>1 crate` clause misses same-crate duplication (sovereign-core
   carries two pipeline modules in one crate) and misses synonyms entirely
   (`ReleasedCitation` vs `Citation`); D1 counts distinct definitions per
   name with crate-span as a severity dimension, and synonym discovery
   stays a register-curation duty, not a query.

**The fix for (1) is free.** The SCIP descriptor survived into
`qualified_name`, 100% populated:

```
types/ScoredChunk#                 → type
StartupOutcome#Failed#             → enum variant (parent carried)
workflow_cmd/HELP.                 → const / term
impl#[Runtime]handle_message().    → method, with receiver type
```

No re-index required. `kind` is itself a specimen of the disease: a derived
column duplicating a fact the data already carries, which then drifted from
it. Derive it or delete it; do not maintain it.

**What SCIP cannot do,** so nobody plans on it: field types, the untyped
channels (`HashMap<String,String>`, `Result<_, String>`, `Value::get`), and
whether a reference sits inside a branch. Those need a `syn` pass — a
separate, smaller tool, scheduled in phase 3 where it is actually needed.

### Register

One row per noun in [`CONCEPTS.toml`](./CONCEPTS.toml): canonical owner,
**totality rule**, **disposition** (converge / distinct / idiom /
external-mirror / layered), phase, verification tier, a **`measure`**
method the tooling re-runs live, and a dated `today` snapshot. Then one
gate: *a registered concept name may not be defined outside its owner, in
production code* — `#[cfg(test)]` items, `tests/`, `examples/` and
`benches/` are out of scope by construction, or a test helper named
`Evidence` breaks the build. The frozen baseline is **minted by `nc-t0b`**
with its filters declared in the artifact; the number only goes down, and
every decrement names its disposition.

**This is the cheapest high-leverage change in the program**, because the
ratchet apparatus already exists — `quality/baselines/`,
`--update-baseline`, `--tighten`, weekly banking, the posture table. Nothing
new is needed. The existing machinery is pointed at concepts instead of
lines.

### Coalesce

Every noun migrates through the same six steps, **one PR each, reversible at
every step**:

| # | Step | Verified |
|---|---|---|
| 1 | Introduce the canonical type alongside the old ones | compiles |
| 2 | `From`/`Into` at every boundary; property test: round-trip is lossless | differential |
| 3 | Convert callers, edges inward | differential |
| 4 | Shadow: both paths run, outputs compared, divergence logged not served | differential |
| 5 | Flip the default once divergence is zero over N runs | journey |
| 6 | Delete the old types; row → `converged` | ratchet −1 |

**Rule: a noun may not pass step 3 until its differential exists.** If you
cannot write it, you do not yet understand the noun and the design work is
unfinished.

### Generate

`SYSTEM_OVERVIEW.md` is 265KB because it describes 59 crates and 5,180
types. A noun-centric document describes ~20 nouns and the handful of verbs
over them — **it fits in a few pages by construction, not by summarising
harder.** Generated from `CONCEPTS.toml` plus the graph, so it cannot drift,
which retires part of the 16,600-line reconciliation layer as a side effect
rather than as a project.

---

## 4. The campaign

Bars are declared as data in
[`initiative-bars.toml`](./initiative-bars.toml) under initiative
`noun-convergence` — fourteen, transcribed from §8 and the exit clauses
below, none invented there. Noun ordering lives in
[`CONCEPTS.toml`](./CONCEPTS.toml); this section does not restate it.

**Shape: a specified head, a fat loop, a specified tail.**

```
HEAD ─────────────────►  LOOP  ─────────────────────────►  TAIL
nc-t0   instrument       until metric == 0:                nc-r1  held-out replay
nc-t0b  census+ratchet     pick highest-ranked noun        nc-r2  portability proof
nc-t0c  golden freeze      run the six-step migration      nc-r3  generated doc
nc-t1   Measurement        land, re-census, tape entry
nc-t1b  agrees & refuses
```

**The loop's iterations are deliberately not enumerated.** Noun fourteen's
shape is not knowable from here, and writing sixteen order specs today would
be exactly the invented precision this document refuses elsewhere. What is
specified is the *template*, the *ranking*, and the *exit conditions* — which
is everything a worker needs and nothing a planner can fake.

### 4.1 Head — five orders, fully specified

These must be right, and they are prerequisites for everything after.

| Order | Does | `serves` |
|---|---|---|
| `nc-t0` | derive `kind` from the SCIP descriptor; decide `ref_kind` | `nc-instrument` |
| `nc-t0b` | `svrn converge census` (D1/D2/D3) + `concept-gate`, baseline frozen at 278 | `nc-ratchet nc-metric` |
| `nc-t0c` | golden freeze across all seven domains, fingerprint-stamped | `nc-goldens` |
| `nc-t1` | `Measurement` keyed by comparability fingerprint | `nc-measurement` |
| `nc-t1b` | replay against the `nc-t0c` freeze: agrees **and** refuses | `nc-measurement` |

`nc-t0b` also closes **DEMO-A** (the census, live, on this repo) and
`nc-portable` is reachable immediately after it — the same command against a
non-Rust repo, **DEMO-B**. Both land before a single type moves, which is
what makes the campaign fundable before it is finished.

No production code changes until `nc-t1`. That is deliberate.

### 4.2 The loop — one order per noun, same template

**Entry:** head complete, all five bars `met`.

**Ranking.** Highest `phase` group first in [`CONCEPTS.toml`](./CONCEPTS.toml)
(1 → 6), and within a group, by the row's re-run `measure` descending —
weighted by risk: the **external-mirror class ranks first at any phase**
(the OpenAI DTO family's canonical already exists and is already shared
cross-crate, so it moves the metric at near-zero risk), and the `layered`
class ranks last (it changes a deliberate boundary and needs the golden
equivalence test first). The `phase` field is a rung group, not a calendar.

**Per iteration** — one order, `nc-m<n>`, `serves: noun-convergence <bar>`:

1. Introduce the canonical type alongside the old ones
2. `From`/`Into` at every boundary; property test that round-trip is lossless
3. Convert callers, edges inward
4. Shadow: both paths run, divergence logged not served
5. Flip the default at zero divergence over N runs
6. Delete the old types; register row → `converged`

**Checked every iteration, not at the end:**

- goldens still green (the tier declared in the noun's `verified_at`)
- the row's `measure` re-run at land time, before/after in the landing
  verdict, and **every decrement names its disposition** — a `distinct`
  rename without a tape rationale is the gamed metric, recorded
- a tape entry written with `rationale`, `alternatives_rejected`, `red_proof`

**Exit, whichever comes first:**

- metric reaches 0, **or**
- an explicit `descoped` transition closes the remaining bars, **or**
- a kill condition fires (see the bar rows)

**No silent caps.** If an iteration bounds its own scope — top-N call sites,
a deferred sub-case, a skipped surface — the landing verdict names what was
dropped. A capped iteration that reads as complete is the same lie one scale
down.

**Loop checkpoint.** After roughly four iterations there is enough method to
transmit, and a negative result is still cheap. That is when `nc-r1` runs —
pre-registered first, per §6.3. Running it at the end would learn the same
thing far too late to act on.

### 4.3 Tail — three orders

| Order | Does | `serves` |
|---|---|---|
| `nc-r1` | the held-out replay arm, two red lines scored separately | `nc-replay` |
| `nc-r2` | census against a non-Rust repo (may land early, from the head) | `nc-portable` |
| `nc-r3` | generate `TARGET_ARCHITECTURE.md` from the register; retire `SYSTEM_OVERVIEW.md` as the contract | `nc-doc` |

`nc-r3` is where the campaign's whole argument gets tested on itself: if the
architecture cannot be generated from the register, the register was never
the source of truth and the last row of §8 is `failed`, not `met`.

### 4.4 Order conventions

Beyond the standard `work-order/v1` frontmatter:

- **`serves: noun-convergence <bar-id> ...`** — attaches to the initiative
  and the specific bars, so `co-lineage.py` can compute coverage against
  bars rather than counting closed orders.
- **The landing verdict reports metric-before and metric-after.** This is
  the structural mitigation for the failure that minted `initiative-bars.toml`
  — sixteen orders landing green while the headline never moved, found by
  hand four months in. Here, an order that lands without moving the metric
  renders LANDED-BUT-UNMOVED at landing time.
- **Diagnosis before fix on a red rung** — the `t1h-failure-taxonomy.md`
  precedent. A failed iteration gets a taxonomy, not a patch.
- **Generated closure artifacts.** `bars.md` renders from the score JSON,
  never hand-typed; each rung ships a `verify-*.sh` a third party can re-run.

### 4.5 Demos

A refactor with no visible artifact is where funding dies.

| Demo | Shows | Closes at |
|---|---|---|
| **A** | `svrn converge census` on this repo → the 278 register, live | `nc-t0b` |
| **B** | the same command on a TypeScript repo → a register comes out | `nc-r2` |
| **C** | the held-out replay, both red lines | `nc-r1` |
| **D** | six generated pages against the 265KB they replace | `nc-r3` |

A and B are cheap and both land in the head.

### 4.6 Concurrency

Thirty-three orders exist on disk; deep-research `t2a` landed with `t2b`
drafted and gated. Running a second large initiative at full tilt against one
worker pool is the obvious way to stall both.

**Recommendation: run the head now, hold the loop.** `nc-t0` through `nc-t0c`
touch no production code, deliver DEMO-A and DEMO-B, and make the disease
visible on a dashboard. Their value stands alone even if the loop never runs
— which is the property that makes it safe to start before committing to the
whole program.

**Worker contention is not the only collision — scope overlap is** (added
2026-08-17). Phase 2 (the verifier cores) sits on ground native-grounding
is actively cutting: judge-ladder tombstones landed 2026-08-14 with a
settling review-by of 2026-09-13, and further judge swaps are being
pre-registered. Converging judges mid-surgery is a merge conflict at
initiative scale — sequence `nc-one-verifier` after that settling pass, or
fold: native-grounding's judge deletions *are* convergence work and can
carry tape entries. Two overlaps run the other way and are pure wins: the
`H5` wire types (`GroundingVerdict` + typed segments) are the newest, most
active wire types and belong to `sovereign_wire` from day one; and
deep-research's ICD family is the ratchet's natural first customer — its
`Verdict` is already the canonical enum, so tenancy is an import swap, and
its next order should land against the register rather than extending
`icd.rs`.

---

## 5. Verification

The hardest constraint: **the measurement instrument is itself part of what
is being refactored.** Resolved by capturing goldens as *artifacts*, before
any change, and replaying every subsequent claim against them.

Three tiers. Each noun declares which one proves it, recorded in its
`CONCEPTS.toml` row.

### 5.1 Freeze — before anything else

Freeze **artifacts, not numbers.** A number produced by machinery you are
about to modify is not a baseline.

| Domain | Frozen | Reuse (§19 — do not rebuild) |
|---|---|---|
| Bench transcripts | every lane's raw transcript at HEAD | `bench_cmd/situated/transcripts.rs` — already re-scores without generating |
| Judge cases | recorded (claim, evidence window) pairs | `bench_cmd/judge_replay.rs` + the gate-audit forensics ledgers |
| HTTP traffic | request/response on :9741, :9742, :8080 | new — thin recording middleware |
| Desktop journeys | trace + screenshot per spec | 77 Playwright specs under `sovereign-desktop/tests/e2e/specs/` |
| CLI surface | `--help` + output for the 170 `[[command]]` rows (the contract's other 226 rows are journeys, steps, experiences — exercised by the journey lane, not `--help`) | `cli-contract-live-verify.sh` |
| Deterministic turns | byte-stable, no model | `DeterministicInference`, 31 existing sites |
| Mesh custody | fan-out and locality | `knowledge_fanout_e2e.rs`, `local_only_corpus_locality.rs`, `corpus_sharing_over_iroh_e2e.rs` |

Every golden is fingerprint-stamped, per the `judge_replay` rule: **a
candidate configuration is a build, not a flag.**

### 5.2 Differential — three comparison modes, one mechanism

Old and new, same frozen input, compared. The *mode* varies; the mechanism
does not.

| Mode | When | Comparison |
|---|---|---|
| **exact** | pure functions — scorers, formatters, parsers, `From` conversions | byte-equality, in CI on every commit |
| **shadow** | live paths — retrieval, the turn, the wire | both run, new is compared and logged, never served; promote at zero divergence over N runs |
| **statistical** | LLM-in-the-loop | non-inferiority via the rubric core's Wilson intervals; significant only when the two 95% intervals are disjoint |

Statistical mode is the reason phase 1 comes first: until the bench is
fingerprint-keyed, its result cannot be trusted to mean what it says.

Most work lands in *exact* mode — the unit of work for most nouns is
literally a conversion plus a property test that the round-trip is lossless.

### 5.3 Journey — the user-visible contract

77 Playwright specs, `sovereign contract census` + `nightly`,
`smoke-attach-mode.sh`, `./scripts/sovereign-ci-bench.sh --quick`.

**A gate, never a design tool.** A defect first caught here means the
differential was missing, and writing it is part of the fix.

### 5.4 The binding rule

> A noun may not pass coalesce-step 3 until its differential exists and
> passes. A phase may not proceed until the prior phase's journey bar is met
> on the frozen goldens.

And the honesty clause (§18.1): **a differential never observed to fail is
not a differential.** Each ships with a deliberately-broken input proving it
goes red.

---

## 6. The harness

### 6.1 Why the tools matter more than the refactor

**Re-derivation is not our disease; it is *the* disease of large
agent-assisted codebases**, and it worsens for a structural reason: an agent
writing code has excellent local context and no cheap way to know the
concept already exists three crates over. It will faithfully write a correct
new `ChatMessage`. Seven times.

Nothing in the toolchain catches that. The arch-gate counts lines; review
catches it only when the reviewer happens to know. This is the failure class
§19 was minted for — added 2026-08-08 after the pattern recurred a third
documented time, **each catch coming from the operator, never from the
builder's own process.** A harness that mechanically answers *does this
concept already exist, and who owns it?* is the missing tool. Building it by
using it is the honest way to find out whether it works.

Day-one surface is four verbs — ceremony is a bug:

```
svrn converge census      # what's duplicated, ranked
svrn converge freeze      # capture goldens, fingerprinted
svrn converge diff <noun> # is it safe to flip?
svrn converge status      # 278 → N, and what moved
```

`replay` arrives when tapes exist; `export` when a second codebase does.
Registering a noun is editing `CONCEPTS.toml` — that needs no verb.

**The harness is a workflow.** Six steps over typed artifacts with
content-addressed caching *is* `Step`·`Artifact`·`Runner`. So the tool
performing the refactor is built on the abstraction the refactor exists to
promote — which is the strongest available evidence the substrate is real.
If `sovereign-workflow` cannot express its own promotion, it was never the
right abstraction.

### 6.2 The replay tape

One append-only entry per migration — the first tenant of `Record`.

```toml
[[migration]]
noun          = "Verdict"
commit_before = "c999974e"
census_hash   = "b3:8f2a…"

  [migration.decision]
  canonical  = "sovereign_contracts::verdict::Verdict"
  totality   = "four states; CouldNotJudge and NeverRan carry a Reason"
  rationale  = """
    Contracts is the lowest tier all ten consumers already depend on.
    Four states not three: a gate that did not execute is not a gate
    that abstained (§18.2)."""
  alternatives_rejected = [
    { option = "keep per-crate Verdict, add a shared trait",
      reason = "a trait does not stop an eleventh definition" },
  ]

  [migration.verification]
  mode      = "exact"
  goldens   = "b3:aa01…"
  result    = "byte-identical on 253/253 sites"
  red_proof = "b3:3d5e…"   # the broken input that proved the check fails

  [migration.outcome]
  verdict      = "landed"
  census_after = "b3:9d11…"
  metric       = { before = 278, after = 268 }
```

Three fields are non-negotiable. **`rationale` and `alternatives_rejected`**
are the part a replaying agent cannot re-derive — a tape without them
replays keystrokes, not judgment. **`red_proof`** because §18.1. The census
hashes bracket the entry, so a tape is self-verifying.

### 6.3 What "replayable" means — two levels

**Level 1, verbatim · deterministic.** Given `(commit, CONCEPTS.toml,
goldens)`, replay produces byte-identical diffs. Checkable, and the
acceptance test for the harness — but it only proves tapes replay, which is
a property of tapes.

**Level 2, method · convergent.** Given the census tool, the harness, the
goldens and the method — but **not** the register — a fresh agent derives
its own register and reaches an equivalent architecture. Different names,
possibly different boundaries, same shape and same metric.

**The experiment** — order `nc-r1`, run at the loop checkpoint (§4.2), after
roughly four iterations have produced a method to transmit and while a
negative result is still cheap. Pre-registered before it runs. Withhold one
iteration's register rows and tape entries, then score **two red lines,
never blended**:

| Red line | Metric | Bar |
|---|---|---|
| Did it find the same things? | set overlap on concept identity | ≥ 0.8 |
| Did it produce a working system? | goldens pass, metric moves equally | binary |

High overlap with failing goldens means the census is good and the
transformation is not. Passing goldens with low overlap means the harness
converges but the method does not transmit. **Both are findings; neither is
a pass.** One blended score would hide exactly the failure worth seeing.

### 6.4 What generalizes

**Free — works on another codebase today.** The census is SQL over the SCIP
schema, and `corpus-engine-scip/src/scip_export.rs` already drives five
exporters: `rust-analyzer`, `scip-go`, `scip-typescript`, `scip-python`,
`scip-java`. Point it at a TypeScript monorepo, run the exporter, and the
duplicate-concept register comes out. Nothing to port. Also free:
`CONCEPTS.toml`, the metric, `status`, `replay`.

**Needs a binding.** The ratchet (one CI entry point per build system), the
codemod scaffold (per-language rewriting — the six-step *shape* is
universal, the mechanics are not), golden capture (one adapter per surface
kind).

**Does not generalize, and should not pretend to.** The totality rules —
what must be total on `Evidence` is a fact about *this* product. Tier
boundaries are architecture. The statistical mode is specific to
LLM-in-the-loop systems.

**So the product is a harness with an opinionated method, not a
push-button.** A smaller claim than "automatic refactoring", and the one
that survives contact with a second codebase.

### 6.5 What cannot be replayed

| Not replayable | Why | Mitigation |
|---|---|---|
| The canonical-name choice | judgment | recorded with rationale |
| Where a noun's boundary sits | architecture | recorded in rejected alternatives |
| Whether a totality rule is *right* | design | proven only by goldens staying green |
| LLM-in-the-loop steps | non-determinism | fingerprinted, bounded by non-inferiority |

One structural limit: **a level-2 replay landing a different architecture is
not necessarily a harness failure.** It may be better. The experiment
measures agreement, not correctness — which is why goldens are a separate
red line.

---

## 7. Risk

| Risk | Mitigation |
|---|---|
| Stalls mid-surgery, two type systems coexist | six independently-revertible PRs per noun; the old type survives to step 6. Stalling leaves aliases, not breakage. |
| Phase 5 overturns the no-trait decision and is wrong | last large phase, gated on the configuration-matrix test existing first. If that test shows the current shape is adequate, descope. |
| Fingerprinting invalidates every existing baseline | expected and correct — they are comparable by luck today. Phase-0 goldens are the bridge; re-mint per `RUNBOOK.md` §6. |
| The ratchet becomes bureaucratic tax | fires only on *registered* concepts. An unregistered duplicate is a finding, not a build break. |
| Agents re-introduce duplicates faster than removal | the ratchet is the answer — precisely the failure the line-count gate cannot catch. Measured live: `deep_research/icd.rs` re-minted five register nouns the week this program was drafted. |
| The register's own prose decays | measured half-life of one day for row qualifiers (2026-08-17 verification: six rows stale, every raw count fine). Mitigation is structural: rows carry a `measure` method the tooling re-runs and a dated `today`; the landing verdict re-measures; prose is never the number's home. |

---

## 8. Exit criteria

Every "today" figure below is a dated snapshot; the binding baseline for
each is **minted at `nc-t0b`** with its method declared in the artifact,
and `svrn converge status` re-runs the methods — nobody tracks these by
recall.

| Metric | Today (snapshot) | Target |
|---|---:|---:|
| Census names with no disposition | 279 (2026-08-20, first-party production, segment-scoped) — was 272 on 2026-08-17, when the scope filter still hid `deep_research/` | 0 |
| Register rows converged | 0 | all `in_program` rows |
| God-object score (`Runtime`, methods × files) | 133 × 32 (2026-08-17) | < 40 × 5 |
| Reconciliation machinery | ~21,500 first-party lines (2026-08-17, incl. the v1 table's own exclusions) | ≤ ⅓ of the minted baseline |
| Untyped daemon-boundary sites | ~1,250 (2026-08-17: 377 `json!` + 686 `.get("` + ~131–163 raw reqwest, CLI prod; 67 desktop `json!`) | 0 |
| Desktop commands on `Result<_, String>` | 246 / 256 (2026-08-17) | 0 |
| Raw sharing-policy bool sites | ~149 (2026-08-17; ~5:1 plumbing:guards) | plumbing 0; guards remain, typed |
| Evidence paths with no typed custody | all | 0 |
| Architecture doc | 265 KB, hand-maintained | ~6 pages, generated |
| Level-2 register agreement | — | ≥ 0.8, goldens green |

Track the first two rows weekly on the existing `--tighten` cadence. The
rest follow from them.

**The claim, stated so it can be judged:**

> We took a 955k-line codebase whose census surfaced ~270 same-named type
> clusters, dispositioned every one — converging the true duplicates,
> renaming the coincidences apart, mirroring foreign schemas once — and
> recorded the process such that a fresh agent replaying the method —
> without the answers — reaches ≥0.8 register agreement with the goldens
> still green. The tools run on Rust, Go, TypeScript, Python and Java, and
> the census works on any of them today.

Every number there is measured or falsifiable. **None of it is true yet.**

---

## 9. First move

Phase 0, item 1: **parse the SCIP descriptor into `kind`.** Smallest change
in this document, needs no re-index, unblocks every detector for every
consumer rather than for one script. Then D1 with its filters, the
hand-adjudicated precision audit, and the concept ratchet — baseline
**minted by the tool at that moment**, filters declared in the artifact,
not carried from this document (every snapshot in it is already a day
stale, by design of the universe).

Roughly a week, and at the end the disease is visible on a dashboard and
monotonically decreasing — which is the thing currently missing, not
analysis.

---

## 10. The unnamed half

**Status:** addendum, 2026-08-20. Proposed, not approved, nothing landed.
Measured at HEAD `66ef25bf` against the SCIP graph re-exported 19:20:53Z
(1,631,325 refs, all with spans, `last_indexed_head` = HEAD) plus `git log`
and `cargo metadata`.

**These figures were hand-measured and therefore NOT MINTED**, which by this
document's own rule (§1, "the numbers are minted, not typed") made them week-old
fish on arrival. Each subsection named the tool that should own its number.

**§10.1, §10.2 and §10.3 now have theirs**, landed 2026-08-20:
`svrn code converge roles` (population and adoption per role) and
`arch_metrics::type_spreads` (the per-crate share, rendered by `arch-report`).
Every figure in those three subsections below is re-derived from the SCIP graph
at `6a6b1317` and carries that stamp. Where the instrument disagreed with the
hand figure the instrument won, and the correction is stated in place rather
than quietly applied — three of them are large. The remaining subsections are
still hand-measured.

### 10.1 The census keys on names, and the larger half has none

§1 says the system re-derives what it should carry, and counts it by name:
278 concepts typed in more than one crate. That count is real and the loop in
§3 is the right response to it. It is also structurally blind to the bigger
half.

Three concept families, measured as distinct first-party production type
definitions with `reach` = distinct referencing crates:

Minted by `svrn code converge roles` at graph `6a6b1317`. Membership is the
family's head nouns — for the first row, the campaign ladder's own published
`*Result *Outcome *Verdict *Status` — matched against the last CamelCase
segment of every type name. No list of member types exists or is maintained.

| family | types | crates | reach ≥ 3 | best |
|---|---:|---:|---:|---|
| verdict / judgement | 334 | 39 | 10% | `Result`, reach 20 |
| citation / provenance | 79 | 23 | 10% | `NoteSource`, reach 9 |
| freshness / staleness | 3 | 3 | **0%** | `Freshness`, reach 2 |

416 types at 10% adoption by NAME. The hand figures were 198 / 112 / 41 at
roughly 2%, and all three moved: the verdict family is 69% larger than the hand
count, citation 29% smaller, and freshness collapses from 41 to **3** — because
almost nothing in that family is NAMED for it, which is the section's own
thesis arriving as a number.

The name half is not the whole population. Counting instead by FIELD — a type
declaring `generated_at` is answering a freshness question whatever it is
called — the same run reports:

| family | carriers | named for some OTHER role |
|---|---:|---:|
| verdict / judgement | 89 | 63 (71%) |
| citation / provenance | 78 | 75 (96%) |
| freshness / staleness | 22 | **22 (100%)** |

That last column is the size of the blind spot, measured rather than asserted.
Every single freshness carrier in the workspace is called something else, so no
name-keyed census can ever reach one. `AuditReport` and `DriftReport` are not
the same name; neither are `StalenessSummary` and `lags_graph`.

Four exhibits that this is one concern and not three:

- `sovereign posture` prints seven rows in **seven status vocabularies**
  (`fresh`, `stale`, `fail (stale)`, `off (by design)`, `present`,
  `present (gaps)`) and **seven age formats** (`12d`, `1h`, `16d`, `7d ago`,
  `-`, `6d`, `9d..95d`). It is an aggregator over a concept with no type, so
  each subsystem it aggregates invented its own.
- **26 hand-written freshness fields in 10 spellings** (graph `6a6b1317`):
  `generated_at` 5, `age_hours` 4, `stale` 3, `built_at` 3, `as_of` 3,
  `staleness` 3, `age_secs` 2, `indexed_at` 2, `computed_at` 1; `age_days`,
  `freshness`, `lags_graph` and `commits_behind` are declared on no type at
  all. Three concepts — when was it made, how old is that, is it too old —
  ten ways.

  **This row read "172 fields in 13 spellings" until 2026-08-20**, and 172 was
  a count of something else: a bare substring grep over every `.rs` file, which
  returns 175 today and is mostly struct-literal initializers, JSON keys and
  comments rather than field declarations. The graph's field rows were checked
  against `RegistrySnapshot` by hand — four fields declared, four rows — before
  the correction was accepted. The spellings were right and the shape of the
  finding survives; the magnitude was 6.6x too big, and four of the thirteen
  spellings name no field anywhere.
- ARCH §18.2's four verdicts appear in **17 files**, against 198
  verdict-shaped types. The most-cited principle in the house is the
  least-typed concern in the codebase.
- `concept_gate.rs` carries an arm that REFUSES a response with no
  `freshness` field — added after observing the failure live on 2026-08-20.
  It wants the envelope badly enough to hand-check for it.

**Owner:** `svrn code converge roles` — the role tier beneath `census` (names)
and `dry-report` (behaviour). Landed 2026-08-20; `corpus-engine-scip/src/roles.rs`.
A mirror, not a gate: no threshold, no exit code, nothing to ratchet.

### 10.2 The sprawl is inside crates, not between them

The program's mental image is crates re-deriving each other's nouns. Measured,
the crate graph is healthy and the mass is intra-crate.

Healthy, four ways: **0** of 441 load-bearing types carry an upward edge;
`layer-gate` exits 0; **18** of 28,847 co-changing file pairs cross a crate
boundary with no structural reference behind them; the adapter tax is 26
`impl From` plus 57 `#[from]` workspace-wide. Crate boundaries match the
change patterns. There is also a real downtown — `sovereign-contracts` +
`oicp-types` + `kernel-types` are 22k lines, 2.6% of the code, carrying 57%
of cross-crate type traffic at 773–1,266 refs per kloc.

The defect is one number: **44% of all 5,139 first-party production types are
referenced by no other file at all.** Used only in the file that declares
them. 28% are exported; 28% are used in 2+ files of their own crate.

Minted by `arch_metrics::type_spreads` at graph `6a6b1317`, rendered by
`svrn code arch-report`. A type is bucketed by the WIDEST reference to it, so
the three buckets partition the population exactly — which the hand figures did
not (46 + 29 + 18 = 93).

| crate | types | private | crate-local | exported |
|---|---:|---:|---:|---:|
| sovereign-contracts | 284 | **13%** | 2% | 85% |
| oicp-types | 70 | **0%** | 4% | 96% |
| corpus-engine | 887 | 28% | 38% | 34% |
| sovereign-tools | 454 | 37% | 26% | 36% |
| sovereign-core | 367 | 39% | 32% | 28% |
| sovereign-mesh | 343 | 59% | 27% | 14% |
| **sovereign-cli-llm** | 608 | **75%** | 25% | **0%** |
| sovereign-cli-dev | 135 | 76% | 24% | 0% |
| sovereign-desktop | 213 | 78% | 22% | 0% |
| sovereign-server | 84 | 80% | 20% | 0% |
| sovereign-cli | 93 | **85%** | 15% | 0% |

Same authorship, same five months, 0% to 85% private. Some of that spread is
correct Rust — a one-endpoint DTO should be private — but it is not idiom that
five crates export literally nothing; it is the absence of anything to reach
for.

**Two corrections, and the hand figures held up better than §10.1's.** Every
private and exported SHARE above lands within 6 points of the hand-measured
one, on all seven crates it named — the shape of this section was right. What
was wrong is the denominator: **8,882 counted enum variants as types.** Types
plus variants at this commit is 8,764; top-level type definitions, the
population `converge census` and `converge roles` both use, is 5,139. The
crate-local figure moves with it, 18% to 28%.

`sovereign-cli-llm` is the extreme and it is not a CLI: `enrich_cmd` is 32,813
lines and `bench_cmd` is 29,460, whole subsystems inside a leaf binary that
exports nothing. Neither the desktop nor the daemon nor MCP can reach them, so
anyone needing enrichment orchestration re-derives it. And
`sovereign-desktop` depends on `sovereign-cli-daemon` — a **binary** — while
not depending on `sovereign-cli-shared` at all, yet carries
`enrich_commands.rs`, `atlas_commands.rs`, `mesh_commands.rs`,
`recipe_commands.rs`, `governance_commands.rs`. Two hosts implementing the
same subject areas, sharing nothing.

**Owner:** `arch_metrics::type_spreads`, landed 2026-08-20 beside
`instability` as `CrateMetrics::types` and rendered by the one
`render_markdown` — as this row specified. `None` there means the census did
not run, which is not the same as a crate whose types are all private.

### 10.3 Adoption is predicted by work carried, not by shape

Why did some concepts converge without any program telling them to?

Minted by `svrn code converge roles` at graph `6a6b1317`. A role is the last
CamelCase segment of a type's name, so `AuditReport`, `DriftReport` and
`FieldglassReport` are one role and nobody maintains a list saying so.
Adoption is the share of a role's types reaching 3+ distinct crates — the same
cut §10.1's table uses, computed once.

| role | what the abstraction does for you | population | adoption |
|---|---|---:|---:|
| Scope | admission | 11 | **55%** |
| Tool | dispatch + execution | 110 | 53% |
| Store | persistence + query | 39 | 44% |
| Registry | dispatch | 27 | 30% |
| Config | nothing | 130 | 15% |
| Error | ~nothing (`thiserror` makes minting free) | 86 | 10% |
| Entry / Args | nothing | 83 / 55 | 5% |
| Summary / Report | nothing | 63 / 102 | 3% |
| **Response** | nothing | 134 | **2%** |

Still monotone in work carried and in nothing else — the ordering the hand
table asserted is the ordering the instrument returns, which is the part of
this section that mattered. Three details changed:

- **Every adoption share is higher than the hand figure**, and the high-work
  end moved most: Tool 29% → 53%, Store 35% → 44%, Config 7% → 15%. The gap
  between "carries work" and "carries none" is wider than §10.3 claimed, not
  narrower.
- **`Registry` and `Scope` should never have shared a row.** Hand-grouped at
  17%, they measure 30% and 55% — Scope is the single best-adopted role in the
  workspace, and burying it in a slash-pair hid that.
- **`Args` is not 0%.** Three of 55 reach three crates. The claim "no shared
  type exists" still holds; the adoption figure was too clean.

`Report` at 3 of 102 remains the control experiment, and it still reads the
same way: the most obvious "shared vocabulary" candidate in the codebase, and
it never spread.

**One row of the hand table has no instrument and is left as measured:**
`Recipe`, at "46 TOMLs, ~100%". Those are recipe DATA files, not Rust types, so
a type-graph census cannot see them — it reports the `Recipe` type role at 2
types / 50%, which is a different fact about a different population. A
data-file census is a different instrument and this one does not substitute for
it (§18.3).

This is the design criterion the program has been missing:

> **Extract work, not shape.** A shared struct saves an author nothing and
> loses to bespoke every time, gate or no gate. A shared thing that renders,
> persists, dispatches or admits wins on cost with no enforcement at all.

`Report` at 3 of 102 is the control experiment, already run. It is the most
obvious "shared vocabulary" candidate in the codebase and it never spread,
because a report is data and data-shaped abstractions do not pay.

### 10.4 Why convergence alone will not hold

§6.1 has the diagnosis half right — an agent "has no cheap way to know the
concept already exists three crates over." The other half is that knowing
does not pay. From the author's seat:

```
mint bespoke : write the struct in the file already open.
               seconds, zero discovery, zero blast radius, no peer collision.
reach shared : discover it -> read it -> judge fit -> maybe edit a crate you
               do not own -> callers() -> wider diff -> review.
```

Fifty to a hundred times the cost, carrying risk the bespoke path does not.
A cost-minimising author picks bespoke, and is right to. **No ratchet changes
that inequality; it only adds a penalty to the losing option.**

The register closes the rename-apart hatch for names it tracks — §1 is right
that a `distinct` rename must record its rationale. It cannot close it for a
concern that never had a name, and `concept_gate.rs`'s own remediation text
offers "converge it onto one owner, **or rename it apart**" as equal
branches. `AuditReport`, `DriftReport`, `ArchReport`, `DryReport`,
`FieldglassReport`.

Four places an intervention can land, by cost to the author:

| where | mechanism | cost | status |
|---|---|---|---|
| the order | `seams:` in `order.md` | zero, it is in the prompt | field exists, unpopulated |
| context | `session-boot` / `inject-notes` | zero | needs a vocabulary to inject |
| the decision | `PreToolUse` on Edit/Write | near zero, answer precomputed | proven by `prefer-code-intel.py` |
| after the edit | `concept-gate` | rework | weakest; teaches rename-apart |

The first three narrow the inequality. Only one reverses it: **a scaffold.**
`svrn code scaffold <role> --name Foo` emitting a file already wired to the
shared trait makes reaching *cheaper* than minting. The precedent is native —
recipe authoring works this way, which is exactly why the corpus axis of
`nc-extends` is the one that scores DATA.

### 10.5 The three blocks

**Complete `kernel-types`.** ARCH_LAYERS.toml:85 already declares it "the
NEUTRAL kernel — identity + provenance." It holds `Custody`, `Grain`,
`Source`, `Attribution`, `ContentHash`, `CorpusId`, `Locator`, `Origin` —
1,210 lines at 897 refs/kloc — and stopped before freshness and verdict. The
envelope's home exists and was minted for it. Likely shape is an embedded
`Provenance` field plus an accessor trait, **not** a `Judged<T>` wrapper;
generic wrapping is painful in Rust and `fieldglass::Honesty` (25 fields) is
already a hand-rolled instance of the flat version. Only three crates depend
on `kernel-types` today, so this is a distribution problem as much as a
design one.

**Move `sovereign-cli-shared` to `capabilities` (L4), then give it `Report`
and `Args`.** The layer move is a precondition, not hygiene: hosts are
terminal, so at L6 the desktop cannot legally depend on it — it is why the
two hosts share nothing. `Report` is then the *renderer of the envelope*, not
a peer abstraction: the honesty footer is the envelope rendered. Targets are
64 `fn render_markdown`, 549 fixed-width column specs, 118 `to_string_pretty`.
`Args` targets 1,075 flag match arms over 553 distinct flags — and
`cli_contract.rs` is already 1,473 lines that AUDIT the CLI surface they
cannot generate.

**Relocate `enrich_cmd` and `bench_cmd` into `capabilities`.** 62k lines,
no design work, largest single mass available. The CLI verb collapses to arg
parsing over a library call and the desktop gets the capability for free.

### 10.6 The behaviour half — converge and delete

`dry-report` is the third tier and it needs an instrument fix before its
numbers are usable. **It has no source scope.** Headline: 3,151 exact groups,
1,056 near clusters, ~96,381 redundant lines. Filtered to first-party
production Rust: **540 clusters, ~13,974 lines — 14.5% of the headline.** The
rest is `target/` build artifacts (one vendored llama.cpp Python function
counted seven times across build-hash dirs), `vendor/`, `research/`, and
`bench/external/` SWE-bench fixtures. `converge::SourceScope`
(`converge.rs:86`) already carries exactly these exclusions — repaired
2026-08-20 when this campaign's own instrument-validation rung found
`"research/"` swallowing `deep_research/`. `dry_report` does not use it.
§10.6-of-ARCH finding, one-line fix, and until it lands anyone acting on the
headline deduplicates build output.

Corrected, duplicated behaviour concentrates where the type census cannot
look, because these are functions:

| concern | clusters | redundant lines |
|---|---:|---:|
| parse / args | 49 | ~1,430 |
| stream / inference | 24 | ~1,163 |
| text extraction | 39 | ~1,043 |
| freshness / stale | 13 | ~402 |
| render / format | 10 | ~229 |
| verdict / judge | 9 | ~206 |

Named targets, deletable with `code redirect` today:

- `cmd_index` (2×111), `run_incremental` (2×91), `cmd_watch_status` (2×68),
  `decide` (2×41) — four verbs implemented twice, split
  `sovereign-cli-dev` / `sovereign-cli`. ~311 redundant lines.
- `strip_html` (2×118) and `strip_mediawiki` (2×139), both split
  `corpus-engine` / `sovereign-tools`. The `strip_html` pair has DRIFTED —
  one copy lacks a `</script>` fix and silently truncates crawled HTML. A
  live correctness bug living in a clone pair.
- `complete_stream` — **LANDED, rung `nc-17`, 2026-08-21. "12 copies" was a
  clone-detector artifact and the real shape is better.** The 12 are mostly
  12-14 line `#[cfg(test)]` `unimplemented!()` stubs, which rhyme without
  sharing a concept. What the report was actually pointing at is a
  **with-finish / without-finish SPLIT**: one streaming decider written twice
  per call shape, because someone needed a finish-reason out-parameter and
  copied the body rather than adapting it.

  - `sovereign-inference/src/embedded/engine.rs` — `complete_stream` was a
    285-line mirror of `complete_stream_with_finish`, and its own doc comment
    said so ("Mirrors `complete_stream`'s slot-routing dance"). Cosines
    0.952-0.965 across three sub-region pairs at 120 / 110 / 101 lines.
  - `sovereign-mesh/src/peer_inference.rs` — the same split one level up:
    `complete_stream_with_id` was a 197-line mirror of
    `complete_stream_with_id_and_finish`, cosine 0.969.

  **All four copies had DRIFTED, and the drift was the prize (§10.6's
  `strip_html` shape, four more times):**

  1. The typed twin grew the Raw/FIM `generate_stream_sync_fim` fork
     (`INLINE_COMPLETION.md` §4/D8); the legacy copy never did, so inline
     completion arriving on `complete_stream` re-prefilled the whole window
     on every keystroke instead of the typing delta.
  2. `generate_stream_dispatch`'s legacy arm passed `slot_ctx.ctx_mut()`
     rather than `slot_ctx` — the raw-context downgrade that costs the
     prefix cache, on a comment that already flagged it.
  3. `sovereign-core/src/pipeline/runner.rs`'s inline frames→text adapter
     matched `Finish { .. }` and dropped it, but `EmbeddedLlamaCpp` reports a
     mid-stream failure ONLY as `Finish { reason: FinishReason::Error(_) }`,
     never as `StreamFrame::Error`. **Every engine-side stream failure on the
     presenter path arrived as a clean end of stream.** The user saw a short
     answer, not an error.
  4. **The worst one.** `send_stream_piece` — the deadline-bounded send that
     converts a half-open SSE client's *indefinite* slot pin into a bounded
     one (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.2; one such client takes
     the node out) — existed ONLY on the legacy `Result<String>` path. The
     typed path, which is every streaming chat completion, used a bare
     `blocking_send`. The hardening, and the RED-FIRST test that proved it,
     were guarding the half nothing streamed on.

  Converged to one body per shape: `complete_stream` delegates to
  `complete_stream_with_finish` through `frames_to_text_stream`
  (`sovereign-contracts/src/traits.rs`), the ONE frames→text adapter, which
  had been hand-written three times. `StreamSink` stopped being an enum,
  `generate_stream_sync` (260 lines) went with the arm that reached it, and
  the send policy now lives in the one sink every decode loop goes through.
  The liveness tests were retargeted onto it and watched to fail first.
- `ctx` construction — 24 copies across two clusters.

Note the complement: verdict, freshness and render are near the BOTTOM of
this table and the TOP of §10.1. Those concerns are re-declared as types
everywhere while each instance's logic stays small and locally different, so
the clone detector barely registers them. **The two instruments see different
halves and the halves need different fixes** — §10.1 needs an abstraction
minted, §10.6 needs losers deleted.

### 10.7 Instruments are mirrors here, not gates

Operator direction, 2026-08-20: **win by being useful and better, not through
force — applied as a software architecture.**

That retires two things this addendum's drafting had proposed: a per-crate
private-type ratchet and a crate-mass ratchet. Both are force. Both would sit
red on `sovereign-cli-llm` indefinitely and be switched off inside a week —
the failure mode `concept_gate.rs`'s own doc comment already names.

What replaces them is a pre-registered adoption test, stated before the work:

> Build the envelope and `Report`. Convert three commands — `atlas_drift_report.rs`
> (48 single-use types), `atos_cmd/run.rs` (43), `cache_audit_cmd.rs` (25).
> Then **do not mandate it.** No gate, no ratchet, no preflight check. Measure
> adoption across the next N new or edited commands. If authors pick it up
> unprompted it earned its place. If they do not, it was not better than what
> they would have written, and it is deleted rather than enforced.

That is the same standard `Report` already failed at 1 of 105, applied
honestly and in advance.

**`nc-extends` cannot serve as the outcome metric as instrumented** — note
`d8cd40a1`, measured by the seat 2026-08-20. The tool axis counts 112
`impl Tool for`, 30 of them in `studio/`, which the campaign declares out of
scope by name; a perfect `nc-13` leaves 30 > 0 and the axis still fails. The
intent axis requires zero files naming an `Intent` variant, which `nc-14`
correctly refuses on the grounds that matching a closed enum is what enums
are for. Both corrections are alignments with rules this campaign already
declared, both are operator-only under "bars move by measurement only," and
§18.6 requires reporting movement-by-CODE separately from
movement-by-RE-CLASSIFICATION. Until that decision is made, the honest
outcome measures are the three below.

| Metric | Today (snapshot, 2026-08-20, un-minted) | Direction | Owner |
|---|---:|---|---|
| Types referenced by no other file | 46% (8,882 types) | down | `arch_metrics` |
| Role adoption, `Report` | 1% (1 of 105) | up | `converge roles` |
| Role adoption, `Args` | 0% (0 of 58) | up | `converge roles` |
| Congestion: distinct crates per `.rs` commit | 2.30 (Aug, part month) | flat or down | git, monthly |
| First-party redundant lines | ~13,974 | down | `dry-report` + `SourceScope` |

Congestion by author month, which is the one trend measurable without new
tooling: **1.84 (Apr) → 2.82 → 3.08 → 3.21 (Jul) → 2.30 (Aug, through the
20th).** It rose 74% across the accretion period and has bent down in the
month this campaign has been running. Hold it loosely — the month is
incomplete and composition may explain it — but that is the number that says
whether any of this works. Bucket by **author** date: this history was
rewritten around 2026-08-11 and committer dates all cluster there, which
silently collapses five months into two quarters.

### 10.8 What makes this fail

| Failure | Tell it is happening | Guard |
|---|---|---|
| The envelope is data-shaped and does not pay | it lands at 2–3% like `Report` or `Response` | it must carry rendering, age computation, staleness banding and the footer, or it is not worth building |
| `sovereign-contracts` becomes the megablock | already absorbs 38.9% of inbound type traffic | the envelope goes to `kernel-types`, not contracts; `Report`/`Args` go to `cli-shared` at L4 |
| Premature convergence — everything crammed into one mediocre type | adoption rises only where mandated | adoption is measured, never required; the delete branch is real |
| Acting on an instrument that overstates | `dry-report` at 7× | scope every instrument before quoting it; §10.6 |
| A gate teaches the workaround | rename-apart is free and unmeasured | `converge roles` counts it from the other end — shipped 2026-08-20, and it sees `AuditReport`/`DriftReport`/`ArchReport` as one role however they are spelled |
| Numbers in this section rot | they already are | every row above names its owning tool; §10.1, §10.2 and §10.3 are minted and re-runnable, the rest are not yet |

### 10.9 Sequence

1. **`sovereign-cli-shared` → L4 `capabilities`** (note `c0c2f007`). Narrow
   the `"sovereign-cli*"` glob at ARCH_LAYERS.toml:174 to the four binaries
   AND add the explicit entry — both halves, because
   `arch-layers/src/lib.rs:298-313` has no specific-beats-glob precedence and
   two matches is `AmbiguousCrate`. Half a day, and it is what makes the
   shared crate reachable by the second host.
2. **`SourceScope` into `dry-report`.** One-line reuse; makes tier three
   quotable.
3. **The four twin verbs and `strip_html`.** Proves the kill-chain end to end
   on already-duplicated behaviour, needs no new abstraction, and closes a
   live HTML-truncation bug.
4. **The envelope in `kernel-types`**, then `Report` as its renderer in
   `cli-shared`, then the three-command conversion and the un-mandated
   adoption window.
5. **`Args` as data** — `nc-13`'s medicine pointed at CLI verbs.
6. **Relocate `enrich_cmd` and `bench_cmd`.**

1 through 3 are mechanical and independently landable. 4 is the only one that
requires design judgement, and it is the only one whose success is uncertain —
which is why it is measured rather than mandated.
