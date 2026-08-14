# Deep research — the method of inquiry the system embodies

Draft for operator re-cut, 2026-08-14. Pairs with `SPINE.md` (this is the
CONTENT of the spine's compass mechanism). Every stage below carries three
columns: the **academic anchor** (the research canon that names the move), the
**CS structure** (its algorithmic form), and the **falsifiable gate** (the bar
or fixture that measures it — the bars live in quality/initiative-bars.toml).
Prescriptive rule: a stage with no named anchor or no checkable gate is not
finished design.

## The method in one sentence

A research question spawns a persistent corpus; the loop is an operationalized
**systematic review with a Popperian compass and journalistic verification**,
running as a terminating program — every stage anchored, every claim gated,
every absence rendered.

## The stages

| Stage | Academic anchor | CS structure | Falsifiable gate |
|---|---|---|---|
| **Charter** — question, budgets, thresholds frozen before the search | Protocol registration: a systematic review publishes its protocol before searching, so results cannot steer the method (FR-3) | Referential transparency: the run is a pure function of (charter, estate-state) — reproducibility by construction | FR-3 (thresholds frozen at launch); `dr-budget-one-decider` |
| **Plan** — decomposition into sub-questions with acceptance shapes | PICO-style framing: "answered when we can name X, date Y, the causal link Z" | The acceptance shape is a typed predicate over the evidence window; the sub-question list is the search frontier | Coverage keys authorable WITHOUT consulting system output (T0 NWCI); F24 (mis-framed plan) |
| **Survey** — what we already own, before any network | Database of prior reviews: existing-first is the review's step 0 | Memoization at product scale — the re-ask is a cache hit over the estate DAG | `dr-estate-visible` (met); P3 (round-2 fetches <20%) |
| **Gap audit** — attack the draft, name the unsupported specifics | Popper: conjecture and refutation. The gate does not confirm; it tries to fail the draft | Four-valued epistemic logic (Belnap–Dunn: true / false / neither / both) as the verdict type; normalized set-membership as the deterministic containment witness | `dr-instrument-validated` (met); `dr-compass-handrun` (met) |
| **Acquisition** — gaps become queries; queries spend budget | Comprehensive search across sources, with the spend bounded | Token bucket, fail-closed; one decider, one name | `dr-budget-one-decider` (web-search half, T1) |
| **Triage** — which candidates deserve a deep read, reason logged | Screening with documented criteria: every exclusion is on the record | The skip-ledger is a persistent worklist with a never-excluder reserve (ε-quota of below-cut fetches) | F25 (systematic triage bias) in the gym deck |
| **Fetch + custody** — stamp provenance at the moment of capture | Journalism: attribution is the fetcher's job, never the model's memory | Content-addressed, append-only store; custody classes are taint labels on the DAG nodes | `dr-custody` (the three reds → green) |
| **Enrichment** — the offline brain abstracts over the estate | Thematic synthesis of qualitative review; the abstraction is derived, tagged, and discounted | The derivation DAG; derived custody = lattice join over the input edges — computed, never remembered | `dr-estate-integrity` (F17/F18); R-8 faithfulness regime |
| **Synthesis** — the draft, constrained | The journalist's draft: nothing asserted that cannot be attributed | Constrained generation; the URL/citation allowlist makes invented citations structurally impossible | F6 (citation not in window → stripped, reported) |
| **Claim gate + render** — every claim verified, absence rendered | Risk-of-bias appraisal with explicit confidence language; the two-source rule | The gate as a taint check over the evidence window; verdicts are the closed four-valued enum | `dr-instrument-validated` (met); the four gaps below |
| **Termination** — saturation | Grounded theory's theoretical saturation: the question is answered when inquiry stops generating new gaps | Floyd's variant function: the gap set strictly decreases each round under a well-founded measure, bounded by N and budget — termination is a proof obligation, not a heuristic | `dr-compass` (>=10 of 12, weekly tier); R-12 |
| **The estate persists** — the corpus outlives the report | Cumulative science: every prior run is step 0 of the next | The append-only DAG; the run directory is a write-ahead log — resume is recovery, any prefix is a valid report | P3 compounding; `dr-estate-visible` |
| **The gym** — the harness that tests the method | Deliberate practice for the system itself: fabricated sources planted, injections planted, every failure mode enumerated | Property-based testing (seeded banks) + fault injection (F-table) + train/test-leakage discipline (shapes, not bank vocabulary) | P5 poisoned drill (100%, no noise band); `dr-instrument-validated` |

## The gaps — where we fall short of the canon, and their homes

Named honestly; each becomes a spec amendment with red-first treatment when
its tier opens. None ships without a checkable gate.

1. **Source appraisal** (T2, with custody). We know a chunk's provenance, not
   its quality. Systematic reviews GRADE their evidence — risk of bias,
   primary vs secondary, citation hygiene. CS form: per-source appraisal
   metadata on the DAG nodes (authority signals, self-citation density,
   recursive citation depth), scored mechanically, never by an unchecked
   model.
2. **Corroboration** (T1b gate work). A claim from one source and from three
   INDEPENDENT sources get the same verdict today. The two-source rule as a
   verdict dimension: independence = distinct provenance components in the
   derivation DAG (a support set's independence is its source count, not its
   chunk count — F22's distinct-origins principle, made verdict-visible).
3. **The epistemic residue** (T1b renderer). The report renders could-not-
   judge per claim; the canon demands the searched-but-absent section —
   "we looked for X and found no evidence either way" — negative findings,
   publication-bias awareness. CS form: the search log as an absence index;
   the manifest's "what was NOT covered" generalized to a first-class
   section.
4. **Question re-framing** (T1b R11 evolution). Good research changes the
   question mid-inquiry; ours can only restart. The hermeneutic move as an
   enumerated state: a structural surprise is a typed re-frame event against
   the same estate — cheap, because the estate persists, and the variant
   function still applies (the estate only grows).
5. **Scale** (T1c gate work). The canon reports over hundreds of sources;
   the bank measures twelve questions. CS form: the v1 report-class question
   + deck as the scale probe; the estate's compounding is the scale
   mechanism — measured, not assumed.
6. **Attribution density** (T1c gate work). A fluent report can be wrong
   (the exemplar's own Gini sentence). CS form: the fraction of numeric
   claims in the output that trace to estate sources, gated per report; the
   two-arm control's lift metric rides this.
7. **Cross-entity synthesis** (T2). The report's best sentences state what
   no single source says; R8 synthesis is currently wiring, deferred. CS
   form: synthesis claims carry their support-set DAG as first-class
   content — a claim that combines entities renders the combination, not a
   paraphrase. Promoted from wiring when the frontier tier opens.

## What prescriptive means

The stage strip and the report manifest make the method itself visible: a
human can read a run and see which protocol step produced each claim, which
criteria excluded each source, and which absences the report refused to
smooth. The system does not merely run a loop — it embodies a named,
defensible research protocol, and the product team can say what method it is
and check it against the product.
