# Native Grounding — the resource economy plan

**Status:** plan of record for the `native-grounding` initiative, drafted
2026-08-12 under order `native-grounding-respec-economy`.

**REVISED 2026-08-12** under order `grounding-glassbox-stack-attribution`,
on operator direction. Four changes, all in this revision and all recorded
rather than silently applied: §0 restates the objective; §9 replaces
*delete-as-you-land* with *tombstone-then-delete*; bar
`E-deletes-land-per-phase` is retired in `quality/initiative-bars.toml`
with a pointer, and `E-tombstone-ledger` declared in its place; and the
phases are re-cut around the **build** list rather than the delete list.
§9.0 carries the diff between the old phase numbering and the new one, so
the transition is the record. **One sequencing question this revision did
not settle is flagged in §9.6 rather than resolved.**

**This document SUPERSEDES `sovereign/docs/specs/NATIVE_GROUNDING_PARITY_PLAN.md`
by name.** The parity plan is not deleted and must not be: it is the record of
how the initiative's headline objective was deferred out of a decision table,
and §12.2 below reads against it. Where the two disagree, this document governs.

**The anchor is `NATIVE_GROUNDING.md` §1–§9 and is not re-litigated here.** H0's
clauses stand as written. This plan re-derives the *route* to them, not the
destination.

---

## 0. What this plan is for, in one paragraph

### 0.1 The objective, in the operator's words

**Improve the user experience via latency reduction, with no hit to response
quality — possibly an improvement — as measured by our benchmarks.**

Operator, 2026-08-12, restating the objective after this plan's first draft had
drifted into treating deletion as the goal: *"It almost feels like we're making
the goal deleting code. No. It's improving the user experience via latency
reduction without any hit to response quality (and possibly even an
improvement) as measured by our benchmarks."*

Concretely: *"Is free will compatible with determinism?"* in the desktop app at
**≤75s wall** (measured today at ~150s; best warm run 79.8s; spread
79.8–247.3s), **quality held or improved on the bench**.

**LOC deleted is a BYPRODUCT this plan reports, never a bar it is judged
against.** A phase that improves latency and holds quality while deleting
nothing has succeeded. This sentence exists because the first draft of §9 read
the other way round, and the ratchet it built (`E-deletes-land-per-phase`) aimed
at the wrong target — see §9.0.

### 0.2 Why the chain, not one stage

Latency is the goal; **quality is a hard constraint measured by the bench**, not
a thing traded against it. The route is **making the whole chain spend fewer
resources, not tuning one stage.** Sixteen orders ran under the spec and none
carried the latency clause. This plan carries it. It is organised in two halves,
and the split is deliberate and load-bearing:

- **Part I derives the chain from what each stage accomplishes.** It contains no
  cost figures at all. A reviewer must be able to finish Part I without knowing
  what today's system costs. That constraint exists because the failure this
  plan corrects is anchor-and-adjust: ask "how do we make audit#2 cheaper" and
  you have already conceded that audit#2 exists.
- **Part II states what today's implementation spends beyond the minimum Part I
  derived.** That delta is the waste. The phases are the route from here to
  there, and every one of them **stops the waste from executing** as it lands —
  by tombstone where a delete is not yet safe, by delete where it is (§9.0).

---
---

# PART I — THE FUNCTION TABLE

*No cost figures appear in Part I. This is the design; Part II is the delta.*

## 1. Method

The spec's own method (`NATIVE_GROUNDING.md` §3), restated as the order of
reasoning every row below follows:

1. **What does this stage ACCOMPLISH?** Stated in terms of what the user ends up
   with, with no reference to the current implementation.
2. **What is the cheapest mechanism that accomplishes it?** Ranked by §3.1 —
   **structural** (an invariant code enforces) beats **statistical** (a
   calibrated score with a measured operating curve) beats **judged** (a
   generative call). A generative judge is the escalation tier of last resort,
   never the default path.
3. **Does the function survive once the other stages are right?** A function can
   *dissolve*: if an upstream mechanism makes a downstream check's failing input
   unconstructible, the downstream check does not get cheaper — it stops being
   necessary. Every delete in §9 of the spec is downstream of some mechanism
   that makes the deleted thing unnecessary. Drop that mechanism and the cost is
   permanent.

**Rule applied throughout:** *a mechanism whose function is served by a cheaper
tier survives on INCUMBENCY, not on FUNCTION.* Incumbency is not a reason to
keep anything.

**Second rule, and it is the one the spec's §9 was missing:** *a mechanism whose
only function is to make the output look tidy is not a grounding mechanism.*
Grounding's function is that the reader is not fooled. Marking an unsupported
span and releasing it satisfies that function. Rewriting the answer until no
marks are needed satisfies a different one — presentation — and it is the
expensive one. Principle 6 of the workspace compass says absence is reported,
never defaulted; a rewrite loop is the mechanism that defaults it.

## 2. The chain, stated as three functions

The chain has exactly three stages that spend resources on a knowledge turn, and
each one exists to deliver a distinct part of what the user ends up holding.

**RETRIEVAL delivers: the smallest set of the user's own text that contains the
answer — and a verdict on whether it does.**

Note that this is two things, and the second is not optional. A retrieval stage
that returns passages without saying whether the answer is in them has pushed an
unanswered question downstream, where answering it costs a generation.

**SYNTHESIS delivers: prose the user reads, in which every factual specific is
one the evidence supports, each marked so the reader can tell sourced from
recalled from inferred.**

Note that "marked" is part of the deliverable, not a decoration on it. An
unmarked answer forces every downstream consumer — the reader, the ledger, the
UI — to re-derive provenance the generator already knew.

**GATE delivers: the guarantee that the first two did their jobs, and the turn's
disposition as a typed fact.**

Note the word *guarantee*. A stage whose job is assurance must be trustworthy in
a way the stages it checks are not. That has a sharp consequence: **an assurance
stage built out of the same generative machinery it is assuring cannot be the
floor.** It can be an escalation rung above a floor. It cannot be the floor.
This is the single strongest constraint in the whole derivation and it is what
makes the mechanism ranking in §3.1 a requirement rather than a preference.

## 3. The function table

### 3.1 RETRIEVAL

| # | Function (what the user ends up with) | Minimum mechanism under §3.1 | Survives once the rest is right? |
|---|---|---|---|
| R1 | The answer-bearing passages are in front of the generator | **Statistical** — vector + lexical recall, then a calibrated relevance ordering. Bounded by structural invariants: principal scoping, dedupe, and a corpus-mix guard so one source cannot take the pool. | **Survives.** Nothing downstream can recover a passage that was never retrieved. This is the one function with no substitute anywhere in the chain. |
| R2 | The system knows, *before generating*, whether the answer is present | **Statistical** — a calibrated containment score over (question, passage) pairs. A cross-encoder answerability margin is the named instrument (spec §5 H1). | **Survives, and its value grows.** R2 is the only place in the chain where the answer/hedge/abstain decision can be taken before any tokens are spent. Every other stage must generate first and judge after. |
| R3 | The generator is told **how much** of the asked-for answer the evidence supports | **Structural** — a size derived from the evidence, carried as a request parameter, not as a request in prose. | **Survives, and it is currently unowned.** See §3.4. |
| R4 | When round-0 recall misses, a second, targeted pass recovers it | **Contested.** Reformulating a query is the one retrieval sub-function with a plausible claim on a generative call. But it fires exactly when the turn is already going badly, and it sits in the pre-generation critical path. | **Survives as a function; its tier is open.** Ranked *below* R2 in priority: a stage that reformulates before it can score containment is guessing at what it is missing. |

**R2's decision is three-way, and the third way is the one that matters.** The
useful outputs are not {answer, abstain} but {answer, hedge, abstain} — and
`hedge` is a first-class disposition, not a failed answer. This is the spec's own
§9 line, *"hedge is a first-class decision, not a failed answer re-rolled."*

### 3.2 SYNTHESIS

| # | Function | Minimum mechanism under §3.1 | Survives? |
|---|---|---|---|
| S1 | Prose that answers the question from the evidence | **Judged — irreducibly.** This is generation. It is the one place in the whole chain where a generative call is not a design smell but the design. | **Survives permanently.** S1 is the chain's floor cost. Everything else in this plan is about making S1 the *only* generative cost on a healthy turn. |
| S2 | The prose does not assert specifics the evidence fails to support | **Structural, at token selection.** The system owns its sampler and sees every logit (`ConstrainedSampler::sample`; four maskers already ride that hook). Constraining *what can be emitted* is structural in the §3.1 sense: it is code enforcing an invariant, not a request that a model behave. This is spec §5 H3. | **Survives — but see §8.** Its *downstream* form does not: see the dissolution note below. |
| S3 | Each span is marked sourced / recalled / inferred, and the sourced ones carry a real address | **Structural.** A typed segment with a resolvable `(corpus_id, chunk_id)`. Emission shape is constrainable by grammar; resolution is deterministic containment. This is spec §5 H4 + §6. | **Survives permanently.** S3 is what makes the gate's job cheap, and it is what the user actually sees. |

**The dissolution, stated precisely — this is the question the parity plan never
asked.** If S2 is enforced where tokens are chosen, then the draft cannot contain
the class of defect a post-hoc audit exists to find. It follows that:

- an audit of the draft finds nothing to repair, so
- there is no repair pass, so
- there is nothing a second audit could check.

**The second audit does not get cheaper. It ceases to have an input.** Note the
shape of that argument: it is not "the audit is expensive so let us skip it" —
that trade was priced once and refused (Part II §7.4). It is *"the audit's
failing input becomes unconstructible."* A check with no constructible failing
input is not a check (compass smell: *a check with no failing input you can
name*). It is a delete.

**And the honest limit on it.** A logit tilt shifts a distribution; it does not
prove a property. ARCH §7.6 is explicit that a model may not be asked to
*guarantee* what code can enforce, and a tilted decode is still model behaviour,
merely better-conditioned behaviour. So S2-structural reduces the rate; it does
not zero it. A residual check is therefore a real function — **but the residual
is span-shaped**, and "this specific does not occur in the sealed evidence" is
decidable by containment, not by judgement. The residual survives at the
*deterministic* tier, which is where the gate meets it.

### 3.3 GATE

| # | Function | Minimum mechanism under §3.1 | Survives? |
|---|---|---|---|
| G1 | No confident-sounding specific reaches the reader unsupported | **Structural first:** does the span resolve verbatim/near-verbatim into the sealed evidence? **Statistical second:** a calibrated sentence-support margin for the paraphrased case. **Judged third, and bounded:** a fixed, small escalation budget for contested sentences only. | **Survives permanently at the deterministic tier.** G1 is the assurance floor and §2's constraint applies: it must not be built from the machinery it assures. |
| G2 | Whatever G1 flags is handled honestly | **Structural** — demote the span to `Unverified` and render it as such. Targeted excision or repair of the specific span is the cheap optional refinement. | **Function survives; the *rewrite* form dissolves.** Marking discharges the grounding function completely. Re-synthesising the whole answer discharges a presentation preference, at a cost the user pays in wall time and the reader pays in a *less* honest artifact — a clean answer that no longer shows where it was thin. |
| G3 | The turn's disposition is a fact downstream consumers can read | **Structural** — one typed verdict, written once, by one decider. Spec §6's `GroundingVerdict`. | **Survives permanently.** |
| G4 | The system can tell what it decided and why | **Structural** — trace every decision at its decider. | **Survives permanently, and is currently unserved.** See §3.4. |

**G3's corollary, and it is a delete generator.** Once the disposition is typed
at the point it is decided, every mechanism that *recovers* the disposition by
inspecting the released prose is redundant by construction — phrase lists that
detect declining, opener lists that detect refusing, classifiers that detect
caveating. A system reading its own decision back out of its own output is
paying twice and getting the second answer wrong sometimes. These survive only
on INCUMBENCY.

### 3.4 Two functions that no stage currently owns

Naming these is part of the deliverable; a function table that only re-labels
existing components would be the fallacy in a different costume.

**R3 — answer sizing.** Nothing in the chain derives how long the answer should
be from how much the evidence supports. The size is requested in prose and hoped
for. Sizing is the archetypal structural job: it is a request parameter. It is
listed here rather than under "optimisation" because **answer length is the
input to the downstream audit's fan-out**, so the stage that fails to own sizing
is the stage that sets every later stage's bill (Part II §7.2).

**G4 — the gate's own glassbox.** The subsystem this entire initiative is about
emits nothing at the daemon's running level; today's cost census was only
possible because an *unrelated* layer happens to time model calls. Compass
principle 1: a decision invisible at `tracing=debug` is not finished. This is a
standing violation on the subsystem the initiative is named for, and it is not a
"nice to have" — it is why every cost question about this stage has had to be
answered archaeologically.

## 4. The spec's §9 ledger, re-labelled: FUNCTION or INCUMBENCY

The spec's §9 already did this once. This extends its method to the two stages
§9 never covered (retrieval and synthesis), and adds the label the order
requires.

**FUNCTION** = some mechanism must do this work.
**FUNCTION, WRONG TIER** = the work is real; a cheaper tier in §3.1 does it.
**INCUMBENCY** = it exists and nobody removed it.

### 4.1 Gate components (spec §9's own rows)

| Component | Label | Reasoning |
|---|---|---|
| `gate_longform` ladder + batched triage + rescan | **INCUMBENCY** | Its detection function is G1, served structurally. Its repair function is G2's *rewrite* form, which discharges presentation, not grounding. Both of its jobs have a cheaper owner. |
| Single-claim verify path | **FUNCTION, WRONG TIER** | "Is the asserted value supported by the evidence" is G1 exactly. It is a containment question being answered by a two-stage generative critic. |
| `classify_caveat` + caveat prose classification | **INCUMBENCY** | Reading a property the system itself set. Once the caveat is a segment type (S3), measuring it is a field read. |
| Decline-recognition zoo (phrase lists, opener lists, `released_pure_decline`) | **INCUMBENCY** | G3's corollary. The disposition is typed at the decider. |
| Retry machinery (retry floor, retry system notes, re-verify) | **INCUMBENCY** | No grounding function. It is the control loop of the rewrite, and the rewrite is G2's presentation form. `hedge` as a first-class disposition is the replacement, per §9's own wording. |
| `verify_grounding` 2-stage Critic + `violation_prob` | **FUNCTION, WRONG TIER** | Same as single-claim verify. Its *score* is a real statistic; its *instrument* is the wrong tier. |
| Anti-fabrication block of the synthesis system prompt | **MIXED — see §5** | |
| 8-surface `GateSurface` profile matrix + the grounding env-flag family | **INCUMBENCY** | Compass #8: one decider, one name. A profile matrix is what a system grows when the decision has no single owner. |
| `citation.rs` + `citation_attribution.rs` quote-then-answer path | **FUNCTION, WRONG TIER** | Forcing the model to copy its support before asserting is a genuine mechanism and it worked. But it is a *prompt-shaped surrogate* for a decode-time constraint: it buys adherence by spending output tokens on a rehearsal. The native form of "you may only assert what the evidence carries" is S2. Its deterministic verifier survives inside G1. |
| Surgical rewrite core (splitter, best-match, Delete/Fix) | **FUNCTION** | The cheap targeted-repair primitive G2 wants. **Keep**, as §9 said — and fix it (Part II §7.3). |
| Deterministic vetoes, numeric audit, quote verification | **FUNCTION** | Already structural, already cheap. This is what the whole plan is trying to make the rest of the stack look like. |
| Ledger / verdict / probe / acquisition | **FUNCTION** | Consumers, not police. Fed better inputs by S3. |

### 4.2 Rows §9 never had — retrieval and synthesis

| Component | Label | Reasoning |
|---|---|---|
| Generative evidence-sufficiency judge in the retrieval loop | **FUNCTION, WRONG TIER** | This *is* R2. A generative call is answering the question a calibrated containment score is built to answer, and it is doing so in the pre-generation critical path. It is also, today, the only owner R2 has. |
| Generative query formulation for the second retrieval round | **FUNCTION, TIER OPEN** | R4. The only component in this table where a generative call may honestly be the minimum. It is ranked *behind* R2 because reformulating before you can score containment is guessing. |
| Retrieval's own no-instrument fallbacks | **FUNCTION** | Reporting absence is correct (compass #6). Listed so it is not mistaken for waste. |
| `KNOWLEDGE_SYNTHESIS_SYSTEM` (the standing synthesis system prompt) | **MIXED — see §5** | |
| Answer sizing (R3) | **ABSENT** | Not incumbency, not function-served: *nobody owns it.* |
| Gate tracing (G4) | **ABSENT** | Same. |

## 5. The standing synthesis prompt, under the function test

The synthesis system prompt is paid on **every** knowledge turn. §9 already
scheduled part of it for deletion. Under the function test, each block is asked
one question: *what work is this prose doing, and does a structural channel do
that work for free?*

| Block | Work it does | Verdict |
|---|---|---|
| "Answer from the passages — they are your evidence" | Frames the task | **FUNCTION.** Prose is the right channel for framing. Keep. |
| "ANSWER, don't deflect" / "don't refuse because retrieval was incomplete" | Pleads against over-refusal | **INCUMBENCY.** This re-litigates R2's decision inside the generator. If the turn reached synthesis, containment was already scored; the generator must not be given a second vote. This block is compensation for R2 having no owner. |
| "A requested length is a ceiling, not a quota" | Pleads for brevity | **INCUMBENCY, and the most consequential row here.** This is R3 asked for in prose. Length is a request parameter. Pleading for what a field enforces is the ARCH §7.6 violation in its purest form — and this particular plea governs the variable that drives the entire downstream bill (Part II §7.2). |
| "Check the question's premise against the passages" | Genuine reasoning instruction | **FUNCTION.** No structural channel exists. Keep. |
| "Prioritise what you can justify" + the three-tier definitions | Defines the provenance taxonomy the model must emit | **FUNCTION, shrinkable.** The taxonomy is real and it is S3's emission contract. Its *rationale* prose is not needed once the tiers are typed and rendered — and they are. |
| The worked citation example | Few-shot for citation shape | **FUNCTION today, INCUMBENCY once constrained.** A demonstration exists to teach a shape. A grammar enforces the shape. |
| "Citation shape is mandatory — never use `[1]`, `[2]`…" | Forbids a syntax | **INCUMBENCY, unambiguously.** This is a grammar, written as a plea, in a system that vendors a constrained-decoding engine and already runs four maskers on this hook. It is the clearest single row in the table. |
| "Preserve source terminology" | Pushes generation toward the source's own words | **FUNCTION today; INCUMBENCY if S2 lands.** Tilting decoding toward evidence-conditioned tokens *is* "prefer the source's wording", performed mechanically. This block is a prose surrogate for H3. |
| Anti-fabrication guardrails (invent-nothing, no `[unverified]` tags, never end on a dead end) | Behavioural pleading against fabrication and against UI dead-ends | **INCUMBENCY, in three different ways.** Fabrication is S2's job structurally and G1's job deterministically. `[unverified]` is a rendering the system owns — it should not be negotiated with the model. "Never end on a dead end" is a UI affordance requested in prose from a component that cannot render UI. |
| "Chunks may be cut mid-sentence — trust your training over the chunk" | Tells the model to prefer parametric knowledge over the evidence in one case | **CONTRADICTORY, and it must be resolved before anything else in this prompt is tuned.** It directly opposes the prompt's own opening instruction and its justify-what-you-can-justify block. The standing workspace convention is that prompts must be succinct and non-contradictory; more sharply, a block that instructs the model to override the evidence is in tension with every mechanism in this plan. |
| Contested-sources block | Explains how to handle a `(contested)` label | **FUNCTION, WRONGLY SCHEDULED.** Meaningful only when a contested source is in the pool; paid on every turn regardless. |
| Catalog-aware-sources block | Explains how to handle a `CATALOG:` block | **FUNCTION, WRONGLY SCHEDULED.** Same shape. |

**The structural finding on the last two rows, and it is principle 11 in one
line: the conditional-append pattern already exists in this exact function.** The
coverage-gap note is built per turn and contributes nothing when there is nothing
to disclose. Two large conditional blocks sit next to it, unconditionally. This
is not a mechanism to build; it is a mechanism to *apply*.

## 6. The dependency structure — what unlocks what

**The order of the phases is set by this graph, not by which cost is largest.**
The biggest number is an anchor; the graph is the design.

```
  TYPED VERDICT (one decider, G3)
        │
        ├──> the disposition is a fact ──> every prose-detector of the
        │                                   disposition is redundant  [4.1]
        │
        └──> MARKED SEGMENTS (S3)
                  │
                  ├──> marking discharges G2 ──> the rewrite is optional ──>
                  │         the second audit has no input                [3.2]
                  │
                  └──> segments carry addresses ──> SPAN RESOLUTION (G1,
                            deterministic) ──> the per-claim generative
                            audit has a cheaper owner

  CONTAINMENT SCORE (R2)
        │
        ├──> the pre-generation disposition ──> the generator stops being
        │        asked to re-decide it ──> the pleading blocks lose their job
        │
        └──> sentence-support margin ──> G1's paraphrase tier

  ANSWER SIZING (R3)  ──> bounds the downstream fan-out entirely  [§7.2]

  MULTI-SEQUENCE CAPACITY ON THE PRIMARY
        │
        └──> EVIDENCE-TILTED DECODE (S2 / H3) ──> the residual becomes
                 span-shaped ──> the deterministic tier is sufficient
```

**Three readings of this graph decide the whole plan.**

1. **The root is already built.** The typed verdict and the marked segments are
   the top of the dependency graph, and both are *shipped and default-on*. Every
   delete hanging off them has been available and unspent.
2. **The rewrite is a hinge, not a leaf.** Deleting the rewrite is what removes
   the second audit's input. Attacking the second audit directly does not work
   and has already been refused once (Part II §7.4). The graph says which end to
   pull.
3. **H3 sits behind a capacity prerequisite the spec did not price**, and
   everything the graph hangs off H3 is therefore later than the spec assumed.
   §8 takes the decision.

---
---

# PART II — THE DELTA

*Cost figures begin here. Everything above was derived without them.*

Every number in this half is measured, cited, and carries the instrument that
produced it. Single draws are labelled as single draws.

## 7. What the incumbent spends beyond the minimum

### 7.1 The end-to-end bound

Four fresh end-to-end runs of the iconic query on BeefyMac via
`svrn chat ask "Is free will compatible with determinism?"` (router confirms
`DeepQuery` confidence 1.00 — the same surface the desktop takes), wall measured
from process start to exit (note `4ed783c9`):

| run | wall s | gate s | synth s | retrieval + CLI s | path | primary |
|---|---|---|---|---|---|---|
| 0 | 177.3 | 89.9 | 49.7 | 37.6 | retry ladder | **COLD** |
| 1 | 247.3 | 124.8 | 71.3 | 51.1 | retry ladder | warm |
| 2 | 123.8 | 54.5 | 36.1 | 33.2 | retry ladder | warm |
| 3 | **79.8** | 21.8 | 25.4 | 32.6 | **no retry** | warm |

**The best measured turn on this hardware is 79.8s and it misses the objective.**
Gate share of wall is 44–51% on ladder turns and 27% without — never the 81%
an earlier single-ratio estimate claimed, a figure corrected on the record in
note `c5b5d8a0` (a maximum reported as a measurement, and two instruments in one
ratio).

**Consequence, stated plainly and it is the reason this plan exists:** deleting
the gate *entirely* still misses 75s on two of four runs — the non-gate remainder
is 58.0 / 69.3 / 87.4 / 122.4s. A gate-only scope cannot reach the objective.

**And the variance is itself the defect.** Five turns with byte-identical
retrieval (chunks=36, `top_similarity=0.83734643` to eight digits) measured
gate 121.5 / 89.9 / 124.8 / 54.5 / 21.8s — ladder turns mean 97.7s, sd 27.5s,
cv 0.28 (note `c5d16402`). **The same question costs between 22 and 125 seconds
depending on nothing the user can see.** No median-only bar can capture this,
which is why §10's bar carries a p90 clause.

**Second sample, same day, from the Phase 1 instrument (2026-08-12, later).**
Six further CLI runs of the iconic query, warm resident primary, read off the
attribution strip rather than a census: **6 of 6 took the ladder** — every one
`rewrite_annotated`, every one paying an audit + a rewrite + a re-audit. The
earlier census saw 1 no-ladder turn in 5; this sample saw 0 in 6. Pooling the
two, **10 of 11 measured turns on this query take the ladder**, so the no-ladder
shape is the exception rather than the coin-flip §7.1's first sample suggested.
This does not change the p90 clause; it *raises* the expected value of Phase 4,
because the shape being eliminated is the one almost every turn takes. Stated as
n=11, warm, one query, one host — a bound, not a distribution.

### 7.2 Where the gate's spend comes from — and it is not where anyone assumed

Code-verified cost model, which predicts the observed call count exactly on four
separate turns (note `c5d16402`):

```
per audit pass over text T:  1 extract_claim_list
                           + N claim_violation_joint      where N = claim_budget(len(T))
                           + 1 scan_unsupported_specifics
full ladder = audit(draft) + 1 rewrite + audit(rewrite) = N1 + N2 + 5 calls
claim_budget(chars) = (chars / CHARS_PER_CLAIM).clamp(min_claims, MAX_AUDITED_CLAIMS)
```

Three facts fall out, and each one kills a different intuition:

- **Retrieval breadth is not a latency lever.** 12 chunks → median 17 calls;
  40 chunks → 18. Flat.
- **The fan-out driver is ANSWER LENGTH**, through `claim_budget`. This is the
  R3-absence bill: the stage that does not size the answer sets every later
  stage's cost.
- **The ladder compounds against itself.** On the operator's turn the rewrite
  came out *longer* than the draft, so audit#2's budget hit the cap while
  audit#1 used five. **The second audit cost more than the first** — 50.9s
  against 25.6s. The repair pass made the verification pass more expensive.

Hand-verified decomposition of the operator's 121.509s gate window (sums to
121509ms exactly):

| stage | s | share | composition |
|---|---|---|---|
| audit #1 | 25.55 | 21.0% | extract 3.1 + 5 per-claim 8.0 + specifics-scan 10.6 |
| rewrite | 43.22 | 35.6% | **one** 35B call, 13,309 prompt tokens → 6,613 chars |
| audit #2 | 50.85 | 41.8% | extract 6.0 + 10 per-claim 21.4 + specifics-scan 18.3 |
| gaps/tail | 1.88 | 1.6% | |

**It is fully serial.** Zero overlap among the 20 primary calls; primary busy
110.7s of 121.5s (91.1%). And batching is not available to buy it back: the
primary slot loads single-sequence — no `n_seq_max` on its ready line, no
`n_parallel` in config, `max_peer_inflight=1`. **This same constraint is what
gates H3** (§8).

### 7.3 The known bug in the surviving component

The one component §9 marked **keep** is the one silently failing back to the path
it was built to avoid. Surgical rewrite is default-ON. On the operator's turn it
fell back to a full 35B re-synthesis: **43.2s**. On another run of the same query
it engaged as designed: three fast-slot edits, **5.36s**. The trigger is a
failure-count cap, and **only a `dbg()` records the fallback** — it is invisible
in production, which is G4 again.

**~37.8s, bug-class, on a component the spec already committed to keeping.**

*Both halves of that were addressed on 2026-08-13 (§7.3.1, §7.3.2) and only one
of them held up. The invisibility is gone: `event="surgical_cap"` and
`event="surgical_unmapped"` now record the routing decision and its reason at
INFO, on every longform repair. The **~37.8s is not recoverable by the cap fix**
— measured yield 0 ms over 8 desktop turns, because a second all-or-nothing gate
inside `surgical_rewrite` binds first. Read §7.3.2 before re-using the 37.8s
figure anywhere.*

#### 7.3.1 The cap is inverted relative to its own stated rationale

Added by the 2026-08-12 revision, because the first draft priced this bug as if
it were the mechanism's cost. The pre-fix code (`grounding/mod.rs:2971` at
commit 46f29387 — the 2026-08-12 revision cited `:2848`, which the G4 diff had
already moved) and the comment above it:

```rust
// When MOST claims fail the draft is fundamentally broken … so cap
// surgery at a small failure count (SOVEREIGN_SURGICAL_MAX_FAILURES,
// default 3).
if surgical_rewrite_enabled() && !failed.is_empty() && failed.len() <= surgical_cap
```

The comment reasons about a **ratio** ("most claims"); the code implemented an
**absolute count**. `claim_budget = (len(text)/600).clamp(min, 10)` rises with
answer length, so a 10-claim longform answer with 4 failures — **60% grounded** —
fell back to full re-synthesis, while a 3-claim short answer with **all three**
failing got surgery.

**Consequence: targeted revision was structurally excluded from longform**,
which is the class of answer it was built for and the class the objective is
measured on.

**FIXED 2026-08-13 (§9 Phase 2).** The cap is now a ratio of the audited
claims, derived from the comment's own word: *most* means a majority, a
majority is more than half, so surgery is available while the failures are a
**minority**. One decider, one name (#8):

- `SURGICAL_MAX_FAILED_RATIO = 0.5` — the derived constant, with the derivation
  written at the definition and an explicit instruction not to tune it to a
  latency number.
- `surgery_admits(failed, audited, ratio)` — the one predicate, pure and
  unit-tested (`grounding/mod.rs`; the tests name the two rows that were
  backwards: `(4 failed, 10 audited)` declined, `(3 of 3)` was admitted).
- `SOVEREIGN_SURGICAL_MAX_FAILED_RATIO` replaces `SOVEREIGN_SURGICAL_MAX_FAILURES`,
  is read in exactly one place, and is declared in `quality/env-flags.toml`.
  `0.0` forces full re-synthesis and `1.0` forces surgery — the two positions
  the surgical calibration harness uses.
- Glassbox: `event="surgical_cap"` at INFO carries `failed / audited /
  max_failed_ratio / surgery_admitted` on every longform repair, and
  `event="surgical_unmapped"` names the *other* fallback separately.

#### 7.3.2 What the fix was worth, measured — and it is not what §9 booked

*Added 2026-08-13 from 8 warm desktop runs (§9 Phase 2 carries the full table).*

**The counterfactual delta of this fix over 8 desktop turns of the iconic query
is 0 ms**, and the reason is a second gate nobody had priced.

Seven of the eight turns reached the repair pass. Replaying each recorded
`(failed, audited)` pair through both rules, the new ratio cap and the old
absolute cap disagree on **exactly one** turn — `failed=4, audited=8`, which the
old cap declined (4 > 3) and the new cap admits (4 ≤ 4). On that turn surgery
was admitted and then **declined itself**: `surgical_rewrite` resolves *every*
failed claim to a sentence before it will edit anything, and abandons surgery
whole if any one is unmappable (`grounding/surgical.rs:240-252`). It logged
`event="surgical_unmapped"` and paid the same 36.2s full re-synthesis.

**The two gates compound in opposite directions.** The ratio cap admits *more*
failed claims than the absolute cap did; the span resolver is all-or-nothing
across the admitted set. Each extra admitted claim is another chance to trip it.
So the cap fix's yield is structurally self-limiting, and on this query the
binding constraint is the span resolver, not the cap.

**The corrected pricing.** §7.3's decomposition of the rewrite's 43.2s into
5.4s mechanism + ~37.8s bug is right about the *mechanism prices* — measured
again here at surgical **1.9 / 2.6 / 2.9s** against full re-synthesis **34.6 /
35.8 / 36.2 / 36.6s**, a ~33s gap per affected turn. It is wrong to book that
gap as recoverable *by this fix*. Best case on the measured sample — had the
span resolver succeeded on the one turn the cap newly admits — is that turn
moving 142.8s → ~109.1s: **mean 115.6 → 111.3s (−4.2s), median 116.5 → 107.2s
(−9.4s)**. The median moves more than the mean only because the changed turn
lands between the 4th and 5th order statistics of an n=8 sample; the mean is
the honest per-turn figure. Against §9 Phase 2's booking of ~25-38s per ladder
turn, the best case is **~4s**, and the measured case is 0.

### 7.4 The lever that was already tried, and why this plan is not it

Skipping audit#2 while keeping the rewrite was attempted and reverted
(calibration 2026-07-17: CONFAB-LEAKED 0→1). **That result is correct and this
plan does not re-propose it.** Rewriting without re-auditing is unsafe by
construction: the rewrite is unaudited new prose from a generative call, so
removing its check is removing the only thing standing behind it.

**The dependency graph says to pull the other end.** Delete the *rewrite* — G2 is
discharged by marking (§3.3) — and audit#2's input ceases to exist. The
distinction is not rhetorical: in the 2026-07-17 configuration there was
unaudited generated text in the released answer; in this one there is none,
because nothing was regenerated. The released text is the audited draft with its
unresolved spans marked.

### 7.5 The standing prompt

The synthesis system prompt measures **9,308 characters / 1,498 words ≈ 2,327
tokens**, paid on every knowledge turn — matching the spec §2 estimate of ~2,500.

**Be honest about what shrinking it buys.** Its direct wall-time value is bounded
by prefill, it is a shared prefix, and the pinned-prefix KV cache is default-on
and names the grounding gate as its consumer — so a large share may already be
amortised. **The prompt is not a latency lever and this plan does not sell it as
one.** Its value is threefold and none of it is milliseconds:

1. **~380 tokens are conditional content billed unconditionally** (the contested
   and catalog blocks) on turns where neither applies. The conditional-append
   pattern already exists in the same function. This is a free delete.
2. **Every pleading block is a lever that competes with the evidence for
   attention, and this is measured, not theorised:** the honesty carve-out
   regressed competence 0.67→0.58 and was reverted (note `dd072a9e`); the same
   answer drew opposite verdicts from a 4B and a 35B (note `0b747975`). Prompt
   blocks trade facets against each other. Removing a block whose job a
   structural channel does is a *quality* action first.
3. **The length block is R3's placeholder**, and R3 sets the downstream bill
   (§7.2). It cannot be fixed in prose.

### 7.6 The retrieval term is unmeasured, and it is stated as unmeasured

Retrieval + CLI is 32.6–51.1s and after the phases below it becomes the largest
remaining term. **Its internal decomposition has not been measured.** The census
instrumented the gate; the end-to-end table derived retrieval by difference,
and it includes CLI process startup, which is not a daemon cost. Absence
reported, not defaulted (compass #6).

Two things are known and both are named so the plan schedules rather than
rediscovers them:

- Retrieval contains **two generative calls of its own** — the sufficiency judge
  and the query formulator — in the pre-generation critical path (§4.2).
- The operator's turn did a **428-embed retrieval burst (17.7s, one sequence at
  a time at ~33 ms each)** that is unique across the entire day; every other
  burst on 2026-08-12 is 26–30 embeds. Worth one look before anyone models
  retrieval cost from that turn.

**This plan does not fund a measurement phase for it** — the order forbids
growing an instrument before funding a delete, and rightly. The decomposition
rides as a side-car on Phase 4's verification runs using the instruments already
on disk (`daemon.err` routing lines + the existing census harness), not a new
harness.

### 7.7 The instrument that is missing, and it is one config decision

**The cross-encoder reranker is not configured on this host.**
`SOVEREIGN_RERANK_MODEL_PATH` is unset; its registry status is `experiment`,
"default-inert but wired into the production daemon; **one owner decision
pending**". The retrieval pipeline's PPR admission step is documented as
"on (dark without a reranker)".

The consequences are exact, and they change the shape of the plan:

- **R2 has no statistical instrument in production today.** The answerability
  margin is not computed; admission reports `NoInstrument`. The parity plan's
  claim that admission telemetry costs 0 ms because "the margin is reused from
  retrieval's existing rerank pass" holds only where a rerank pass exists. Here
  there is none.
- **G1's paraphrase tier (sentence-support margin) has the same dependency**, and
  it is the same 0.6B model.

This is principle 11 in its purest form: **the mechanism is vendored, wired, and
inert, waiting on a decision nobody was assigned to take.** It is not a build.

### 7.8 The auditor is shown less evidence than the drafter — D1's finding

**This section exists because Phase 3 stopped on it.** The order
`grounding-preventative-medicine` gated its own prompt and sizing work behind
one question — *is the per-claim audit's 50% failure rate real?* — and the
answer is no. **7 of 27 sampled failures are genuinely unsupported (25.9%);
the J-BROKEN line is <50%.** Counting was deliberately conservative: every
borderline claim was scored *genuine*, which biases against this verdict.

**One evidence universe is built once and then split three ways.**
`gate_evidence_with_sources(&kc.chunks)` seals exactly the chunks that built
the synthesis prompt. What each consumer is then shown:

| consumer | what it sees | site |
|---|---|---|
| the drafter | every chunk, full text (a typical turn: 28 Leaf + 8 Summary) | `kc.prompt` |
| the per-claim judge | `leaf_chunks[:8]` + up to 4 re-searched hits; Summary chunks only when the claim is classed THEMATIC | `grounding/mod.rs:2562`, `max_chunks = 8` on **every** surface (`config.rs:443`) |
| the specifics scan | every Leaf chunk **truncated to 1500 chars**; **no** Summary chunks | `judge.rs:492-496` |

So a claim the drafter grounded in leaf chunk #18, in any Summary chunk, or
past 1500 chars of a chunk **cannot be cleared by the mechanism that judges
it.** It is unfalsifiable by construction, and it fails.

**The three causes are not the same size, and the truncation is the smallest —
stated here so it is not oversold.** Over 27 distinct leaf chunks the median is
897 chars and only 19% exceed 1500, so the cap hid ~7% of leaf text: it bit
rarely and expensively (the "Luck Objection" at offset 1497) rather than
constantly. Sized against the 57 recorded failures: **window 32 (56%), summary
20 (35%), genuine in-view 5 (9%)**.

**Watched failing, verbatim.** Selected from the 27; the full sample and the
per-claim reasoning are in note `95b82f97`.

- *"John Martin Fischer and Paul Russell focus on reasons-responsiveness as a
  condition for freedom"* — vp 0.993, FAILED. Summary #29 reads "Key figures
  such as John Martin Fischer and Paul Russell advance strategies like
  reasons-responsiveness or Strawsonian approaches."
- *"Hume and Hobbes distinguished between internal necessity and external
  physical impediments"* — vp 1.000, FAILED, on three separate turns. Leaf #18
  states it almost word for word. Chunk #18 is outside the 8-chunk window.
- *"Mele raises the Luck Objection…"* — vp 0.942, FAILED **on evidence the
  judge was shown**: leaf #4, in-window, reads "The Luck Objection (Mele): …
  How can a lucky outcome ground moral responsibility?" This one is a plain
  judge error and not an evidence-view artifact — the two failure modes are
  separable and both are present.
- The specifics scan flagged the literal string `"Berofsky 1987, Campbell
  1997"`, which sits at offset 221 of leaf #25 — **inside** its own view; and
  `"The Luck Objection"`, verbatim in leaf #4 at offset **1497** — two
  characters past its own truncation.

**The drafter demonstrably saw what the auditor did not**, and the proof is
empirical rather than architectural: the drafts contain phrases lifted verbatim
from Summary chunks ("no viable form of indeterminism", "Strawsonian
approaches", "conceptual errors, linguistic usage"). A model can only produce
those having been shown them.

**What it costs in this plan's own currency.** Zero-failure passes observed:
**0 of 18** — 9 turns across both instruments, and every single pass paid a
rewrite and a re-audit. Counterfactually, removing only the failures judged
false turns the five CLI first-passes into 1 / 2 / 1 / 0 / 1 real failures, so
one of them becomes a zero-failure turn: the ~99s cliff at
`grounding/mod.rs` reached with **no** prompt change and **no** sizing change.
That counterfactual is what the repair below then confirmed live.

**The consequence for §9.** Phase 3's thesis — *the patient arrives sick and we
made it sick on purpose* — is right about the iatrogenesis and wrong about the
organ. The injury is in the auditor's evidence view, not in the synthesis
prompt. §5's contradiction is still a defect and still worth removing; it is
not what is driving the failure ratio. **Phase 4 is repriced too**: the 78.2s
repair chain is substantially serving self-inflicted failures, so the ladder's
cost must not be re-derived until this is fixed.

**THE REPAIR — operator decision, 2026-08-13.** Two of the three causes are
budget constants and are simply wrong; the third is a real invariant and is
resolved rather than reverted.

- **The judge's window** becomes the retrieved leaf set. The bound is derived,
  not picked: the drafter's evidence already passed `prompt_budget::enforce`
  for this turn's context window, and a judge prompt is strictly smaller than
  the drafter's, so what fit the drafter fits the judge by construction.
  `GroundingProfile::max_chunks` is REMOVED — it had exactly one consumer, and
  leaving it would leave a knob that reads as though it governs the audit's
  evidence and no longer does.
- **The specifics scan's 1500-char truncation** is lifted, on the same bound. A
  scan whose charter is "this specific is NOT in the evidence" cannot be shown
  a truncated copy of the evidence and be asked that question honestly.
- **The `[source:` exemption** is extended to `[Web:`. The 2026-07-01 fix named
  one header shape and the same builder emits two, so the system went on
  flagging its own passage headers as fabricated specifics.
- **Fix B is resolved, not reverted.** Operator, verbatim: *"The RAPTOR
  summaries are some of the most important features of this application so we
  absolutely should not throw them out… it can be acceptable to cite the raptor
  summaries themselves and allow users to view what we pulled from… epistemic
  honesty is the point."* The invariant that replaces the exclusion: **a
  summary is a legitimate evidence node when its provenance is inspectable.**
  "A fabrication grounding a fabrication" is answered by traceability, not by
  blindness.

**The two halves of Fix B's resolution are not the same problem**, and only one
of them is free:

- *The specifics scan* may see summaries immediately, because of an asymmetry:
  its entire output is ACCUSATIONS. Showing it a summary can only withdraw a
  false alarm about text the system itself put in the drafter's prompt; it can
  never clear a fabrication, because nothing at that site clears anything. This
  is 16 of the 20 summary-grounded failures.
- *The per-claim judge* is the site Fix B was written for, and admitting a
  summary there DOES clear claims. It therefore requires the provenance to
  travel with the summary — `RaptorNode::quote_spans`, "3-5 verbatim spans
  pulled from member chunks… the model's hallucination-safe quotable surface",
  with `evidence_chunk_ids` as the resolve-to-leaf path. **The substrate exists
  and nothing new is needed** (`sovereign-contracts/src/types/document.rs:463`,
  persisted in `raptor_nodes` and `conv_raptor_nodes`). What is missing is one
  link: `raptor_grounding.rs::raptor_scored_chunk` injects the summary with
  `chunk_id: None` and no node id, so `RaptorCand` drops a `node_id` that BOTH
  candidate paths already hold (`RaptorHit.node_id`; the scan path carries the
  whole node). Threading it — plus a by-node-id read on `conv_raptor_nodes`,
  the one accessor that genuinely does not exist — is the remaining work.

**Cost is bounded by a mechanism already default-on.** Every sibling claim
declares the same shared-window prefix (`judge::stable_passages_prefix_len`),
so `SOVEREIGN_PREFIX_STATE` — whose only consumer is this gate — pins the
evidence state once per turn and restores it for claims 2..N. The turn pays one
larger prefill, not N, against N false failures that each currently buy a
rewrite plus a full re-audit.

**MEASURED AFTER THE REPAIR — 6 warm desktop turns against the recorded 4.**
Real app on the command bridge, one fresh conversation per turn, 35B resident
throughout. The judge window reads 28 on every pass where it read 8 before.

| | BEFORE (4 turns / 8 passes) | AFTER (6 turns / 10 passes) |
|---|---|---|
| first-pass failed/audited | 0.78 0.60 0.86 0.29 — **median 0.69** | 0.12 0.00 0.00 0.50 0.25 0.40 — **median 0.19** |
| **ZERO-FAILURE first passes** | **0 of 4** | **2 of 6** |
| zero-failure, all passes | 0 of 8 | 5 of 10 |
| failed claims per turn | 6.75 | 1.67 |
| `rewrite` | ran 4 of 4, mean 31.8s | ran 4 of 6, mean **11.1s** (mostly surgery) |
| `re_audit` | ran 4 of 4, mean 81.2s | ran 4 of 6, mean **34.5s** |
| `audit` | mean 77.2s (window 8) | mean **69.7s** (window 28) |

**The audit did not get more expensive for seeing 3.5x more evidence**, and
that is the claim a sceptic should attack first, so it is the one with the
mechanism measured rather than argued. Timings: 84.7 / 113.0 / 92.9 / 75.7 /
37.2 / 14.7s at window 28 against 97.5 / 55.9 / 88.7 / 66.6s at window 8 —
overlapping distributions, high variance in both, and the two cheapest passes
of either arm are in the AFTER set. Read it as **"no measurable increase"**,
not as an improvement.

**Why it is affordable, from the daemon's own log across the arm window
(14:55-15:13Z): 152 `prefix_state` RESTORES, 0 pin evictions, 0 model
unloads.** Every sibling claim declares the same shared-window prefix
(`judge::stable_passages_prefix_len`), so `SOVEREIGN_PREFIX_STATE` — whose only
consumer is this gate — pinned the wider evidence once per turn and restored it
for claims 2..N instead of re-prefilling. The 3.5x wider window did NOT thrash
the pin LRU: zero evictions inside the window, against evictions visible
elsewhere in the same log under other load. **The turn pays one larger prefill,
not N** — that was the prediction, and this is the measurement.

**Two things this arm deliberately does NOT claim.** (1) Its WALL numbers are
dirty — a cargo link ran against part of it — so `E-wall-time` is not
transitioned on it; the ratio and zero-failure counts are not wall-sensitive,
which is why `E-draft-grounding` can resolve on the same arm that cannot
resolve the other. (2) The binary under test does **not** carry the
`designed` ⊃ `signed` veto fix (below), and turn 1's only failure was that
bug — so these numbers are a **lower bound** on the repair.

**A FOURTH CAUSE, found by the first repaired turn, and it is the cheapest
defect in this document.** Both deterministic vetoes gate on "is this claim
even about a corpus artifact?" via `low.contains(word)` — a SUBSTRING test.
`"designed"` contains `"signed"`; so do `"pre-sent"`, `"ab-sent"`,
`"con-sent"`, `"repre-sent"`, `"sent-ence"`, `"es-sent-ial"`,
`"class-ical"`, `"de-notes"`, `"document-ed"`. A gate scoped to emails,
letters and source files was open on most sentences an essay contains, and the
bigram check then flagged *"Harry Frankfurt"* — named in four of that turn's
own chunks — because the corpus writes the surname alone. **That single veto
was the only thing between that turn and a zero-failure turn.** Fixed with one
word-boundary helper shared by both call sites, pinned by a test that asserts
the gate still opens on *"Betty Alexander sent an email"*. The
surname-vs-full-name half is a separate judgment about name resolution and is
banked, not fixed here.

**What remains, measured rather than assumed.** Of the 10 failures still
present across the 6 after-turns, **0** have their support past the 1500-char
cut that the per-claim judge (`claim_violation_joint`) still applies to each
chunk. That truncation is the same defect class as the scan's and it is
knowingly left in place: on this query it costs nothing, and changing it
without a measurement would be replacing one unexplained constant with
another. Three of the ten are the Fix B per-claim-judge half — a summary-only
claim the judge still may not clear — which is exactly the work
`quote_spans` carriage is for.

**The instrument.** `SOVEREIGN_GATE_AUDIT_FORENSICS=<path>` (default unset)
appends one `audit` record per pass, one `claim` record per decision naming the
mechanism that made it, and one `audit_result` closing the pass. Before it,
`dbg()` printed the claim and its violation probability but never the evidence
it was judged against — so "is this failure real?" was unanswerable from
outside the process. ARCH §18.4: validate the instrument before the result.

## 8. H3 (evidence-tilted decoding) — the decision, on the record

The order requires this to be scheduled or killed with a reason. Silence is what
produced the order: H3 is one of the spec's five mechanisms and the whole of its
Phase 4, and it has **no transition after `declared`** — never ordered, never
scoped, never killed.

**Verified before deciding (compass #4): no H3 prototype exists anywhere.** An
exhaustive sweep of the unmerged branch for `contrastive`, `noctx`, `cad`, a
decoding `alpha`, logit-level ablation, and new maskers at
`ConstrainedSampler::sample` returns **nothing**. Every hit is prose in the spec
itself (§5 H3, lines 217–241). The sampler gained exactly one function on that
branch — seed resolution. **H3 was never started, in any form, on any branch.**
That absence is the fact this section is deciding against.

### 8.1 The case for it is strong and unchanged

H3 is the only mechanism that makes grounding native to generation (§3.2 S2), and
the dissolution argument in §3.2 is the single largest structural delete
available anywhere in this initiative. Every primitive it needs is live and
verified in spec §4: the sampler hook with four maskers already on it, per-step
logit rows, shared-prefix KV copy, the second-context-same-weights slot pattern,
lockstep multi-sequence decode. And α=0 is bit-identical to today's path — the
safety rail and the A/B lever in one, which makes it unusually cheap to falsify.

### 8.2 Two facts the spec did not have, and they change the verdict's shape

**(a) A capacity prerequisite the spec did not price — but the substrate exists.**
H3 needs two sequences in one context on the primary. Measured: the production
primary slot loads single-sequence — no `n_seq_max`, no `n_parallel`,
`max_peer_inflight=1` (note `c5d16402`), and the embedded engine carries an
explicit *refusal gate* for `n > 1` whose own message says the batched
multi-sequence decode "lands in Phase 2". So on the production path it is not
available.

**But it was built, off main.** `sovereign-inference/src/k_sample.rs` (~1,184
lines, branch-only, zero runtime callers) is a working multi-sequence decoder
with `n_seq_max` and shared-prefix `copy_kv_cache_seq` fan-out, and it drove a
36B over 4,207 pairs × 3 arms at 0.49 pairs/s. It implements *independent k-way
sampling*, not two-sequence lockstep logit combination — so it is not H3 — but it
is the nearest reusable substrate and it retires the question of whether
multi-sequence decode works here at all. **This makes the probe in §8.3 markedly
cheaper than the spec's Phase 4 implied: instrumenting an existing decoder, not
building one.**

**(b) The spec's own throughput floor fails this objective's arithmetic.** H3's
declared gate accepts decode throughput **≥0.45x baseline** — i.e. it accepts
more than doubling synthesis. Against a 75s wall objective where synthesis alone
is measured at 25.4–71.3s, that is not a tolerable trade. Taking the best
measured warm run (run 3: retrieval 32.6 + synth 25.4 + gate 21.8 = 79.8s) and
granting H3 everything it promises on the gate side:

| H3 achieved throughput | synth s | gate s (dissolved) | retrieval s | **wall s** | vs 75s |
|---|---|---|---|---|---|
| 0.45x (spec's declared floor) | 56.4 | ~2 | 32.6 | **91.0** | **MISSES — worse than today's best turn** |
| 0.60x | 42.3 | ~2 | 32.6 | **76.9** | misses |
| 0.80x | 31.8 | ~2 | 32.6 | **66.4** | meets |
| 1.00x (free) | 25.4 | ~2 | 32.6 | **60.0** | meets |

**H3 as specified can consume the entire budget it frees.** That is a finding, and
it is new.

### 8.3 The decision

**H3 is SCHEDULED, and RE-GATED, and it is NOT on the critical path to 75s.**

1. **Re-gated on latency.** H3's bar gains a clause it never had: **decode
   throughput ≥0.80x baseline**, superseding the spec's ≥0.45x, because the
   objective is wall time and the arithmetic in §8.2 shows 0.45x failing. A
   mechanism that improves hallucination while making the turn slower than
   today's best turn does not serve this initiative's objective.
2. **Its kill decision reduces to one measurement that needs no labels.** Run a
   two-sequence lockstep decode on the primary at α=0 — bit-identical output, by
   construction — and read the throughput ratio. No calibration set, no banks, no
   judge, no quality question at all. **That single number decides H3's viability
   against this objective before any masker is written.** It is scheduled as a
   day-scale decision item inside Phase 3, not as a phase, and explicitly not as
   a measurement floor the plan waits on.
3. **It is deliberately not the keystone of the 75s objective.** §9's arithmetic
   shows Phases 1–2 reach the bar without it. H3 remains the keystone of H0's
   *quality* clause and of the largest LOC deletes, and it is sequenced there.

**The seat flagged a live risk that most of §9's economy might be unreachable
until H3 exists. On the measured numbers, it does not materialise** — the
dependency graph shows the largest wall-time deletes hanging off the typed
verdict and marked segments, both already shipped, not off H3. This is the most
important negative finding in the document.

## 9. Phases, and the tombstone ledger

### 9.0 What changed in this revision — the ratchet, retargeted

**The first draft of this section said "every phase deletes as it lands." That
is replaced, on operator direction, by TOMBSTONE-THEN-DELETE.** Operator,
2026-08-12: *"wire up the most correct system first, tombstone the old system,
then when we're elated with the new one we can make deleting a single pass."*

**Why the ratchet was aimed at the wrong target, stated plainly because the
correction is the record.** The failure this plan exists to fix was never that
code stayed on disk. It was that **the old system kept EXECUTING**. Dead code
costs nothing at runtime; a live second stack cost 43 seconds of the operator's
turn. A delete-per-phase ratchet buys the LOC number and the latency number
together, which sounds efficient and is actually a coupling: it makes every
latency win wait on a delete's blast radius, and it makes every delete carry a
latency win's risk. Tombstoning decouples them — it takes the **whole** latency
win immediately and keeps the escape hatch.

**Tombstone means: the path stops executing on the default configuration, the
code stays, and the switch that would re-run it is a declared, dated,
reviewable fact.** It does not mean `#[allow(dead_code)]` and a hopeful comment.

**The guard that stops this from becoming "nothing is deleted until H0
graduates" again — the failure mode this whole plan was written to correct.**
Three clauses, all structural:

1. **Every tombstoned path gets a `sovereign/DEFAULTS_LEDGER.md` row in the same
   commit** — flip condition, settling-plan item, review-by date — per the
   standing house rule for anything shipped default-off or dark. A tombstone with
   no ledger row is not a tombstone; it is an orphan.
2. **A tombstoned path that fires shows up in the strip** (Phase 1). Once G4
   lands, the product itself is the audit: an operator who sees an `OLD STACK`
   row on a turn is looking at a tombstone that leaked, without opening a log.
   This is the specific reason the glassbox is Phase 1 rather than a later
   nicety — it is what makes tombstoning verifiable instead of asserted.
3. **The deletion pass is a named phase with a named trigger** (Phase 5), not a
   graduation event nobody owns.

**Bar transition.** `E-deletes-land-per-phase` encoded the replaced ratchet and
is **retired** in `quality/initiative-bars.toml` with a `descoped` transition
pointing here — not silently edited, because the transition *is* the
post-mortem (that file's own §"TRANSITIONS ARE THE POST-MORTEM"). Declared in
its place: **`E-tombstone-ledger`** — every tombstoned path carries its ledger
row in the same commit, and no tombstoned path executes on the default
configuration. **`E-glassbox-attribution`** is declared for G4, which had no bar
at all despite §3.4 naming it unowned.

**What §9's original inversion still stands on.** `NATIVE_GROUNDING.md` §9's
"nothing is deleted until H0 graduates" is still wrong and still inverted here:
H0 never graduated, so across sixteen orders nothing was ever *retired* and the
system got more expensive. The correction is that the thing which must land per
phase is **the old path ceasing to run**, not the diff being negative.

#### The phase re-cut — old numbering to new

Phases are now named for what gets **built**, because that is the order the work
has to happen in. The delete list is an output.

| new | phase | was |
|---|---|---|
| **1** | **G4 glassbox** — per-turn stack attribution, on the wire and in the app | *nothing* — G4 was named UNOWNED in §3.4 and carried by no phase |
| **2** | **The repair fix** — the inverted surgical cap (§7.3.1) | a parenthetical inside old Phase 1 ("bug-fix landing in the same phase") |
| **3** | **R3 answer sizing** — derived `max_tokens` + the prompt rows | old Phase 3 |
| **4** | **Tombstone the ladder** — rewrite, audit#2, retry machinery, decline zoo, caveat classifier | old Phase 1's delete list, converted from deletes to tombstones |
| **5** | **One deletion pass**, when the operator is elated with the new stack | old Phase 1/2's deletes, collected |
| **6+** | **The residual program** — old Phase 2a/2c and H3 (old 4A/4B), plus the control-surface collapse (old Phase 5) | **§9.6 — SEQUENCING NOT SETTLED, flagged rather than resolved** |

**Phases are named for functions and for builds, not for components.** Every
table carries a latency column; the parity plan's §6 selected the program's path
from a table with no latency column at all, and that omission is how the
objective was lost.

### The thesis, stated once

**The system currently runs both stacks.** The native path's typed verdict and
segments shipped and are default-on; the incumbent judge ladder runs unchanged on
the same turn. The parity plan says so in its own words: admission runs as
telemetry with `enforced = false`, "net: zero model calls added per turn", and
"the incumbent's ~35 judge calls … remain untouched until P3c" — and no P3c order
was ever opened. **Sixteen orders bought the replacement and none retired the
incumbent.** That is the whole delta, and Phase 1 is where it starts being spent.

### Phase 1 — G4: make the strip say which stack served the turn

*Function basis: G4 (§3.4) — named ABSENT, owned by nothing, a standing
compass-#1 violation on the subsystem this initiative is named for.*

**This phase adds an instrument and deletes nothing, and it is first anyway.**
Under the replaced ratchet that was disqualifying; under the objective in §0.1 it
is compass #7 — *validate the instrument before the result*. Establishing the
one sentence "the system runs both stacks and the old one owns most of the turn"
cost roughly four hours of archaeology (a journal join, a `daemon.err` census, a
code read). Every later phase in this plan is verified by **reading the strip**
instead of running that census again, so this phase's cost is paid back four
times over inside this document's own scope.

| Build | ms/turn | note |
|---|---|---|
| Per-turn stage attribution on the wire — one typed shape, one producer | +0 measurable | timing + reporting only; no verdict, action, threshold or prompt changes |
| The strip in `AnswerProvenance.svelte` — extended, not replaced | — | reads the wire type; computes no attribution of its own (#8) |
| The surgical branch recorded as a categorical fact at the branch site (§7.3) | — | today only a debug-gated `dbg()` knows which of the two mechanisms ran |
| **Deletes** | **none** | and that is not a failure — §0.1 |

**The honesty property, and it is the whole phase.** The strip must be derived
from **what ran**, never from what the flags say. Two live precedents on this
initiative: `enforced=false` telemetry was accurate on every event and told
nobody the incumbent ladder was still running on the same turn; and the surgical
fallback is known to the code and invisible everywhere else. A strip that renders
"new stack" because a flag is on will lie exactly the way those did.

**A mechanism that fires while contributing no row to the strip is a DEFECT IN
THE STRIP, not an omission.** The design must therefore carry its own residual:
whatever wall time the rows do not account for is rendered as unattributed time,
so an unrowed mechanism shows up as seconds nobody claimed rather than as
silence.

**The detector fired twice during Phase 1's own build, which is the evidence
that it works.** Both are recorded here because a defect-detection property
nobody has watched catch anything is compass #5's *never-ran* verdict wearing
*passed*:

1. **Retrieval, first live turn.** An 8.1s turn residual with no `retrieval`
   row. Cause: the duration is only known after the scope that produced it has
   closed, so the ambient record was a silent no-op. Fixed with an explicit
   ledger handle; pinned by a regression test.
2. **The citation path, first `citation_grounded` turn.** 11.08s in the
   *gate* residual, and — worse — the turn rendered **"no grounding stack
   ran"**, because `served_by` was derived from named rows only. The
   quote-then-answer path (`grounding/citation.rs`, ECONOMY §4.1: FUNCTION,
   WRONG TIER) had no row.

**The second one forced a design clause, and it is the sharper half of this
phase.** A residual at **zero** is arithmetic and must not vote on who served
the turn — otherwise every turn reads "both stacks" by construction, which is
the flag-lie in a new costume. But a **non-zero gate residual is not
arithmetic**: the gate window exists only if the gate ran, so seconds inside it
that no row claimed are positive evidence that incumbent code executed by some
mechanism this build does not yet name. It therefore votes, and it counts
toward the old stack's total. **Under-reporting the old stack is the one
direction this strip must never fail in**, and before that clause it could.

**Bar:** `E-glassbox-attribution` — a human, without opening a log, a terminal or
a journal, reads off which stack served the turn, what each stage cost, and if
the old stack ran, which mechanism and how much. Lane: the acceptance test on
the iconic query, one ladder turn and one no-ladder turn, cross-validated for at
least one turn against the `gate-census.py` join (the validated instrument — if
they disagree, the strip is wrong until proven otherwise), plus the negative case
watched failing (`SOVEREIGN_SURGICAL_MAX_FAILURES=0` must make the strip *name*
the fallback). *Kept verbatim as the record of how this bar was run; that knob
no longer exists — the equivalent forcing position since 2026-08-13 (§7.3.1) is
`SOVEREIGN_SURGICAL_MAX_FAILED_RATIO=0`.*

### Phase 2 — The repair fix: uninvert the surgical cap

**LANDED 2026-08-13. The build is correct and the booking below was wrong;
the measured yield is ~0s. §7.3.2 has the mechanism, this section has the
numbers.** Read the strikethrough row as the record of what was expected —
this file's convention is that a corrected estimate stays visible.

*Function basis: §7.3.1. The one component `NATIVE_GROUNDING.md` §9 marked
`keep` was disabled on exactly the answers it was built for.*

| Build | ms/turn freed | basis |
|---|---|---|
| ~~Cap surgery on the **ratio** its own comment reasons about, not on an absolute failure count~~ | ~~**~25–38s** on a ladder turn that would otherwise fall back~~ | ~~two independent measurements — see below~~ |
| Same build, landed | **0s measured** over 8 warm desktop runs; **~4s/turn** as a best case | the counterfactual replay in §7.3.2 |
| **Deletes** | none | the mechanism was already `keep` |

#### The measurement — 8 warm desktop runs, and it is the first n≥5 on the app

`E-wall-time` and `E-variance` are stated in the desktop app, so they were
measured there: the real `sovereign-desktop` process on the real profile
(command bridge :9745, frontend :5173), one fresh conversation per turn so no
turn carries the previous one's context. Question: *"Is free will compatible
with determinism?"*

**Primary residency, stated with the numbers:** `primary_idle_secs = 1800`;
the primary (`Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`) loaded at 2026-08-13T04:33:20Z
and the daemon log records no unload or reload for the whole 04:39–05:00Z
measurement window. **The cold turn is reported separately and is not pooled:**
207.8s wall, with the 33.6s model load inside it.

| run | wall s | retrieval | draft | audit | rewrite | re-audit | mechanism | gate s |
|---|---|---|---|---|---|---|---|---|
| 0 | 148.8 | 12.1 | 30.7 | 35.6 | 34.6 | 33.6 | full_resynthesis | 103.8 |
| 1 | 78.1 | 7.2 | 13.9 | – | – | – | *(abstained_specifics — no repair pass)* | 44.1 |
| 2 | 105.2 | 7.2 | 25.6 | 38.7 | **2.9** | 27.6 | surgical_rewrite | 69.3 |
| 3 | 142.8 | 5.0 | 33.4 | 30.4 | 36.2 | 36.0 | full_resynthesis | 102.6 |
| 4 | 99.2 | 7.5 | 30.3 | 29.2 | **2.6** | 27.8 | surgical_rewrite | 59.6 |
| 5 | 127.8 | 7.2 | 28.6 | 29.2 | 36.6 | 24.8 | full_resynthesis | 90.5 |
| 6 | 132.3 | 7.6 | 24.5 | 21.9 | 35.8 | 40.0 | full_resynthesis | 97.7 |
| 7 | 90.2 | 7.1 | 23.6 | 29.6 | **1.9** | 26.5 | surgical_rewrite | 58.1 |

**median wall 116.5s · p90 144.6s · min 78.1 · max 148.8 · gate mean 78.2s, sd
23.2s, cv 0.297.** Both bars FAILED, and both are now judged rather than
uncovered (§10.4).

#### The counterfactual, and why the fix bought nothing here

Every repair decision is now logged, so the old rule can be replayed against the
same turns instead of re-run against different ones:

| turn | failed | audited | ratio cap (0.5) | old cap (≤3) | differs | outcome |
|---|---|---|---|---|---|---|
| 0 | 5 | 8 | decline | decline | | full 34.6s |
| 2 | 2 | 5 | admit | admit | | surgical 2.9s |
| 3 | 4 | 8 | **admit** | **decline** | **yes** | admitted → `surgical_unmapped` → full 36.2s |
| 4 | 3 | 7 | admit | admit | | surgical 2.6s |
| 5 | 6 | 7 | decline | decline | | full 36.6s |
| 6 | 4 | 6 | decline | decline | | full 35.8s |
| 7 | 2 | 5 | admit | admit | | surgical 1.9s |

One turn in seven changes decision, and it lands on the span resolver's
all-or-nothing gate (§7.3.2). **Net 0 ms.**

Two things this table says that no prior measurement could:

1. **The iconic query's drafts fail a median 50% of their audited claims**
   (.40 .40 .43 .50 .63 .67 .86). The query sits exactly on the boundary the
   ratio cap draws, which is why moving the boundary moves so little. §7.3.1's
   worked example — a 10-claim answer 60% grounded — is not this query's modal
   shape; this query is closer to half-and-half.
2. **The negative case is real and unforced.** Three of seven turns declined on
   majority failure (5/8, 6/7, 4/6), logged with their counts. Every prior
   observation of the fallback came from *forcing* it with
   `SOVEREIGN_SURGICAL_MAX_FAILURES=0`.

#### Byproduct: two n=1 desktop figures that do not survive n=8

Both were read off a single G4 turn on 2026-08-12 and both were carried into
this order's brief as facts about the app. The same 8 runs contradict them, and
they are corrected here rather than in §7.6 / §9.1 because this table is where
the evidence is. **§9.1's own arithmetic should be re-derived against these,
which is a sequencing question for the seat, not a revision to make here.**

| claimed (n=1) | measured (n=8, warm, same app) |
|---|---|
| retrieval is **13.3s** in the app vs 8.1-9.0s on the CLI — a divergence worth chasing | retrieval **median 7.2s, mean 7.6s** (5.0-12.1). No app/CLI divergence survives; the single 13.3s reading was the tail, and §7.6's 32.6-51.1s booking is further from the app than it is from the CLI. |
| after Phase 4 tombstones the ladder, draft synthesis alone is **67.6s** of an **~84s** desktop floor, making R3 load-bearing outright | draft **median 27.1s, mean 26.3s** (13.9-33.4). The post-tombstone shared floor — retrieval + draft + unattributed turn time, i.e. everything the tombstone does *not* remove — is **median 34.4s, mean 34.0s, max 43.1s**. |

The second row is the encouraging one and it should not be buried: on this
query, **removing the incumbent ladder alone lands the turn at ~34s median and
~43s worst-of-8, against a 75s bar.** That does not make R3 unnecessary — it
makes R3 *conditional* rather than load-bearing, which is the opposite of what
the n=1 reading implied. Neither figure is a promise: it is the floor measured
on the surviving stages of turns that also ran the ladder, so it does not
account for whatever a tombstone changes about scheduling or cache behaviour.

#### What did NOT get watched, stated as could-not-judge

The order's gate was "the fix watched **engaging** on a longform turn that
previously fell back." That turn is turn 3, and on it surgery was admitted and
then abandoned by the span resolver. **So the cap change was watched taking
effect, and surgery engaging as a consequence of it was not.** The strip
witnesses on file (`strip-surgical.png`, `strip-fullresynth.png`) show
`rewrite 1.9s — surgical span edits` on a 5-audited/2-failed turn and
`rewrite 35.8s — full re-synthesis (surgical fell back)` on a 6-audited/4-failed
turn; neither is a turn whose routing the fix changed. Calling either one the
gate's witness would be the substitution this initiative keeps catching (#6).

**Two prior measurements, kept for the record.** §7.3's pair (43.2s fallback vs
5.36s engaged) came from a census; on 2026-08-12 the same comparison was read
off the product on four consecutive CLI turns — surgery at **1.7 / 1.9 / 2.7s**
against a forced fallback at **27.5s**. Both are measurements of the two
mechanisms' *prices*, and tonight's 1.9/2.6/2.9 vs 34.6-36.6 agrees with them.
Neither was ever a measurement of what the cap fix would recover, and this phase
is the correction of that inference.

**This is a bug fix and it is priced as one.** It recovers none of what the old
Phase 1 booked against *deleting the rewrite* — see §7.3.2 for why the delete's
price is likewise not the cap's to claim.

#### Quality: unchanged by construction, and the construction is one line

The two mechanisms are two branches of one labelled block (`'produce`) whose
single value feeds **one** re-audit call — `audit(second, true)`, the file's
only occurrence of that string, at `grounding/mod.rs:3180`. There is no second
call site and no arm-specific
re-audit, so which branch produced the text cannot change what checks it. (The
2026-08-12 revision cited `:2852-2857` for this; that range is the *first*
audit's stage-ledger record, not the re-audit. Corrected here.)

The 2026-07-17 CONFAB-LEAK probe is carried anyway, and its ON arm is
deliberately stronger than what shipped: it forces the ratio to **1.0**, routing
*every* repairable turn through surgery, where the shipped default routes half.
A clean ON arm therefore bounds the shipped behaviour from above.

**Result, re-run at HEAD 2026-08-13** — same bank (`secret_agent`, 25 probes),
same forced-longform config, both arms in one session on one binary:

| | OFF (no surgery) | ON (ratio 1.0 — surgery on every repairable turn) |
|---|---|---|
| competence-when-present | 0.71 PASS (12/17, timid 3) | **0.71 PASS (12/17, timid 3)** |
| honesty-when-absent | 0.88 PASS (7/8, hallucinated 1) | **0.88 PASS (7/8, hallucinated 1)** |
| hallucination-rate | 0.12 | **0.12** |
| **CONFAB-LEAKED** | **1** | **1** |
| misses → gate / model / retrieval | 0 / 1 / 3 | **0 / 1 / 3** |
| grounding-fidelity | 0.67 (6 of 9) | 0.71 (5 of 7) |
| blatant-confab-rate | 0.12 (3 of 25) | 0.08 (2 of 25) |
| VERDICT | PASS | PASS |

**The null is reported beside the number (E-naive-baseline).** CONFAB-LEAKED is
**1 on the OFF arm too** — the leak is a property of the bank and the chain, not
of surgery, and a report that quoted only the ON arm's 1 would have looked like
a regression it is not. The two rows that differ are 1-item moves on different
denominators (9 vs 7 value-bearing answers) at n=25; they are **not** claimed as
an improvement.

The fabrication floor does not move, measured, with surgery forced twice as far
as the shipped default takes it.

### Phase 3 — REDIRECTED 2026-08-13: repair the auditor's evidence view

**Phase 3 was written to hand the audit a better draft. It was aimed at the
wrong patient, and its own gating deliverable is what proved it.** Order
`grounding-preventative-medicine` required the per-claim audit to be validated
before anything else in the phase ran (D1). The verdict is **J-BROKEN** — 7 of
27 hand-judged failures genuinely unsupported (25.9%), against a stop line of
<50%. The draft was mostly already grounded; the auditor could not see the
evidence it was grounded in. §7.8 carries the finding and the repair.

**The two original deliverables are DEFERRED, not cancelled, and not silently:**

| deliverable | status | why |
|---|---|---|
| **D2** — resolve the §5 "trust your training over the chunk" contradiction | **deferred** 2026-08-13 | Still a real defect: §5's verdict CONTRADICTORY stands and a prompt that instructs the model to override its evidence is wrong at any yield. But it is not what drives the failure ratio, and §5's own rule ("must be resolved before anything else in this prompt is tuned") gates *prompt tuning*, which is no longer this phase's lever. Carry it into the next prompt-touching order. |
| **D3** — R3 answer sizing via a derived `max_tokens` | **deferred** 2026-08-13 | R3 remains the unowned function §3.4 names, and the sizing asymmetry found while scoping it is real and recorded: the DeepQuery path tells the model it has `inference_config.max_tokens` (2048) while the request enforces `max(config, 4096)` — the plea and the parameter disagree by 2x, and neither derives from the evidence (`retrieval/mod.rs:507`, `streaming.rs:2680`), while the KnowledgeQuery sibling already derives one via `resolve_output_budget`. Deferred because sizing cuts the audited COUNT, and the audited count is not what is costing the turn while two thirds of its verdicts are artefacts. Re-price it after the evidence-view repair lands. |

**Sequencing note, which is the reusable lesson:** R3 was sequenced before the
tombstone so the ladder would be measured at its real sized cost. The same
argument now applies one level down — **the ladder must not be re-priced, and
sizing must not be tuned, until the auditor stops manufacturing its own work.**
Phase 4's 78.2s is inflated by this defect.

*Original Phase 3 text, retained because it is what the deferred deliverables
carry forward. Function basis: R3 (§3.4) — the unowned function whose absence
sets the whole downstream bill (§7.2).*

| Delete | LOC | tokens/turn | ms/turn |
|---|---|---|---|
| The length-pleading prompt block → a derived `max_tokens` | prompt | ~110 | bounds the fan-out structurally rather than hopefully |
| Contested + catalog blocks made conditional (the pattern already exists in the same function) | prompt | **~380 on turns where neither applies** | prefill only |
| Citation-shape prohibition + worked example → a grammar on the existing constrained-decoding hook | prompt | ~240 | prefill only; removes a class of malformed-citation failure structurally |
| The "trust your training over the chunk" block — resolve the contradiction (§5) | prompt | ~60 | quality, not latency |

**Honest accounting on the prompt rows:** their wall-time yield is small and
§7.5 says so. They are funded because that is where the prompt stops containing
instructions that contradict the plan's mechanisms.

**R3 is the exception and it is now load-bearing.** Answer sizing is the only
lever in this plan that cuts *both* remaining large terms at once: a shorter
answer costs fewer decode tokens in synthesis **and** fewer claim judges
downstream, because `claim_budget` is a function of answer length (§7.2). If
Phase 6a misses its gate, R3 is what carries the objective — which is why it is
specified structurally (a derived `max_tokens`) rather than as prompt tuning.

**R3 is sequenced BEFORE the tombstone deliberately**, and that is a change from
the pre-revision order. Answer length drives `claim_budget`, which drives the
ladder's fan-out (§7.2); sizing the answer first means the ladder being
tombstoned in Phase 4 is measured at its real, sized cost rather than at a cost
R3's absence inflated. It also means that if Phase 4 has to be partially reverted
on a quality finding, the reverted ladder is the cheaper one.

**The H3 throughput probe runs here** (§8.3 item 2): one two-sequence lockstep
decode at α=0, one number, no labels. Its verdict selects between 7A and 7B.

### Phase 4 — Tombstone the ladder

*Function basis: G2 is discharged by marking (§3.3); G3's disposition is typed at
the decider (§3.4 corollary). Both replacements are already on main and
default-on. This is old Phase 1's list — converted from deletes to tombstones,
per §9.0.*

| Tombstone (stops executing in this phase) | LOC still on disk | tokens/turn | ms/turn freed |
|---|---|---|---|
| The rewrite pass on the longform path — mark unresolved spans instead of re-synthesising | (within the ladder) | 0 | **5.4s** — the mechanism's price after Phase 2 (§7.3.1), *not* the 43.2s the first draft booked |
| Audit #2 — no rewrite, no input (§7.4) | (within the ladder) | 0 | **50.9s** on the operator's turn |
| Retry machinery: retry floor, retry system notes, re-verify | ~400 | 0 | ladder-vs-no-ladder on the same query with identical retrieval: **54.5–124.8s → 21.8s** |
| Decline-recognition zoo: phrase list, `released_pure_decline`, refusal-opener list | ~250 | 0 | ~0 (correctness) |
| `classify_caveat` + caveat prose classification | ~150 | 0 | ~0 runtime; retires an LLM dependence in the scorer |
| **Adds** | **nothing** | | Every replacement ships today. |

**Every row above carries a `sovereign/DEFAULTS_LEDGER.md` entry in the same
commit** (§9.0 guard 1), and every row is verifiable from the product after
Phase 1: if a tombstoned path fires, the strip shows an `OLD STACK` row.

**Expected wall after Phase 4:** every turn takes the no-ladder shape.
32.6 + 25.4 + 21.8 ≈ **79.8s**, and — more importantly — the 79.8–247.3s spread
collapses toward a single value. **The variance is the user-visible defect
(§7.1); this phase removes it, and it adds no mechanism to do so.**

**Quality gate:** chaos on saltgrass + `secret_agent`, 3 seeds. Bars: no
hallucination-rate regression beyond lane tolerance, honesty ≥ the current
flag-off level, and `grounded_addressed / grounded` reported per turn. The
2026-07-17 CONFAB-LEAK case is a named required probe — §7.4 argues why this
configuration differs, and a *gate you have not watched fail is not a gate*
(compass #5).

### Phase 5 — One deletion pass, when the operator is elated

*Trigger, named so it cannot become "when H0 graduates": the tombstones from
Phase 4 have held across the settling window declared in their
`DEFAULTS_LEDGER.md` rows, the E-wall-time and E-variance bars have readings,
and the operator says the new stack is right.*

One pass removes the tombstoned code. **LOC is reported here as a byproduct and
is not this phase's justification** (§0.1). The justification is that a
tombstone with no delete date becomes the next incumbent.

### 9.6 The residual program — SEQUENCING NOT SETTLED

**This is flagged, not resolved.** The operator's build list names five phases.
Three bodies of work in the pre-revision plan are not in it, are not killed, and
have no agreed position:

| work | pre-revision | where it sits now | why the position is not obvious |
|---|---|---|---|
| **6a** — G1's deterministic tier replacing the per-claim `claim_violation_joint` fan-out | Phase 2a | after Phase 5 | It is a **replacement**, so under tombstone-then-delete it could equally precede Phase 4: you cannot tombstone the per-claim judge until something else answers G1. But Phase 4's list does **not** include the per-claim judge — only the rewrite/audit#2/retry/decline rows, whose function is discharged by marking and needs no replacement. On that reading 6a is genuinely later. |
| **6c** — R2's owner moving from a generative judge to H1's answerability margin | Phase 2c | after Phase 5 | Blocked on a **decision, not a build**: `SOVEREIGN_RERANK_MODEL_PATH` is unset on this host (§7.7), so R2 has no statistical instrument in production today. That decision could be taken at any time and is cheap; it does not obviously belong at position 6. |
| **7A/7B** — H3, and the throughput probe that selects between them | Phases 4A/4B | probe in Phase 3; arms after Phase 5 | §8.3 already says H3 is not on the critical path to 75s. Unchanged by this revision. |
| **8** — collapse the `GateSurface` matrix + the env-flag family | Phase 5 | after Phase 5 | Interacts with tombstoning: a tombstone *is* a flag, so the flag-family target (18 → ≤6) and the tombstone ledger pull in opposite directions for the duration. |

**The question, stated once:** does 6a (the deterministic G1 tier) precede or
follow the deletion pass? It is the only one of the four whose position could
change what Phase 4 may tombstone. **The other three are orderable at the seat's
convenience and nothing below depends on their number.**

#### 9.6.1 Ruling — 6a is deliberately unsettled, and here is its trigger

*Seat ruling, operator-approved 2026-08-12, on the escalation above.*

**6a stays at position 6, and it is unsettled ON PURPOSE rather than merely
undecided.** The distinction matters and is the whole content of this ruling.

First, the ordering constraint does not exist. Tombstone-then-delete would
require a replacement to precede the tombstone of what it replaces — but Phase
4's list **does not include the per-claim `claim_violation_joint` fan-out**. It
covers the rewrite, audit#2, the retry machinery and the decline rows, every one
of which has its function discharged by marking (§3.3) and needs no replacement
built first. So nothing obliges 6a to move earlier.

Second, and this is the ruling: **6a is a data-driven decision deferred to a
measurement that does not exist yet.** After Phase 4 the residual gate is
audit#1 — extract + N per-claim + specifics-scan, **25.55s** on the operator's
turn (§7.2) — and 6a is the lever aimed at exactly that residual. Whether it is
*needed* depends on whether Phases 2–4 already land ≤75s, which is measurable
after Phase 4 and only guessable before it.

| Phase 4's measured outcome | 6a's position |
|---|---|
| median ≤75s, p90 ≤90s | 6a becomes a cost-and-quality improvement, scheduled at the operator's leisure, **after** the deletion pass |
| misses either clause | 6a is the next build and **precedes** the deletion pass |

**Deferring a deletion is cheap; building a deterministic tier on a guess is
not.** This is the same discipline as this plan's refusal to quote 60s as a
result (§9.1): a conditional chain is not an outcome.

**THE DECISION TRIGGER, named so this cannot become another P3c.** The
measurement that settles 6a's position is **Phase 4's end-to-end wall-time
distribution over ≥5 warm-primary runs of the iconic query** — the same lane as
`E-wall-time` (§10.1), read against both its median and its p90 clause. The seat
takes 6a's position from that table on the day it exists, and not before.

*An unsettled item with a named trigger is honest. An unsettled item with no
trigger is how P3c — a real phase, cited by the parity plan, for which no order
was ever opened — cost this initiative four months.*

Their content follows unchanged, renumbered only.

### Phase 6 (was Phase 2) — Stop paying twice for the same question

*Function basis: G1 is a containment question (§3.3), answered structurally
first, statistically second. R2's owner moves from a generative judge to a
calibrated score (§4.2).*

**Read §11.2 before this phase.** An earlier draft of this plan claimed Phase 2
reached the objective on evidence in hand. **The branch survey falsified that,
and the correction is kept in the open rather than edited away.** H4's sentence
margin has a *final* verdict against a bank that could judge, and it lost to a
scorer that consults nothing. That kills one of the two tiers below.

**6a (was 2a) — the deterministic tier. This is the phase's load-bearing bet.**

| Delete | LOC | ms/turn freed |
|---|---|---|
| Per-claim `claim_violation_joint` fan-out on the draft → span resolution / containment against the sealed evidence | (within the ladder, ~1,400 with Phase 1) | audit#1's per-claim leg: **8.0s** on the operator's turn |
| `scan_unsupported_specifics` → deterministic specifics containment | (same) | **10.6s** on the operator's turn |
| `verify_grounding` 2-stage Critic + `violation_prob` | ~300 | the single-claim path's full cost |

**6a's gate is pre-registered against the naive baseline, not against the
incumbent**, and that is the entire lesson of §11.2: a mechanism can clear its
kill bar and still be *worse than answering "supported" unconditionally*. The
bar is therefore: **beat the naive always-supported ceiling on the
longform-negative banks (0.7955 holdout / 0.7347 calibration), with negative
recall reported as a first-class number, not folded into agreement.** H4's
sentence margin scored 0.7674 against that ceiling with negative recall 2/9;
6a fails the same way if it cannot do better, and the plan says so up front.

Deterministic containment was never isolated by any H4 run — the gate scored span
resolution and sentence margin together, and the margin is what carried the
verdict. **This is a genuine gap in evidence, not a claim of a win.**

**6b (was 2b) — the statistical tier for G1's paraphrase case: KILLED here, on the
record.** See §11.2. It is not deferred, not carried as an option, and no phase
below depends on it.

**6c (was 2c) — the one statistical swap that IS evidenced: R2's owner.**

| Delete | LOC | ms/turn freed |
|---|---|---|
| Generative evidence-sufficiency judge in the retrieval loop → H1's answerability margin | (evidence_loop) | one generative call **in the pre-generation critical path** |

H1's margin is the best-evidenced mechanism in the entire program: AUROC 0.8990
vs `top_cosine` 0.7994 over 4,207 pairs, kill bar cleared in 1,000/1,000
bootstrap resamples, honesty-recall **0.665 @ 5% false-alarm against
`top_cosine`'s 0.235 — 2.8x the honesty at the same cost**. H2b's own kill
report puts it more sharply than this plan would dare to: *"the number a fixed
supply must beat is 0.9044 from a 0.6B reranker that runs in milliseconds"* —
and three decodes of a 36B added one part in a thousand over it.

**Two caveats carried, not buried.** (i) H1's point estimate landed 0.0005 below
its declared *beat* bar, so the artifact records `survives`; the bootstrap puts
that distinction at a coin flip and the program proceeded as though it had been
met. (ii) The branch's own Goodhart check found the calibration set's absent
pools are a rotation within a single article, so **+0.0995 is the topic-constant
number** — the task measured was closer to "is the answer in this article" than
"is it in this passage". Re-gating H1 at the seam where it will actually be used
is part of 6c, and it needs the calibration set that lives off main (§11.3).

**Prerequisite for 6c, and it is a decision not a build:** take the owner
decision on `SOVEREIGN_RERANK_MODEL_PATH` (§7.7). Vendored, wired, inert,
pending.

**Expected wall after Phase 4 + 6a, *if 6a clears its naive-baseline gate*:**
gate 21.8s → ~2s; 32.6 + 25.4 + 2 ≈ **60s**, meeting the objective with headroom.
**If 6a misses, Phase 6 does not reach the bar and Phase 3's R3 sizing must carry it.** That
conditional is stated here rather than discovered later; see §9.1.

### Phase 7A (was 4A) — Make the constraint native (only if the H3 probe clears ≥0.80x)

| Delete | LOC | tokens/turn |
|---|---|---|
| Anti-fabrication guardrail block — S2 enforced at token selection | prompt | ~350 |
| "Preserve source terminology" — the tilt does this mechanically | prompt | ~150 |
| `citation.rs` + `citation_attribution.rs` quote-then-answer path — its function is subsumed; its deterministic verifier survives inside G1 | ~1,800 | — |

### Phase 7B (was 4B) — H3 killed on the record (if the probe misses)

No prose surrogate is deleted. The blocks in 4A survive on **FUNCTION**, because
the mechanism that would have replaced them does not exist. The residual
detection stays with Phase 2's deterministic sweep plus the margin, and the
initiative's LOC target is restated downward with the arithmetic. **This is a
legitimate end state and it is pre-registered here so it cannot be quietly
avoided.**

### Phase 8 (was 5) — Collapse the control surface

| Delete | LOC | note |
|---|---|---|
| 8-surface `GateSurface` profile matrix → one decider | ~400 | compass #8 |
| The grounding env-flag family, 18 → ≤6 | — | spec §9's own target; each retirement carries its `quality/env-flags.toml` row and its `DEFAULTS_LEDGER.md` row in the same commit |

### 9.1 The arithmetic to 75s, stated with what is certain and what is not

The order requires a stop-and-say-so if ≤75s is unreachable on this hardware
without a quality regression. **It is not unreachable, and the arithmetic below
is the evidence — but neither is it proven reachable on evidence in hand, and
this plan will not claim it is.**

| step | wall s | basis | confidence |
|---|---|---|---|
| Incumbent, warm, best of 4 | 79.8 | measured (`4ed783c9`) | measured |
| Incumbent, warm, median of 3 | 123.8 | measured, n=3 | measured |
| After Phase 1 (glassbox) | **unchanged** | reporting only, no behaviour change | **by construction** |
| After Phase 2 (surgical cap) | **≈−25 to −38s on a ladder turn that would have fallen back** | 43.2 vs 5.36s by census (§7.3.1); 27.5 vs 1.7–2.7s off the Phase 1 strip (§9 Phase 2) | **two instruments, n=1 per arm each — a bound, not a distribution** |
| After Phase 3 (R3 sizing) | not derivable | cuts synth decode and `claim_budget` together; magnitude unmeasured | **structurally sound, unquantified** |
| After Phase 4 (tombstone the ladder) | **≈79.8, and the 79.8–247.3 spread collapses** | every turn takes the measured no-ladder shape; nothing added | **arithmetic from measured runs** |
| After Phase 6a, if it clears | ≈60 | gate 21.8 → ~2s | **conditional on 6a's gate** |

**Note the re-ordering's effect on the arithmetic and it is not cosmetic.** Under
the pre-revision order the ~79.8s no-ladder shape arrived at Phase 1; it now
arrives at Phase 4. Phases 2 and 3 land latency *before* it — Phase 2 with a
measured magnitude, Phase 3 with an unquantified one — so the plan is no longer
front-loading its whole win onto a single deletion phase. That is the point of
the re-cut: **the first three phases are all things that can be verified from the
product**, and only the fourth needs a chaos run to clear.

**Three independent levers each individually sufficient for the remaining ~5s**,
which is why the objective is not declared unreachable: Phase 6a's deterministic
tier; Phase 3's answer sizing; and the retrieval term, which is 32.6–51.1s,
**unmeasured internally** (§7.6), and known to contain two generative calls of
its own. It would take all three failing to put 75s out of reach on this
hardware, and no measurement in hand predicts that.

**What this plan refuses to do is quote 60s as a result.** A single conditional
chain reported as an outcome is the failure mode note `c5b5d8a0` corrected on
this very initiative eleven days ago.

### Running ledger — what stops EXECUTING, and (as a byproduct) what leaves disk

**Read the first column, not the second.** The first column is what this plan is
judged on; the second is reported because `NATIVE_GROUNDING.md` §9 set a LOC
target, and a byproduct that is tracked is not thereby a bar (§0.1).

| | stops executing | LOC leaving disk (byproduct) | judge prompts retired from the runtime | env flags |
|---|---|---|---|---|
| Spec §9 target | — | ≥4,000 | ~8 → 0 | 18 → ≤6 |
| Phase 1 (glassbox) | nothing | **0 — and that is a success, not a miss** | — | +0 (the strip is not flag-gated) |
| Phase 2 (surgical cap) | the inverted-cap fallback to full re-synthesis | 0 | — | — |
| Phase 3 (R3 sizing) | ~790 tokens/turn of prompt | prompt | — | — |
| Phase 4 (tombstone, unconditional) | the longform rewrite, audit#2, the retry machinery, the decline zoo, the caveat classifier | **0 — tombstoned, not deleted** | the longform rewrite + audit#2 | +5 tombstone flags, each with a `DEFAULTS_LEDGER.md` row |
| Phase 5 (deletion pass, on trigger) | — | ~800 + the rewrite/audit#2 path | — | −5 (the tombstone flags retire with the code) |
| Phase 6a (gated) | the per-claim fan-out, specifics scan, single-claim verify | ~1,650 | same | — |
| Phase 6c (gated) | retrieval's sufficiency judge | evidence_loop | same | — |
| Phase 7A (gated on the H3 probe) | the quote-then-answer path | ~1,800 | — | — |
| Phase 8 | the `GateSurface` matrix | ~400 | — | 18 → ≤6 |

**Only Phase 4's row is unconditional.** Every other row from 6 onward names its
gate. A ledger that totals conditional deletes into a headline is how "≥4,000
LOC" stayed on the books for four months while the codebase grew.

**Phase 4 temporarily makes the env-flag count WORSE** (five tombstone flags
against §9's 18 → ≤6 target), and the plan says so up front rather than
discovering it at Phase 8. That is the price of decoupling the latency win from
the delete's blast radius, it is bounded by Phase 5, and every one of the five
carries a review-by date in `DEFAULTS_LEDGER.md` — which is what makes the debt
dated rather than permanent.

## 10. Bars

**The bar is the user's experience.** Per-mechanism metrics are supporting
evidence and may not stand in for it.

### 10.1 The end-to-end bar

**On the iconic query, in the desktop app, with a warm resident primary:
median wall ≤ 75s AND p90 ≤ 90s, over ≥5 runs.**

- **The p90 clause is not decoration.** The incumbent's defect is variance
  (§7.1: cv 0.28 on the gate alone, identical retrieval). A median-only bar is
  satisfiable by a system that still hands the operator a 247s turn. The spec's
  p50-only clause is superseded here for that reason.
- **Primary residency is stated with every number** (`primary_idle_secs = 1800`);
  a cold-primary turn is a different machine and is reported separately, never
  pooled.
- **Incumbent baseline for comparison, stated with its n:** four CLI runs,
  wall 79.8 / 123.8 / 177.3 (cold) / 247.3; warm-only n=3, median 123.8, min
  79.8. n=4 is below this bar's own ≥5 requirement and is labelled as a bound on
  the incumbent, not a measurement of it.
- Lane: `./target/debug/sovereign-cli chat ask "Is free will compatible with
  determinism?"`; the census harness at `~/.sovereign/comaintainer/gate-census.py`
  reproduces the per-call decomposition from artifacts already on disk.

### 10.2 Quality bars carried alongside, per phase

No phase that changes what the system DECIDES lands on latency alone. Every such
phase runs chaos on saltgrass + `secret_agent`, 3 seeds, and must hold
competence / honesty / hallucination within committed-baseline tolerances.
**Phase 4** additionally requires the 2026-07-17 CONFAB-LEAK probe as a named
case.

**Phase 1 is the exception and it is named as one rather than assumed.** The
glassbox changes what the system *reports* and never what it decides; no verdict,
action, threshold or prompt moves. Running a chaos suite against it would be
measuring noise, and reporting "no regression" from a run that could not have
regressed is a green nobody earned (compass #5's *never-ran* verdict, dressed as
*passed*). Its gate is the acceptance test in §9 Phase 1, plus the desktop
`npm run check` / `npm run test` and the two workspace gates. **The falsifiable
claim that Phase 1 changed no behaviour is carried by those gates and by the
diff, not by a bench number.**

**`E-draft-grounding` — the leading indicator, declared 2026-08-13.**

**On the iconic query, in the desktop app: median failed/audited ratio and the
count of ZERO-FAILURE turns, over >=5 warm-primary runs.** A zero-failure turn
is the one outcome that skips the rewrite AND the re-audit entirely
(`grounding/mod.rs`, `if failed.is_empty()`), so it is worth ~99s where moving
the ratio inside the failing band is worth ~33s. **The zero-failure COUNT is
the bar's headline; the median ratio is its supporting statistic** — a ratio
that improves while the count stays at zero has not reached the cliff.

Recorded baseline, and it is the reason this bar exists: **0 zero-failure
passes out of 18** across 9 turns (5 CLI + 4 desktop), median first-pass ratio
0.60. Every single pass paid a rewrite and a re-audit.

It is declared as a LEADING indicator, not a substitute for `E-wall-time`:
§10's rule stands that the bar is the user's experience. This one exists
because the wall-time bar cannot say *why* a turn cost what it cost, and the
whole of §7.8 was invisible until something counted failures by mechanism.

### 10.3 The naive-baseline bar — the one §11.2 bought

**Every mechanism gate in this plan reports, beside its own number, the score of
the cheapest scorer that consults nothing** (always-supported, always-answerable,
whichever is the degenerate strategy for that facet), on the same pool it
scored. A mechanism that does not beat the null does not ship, regardless of
which kill bar it cleared.

This is not a general-purpose good practice added for tidiness. It is derived
from two recorded events on this initiative: H4's earlier runs scored against a
set whose null strategy achieved 1.0000, and its final run lost to a null of
0.7906. Both were only visible because someone computed the null.

### 10.4 Registered in `quality/initiative-bars.toml`

Five bars were added under the `native-grounding` initiative so
`co-lineage.py coverage` renders this plan's coverage from day one:
`E-wall-time`, `E-variance`, `E-deletes-land-per-phase`, `E-naive-baseline`,
`E-h3-probe`. Their full text lives in the registry. **A bar the rollup cannot
see is a bar that can be silently deferred again** — which is precisely what
happened to `H0-latency`.

**Revision 2026-08-12 — one retired, two declared.**

- **`E-deletes-land-per-phase` is RETIRED**, with a `descoped` transition
  pointing at §9.0. The registry's transition vocabulary is a closed set of
  seven and does not carry a `superseded` value; `descoped` is the member whose
  definition fits ("deliberately removed; closes the bar by decision, not by
  evidence") and the `by` field carries the pointer. It is **not** edited in
  place: this bar's whole content was the ratchet the operator replaced, and
  editing it would erase the fact that a ratchet was tried, aimed at LOC, and
  retargeted at execution. The transition is the record.
- **`E-tombstone-ledger` is DECLARED** in its place: every tombstoned path
  carries its `DEFAULTS_LEDGER.md` row in the same commit, and no tombstoned
  path executes on the default configuration. It is the tombstone-then-delete
  discipline made falsifiable — the guard against §9.0 collapsing back into
  "nothing is deleted until H0 graduates".
- **`E-glassbox-attribution` is DECLARED** for G4, which §3.4 named ABSENT and
  for which no bar had ever existed. It is a human-legibility bar on purpose: the
  defect is that a person could not tell what the system did.

`E-glassbox-attribution` is covered from the day it lands — order
`grounding-glassbox-stack-attribution` performs it — which makes it the one E-bar
that does **not** render `UNCOVERED / never-attempted`. `E-tombstone-ledger`
renders uncovered until a Phase 4 order exists, and that is correct.

**All five render `UNCOVERED / never-attempted` on the day they land, and that is
correct — do not "fix" it.** Coverage is a property of *orders* (does any order's
`serves:` name this bar); verdict is a property of *evidence*. These bars are
declared by a plan; the orders that execute the phases do not exist yet. Adding
them to this order's `serves:` line would manufacture the appearance of coverage
for work nobody has done — the exact failure the registry's own header warns
about. The rollup's headline moving from 1-of-13 uncovered to 6-of-18 is this
plan making five previously-untracked promises *visible*, not a regression.

`H0-latency`, `H0-judge-free` and `H0-loc` are re-entered by this plan (they were
`deferred` by the parity plan §5 and never re-entered). `H3-tilted-decode` keeps
its `never-attempted` verdict — no evidence event has occurred — and its decision
is carried by `E-h3-probe`, which is the falsifiable thing §8.3 actually
produces.

## 11. Inventory — the unmerged branch, and what to do with it

### 11.1 What is actually there

`skunkworks/native-grounding` is 42 commits and ~143k insertions ahead of main
and has never been merged. Three lineage facts frame this section:

- **`NATIVE_GROUNDING.md` itself was not on main until 2026-08-12.** Sixteen
  orders cited a contract their own repo could not resolve. It is on main now,
  byte-identical to the branch's copy.
- **Phase 0's deliverables landed in a worktree on that branch**, which is why
  `co-lineage.py` reports that no verdict artifact for `P0-tau-sweep` or
  `P0-scorer-determinism` resolves on main.
- **The 143k-insertion headline overstates the gap.** Five of the 42 commits are
  already on main by patch-id; of the 201 files in the three-dot diff, 22 are
  byte-identical to main and 96 are genuinely absent. **Main has also diverged
  *forward*:** main carries `native_grounding/admission.rs` and `segments.rs`,
  which the branch does not; the branch carries `sentence_sweep.rs` and
  `meaning_cluster.rs`, which main does not. This is a fork, not a backlog.

The separate worktree on `feat/native-grounding-default-on` is 0 ahead / 27
behind main and is **not** a descendant of the skunkworks branch. It is unrelated
soak/telemetry work and is out of scope here.

### 11.2 The correction this survey forced on this plan

**An earlier draft of §9 Phase 2 asserted that H4's `failed` verdict was caused
by label supply rather than by the mechanism, and that merging a better bank was
therefore a Phase 2 prerequisite. Both halves were wrong, and the survey proved
it. The claim is retracted here rather than quietly edited out.**

1. **The longform-negative banks are already on main, byte-identical.**
   `saltgrass.toml`, `saltgrass_compound.toml`, the 2026-08-08 longneg harvest
   results, and `LONGFORM_NEGATIVES_FINDINGS.md` all resolve on main today.
   There was no prerequisite to merge.
2. **H4 was re-run against exactly that bank, and it lost anyway.** The final run
   (`h4/rerun_20260809_longneg/`) scored agreement **0.7674** (33/43) against a
   naive always-supported ceiling of **0.7906** on the pool it actually scored
   (0.7955 over all labels). `naive_baselines.json` records
   `"mechanism_beats_naive": false`. The bank's own representativeness verdict
   says it plainly: **"TWO-CLASS AND ADEQUATE TO JUDGE. This run's failure to
   beat is a property of the mechanism, not of the bank."**
3. **Negative recall was 2 of 9.** The sentence margin caught two of the nine
   genuinely-unsupported claims. The branch's own words: *"A scorer that answers
   'supported' unconditionally, consults no evidence and loads no model, agrees
   with the incumbent ladder more often than the sentence margin does … it is
   negative-value as a replacement for the incumbent's per-claim call."*

**Consequence, carried into §9:** the sentence-support margin is **killed** as
G1's paraphrase tier. What survives untested is the *deterministic* tier — no H4
run isolated span resolution from the margin — and Phase 6a is gated against the
naive baseline precisely because that is the bar this mechanism failed.

**The generalisable lesson, and it belongs in the method not just the ledger: a
mechanism can clear its declared kill bar and still be worse than a scorer that
looks at nothing.** H4's earlier runs recorded `could_not_judge` against a
single-class set whose naive ceiling was 1.0000; the fix produced a set that
could judge, and the mechanism then lost to the null. Every gate in this plan
therefore carries a naive-baseline column. That is compass #7 — validate the
instrument before the result — extended one step: *validate the baseline before
the mechanism.*

### 11.3 Triage

**MERGE — evidence and instruments the phases will re-run.**

| item | why |
|---|---|
| The per-turn **stage-timing sidecar** in the chaos harness (`StageTimings`, `stage_ms` = turn/search/synth/verify/value/score, `null` for did-not-run, pinned by test) | The cheapest unblock of every latency claim this plan makes. **But see the caveat below — it does not by itself close `P0-latency-harness`.** |
| The four machine-readable verdicts (`h1_verdict.json`, `h2_verdict.json`, `h2b_verdict.json`, both `h4_verdict.json`) and the six FINDINGS/VERDICT docs | These are the **only** record on any branch of what H1/H2/H2b/H4 answered, and they exist nowhere on main. Losing them means re-deriving answers already paid for. |
| The 4,207-pair SEP calibration set + contamination report (`clean: true` against all three banks) and its derived score files | Real, balanced label supply (naive ceiling ≈0.50) — the only such set the program has. Required by Phase 6c's H1 re-gate. Carry its Goodhart caveat with it (§9 Phase 6c(ii)). |
| The τ-sweep reader, operating-curve and calibration modules; the H1/H2/H4 bench gates; `shared_backend()` (lets a reranker and an embedder co-load in one process) | Phase 2's gates need them, and `shared_backend()` is a hard prerequisite of running the reranker alongside the embedder at all. |
| The `n` / `logprobs` / `seed` / `token_logprobs` contract surface **together with the engine's refusal gate** | The refusal gate is the valuable half: it makes an unimplemented capability fail loudly instead of silently degrading (compass #6). |

**Caveat on the timing merge, stated because the branch states it:** the stage
timings landed in the transcript **sidecar**, not on `ResultRow` — and the gates
read `ResultRow`. Both H4 verdicts carry the admission verbatim: *"the chaos
ResultRow carries no per-stage timing, so this gate cannot put a measured
incumbent number beside its own."* **`P0-latency-harness` remains
`could-not-judge` after this merge**, and closing it is a named item, not an
assumed side effect.

**ARCHIVE with the verdict — answers, not code.** H2's k-sample decoder, sampler
sweep and meaning clusterer; H2b's two-arm counterfactual harness. Both have
clean quantified kills: H2 *"k=5 has no variance to offer at any temperature"*
plus a single-class label set (0 hallucinating / 71 clean) that makes AUROC
undefined for **every** signal including the Critic's own; H2b arm-B refusing on
80.9% of absent pairs, dependence AUROC 0.6234 against a 0.85 clause and
**+0.0010 over the reranker margin**. **These answers are cited by this plan, not
re-derived.** `k_sample.rs` is the exception: it is archived as *code* too,
because §8.2(a) reuses it as H3's probe substrate.

**DEAD.** The five commits already on main by patch-id; the 22 byte-identical
files; and — as *evidence* — any H4 result computed against the single-class
held-out set whose naive ceiling was 1.0000. That was a measurement of the set,
not of the mechanism, and it must not be cited either for or against H4.

**DO NOT MERGE DIRECTIONALLY — re-derive.** `native_grounding/mod.rs`,
`span_resolver.rs`, `grounding/mod.rs` (~1,132 changed lines) and
`grounding_verdict.rs`. Main took a different continuation and is ahead on this
surface. The branch's `native_grounding/mod.rs` declares itself *"zero callers in
the runtime, by construction — the offline measurement surface for the H4 gate"*;
main's is live. Merging toward main would regress it.

### 11.4 The plan's actual dependency on unmerged work

**Phases 1, 2, 3, 4 and 5 depend on nothing off main.** Phase 6a depends on
nothing off main (its bank is already there — §11.2). **Phase 6c depends on the
SEP calibration set and `shared_backend()`.** The H3 probe in Phase 3 reuses
`k_sample.rs`. That is the whole of it, and it is deliberately narrow: a plan
that needed a 143k-insertion merge before its first delete would be the
measurement-scaffolding failure this order forbids.

**The re-cut improved this**, incidentally: under the pre-revision numbering the
first three phases spanned an off-main dependency (2c); under the new one the
entire operator-named sequence, Phases 1 through 5, is executable from main
alone.

## 12. What would falsify this plan

### 12.1 The pre-registered kills

| # | If this is observed | Then |
|---|---|---|
*Renumbered by the 2026-08-12 revision; K7 and K8 are new and both are properties
of the revision itself.*

| # | If this is observed | Then |
|---|---|---|
| K1 | **Phase 4** lands and the wall-time spread does not collapse toward the no-ladder shape | The ladder was not the variance source. Re-derive §7.1 before funding Phase 6. |
| K2 | **Phase 4's** chaos run regresses hallucination beyond lane tolerance — in particular the 2026-07-17 CONFAB-LEAK case | Marking does not discharge G2 in practice. Un-tombstone the repair pass — which under §9.0 is a flag flip, not a revert — re-price §9, and say so. **This kill is materially cheaper to act on under tombstone-then-delete than it was under delete-as-you-land, and that is one of the reasons the ratchet was retargeted.** |
| K3 | **Phase 6a's** deterministic tier does not beat the naive always-supported ceiling on the longform-negative banks | G1's cheap tier is exhausted — the margin already lost there (§11.2) and containment would have joined it. The per-claim judge stays, bounded, and **Phase 3's R3 sizing becomes the sole route to the bar.** Say so and re-price §9. |
| K4 | The H3 probe reads < 0.80x throughput | Phase 7B. H3 killed on the record for this objective, prose surrogates retained on FUNCTION, the LOC target restated downward with the arithmetic. |
| K5 | **Phases 1–4** land and the median wall still exceeds 75s | Retrieval is the binding term (§7.6) and it is unmeasured. That, not the gate, becomes the next order — and its decomposition is a legitimate instrument at that point, because latency wins have already been landed. |
| K6 | Any phase's gate reports a win without a naive-baseline column beside it | Reject the result unread. This is §11.2's lesson and it is the one failure this initiative has already made twice. |
| **K7** | **Phase 1's strip renders a turn as new-stack-only while the `gate-census.py` join over `daemon.err` + the grounding journal shows incumbent mechanisms ran on that turn** | The strip is attributing from flags, not from execution — the exact failure §9.0 Phase 1 exists to prevent, reproduced in the instrument built to prevent it. The census is the validated instrument (compass #7); the strip is wrong until proven otherwise. **Do not proceed to Phase 4 on a strip that has failed this check**, because Phase 4's verification *is* the strip. |
| **K8** | **A tombstone from Phase 4 is still executing 30 days after its `DEFAULTS_LEDGER.md` review-by date, or Phase 5 has no dated trigger** | Tombstone-then-delete has collapsed into "nothing is deleted until H0 graduates" wearing a new hat. That is the failure this plan was written to correct, recurring one level up. `E-tombstone-ledger` fails, and the deletion pass is scheduled by the seat rather than waited for. |

### 12.2 What this plan asserts that the parity plan did not

The parity plan's §6 selected the program's path from three candidates compared
on honesty, competence and cost — **with no latency column**. Every comparison
table in this document has one. That is the specific structural correction, and
if a future revision of this plan drops it, the objective will be lost the same
way twice.
