# The refactor factory

**This file replaces four documents written on 2026-08-23** —
`AGENT_MAINTAINED_CODEBASE.md`, `CONVERGENCE_PLAN.md`, `CONVERGENCE_VALIDATION.md`
and the first draft of this one. They are deleted, not archived. Four
overlapping documents on one subject is the same disease as ten `Verdict`
definitions, and leaving the superseded ones on disk is how a codebase stops
being its own source of truth.

**This spec is provisional until the tool exists. The tool is the artifact; this
file dies when `svrn refactor` can print its own plan.**

## THE BAR — the objective, and why it cannot be gamed

Operator, 2026-08-23, verbatim and total:

> An agent writes new code because new is local, certain, and needs no
> discovery; reuse needs knowing something exists and trusting it fits.

That is a COST MODEL, not a complaint. Five terms: new is *local* (no
navigation), *certain* (it will work), *no discovery* (no search). Reuse costs
*knowing* (discovery) and *trusting* (verification). The agent picks correctly
given those costs. **The objective is to invert the costs, and the bar must
measure the DECISION, not the code.**

### The bar

> **MISS RATE — of the new symbols agents create, the fraction that duplicate
> something which already existed and was reachable.**
>
> `misses / all-new-symbols`, over commits this initiative did not direct.

A *miss* is one decision point where reuse was available and the agent wrote new
anyway. That is exactly the sentence above, counted.

### Why it is not Goodhart-able — the five attacks, and why each fails

1. **Convert old code to move the number.** Fails. Converting changes the STOCK.
   The bar is a rate over NEWLY BORN symbols. Refactoring 6,936 `corpus_id`
   sites moves it by zero.
2. **Write documentation.** Fails. Docs are not in the scan window at the moment
   of decision, and the bar only counts decisions.
3. **Mint more shared types so there is more to reuse.** Fails, and *backfires* —
   every new type is another thing a future symbol can duplicate. Adding
   abstractions raises the denominator's risk, not the score.
4. **Do less work.** Fails. It is a RATIO over one population (new symbols), not
   a count, so volume cancels.
5. **Do the reuse ourselves.** Fails by construction — commits belonging to this
   initiative are excluded from the numerator AND the denominator. We cannot
   manufacture our own result. (This is the guard `hpr-unprompted` already
   carries; it generalises.)

**The only way to move this bar is for an agent nobody instructed to reach for
an existing thing instead of writing a new one.** That is the objective, stated
as arithmetic.

### The one attack that DOES work, and its guard

Loosen what counts as "duplicates something existing" and the miss rate falls
without anything improving. That is the real vulnerability.

**Guard: the equivalence detector is FROZEN before the first intervention and
its threshold is never touched again.** `converge shape` (IDF-weighted field-set
cosine, 92.5% precision) plus `converge noun` are the adjudicators, at recorded
settings. Changing a threshold mid-campaign invalidates the series and the
series restarts. Same discipline as gate zero, applied to the bar itself.

### Baseline first, and it is computable TODAY

The miss rate is measurable RETROSPECTIVELY over git history. **Compute six
months of baseline BEFORE any intervention lands** — then the baseline cannot be
chosen later to flatter a result. Anchor: the corpus already grew 1,996 / 1,535 /
1,000 / 850 / 643 new type definitions per month from April to August 2026.

### When the bar does not move, the cost model says why

The three clauses are the diagnostic, in order:

| clause | question | instrument |
|---|---|---|
| **discovery** | did the agent ever scan the existing thing? | `cache-audit --ramp` — acquisition before first Edit |
| **trust** | it saw it and declined — why? | residue adjudication; ask the agent |
| **cost** | was writing genuinely cheaper? | `hpr-cost.py` — places-to-add, token cost |

A flat bar with high discovery and low trust is a DIFFERENT problem from a flat
bar with zero discovery, and they need opposite fixes. Reporting "the bar did
not move" without naming which clause failed is not a result.

### What this bar is NOT

It is not the refactor factory's bar. **The factory converges the STOCK; this
bar measures the FLOW.** They are different problems and conflating them is how
noun-convergence spent nine waves on a number that could not see its own work.
The factory earns its keep by making reuse cheap enough that the flow bar moves
— but a converged backlog with an unchanged miss rate is a treadmill, and this
document should say so out loud when it happens.

## What this is for

The codebase is agent-maintained. An agent writes new code because new is local,
certain, and needs no discovery; reuse needs knowing something exists and
trusting it fits. So the default action produces a duplicate and duplicates
compound. Converging them by hand is arithmetically impossible: `corpus_id`
alone is 6,936 mentions across 497 files, and it is one item of hundreds.

**The factory is a tool that executes a convergence from a declarative spec,
deterministically, at whole-codebase scale, with agents only where judgement is
genuinely required.**

## The whole job — what the factory must eventually consume

Not one atom. All of it. Measured 2026-08-23 at `63c72af8`:

| work item | population | source |
|---|---|---|
| stringly-typed field atoms | ~18 core, ~2,100 declarations | field census + confusability gate |
| cross-crate duplicate SHAPES | 112 groups, 282 types | `converge shape` |
| duplicate NAMES across crates | 247 names, 33 reachable | `converge census` |
| duplicate BEHAVIOUR | 182 exact + 335 near groups | `dry-report` |
| hand-rolled API adoption | ~144 arg loops + the rest | `hpr-cost.py` |

**Five kinds, one machine.** If the factory only does newtypes it has stopped
short.

## A refactor is DATA, not code (ARCH §6)

One spec shape covers all five kinds:

```toml
id     = "corpus-id"
kind   = "newtype"        # newtype | adopt-api | delete-loser | merge-shape | retype-field
target = "kernel_types::CorpusId"

[discover]
seed = { field = "corpus_id", from = "String" }   # the edit that makes rustc enumerate

[safety]                   # PROVEN before apply, never asserted
wire   = "transparent"     # round-trip fixture must be byte-identical
surfaces = ["json", "sqlite"]

[prepare]                  # collapse error classes before touching call sites
impls = ["AsRef<str>", "Borrow<str>", "PartialEq<str>", "From<CorpusId> for String", "FromStr"]

[rules]                    # (expected, found) -> edit. Generic across ALL newtypes.
"&str <- &CorpusId"        = "append .as_str()"
"String <- CorpusId"       = "append .into_string()"
"CorpusId <- String"       = "wrap CorpusId::new(_)?"
```

Specs live in `quality/refactors/*.toml`. The rule table is **shared across
specs** — `&str <- &T` is the same edit for every string newtype, so atom 2
inherits atom 1's rules. **Rules accumulate; the marginal cost per atom falls.**

## The engine — six stages, one of them agentic

**1. DISCOVER — deterministic, exhaustive.** Apply the seed edit; run
`cargo check --message-format=json`. Every error carries `file`, `line`,
`byte_start/end`, `expected`, `found`. **For a type change the compiler is the
exhaustive site enumerator** — the "did I find every site?" question does not
exist. SCIP (`~/.svrnmesh/indexes/commonwealth-ai/scip_graph.db`, 320,487
symbols / 1,645,234 refs with spans) gives the pre-flight estimate; rustc gives
the truth.

Measured limit: **E0308 carries NO `suggested_replacement`.** `cargo fix` cannot
do this work. The engine keys on the `(expected, found)` type pair instead —
which is why the rule table generalises.

**2. CLASSIFY — deterministic.** Group errors by `(code, expected, found,
syntactic context)`. Never a model: structured data, and §7.6 forbids asking a
model to guarantee what code enforces.

**3. PREPARE — the leverage, and it runs BEFORE any call site is touched.**
Error-class count is a property of the TARGET TYPE, not of the codebase, and we
control it. `CorpusId` today implements only `Display` and `Debug`; every
`&corpus_id` into a `&str` param, every `map.get(&corpus_id)`, every
`corpus_id == "x"` is an error solely because an impl is missing. Adding the
`[prepare]` impls deletes whole classes at once. **A ~50-line edit is worth
thousands of call-site edits — always run prepare first and re-measure.**

**4. APPLY — deterministic loop.** For each class with a rule: span-precise
edits, then re-check. Each pass strictly reduces the error count, so it
converges. A bad rule fails loudly at the next check rather than landing
silently — which is why a full AST rewriter is not required up front. `syn` is
in the lockfile for the specific class that proves unsafe to match textually;
reach for it per-class, never in advance.

**5. RESIDUE — the ONLY agentic stage.** Errors whose `(expected, found)` pair
has no rule. Two dispositions: propose a new rule (which then joins the shared
table and is reused forever), or flag a genuine semantic decision. Measured for
`corpus_id`: `CorpusId::new` returns `Option`, and the sites that could actually
be empty number **28**, not the 1,664 I first assumed. Residue is small and
shrinks per atom. This is where an ensemble is worth spending; nowhere else is.

**6. PROVE — the generic safety gate, and the reason this can run unattended.**

## The wire differ — the invention that makes scale safe

The near-miss that justifies this: I planned `node_id: String` → `NodeId` as a
"safe warm-up." `NodeId` is `define_id!`-generated, `[u8; 16]`, with **derived**
serde — it serialises as a **16-integer JSON array**. The 38 sites are HTTP
response types in `routes_status.rs`, `mesh_admin.rs`, `corpus_collaborate.rs`.
The migration turns `"node_id": "abc"` into `"node_id": [12,34,…]` on live mesh
endpoints, **and `cargo check` passes.** Both gates green, every client broken.

**The compiler is exhaustive over types and blind to encoding.** So the factory
carries its own encoding gate, generically:

> For every type whose definition the refactor touches, serialise a fixture
> before and after and diff the bytes — across every surface the spec declares
> (`json`, `sqlite`, and any other persisted form). A non-empty diff FAILS the
> refactor unless the spec declares the change intentional.

Deterministic, reusable, and it catches an entire class of silent production
break without anyone reading a diff. `ContentHash` already carries a hand-written
version of this idea (`serde_wire_form_is_a_plain_hex_string`, hash.rs:185) —
the factory generalises it.

**PER-ITEM ENTRY GATE, mandatory:** representation match · wire form match with
a passing fixture · trait surface adequate · fallibility volume counted. **An
item failing the wire check is not scheduled** — it is filed as a finding.

Already filed by that gate: **kernel-types holds three incompatible id
encodings** — `CorpusId` transparent string, `ContentHash` custom hex string,
`define_id!` derived byte array. §10.6 violated inside the crate that exists to
end exactly that. No `define_id!` id can be adopted at a `String` site until it
is resolved, and resolving it breaks the 117 sites already using `NodeId`.

## Where agents fit

- **The seat (one smart agent, in the loop):** authors specs, adjudicates the
  per-item entry gate, rules on residue that is a real semantic decision. This
  is O(items), not O(sites) — the whole point of the design.
- **Ensembles, only on residue:** propose rules for unknown type pairs; classify
  ambiguous sites (`is this corpus_id really a corpus id?`) as a closed set with
  a mandatory `unsure`. Substrate exists — `corpus-engine/src/enrichment/code_intel/mod.rs`
  is an incremental, body-hash-cached, concurrent batch driver over every symbol,
  with `PHASE_ID` routing bulk work to the daemon's `fast` model. **Generalise
  its prompt/parse/output triple; do not copy it into a sibling module.**
- **Never a model:** classifying compiler errors, writing the fix, deciding
  whether an item is worth converging.

## The split loop — the applied process (2026-09-03, grounding)

The six stages above converge a REPRESENTATION. The factory's second
pipeline splits a STRUCTURE — the ARCH §3.1 god file — and it is the half
that has been run end to end, on the grounding module: `mod.rs` 6,042 →
1,177, `judge.rs` 3,011 → 679, `citation.rs` 1,694 → 987,
`citation_attribution.rs` 1,403 → 848. The grounding module holds no §3.1
violation. The process, as proven (not as designed):

```
svrn code suggest-seams <file>            # SCIP: clusters, shared helpers, dead code
svrn code suggest-seams <file> --plan     # the same facts as executor TOML
#   author: trim steps, add mod-declaration/re-export patches, set verify_cmd
cargo xtask refactor-apply quality/refactors/plans/<plan>.toml --land
#   + the SYSTEM_OVERVIEW §1.1 touch (human — wording needs judgment)
```

**Division of labor — the load-bearing decision.** The model does not
mutate. Four supervised solve attempts against `judge.rs` produced ZERO
promoted edits: the model re-emits code it was asked to move, and a
1,166-line tests module is ~40k output tokens against a 4,000-token
budget — every candidate died before emitting. The fix was not a better
prompt; it was removing the model from the mutation loop entirely:
`EditAction::MoveLines` (model emits the DECISION — a span and a
destination — the tool moves the bytes), and now `refactor-apply`, which
needs no model at all. The model's remaining role is semantic judgment:
is this seam right, does this concern cohere — O(1) per split, not
O(lines).

**The author's duties in a plan** (what the executor refuses to guess):
mod declarations and re-exports for extracted clusters, the verify
command, and which steps to keep — the generated plan is advisory
material (citation.rs's carried 26 steps; the tests-move two were the
batch that shipped).

**Preconditions and etiquette**, each earned:
- *Workdir at the crate level* for solve jobs — the fitness ladder is a
  generated `tests/max_file_size.rs`, which a virtual-manifest root never
  compiles.
- *Warm the primary before submitting* (one 5-token completion): a cold
  slot sheds every concurrent candidate (503 `local_queue_full`) — and
  the solver's backend now honors the shed's `retry_after` and
  serializes on loopback providers.
- *Quiet window*: the shared daemon can be stopped by a peer's e2e
  harness (the `:9741` port invariant); solve jobs are in-memory and die
  with it. No destructive tree operations (`git stash -u`) on the
  shared checkout — scoped commits only.
- *Simplex*: the deterministic executor prefers a file module converted
  to a directory module (`judge.rs` → `judge/*.rs`), relative
  `include_str!` paths must be re-based by hand, and the mod-tests
  recipe is move-body + patch-opener.

**The failure catalogue** — six distinct defects, each fixed at its
layer, each found by a supervised attempt:
language-for-parsing derived from the shallowest source file (monorepo
roots always misfire — now read off the verify command); diagnostics
collapsed into one static string (the runner's own report now rides the
`NoBaseline` reason); a daemon serving stale code after an in-place
rebuild (submit now refuses with the repair); Rust trial timeouts below
one cold build (900s floor); the emission-compliance ceiling
(`move_lines`); concurrent candidates shedding each other on a single
slot (loopback seat + `retry_after` backoff).

**Residue-per-item**, per the bar above: grounding split one needed the
executor built; split two (citation_attribution) was one checked-in plan;
split three (citation) was plan → trim → apply. The trajectory is the
right direction; the number to keep honest is minutes-per-file as the
oversized list (137 names at freeze) burns.

## What gets built, in order

**The build order lives in [`REFACTOR_LEDGER.md`](./REFACTOR_LEDGER.md), not
here.** That document specifies the execution model — how work is stored, handed
out and proven closed — and its build order supersedes the one this section used
to carry (`plan` → wire differ → `prepare` → `apply`). Two build orders on disk
is the same disease as two `Verdict` definitions; there is one, and it is there.

What this document still owns: the spec format, the six stages, the wire differ,
the entry gate, and the bar. The engine is specified here; the dispatch is
specified there.

**First real subject: `corpus_id`, one crate.** Not because corpus_id matters
most, but because it is the only high-reach item whose representation and wire
form both already match, so it exercises every stage without confounding the
measurement.

## Completion

The factory is done when the work table at the top is empty and the loop that
refills it has stopped: `svrn refactor plan` over the whole corpus reports no
item above threshold, and stays that way for a quarter without anyone steering
it.

It fails honestly if the residue does not shrink per item — if atom five needs
as many new rules as atom one, the type-pair table does not generalise and the
factory is just hand-editing with extra steps. **Measure residue-per-item from
the first subject onward; that number is the factory's own verdict on itself.**

---

# Pre-registration — can this methodology reach `TARGET_ARCHITECTURE.md`?

Written 2026-08-23, BEFORE the factory exists, so the verdict cannot be fitted
afterwards. The target's own terminal test: that document "can one day be
written **honestly** — generated from the register and the graph."

## Where the target stands today

84 status markers in `quality/TARGET_ARCHITECTURE.md`: **18 `holds` · 23
`partial` · 43 `target`.** Half the architecture does not exist yet.

## What the factory can and cannot reach — §7's table, row by row

The honest split. A row is factory-shaped if it is a MIGRATION (the abstraction
exists or is trivially minted; the work is moving N sites onto it).

| §7 row | target mechanism | factory-shaped? |
|---|---|---|
| Answer grounded in evidence | type | **YES** — adopt-api; `kernel_types::Answer` exists, `grounding/mod.rs` + `streaming.rs` never migrated |
| Sharing policy respected | type | **YES** — retype-field; ~149 raw-bool sites, 5:1 plumbing:guard |
| Chunk custody respected | type | **YES** — retype-field; metadata string key → type |
| Provenance present | type | **YES** — retype-field; 8 `metadata["provenance"]` writer sites |
| Model never originates a number | audit + `Origin` | **PARTLY** — threading `Origin` is retype; the audit is not |
| A capability is wired | `Capability<T>` | **PARTLY** — retype + adopt-api, but the type must be designed first |
| Two measurements comparable | fingerprint key | **NO** — a new mechanism, then threading |
| A command exists | **generated** from contract | **NO** — a code generator |
| An endpoint is guarded | **registry** attaches guard | **NO** — an architectural pattern change |
| Docs match code | **generated** from `CONCEPTS.toml` | **NO** — a generator |

**Four clean yes, two partial, four no.** The factory addresses the MIGRATION
half of the target and not the GENERATION half. Saying otherwise would be the
well-formed, exit-0, wrong claim §18 exists to catch — and §7 of the target
already warns about exactly this, in a paragraph about rung 11 minting types
without migrating the path.

**Estimate, stated as a number so it can be wrong:** the factory can move most
of the 23 `partial` rows to `holds` (the mechanism exists; migration is the
work) and roughly a third of the 43 `target` rows. **Call it ~35 of 84 markers
— about 40% of the target architecture.** The remaining 60% is design work
(minting abstractions that do not exist) and generator work (xtask, registries),
neither of which is refactoring and neither of which this methodology claims.

## Six hypotheses, each with a falsifier and a number

**H1 — RESIDUE SHRINKS.** The `(expected, found)` rule table generalises, so
item N costs less than item N-1.
*Predict:* item 5 needs ≤30% of the new rules item 1 needed.
*Falsified if:* rules-added-per-item does not decline across the first five.
**This is the factory's own verdict on itself. If H1 fails, everything else is
hand-editing with extra steps and the honest move is to stop.**

**H2 — THE SAFETY NET HOLDS.** Compiler + wire differ together catch every
silent break.
*Predict:* zero encoding or behaviour escapes across the first five items.
*Falsified if:* one lands. The `node_id` near-miss is the known failure mode;
one escape means the differ's surface list is incomplete.

**H3 — PREPARE COLLAPSES CLASSES.** Shaping the target type before touching
call sites is the cheap leverage.
*Predict:* adding the `[prepare]` impls to `CorpusId` cuts the rustc error count
by ≥5x versus the unprepared seed.
*Falsified if:* <2x. Then error classes are a property of the codebase after
all, and per-item cost is far higher than modelled.

**H4 — ONE ENGINE, TWO SHAPES.** Compiler-blind refactors run through the same
spec format with only the discovery plug swapped.
*Predict:* the arg-loop spec differs from the corpus_id spec only in
`[discover]` and `[rules]`.
*Falsified if:* adopt-api needs a parallel machine. **This is the overfitting
test and it is why the second subject was chosen to be maximally unlike the
first.**

**H5 — THE FACTORY BEATS HANDS.** Measured against a real control: hpr converted
3 arg loops by hand across 2 worker sessions.
*Predict:* ≥5x cheaper per loop once n≥10.
*Falsified if:* cost per loop is no better than hand-editing. Then the honest
outcome is to fund workers, not a factory.

**H6 — THE 40% ESTIMATE.** Stated above.
*Predict:* ~35 of 84 markers reachable by migration alone.
*Falsified if:* after the first five items the reachable count is <20 or >55 —
either way the model of what is migration-shaped was wrong.

## What this methodology explicitly does NOT claim

- **It does not make the target document generatable.** That is §8's `xtask
  target-arch`, and it is a generator, not a refactor. The factory can make the
  STRUCTURE true; rendering it honestly is separate work.
- **It does not design missing abstractions.** `Capability<T>`, the measurement
  fingerprint, `SharingPolicy`'s resolution decider — someone has to invent
  those. The factory adopts; it does not conceive.
- **It does not touch the gated invariants** — answer quality, judge
  calibration, retrieval recall, honesty under adversarial questioning. §7 is
  explicit that types cannot hold those, and nothing here changes that.
- **It does not stop divergence.** Converging is not preventing. The birth rate
  of new duplicates is a separate problem and this document does not address it.

## The one number to watch

**Rules added per item, over the first five items.** It is H1, it is the
cheapest thing to measure, and it decides whether the factory is a machine or a
metaphor. If it does not fall, stop and say so.
