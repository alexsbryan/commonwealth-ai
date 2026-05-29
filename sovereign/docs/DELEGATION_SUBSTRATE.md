# The Delegation Substrate — north star

**Status: intent doc, not a contract.** Unlike `SYSTEM_OVERVIEW.md`
(which asserts what exists *today* and must verify per `ARCH_PRINCIPLES
§1.1`), this file describes a *direction* — the general capability the
"Digital Team" work is building toward, and the discipline for getting
there. Where it names code, the citation is verified at time of writing
and tagged **[built]**; everything else is tagged **[planned]** or
**[seam]**. If you build a piece, move its line from planned to built
and cite the file. Do not let the aspiration drift into a false claim of
current state — that is the failure mode `§1.1` exists to prevent.

Related: the v1 plan at `~/.claude/plans/here-s-your-challenge-you-ve-
unified-avalanche.md` (retrieval-delegation skeleton); `ATOS.md`
(feature lifecycle the delegations attach to); `WORK_ATLAS.md`
(coordination); `docs/TDD_MACHINE.md` (a code-gen backend, deferred).

---

## 1. The capability, named

The Digital Team, generalized, is a **measured-capability delegation
engine**: a way for a principal (me, Claude Code) to hand bounded work
to flaky-but-improvable local juniors, where *the system itself is the
training environment*. Strip the software domain away and the invariant
shape is eight pieces:

1. **Contract** — bound the work: tool surface, budget, definition of done.
2. **Gate** — a verifier *cheaper than the generator*. The whole game.
   Delegation pays only when `P(success) × gain > P(fail) × loss`; the
   gate is what makes `loss` small and `P` knowable. A delegation
   without a cheap gate is not a delegation, it is a gamble.
3. **Capability-matched routing** — a *measured* (never assumed) map of
   task-class → which junior can do it at what success rate.
4. **Escalation seam** — clean hand-back-up, with enough context, when
   the junior hits a capability wall or fails the gate.
5. **Coaching flywheel** — capture failures → cluster by signature →
   turn clusters into recipe / gate / prompt edits. The system learns.
6. **Economic measurement** — does delegation actually save the
   principal's scarce resource (tokens / attention), per task-class.
7. **Coordination** — multiple juniors and peers, without collision.
8. **Glassbox** — every delegated decision traceable (`ARCH §0.1, §9`).

### The structural compression

Only **three** of these are domain-specific:

- the **tool surface** the junior may call,
- the **gate / verifier**,
- the **capability map** (task-class → measured ability).

The other five — contract, escalation, flywheel, measurement,
coordination, glassbox — are **domain-invariant**. A general delegation
tool is those five built *once*, with the three *plugged* per domain.

This is the entire thesis of the document. Everything below is in
service of building the five invariants well and keeping the three
plug-points clean.

---

## 2. We have already built the invariants — twice

The reason this is a *recognition* exercise and not an *invention* one:
the substrate's core pieces already exist as parallel implementations
across two domains in this repo. The general tool is their unification
behind named seams, not a greenfield project.

| Substrate piece | Software domain | Data / Enron domain |
|---|---|---|
| **Gate / verifier** | `sovereign-tools/src/code/lint_status.rs`, `test_status.rs`; `sovereign-agent-bench/src/judge.rs` (dims a/b/c) **[built]** | `sovereign-eval/src/entity_resolution_score.rs` `b_cubed()` + pairwise-F1; `corpus-engine/assets/judges/business_entity_v1/{prompt.md,exemplars.jsonl}` calibrated judge **[built]** |
| **Capability bench** (task-class × measured ability + split discipline) | `sovereign-agent-bench` (8 problems, scored 0..3, `--judge-trials N`) **[built]** | `sovereign-eval/src/entity_resolution_bench.rs` `Split{Train,Test,Holdout}` + peek-budget — *more* disciplined than the software bench **[built]** |
| **Substrate-once pattern** (build invariant, verticals inherit) | *(this document)* | `corpus-engine/src/enrichment/reconciliation/{multi_origin,oplog,signals}.rs` + asset-store / described-asset dispatcher (architecture-over-Enron) **[built]** |
| **Coaching flywheel** | `corpus-engine-notes` (open `kind` String, `notes.rs:84`) + `lessons` aggregator **[planned]** | same notes DB **[built infra / unused for this]** |
| **Coordination** | `sovereign-work-atlas` `put_claim`/`release_claim` **[built]** | same |
| **Glassbox** | `tracing`, `ResponseProvenance` **[built]** | same |

The two benches and the two gates are the load-bearing observation: the
honest-measurement discipline that makes delegation safe has been
independently arrived at in both domains. The Enron side's
`Split{Train,Test,Holdout}` + peek-budget is the stronger of the two and
is the model the unified capability registry should adopt.

---

## 3. The discipline — generality is earned, not declared

There is a trap in "design the general tool, then build toward it":
top-down speculative generality is exactly what `ARCH_PRINCIPLES §10.3`
(helper before trait until ≥4 copies) and `§16` (patterns earn their
place from production, not theory) forbid. Build the grand substrate
first and you abstract over guesses.

The discipline that resolves it is *this codebase's own
architecture-over-Enron philosophy* applied to delegation:

> Build v1 **concrete**. **Name** the three seams so the second
> implementation is a plug-in, not a refactor. Let the **second and
> third use-case pull the generality out.**

The Enron substrate is the proof it works: no one designed a "general
binary-ingest substrate." Someone built asset-store + dispatcher +
reconciliation for *one* corpus, named the seams (`AssetSubExtractor`
trait + registry, `EdgeType::Attaches`, reversible oplog), and now Firm
Inbox / calendar / transactions / sensor verticals inherit it unchanged.
Delegation follows the same arc.

### The three seams to name (and only these)

- **`Gate`** **[seam]** — `trait Gate { fn verify(&self, output) -> GateReport }`.
  A `GateReport` carries `all_passing: bool` + diagnostics. The citation
  check, `lint_status`, `test_status`, and `b_cubed`-threshold are all
  *implementations*. The delegation loop depends only on the trait.
  Structural, not procedural (`ARCH §7.1`): the loop's success type is
  constructible *only* from a passing `GateReport`.
- **`CapabilityClass`** **[seam]** — a closed enum of task-classes
  (`Retrieval`, `DocDraft`, `EntityResolution`, …, later `Scaffold`,
  `RedGreen`). Routing reads a measured `CapabilityClass → {junior,
  score, gate}` table. The class is the join key between a delegation,
  the gate it needs, and the bench row that measured it.
- **`Recipe` / `FailureSignature`** **[seam]** — the coaching flywheel's
  data. A recipe is hand-edited (`ARCH §6`: data, not program); a
  failure signature is a structured, clusterable reason-for-failure that
  the `lessons` aggregator groups and that I act on weekly.

Everything else in the loop (`execute_reason_with_tools` at
`sovereign-core/src/executor.rs:808` **[built]**, the work-atlas
drop-guard, the `spend_tag`/`escalation` notes, the telemetry) is
domain-invariant and written once.

### The guardrail

Do **not** build the seams' second/third implementations speculatively
into v1. Build v1, name the seams, **stop**. Each new `Gate` impl lands
only when a real delegation needs it. The general substrate falls out of
serving verticals you are already building — not from a generalization
project with no second caller.

---

## 4. The sequence

Each step is concrete, ships value, and is the cheapest possible test
that the *next* level of generality holds.

1. **Retrieval-delegation v1 — concrete, seams named general.**
   The citation gate is `impl Gate for CitationGate`, not a free
   function. The fast-slot junior is `route(CapabilityClass::Retrieval)`,
   not a hardcode. One implementation, general names. Cost over v1: a few
   trait names, not new behaviour. (Full plan: the v1 plan file.) **[planned]**

2. **Second software task-class → proves `Gate` generalizes within a
   domain (cheapest test).** Doc/drift drafting with
   `impl Gate for NarrativeMatchGate` (built on `drift_findings`). If two
   software gates plug into one unchanged `delegate` loop, the seam is
   real — the `§5.4` "two sources, one pipeline" validation. **[planned]**

3. **First cross-domain gate → the moment it is a general tool.**
   Wrap the existing `entity_resolution_score::b_cubed` **[built]** as
   `impl Gate for EntityResolutionGate`. Now `delegate` can run: *"junior,
   resolve these entity-pairs on the Enron train split; gate = B³ ≥ θ."*
   Same loop, same contract / escalation / flywheel / telemetry — a data
   task, a data verifier, a data capability-bench, all of which already
   exist. Generality is earned in roughly one PR *because the pieces
   pre-exist*. **[planned]**

4. **Capability map becomes a measured table, not a constant.**
   The two benches become rows in one `CapabilityClass → {junior,
   measured_score, gate}` registry, adopting the Enron bench's
   peek-budget / holdout discipline as the honest-measurement spine.
   *This* is what makes routing principled across domains. **[planned]**

The verticals already on the roadmap make step 3 inevitable: Firm Inbox,
sales intelligence, and the calendar / transactions / sensor ingest
verticals are all delegation-shaped — *"junior, reconcile these origins;
gate = oplog reversible + B³ holds."* The substrate serves them; it is
not built apart from them.

---

## 5. What this is not

- **Not a mandate to build steps 2–4 now.** v1 (step 1) is the only
  committed build. Steps 2–4 are the shape the seams must *admit*, not
  work to schedule.
- **Not a code-gen plan.** Code-generation delegation (scaffold,
  red→green) is deferred behind a measured capability gate — see the v1
  plan's Phase 3 and the bench evidence that motivated it. The
  delegation substrate is gate-agnostic; whether a *code-gen* gate is
  cheap enough to pay is an empirical question answered by the bench, not
  by this document.
- **Not a new runtime.** The loop is `execute_reason_with_tools`
  **[built]**; the coordination is the work-atlas **[built]**; the
  flywheel is the notes DB **[built]**. The substrate is an arrangement
  of existing parts behind three named seams, not new machinery.

---

## 6. The one-line test for any future delegation feature

Before adding anything here, ask: *does it make the five invariants
better, or is it really a fourth domain-specific plug?* If it is a plug,
it belongs behind `Gate` / `CapabilityClass` / `Recipe` — not in the
loop. If it is an invariant, it is written once and every domain
inherits it. Anything that does not fit either bucket is probably
speculative generality, and `§16` says leave it out until production
asks for it.
