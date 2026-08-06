# Epistemic State — the answer as a typed object

**Date:** 2026-07-18
**Status:** system design, pre-implementation. Grounded in three code
surveys run 2026-07-18 (retrieval/grounding pipeline, memory provenance,
gap-recognition inventory); every claim about current behavior cites a
file in this repo. Subplans derive from §9 — one initiative, one plan,
each with its own gates (re-chunked from six phases into four sized
initiatives 2026-07-18).

← companions: `RETRIEVAL_REDESIGN.md` (the retrieval half of the same
program — §4 unifies with its S2/S5), `specs/TIERED_RETRIEVAL_MEMORIES.md`,
`../crates/sovereign-desktop/TEACHABLE.md` (fact-vs-lesson split),
`../KNOWLEDGE_BASE_FEAT.md` (the unbuilt coverage-pipeline spec this
subsumes), `GROUNDING_GATE_ENV.md`.

---

## 1. Vision, stated as a product bar

Whatever model runs inside, the system:

1. hands it the best evidence set the install can produce (the
   RETRIEVAL_REDESIGN program — in flight, not this doc's scope);
2. is honest, **per statement**, about the basis of what it says —
   verified against sources, recalled from memory (and how confidently),
   or general knowledge;
3. recognizes what it doesn't know, says so plainly, and **conjectures
   where the missing knowledge could be fetched** — a corpus to install,
   a folder to connect, a web search, a document to paste;
4. failing all else, surfaces the questions the user hadn't thought to
   ask.

(2)–(4) are this doc. The bar is *utmost epistemic humility while being
profoundly helpful* — never confident beyond the evidence, never a dead
end when the evidence runs out.

## 2. Root cause — the answer is a string

Every epistemic judgment the system needs for (2)–(4) is **already
computed somewhere in the turn** — and then destroyed, because the only
artifact a turn produces is prose plus ad-hoc metadata. The receipts:

| Judgment | Where computed | What survives to the user |
|---|---|---|
| Per-claim support verdicts | grounding gate: claim extraction + forced-choice judges (`runtime/grounding/judge.rs`) | collapsed to one release/rewrite/abstain action; claim-level knowledge dropped (receipt string in metadata) |
| "This is general knowledge, not your sources" | evidence-shape signals (`handlers/knowledge_query.rs:491`, `:1102`) | a **string prefix** — `GK_CAVEAT_PREFIX` (`runtime/prompts.rs:581`), committed as decoded tokens, string-matched and stripped downstream (`grounding/mod.rs:258`) |
| Memory epistemic bands (told-directly / inferred / tentative) | `memory::format_memories_for_prompt` (`memory.rs:916-951`) | erased — a cited memory renders identically to a cited document (`SourceAttribution.svelte` has no memory group; `knowledge_lookup/mod.rs:269` flattens to `ev-NNNN` handles) |
| Memory verification outcome (verified vs fail-open) | `runtime/memory_grounding.rs:124-136` (deliberately fail-open) | invisible — "I remember you said X" ships with a weaker guarantee than "your sources say X" and the user cannot tell |
| What's missing + where it might live | gap-check judge (`gap.rs`) → `satisfying_source` / `search_hints` | an isolated card, decoupled from the abstention that fires on the same turn |
| Abstention | `grounded_abstention` (`grounding/mod.rs:241`) | a static template whose only advice is "try rephrasing" |
| "Does ANY corpus cover this topic?" | `nearest_vector_distance` (`corpus-engine/src/index/search.rs:276`), validated 2026-07-13 | trapped in the default-off retrieval prefilter; prunes fan-out, never becomes a user-facing coverage verdict |
| "Install corpus X?" conjecture | Curator `Sufficiency::Insufficient` (`pipeline/runner.rs:302`) | dark — the team pipeline was experimentally rejected (2026-05-03), default OFF |
| Which sources were used | prose — the UI **regex-parses the `Sources:` block out of the answer text** (`SourceAttribution.svelte:4-10`) | fragile, provenance-blind |
| Open questions per corpus | `detect_open_questions` (`field_engine.rs:298`) | **return value dropped unbound** — inference spent, result discarded |
| Coverage-aware search types | `SearchMethod` / `CoverageDecision` (`sovereign-contracts/src/types/mod.rs:775,804`) | stubs, "not constructed anywhere" |

This repo diagnosed the identical disease in retrieval:
RETRIEVAL_REDESIGN §3 — set selection "does not exist; its job is
emulated by a pile of interacting heuristics." Here: **the epistemic
verdict does not exist; its job is emulated by string prefixes, metadata
keys, and decoupled cards.** Point-wiring the fragments (abstention →
gap card, memory label → UI chip, …) is O(producers × consumers) and is
how the current disconnection arose. The fix is a hub.

ARCH_PRINCIPLES governs the shape: §2 (closed sets belong in enums, not
strings), §5.4 (pipeline stages parameterize on data), §6 (the SICP
data/program separation — the epistemic status of an answer is *data*),
§7 (invariants must be structural), §9 (glassbox).

## 3. The artifact

Every answer turn produces an **`EpistemicState`** — prose plus a typed
account of what the system asserts, on what basis, and what it could not
cover. Vocabulary lives in `sovereign-contracts` beside `role.rs` and
the UI types.

```rust
pub struct EpistemicState {
    /// What the question needs — from the demand plan (§4.1).
    pub demands:  Vec<Demand>,
    /// What the answer asserts, per claim, with basis + verification.
    pub holdings: Vec<Holding>,
    /// Demands no holding covers: the honest residue, with conjecture.
    pub gaps:     Vec<Gap>,
    /// Derived, never model-asserted (§3, D4).
    pub verdict:  TurnVerdict,
    /// Verbatim passages the gate RELEASED, in release order (§4.5).
    pub citations: Vec<ReleasedCitation>,
}

pub struct ReleasedCitation {
    pub text:    String,          // verbatim source span, as released
    pub locator: Option<String>,  // "CHAPTER VII" — None where no join exists
    pub target:  CitationTarget,  // always present; see §4.5
}

pub struct CitationTarget {       // the one definition of the pair
    pub corpus_id: String,
    pub chunk_id:  u64,
}

pub struct Demand {
    pub facet: DemandFacet,       // SubQuestion | Entity | Stance | Section
    pub text:  String,
    pub covered: CoverageLevel,   // Supported | Retrieved | Absent
}

pub struct Holding {
    pub claim: String,            // from the gate's claim extractor
    pub provenance: Provenance,
    pub verification: Verification, // Verified | FailedOnce | FailOpen | Unverified
}

pub enum Provenance {
    Corpus  { corpus_id: String, chunk_id: u64 },
    Memory  { band: MemoryBand, entry_id: String },   // ToldDirectly | Inferred | Tentative
    GeneralKnowledge,
    ToolDerived { tool: String },                     // numeric_audit's world
}

pub struct Gap {
    pub demand: usize,                 // index into demands
    pub statement: String,             // "what would settle this"
    pub coverage: GapCoverage,         // TopicUncovered | ClaimUncovered
    pub routes: Vec<AcquisitionRoute>, // from the resolver, catalog-only (§4.3)
}

pub enum AcquisitionRoute {
    InstallRecipe { recipe_id: String },     // sovereign-recipes registry
    ConnectFolder, ConnectVault, ImportConversations,
    WebSearch { queries: Vec<String> },
    ProvideDocument { kind: String },        // "a primary source, a filing…"
}

pub enum TurnVerdict {
    Grounded, Mixed, MemoryRecall, GeneralKnowledge, CannotKnowFromHere,
}
```

**Design decisions (the load-bearing ones):**

- **D1 — assembly is collation, not another judge.** The ledger is
  assembled deterministically from producers already in the turn (§4).
  No new post-hoc LLM pass; the assembler is a pure function over what
  the pipeline computed. This is what makes the program *simpler* than
  the seam-patching alternative.
- **D2 — one demand model for retrieval and humility.** The demand plan
  (RETRIEVAL_REDESIGN S2, already roadmapped as P2 there) is the SSOT
  for both "what to fetch" and "what we still don't know." A gap is a
  demand whose facet ended `Absent` (or `Retrieved` but never supported
  by a holding). This subsumes S5 (per-facet gap check) and **retires
  `gap.rs`'s post-hoc windowed judge** — which exists only because the
  pipeline discards its own demand structure and a second model must
  re-derive it from a truncated string. The 2026-07-15 Einstein
  head+tail false-positive was this root cause presenting as a bug.
- **D3 — verdict is derived.** `TurnVerdict` is a pure function of
  holdings + gaps. No model ever asserts its own epistemic standing.
- **D4 — the acquisition resolver is data, not a role.** `Gap →
  routes` is a deterministic resolver: gap text embedded against an
  **acquisition catalog** (the 26 recipe descriptions from
  `sovereign-recipes/registry.toml` + the connector affordances + web),
  disambiguated by the coverage verdict — `nearest_vector_distance`
  fan-out distinguishes *TopicUncovered* ("no corpus is near this
  topic → here's where it would live") from *ClaimUncovered* ("your
  corpus covers the topic but not this claim → deeper source / web").
  A fast-slot pass may *phrase* the route into prose but can never
  invent one (structural: routes come only from the catalog).
- **D5 — fail-open stays fail-open, but visible.** The relational
  memory verifier's availability posture is untouched; its outcome is
  recorded on the holding (`Verification::FailOpen`) so honesty is by
  construction, not by a stronger gate.
- **D6 — the ledger never blocks the answer.** Assembly failure
  degrades to `verdict: Mixed` + a glassbox tracing event
  (`target: "epistemic.ledger"`), never a dropped or delayed turn.
  Same philosophy as the gate's fail-open on judge failure.
- **D7 — persistence + wire.** The ledger persists on the message like
  `grounding_gate` metadata today (versioned schema field
  `epistemic_state_v: 1`), flows through the server projection layer
  (`projection.rs` already surfaces typed provenance to mobile), and
  streams as frames on the existing `turn-narration` channel (the
  verification-counter precedent: drop-on-full, never backpressure),
  with the final ledger on the completion event.

## 4. Producers — where each field comes from

Each bullet is a refactor of an *existing* component to emit into the
ledger instead of (or in addition to) its local side effect.

### 4.1 `demands` — the demand plan, retained

S2 `demand_plan` (one fast-slot llguidance call → sub_queries, entities,
stances, sections; pure-Rust `decompose_question` fallback) lands per
RETRIEVAL_REDESIGN P2 — with one change to its contract: its output is
**retained on `PipelineState` through the whole turn** rather than
consumed by fan-out and dropped. Simple/factual intents skip the
planner (router already classifies); their demand set is the single
query. Coverage levels are stamped in two passes: `Retrieved` when any
pooled chunk matches the facet (the coverage_select facet-tagging
already computes this affinity), `Supported` when a holding cites it.

### 4.2 `holdings` — the gate stops discarding its work

- The gate's claim extractor + per-claim forced-choice verdicts
  (`grounding/judge.rs`) emit `Holding { claim, Corpus{..}, Verified |
  FailedOnce }` instead of collapsing to a single action. The citation
  forcer already binds claims to chunks; that binding is the
  `Provenance::Corpus` payload.
- Memory injection (`memory.rs` bands) registers the recalled entries
  + bands on the turn context; the witness/relational verifier stamps
  `Verified` or `FailOpen` per referenced entry (it already computes
  `referenced` for pin attribution — `TurnProvenance.recalled_memories`).
- `knowledge_lookup`'s `EvidenceKind::Memory` rows map to Memory
  holdings (the tool already tags them; the tag stops dying at the
  `ev-NNNN` handle).
- The GK signals (zero-chunk, agentic-insufficient + non-anchored)
  become `Provenance::GeneralKnowledge` holdings; the decoded prefix
  can remain as UX text, but **the verdict field is the SSOT** and the
  string-match protocol dies (§6).
- `numeric_audit`'s tool-derived figures are `ToolDerived` holdings —
  the "no confabulated numbers" guarantee becomes visible provenance.

### 4.3 `gaps` — coverage, then conjecture

- Per-facet coverage (D2) writes `Gap` rows for `Absent`/unsupported
  demands.
- The coverage verdict lifts `nearest_vector_distance` out of the
  experimental prefilter (`runtime/retrieval/corpus_search.rs`) into a
  first-class signal, fanned across installed corpora **only on turns
  that end with gaps or abstention** (cost control; reuses the
  pipeline's query embedding).
- The acquisition resolver (D4) attaches routes. The Curator's one good
  behavior ("Would you like me to install <corpus>?",
  `pipeline/runner.rs:302`) is resurrected as catalog data on a live
  path; the rejected pipeline module itself stays dead.

### 4.4 `verdict` — pure derivation

All holdings Corpus+Verified → `Grounded`. Any Memory holding →
`MemoryRecall`/`Mixed`. GK holdings present → `GeneralKnowledge`/
`Mixed`. No supported holdings + gaps → `CannotKnowFromHere` (the
abstention state — now structurally carrying its gaps and routes,
because they are fields of the same object).

### 4.5 `citations` — the gate's passages, made openable

Landed 2026-08-06. The grounding gate's citation path releases verbatim
quotes and, where the corpus's chunk→section join resolves, labels them
with a section heading. That output existed downstream **only as prose
formatted into the answer string** (`runtime/grounding/mod.rs`), which
made the system's best-attested citation — verbatim, gate-verified,
chapter-located — the one citation in the product a reader could not
click. This field is that output as data.

- **Produced** by the gate at release, riding the `grounding_gate` meta
  blob the assembler already receives; the assembler collates, it does
  not re-derive (§10.6). An abstained turn cites nothing, the same rule
  `holdings` follows.
- **Bound at the QUOTE, not the claim.** `Provenance::Corpus` carries
  one `chunk_id` per *claim*, and `GateClaim` has no passage binding —
  so putting the handle there would attribute a chunk the claim was
  never individually verified against. Since multi-quote citations
  became the default (2026-08-05), a released citation routinely spans
  two chunks, which one `chunk_id` cannot honestly represent. The
  passage is the thing that has a chunk, so the passage carries it.
- **`target` is not optional.** A quote the gate cannot bind to a single
  chunk produces **no row** rather than a row with a dead handle: a row
  here is a promise that clicking opens the passage quoted. Nothing is
  lost from what the reader can READ — the prose rendering still shows
  every quote.
- **`locator` and `target` are independent in both directions.** A
  corpus with no section structure yields an openable passage with no
  chapter name; a synthetic chunk yields the reverse. Neither is a proxy
  for the other, and `SOVEREIGN_CITATION_LOCATOR` (the display control
  arm) does not govern openability.

## 5. Consumers — everything becomes a view

- **The answer footer** (desktop + mobile via projection): citations
  grouped by provenance class. Memory citations are **visibly distinct**
  ("From what you've told me — Mar 12", band-labeled, with "remembered,
  not verified" when `FailOpen`). Corpus citations keep today's look.
  The verification receipt and coverage chip become ledger views.
- **The abstention rendering** replaces the static template: what was
  sought (demands), what was found (holdings, possibly none), what's
  missing (gaps), and 1–2 actionable routes (Install / Connect / Search
  buttons wired to the Library AddSheet catalog tab and the existing
  web-search affordance). `ResearchGapCard.svelte` (currently orphaned)
  is either revived as this view or deleted; `InformationRequestCard`
  folds in.
- **Benches**: the chaos scorer reads `verdict` instead of sniffing the
  GK prefix; a third lane scores gap conjectures (§8).
- **Glassbox**: `tracing target: "epistemic.ledger"` per assembly
  (verdict, holding count by provenance, gap count, resolver picks);
  the inner-work ProvenancePanel pattern generalizes to the main chat
  as the debug view of the ledger.
- **Explore / notebook surfaces** (P5): `ResolutionStatus::Open`
  Question atoms and persisted open-questions become a "your sources
  leave this open" feed — the pillar-4 surface.

## 6. Deletions

The program is judged partly by what it removes:

1. `gap.rs` post-hoc windowed judge — retired after a parity A/B
   against per-facet coverage (P1). Its `InformationRequest` DTO
   survives as the Gap view-model until P3 replaces the card.
2. The GK string-prefix *protocol* — `GK_CAVEAT_PREFIX` detection /
   stripping via string match (`grounding/mod.rs:258`, scorer
   prefix-sniffing). The user-facing caveat sentence remains, rendered
   from the verdict.
3. `SourceAttribution.svelte`'s prose-parsing of the `Sources:` block —
   the model may still emit prose markers (they aid gate citation
   compliance), but no rendering path reads epistemic info from prose.
4. `SearchMethod` / `CoverageDecision` stub types — subsumed by
   `CoverageLevel`/`GapCoverage` or deleted with a deprecation note in
   KNOWLEDGE_BASE_FEAT.md.
5. The `detect_open_questions` discard — repaired (bound + persisted to
   the skeleton sidecar), not deleted.
6. The team-pipeline module stays rejected; P2 lifts its
   `suggested_action` phrasing into catalog data and its module-level
   doc gains a pointer here.

## 7. Invariants (structural, test-pinned per ARCH §7)

- **I1 — every answer surface produces a ledger.** Closed surface enum
  (the `GateSurface` precedent); the completion event type carries the
  field, `Option` only during dark phases, non-optional at P3.
- **I2 — verdict is derived, never model-asserted.** Pure function,
  unit-pinned on synthetic ledgers.
- **I3 — a Memory holding cannot render in the corpus citation group.**
  Type-level: the renderer matches on `Provenance`; no stringly escape
  hatch.
- **I4 — acquisition routes come only from the catalog.** The resolver
  returns catalog entries; the phrasing pass receives resolved routes
  as data and a test pins that no route id outside the catalog
  survives to the DTO.
- **I5 — the ledger never blocks or delays release.** Assembly is
  post-verdict collation; failure degrades per D6. Latency gate: p50
  answer wall unchanged on non-gap turns.
- **I6 — no rendering path reads epistemic info from prose** (after
  P3). Pinned by a UI test rendering a message whose prose contradicts
  its ledger — the ledger wins.

## 8. Measurement

- **Chaos parity gate (must pass before any scorer migration):** the
  two red lines re-scored via `verdict` must reproduce the
  string-sniffed baselines on the committed banks.
- **Third chaos lane — conjecture accuracy:** each answer-absent bank
  item is labeled with the acquisition class that would satisfy it
  (`in-catalog-recipe:<id>` / `user-document` / `web` / `unknowable`);
  the lane scores `gaps[].routes` top-1 against the label. Tracked
  first, hard-gated once a baseline exists (the standing bench
  convention).
- **Memory-provenance probe:** a labeled probe set through the witness
  + factual paths asserting the rendered distinction (memory chip
  present, band correct, FailOpen marked) — extends the inner-chaos
  harness's fixture pattern.
- **Ledger fidelity:** sampled turns, judge checks holdings ↔ prose
  claims correspondence (calibration bank, grounding-bench style).
- **Hard gates throughout:** chaos red lines, grounding bank,
  ci-bench retrieval lanes, p50 latency — no regression.

## 9. Roadmap — four sized initiatives (re-chunked 2026-07-18)

T-shirt sizes are focused work **including bench/gate time** (this
repo's real cost driver): XS < half day · S ~1 day · M 2–4 days ·
L 1–2 weeks · XL > 2 weeks. Ordering rule unchanged: **dark first,
render late, measure always.** The original P0–P5 phases survive as
milestones inside the initiatives; every initiative lands env-gated,
A/B'd, with a decision note, and losing knobs retired, not accreted.

**The load-bearing re-chunk decision: P1 splits.** The ledger needs
*a* demand set, not the LLM demand plan — `decompose_question`, the
entity extraction already in `entity_boost`, and coverage_select's
facet affinity give a v1 demand model with **zero new model calls**
(**P1a**, M). The LLM `demand_plan` (**P1b**, L) is the same feature
as RETRIEVAL_REDESIGN S2 and inherits that program's
prompt-iteration + re-baseline cost. The split means the headline
product behavior does not wait on model work.

### I1 — "The honest turn" (P0 + P1a + P2) — L. First.
**STATUS: IMPLEMENTED 2026-07-18** (all three milestones; gates:
workspace suite 7,767 green at A+B, desktop svelte-check 0/0 +
vitest 315/315, C suite at close). Deviations from the letter of
this plan, each recorded at the code site:

- Vocabulary lives in `sovereign-contracts/src/types/epistemic.rs`
  (own submodule, not narration.rs — ARCH §3 file-level SRP).
- `Provenance::Corpus.corpus_id` is `Option<String>` — honest
  attribution is only possible on single-corpus pools until
  claim-level search binding (I2); `TurnVerdict::Unverified` added
  for evidence-used-but-ungated turns.
- The demand set is built + coverage-stamped inside
  `prepare_knowledge_query_plan` and RETAINED ON THE PLAN
  (`KnowledgeQueryPlan.demands` + `query_embedding`) — no new
  pipeline step, no golden-test churn. Demands/gaps ship on the KQ
  family (KnowledgeQuery/ComparisonQuery, both variants); Deep/Simple
  surfaces carry the P0-level ledger (holdings + verdict) in I1.
- The recall verifier's fail-open became VISIBLE
  (`RecallGroundingVerdict.fail_open`) and its outcome persists on
  `TurnProvenance.recall_verification` (non-streaming witness path;
  the streaming witness runs no verifier and records `None`).
- Ledger routes are stamped on the PRIMARY gap only, strictly on
  gap turns (one embed on already-slow turns); catalog embeddings
  are lazily disk-cached (`~/.sovereign/catalog-embed-cache.json`,
  embed-model-keyed) — no committed bake.
- Card route buttons navigate to the Library shelf
  (`onOpenLibrary`); the AddSheet tab deep-link exceeded a small
  prop-thread and is deferred to I2 as planned.
- Chaos banks: 22 absent items labeled (`acquisition_class`:
  adjacent → `unknowable`, out-of-domain → `install_recipe`);
  the tracked lane reports via `CalibrationReport.
  {n_acquisition_labeled, acquisition_matched}` + a TRACKED line in
  the runner summary. Red lines untouched.

**Live before/after demo** (real embed slot + real installed corpora,
no mocks): `cargo run -p sovereign-cli-llm --features
corpus-engine/treesitter --example epistemic_demo` (daemon must be
up). Prints the predecessor's dead-end abstention next to the
ledger's coverage verdict + acquisition conjecture per question —
the resource-pitch artifact. Verified 2026-07-18 on 33 installed
corpora: chaos "Heat's first name" → ClaimUncovered (0.80 similarity
in chaos-secret-agent) → web search; "EU AI Act foundation-model
rules" → TopicUncovered (0.50) → Install federal-register recipe.

Outcome: every KQ/Deep/Simple turn carries a ledger; gap/abstain
turns show what's missing plus 1–2 catalog-grounded acquisition
routes on the *existing* InformationRequestCard; the third chaos lane
reports (tracked). Zero new model calls, near-zero retrieval-pipeline
risk. Milestones:

- **P0 (M) — vocabulary + dark assembly.** `EpistemicState` in
  `sovereign-contracts`; deterministic assembler collating what
  already exists (gate claims/verdicts, memory bands + verifier
  outcomes, GK signals, source list); persisted to message metadata +
  glassbox tracing. Zero UI change. Gate: metadata strict superset;
  suite green; chaos untouched.
- **P1a (M) — deterministic demands + coverage.** Demand set from
  existing signals; per-facet coverage stamps + Gap rows + derived
  verdict; coverage verdict via lifted `nearest_vector_distance`
  (gap/abstain turns only; latency A/B). Gate: retrieval lanes green;
  p50 unchanged on non-gap turns.
- **P2 (M) — acquisition catalog + resolver.** Catalog assets (recipe
  descriptions + connectors) embedded once at build/boot; resolver +
  route DTOs; routes attached to gaps; existing InformationRequest
  card renders routes (old visual shell, new data). Gate: third chaos
  lane exists and reports (tracked); labeled bank seeded.

### I2 — "Rendering honesty" (P3 + P5 + P4b) — L–XL. Second.

Outcome: the wholesale user-visible change. Milestones:

- **P3 (L) — render the ledger.** Answer footer + abstention
  rendering + memory distinction (bands, "remembered, not
  verified"); deletions §6.2–6.3; scorer migration behind the chaos
  parity gate. Gate: I3/I6 pinned; desktop `npm run check` + vitest +
  Playwright chaos specs.
- **P5 (M) — surface completion.** Extend ledger production to
  attached-doc and complex-task (both already emit gate metadata;
  assembly generalizes); flip invariant I1 non-optional.
- **P4b (S–M) — the unasked-questions surface.** Open Question atoms
  + persisted open-questions feed the gap view and notebook Explore,
  riding the components P3 builds. Gate: enrichment suite; digest
  byte-parity where unchanged.

### I3 — "Unasked questions persist" (P4a) — S. Schedulable anywhere.

Bind + persist the `detect_open_questions` result
(`field_engine.rs:298`) to the skeleton sidecar. Independent of
everything; a good warm-up inside I1.

### I4 — "Demand intelligence" (P1b) — L. Last, on the retrieval cadence.

The LLM `demand_plan`, executed **under RETRIEVAL_REDESIGN** (it is
that doc's P2/S2; the bench harness and A/B discipline live there);
the ledger consumes its output through the seam P1a establishes.
`gap.rs` retires here, after the parity A/B on its own firing set.
Gate: retrieval lanes + gap-check parity + re-baselines.

**Priority order: I1 → I2 (I3 riding wherever) → I4.** I1 is highest
value-per-risk — the vision's gap-plus-conjecture behavior,
dark-to-visible in one initiative. I2 converts internal honesty into
the product's felt character. I4 deepens gap *quality*, but the
abstention+conjecture flow works day one without it. Arc total: ~5–7
focused weeks.

## 10. Alignment

**Context.** The product vision demands per-statement epistemic honesty
and gap-conjecture; three surveys show every needed judgment already
computed but destroyed at stage boundaries because the answer is a
string. This program makes the epistemic state a typed artifact with
one producer path and many views.

**What this extends.** The grounding gate's claim machinery
(`runtime/grounding/`), memory bands (`memory.rs`), the demand-plan and
per-facet gap-check already roadmapped in RETRIEVAL_REDESIGN (S2/S5),
`nearest_vector_distance`, the recipe registry, `turn-narration`
streaming, the `GateSurface`/`role.rs`/`RetrievalPipeline`
data-over-program precedents.

**What this removes.** `gap.rs`'s post-hoc judge; the GK string-match
protocol; prose-parsing in `SourceAttribution.svelte`; the
`SearchMethod`/`CoverageDecision` stubs; the `detect_open_questions`
discard; the orphaned `ResearchGapCard` (revive-or-delete decided in
P3).

**Restraint patterns.** ARCH §2 (closed enums over strings — the whole
program is this principle applied to the answer), §5.4/§6 (stages
parameterize on data; epistemic status is data), §7 (structural
invariants I1–I6), §9 (glassbox), §10 (behaviour-preserving dark
phases, one dimension at a time), §11 (every survey claim cited;
benches before belief).

**Could this be done with less?** The less-version is the five-seam
patch set (wire abstention→gap card, add a memory chip, etc.). It was
considered and rejected as symptomatic: each patch adds a wire between
two of ~6 producers and ~5 consumers without removing the string
substrate that caused the disconnection, and the next epistemic feature
pays the same seam tax again. The ledger is the minimum shape that
makes honesty *structural*; its phases are individually small, dark,
and reversible.

## 11. Risks and open questions

- **Claim↔prose span mapping** on small models: holdings are
  claim-granular, not span-granular, in v1 — the footer aggregates;
  inline per-sentence badges are explicitly out of scope until the
  streaming gate pipeline (`specs/STREAMING_GATE_PIPELINE.md`) matures.
- **Coverage fan-out cost:** `nearest_vector_distance` per installed
  corpus is an ANN probe per corpus; bounded by running only on
  gap/abstain turns + reusing the query embedding. Budget measured in
  P1 with the per-leg timers.
- **Metadata growth:** ledgers on every message; cap holdings/gaps
  counts, and the schema version field allows pruning policy later.
- **Verdict taxonomy stability:** `TurnVerdict` is wire-visible;
  additions are back-compatible, renames are not — review the enum at
  P0 like a wire type (serde aliases per recipe-schema convention).
- **Bench-bank labeling effort** for the third chaos lane (acquisition
  classes on absent items) — one curation pass, mirrors the witness
  fairness contract.
- **Sequencing with RETRIEVAL_REDESIGN:** P1b and retrieval-S2 are the
  same feature. Decision (2026-07-18 re-chunk): it executes **under
  the retrieval program** as initiative I4 here; the ledger consumes
  its output through the P1a seam. The two docs cross-reference to
  prevent drift.
