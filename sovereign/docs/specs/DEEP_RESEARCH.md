# Deep research — corpus construction, not context-filling

**Status:** Design draft, 2026-08-13. No order opened; no code. Derived
from an operator design conversation (deep research idiomatic to the
stance, offline-brain precedent, frontier-key admission).
**Companion docs:** `docs/internal/STRATEGY.md` (stance + phase ladder),
`PRODUCTION_SEARCH_INTEGRATION.md` (search backends),
`sovereign/ARCH_PRINCIPLES.md` §18 (verdict discipline), §7.6 (never ask
a model to guarantee what code can enforce).

## Thesis

Cloud deep research is expensive because a frontier model *reads* the
web in-context at frontier prices, per query, and persists nothing. We
invert it: **a research question spawns a corpus.** Reading and
enrichment run on the local daemon (the offline brain — the same move
tiered conversation memory makes for long chats: don't hold it in
context, enrich in the background, retrieve at answer time). The
research estate persists and compounds: every prior run is step 0 of
the next one, so marginal cost per question *falls* over time. The
final report is not generated prose with decorative citations — it is a
verified artifact where every claim carries a gate verdict.

Frontier models may participate — as **gated generators in named
roles**, behind a custody boundary, never as judges. Frontier writes;
sovereign verifies. That is the instrument test in its purest form, and
it is what lets a BYO-frontier-key feature ship without breaking stance
item 4 (nothing phones home) — the boundary is a code path, not a
policy.

## Operator journey (scenes pin the components)

1. **Ask.** Operator poses a research question and a budget (depth,
   wall-clock, optionally a frontier key + spend cap). → R0 charter,
   R1 Director.
2. **It checks what you already own.** Prior research corpora, personal
   corpora, mesh snapshots are queried before any network call. → R2.
3. **It finds out what it doesn't know.** A draft answer is attempted
   and gated; the gate names the unsupported specifics and unanswerable
   sub-questions. Those gaps — not model vibes — drive acquisition.
   → R3.
4. **It reads overnight.** Search → triage → fetch → ingest →
   enrichment run on the daemon at electricity prices, checkpointed and
   resumable. → R4–R7, R11.
5. **It writes, and the writing is checked.** Synthesis (local or
   frontier) over the curated evidence window; every claim re-gated;
   could-not-judge rendered as an explicit open-questions section, never
   smoothed into prose. → R8, R9.
6. **You keep the estate.** The corpus outlives the report. Follow-ups
   cost one retrieval + one gated synthesis. Snapshots are
   mesh-shareable. → the cost story.

## Trust classes — the razor

Every component is assigned exactly one class. The classes decide which
models (if any) may hold the role, and how much failure tolerance the
role gets.

| Class | Who may hold it | Failure posture |
|---|---|---|
| **C — Custody/Control** | Deterministic code only. No model, ever. | Bulletproof. A defect here is a stance violation (custody leak, false green, silent default). Tested exhaustively; structural enforcement (§7). |
| **I — Instrument** | Calibrated local models only (judges, embed). | Bulletproof in *verdict integrity*: four verdicts, never two; a verdict is never defaulted. The model may be imperfect; the reporting of what it did may not be. Changes go through §18.6 (pre-registered predictions, adversarial arms). |
| **G — Generator** | Any model: local 35B by default, frontier via BYO key where marked. | Fault tolerant. n-shot, retries, judge panels, best-of-k are all fine. Errors cost budget, latency, or coverage — and are recoverable by the loop. |

**The razor:** a component may be Class G if and only if its worst
output can only waste budget or reduce coverage — never mint a false
green, never corrupt evidence, never cross the custody boundary. If a
bad output from the role could do any of those, the role is C or I, or
the design is wrong.

Corollaries:

- Judging is never G, and never frontier. The moment the generator's
  provider also grades the output, the honesty grade is gameable and
  the un-gameable grade (stance item 3) dies.
- Provenance stamping is never a model task. A model that "remembers"
  where a chunk came from is a §7.6 violation; the fetcher knows the
  URL, so the code stamps it.
- Generator roles get the *n-shot dividend*: because R3/R9 catch bad
  output downstream, R1/R5/R8 can run cheap models, sample multiple
  candidates, and pick — quality becomes a budget knob, not a
  correctness risk.

## Component roster

Control flow between components is deterministic Rust (Class C
throughout — the loop itself is never model-driven). Per component:
responsibility, I/O, class, model, failure posture, and the existing
surface it reuses (cited per §19 — the inventory outranks the plan).

### R0 — Charter (input contract)

- **Responsibility:** capture the research question, depth/wall-clock
  budget, target corpus id, allowed source classes, optional frontier
  key + spend cap. One typed struct; the run is reproducible from it.
- **In:** operator request. **Out:** `ResearchCharter` (serialized into
  the corpus dir — it is provenance for the whole estate).
- **Class C.** Reuse: recipe shapes (`corpus-engine/src/recipe.rs`) —
  a research corpus is a recipe-described corpus like any other.

### R1 — Director (decomposition and planning)

- **Responsibility:** decompose the question into sub-questions with an
  acceptance shape each ("answered when we can name X, date Y, the
  causal link Z"). **Plans once, at launch.** There is no mid-run
  re-plan transition: a structural surprise (the question was
  mis-framed, a sub-question dissolved) seeds a *new run* against the
  same estate — cheap, because the estate persists. One fewer loop
  state; see Flight rules.
- **In:** charter + R2's survey of what the estate already covers.
  **Out:** ordered sub-question list (typed).
- **Class G. Frontier-eligible** (planning quality is where frontier
  spend buys the most per token; input is the question + coverage
  summary — small, and public-provenance by construction).
- **Failure posture:** a bad plan wastes fetches or misses coverage;
  R3 catches missed coverage as persistent gaps. n-shot fine.

### R2 — Estate survey (existing-first retrieval)

- **Responsibility:** answer "what do we already own about this?"
  before any network call. Query prior research corpora, personal
  corpora, notes, mesh-reachable snapshots.
- **In:** sub-question. **Out:** ranked evidence + per-sub-question
  coverage signal.
- **Class:** deterministic retrieval (C) over the calibrated embed
  model (I).
- **Reuse — and a §10.6 obligation:** `KnowledgeLookupTool`
  (`sovereign-tools/src/knowledge_lookup/`) is already the unified
  evidence front door (corpus + memory + notes) with Tier-3 web
  escalation. That is this loop in miniature. R2/R4 must route through
  that seam or absorb it — two implementations of "existing-first,
  then web" is the smell table's last row.

### R3 — Gap auditor (the gate as compass)

- **Responsibility:** attempt a gated answer per sub-question from
  current evidence; emit four-verdict results plus the *named* missing
  evidence (unsupported specifics, unanswerable sub-questions). This
  is the loop's steering signal and its termination test.
- **In:** sub-question + draft answer + evidence window.
  **Out:** verdicts (passed / failed / could-not-judge / never-ran) +
  gap list.
- **Class I. Local only, always.** Reuse: the grounding gate and
  `scan_unsupported_specifics`
  (`sovereign-core/src/runtime/grounding/`), chunk judge + tau
  calibration. The atlas's native `questions` atoms feed the gap list.
- **Failure posture:** verdict integrity is bulletproof; judge
  *quality* improvements go through §18.6 like any judge change.

### R4 — Acquisition (query forming + search execution)

- **Responsibility:** turn a named gap into search queries (G, cheap,
  n-shot — bad queries cost a fetch round, nothing more); execute them
  through the search orchestrator (C).
- **In:** gap list. **Out:** candidate sources (url, title, snippet,
  backend id).
- **Reuse:** `SearchOrchestrator` + `WebSearchRegistry` with
  zero-config DDG fallback and keyed Tavily/Brave backends,
  `SearchPrivacy`, `BudgetView`
  (`sovereign-tools/src/web/search/`); spec
  `PRODUCTION_SEARCH_INTEGRATION.md`. Search *budget* enforcement is
  C — the orchestrator's `BudgetView`, not model restraint.

### R5 — Triage reader

- **Responsibility:** decide which candidates deserve fetch + deep
  read, with a one-line reason each (glassbox: the reason is logged).
- **In:** candidate sources + sub-question. **Out:** fetch list.
- **Class G. Frontier-eligible but rarely worth it** — this is the
  canonical cheap-local n-shot role. Worst case: fetch something
  useless (wasted budget) or skip something good (R3 re-surfaces the
  gap next round).

### R6 — Fetch, ingest, provenance stamp

- **Responsibility:** fetch, extract, chunk, embed, and stamp every
  chunk with provenance: source URL, retrieval timestamp, backend, and
  a **custody class** (`public-web` | `personal` | `peer`). The
  custody class is the field R10 keys on.
- **In:** fetch list. **Out:** chunks in the research corpus.
- **Class C.** Fetch failures are *reported* per-source in the run
  manifest — a source that 404'd is recorded absent, never silently
  dropped (stance item 2).
- **Reuse:** `WebFetchTool` (`sovereign_tools::web`), `CorpusEngine`
  ingest — `InsertChunk` already carries `url`, `source_doc_id`,
  `content_hash` (`corpus-engine/src/index/mod.rs`), and corpus-level
  `CorpusProvenance` exists. **New:** the chunk-level custody class —
  small, and a prerequisite for everything frontier-related.

### R7 — Enrichment (the offline brain proper)

- **Responsibility:** atlas atoms + RAPTOR tiers over the research
  corpus, on the daemon, checkpointed, at idle/overnight priority.
- **In:** research corpus chunks. **Out:** atoms, summaries, tiers.
- **Class G with an asterisk:** its outputs *enter the evidence pool*,
  which is exactly how a generator's mistake becomes a false green
  downstream. Two structural mitigations, both **preconditions** (see
  below): faithful-mode summarization, and derived-vs-primary tagging
  that survives all the way to the gate so R3/R9 can weight or exclude
  derived evidence.
- **Reuse:** the entire enrichment pipeline (tiered driver, custom
  ontologies via `[enrichment.ontology]`, checkpoints). A research
  charter can carry a domain ontology — a legal-research corpus atlases
  differently than a biomedical one, with zero engine changes.

### R8 — Synthesist (report writer)

- **Responsibility:** write the report from the curated evidence
  window and R3-passed sub-answers, citing chunk-level sources.
- **In:** egress-filtered evidence window + sub-answers + charter.
  **Out:** draft report with citations.
- **Class G. The flagship frontier role.** This is where local models
  are honestly weakest and where frontier spend per token buys the
  most. The frontier never reads the web here — it reads the distilled
  window the local stack curated. Judge-panel / best-of-k over drafts
  is idiomatic since R9 gates every claim regardless.
- **Citation integrity is not delegated to the model:** the URL
  allowlist sampler constraint
  (`sovereign-inference/src/url_constraint.rs`) already makes invented
  URLs structurally impossible for local synthesis; for remote
  synthesis the equivalent is post-hoc: any citation not in the
  evidence window is stripped and reported (C).

### R9 — Claim gate + report renderer

- **Responsibility:** split the draft into claims (with citations);
  gate every claim against the corpus (I — same instrument as R3,
  dual-string per FR-6: two decorrelated instruments must agree,
  disagreement resolves to could-not-judge);
  render the verdict-stamped report (C): supported claims with
  chunk-level citations, failed claims removed or flagged,
  could-not-judge as an explicit **Open questions** section, plus the
  run manifest (sources fetched, sources failed, budget spent, what
  was *not* covered).
- **In:** draft report. **Out:** the deliverable.
- The report never claims completeness it didn't earn: a
  budget-truncated run says so on the first page.

### R10 — Egress boundary

- **Responsibility:** the single choke point every remote-model call
  passes through. Releases a payload iff every chunk carries
  `custody: public-web` (or a class the operator explicitly marked
  shareable). Personal corpora are structurally unreachable by remote
  slots — by construction, not configuration (§7). Every egress event
  is traced: provider, token count, custody proof.
- **In:** candidate payload + destination. **Out:** the payload, or a
  typed refusal that names what was withheld — which R11 reports and
  R8 falls back on (local synthesis for the private-evidence portion).
  Absence is reported, never defaulted.
- **Class C. This component is the entire answer to "how do frontier
  keys not break the stance."** It is also deliberately *not* part of
  the model zoo: frontier providers get a visa (named roles, this
  boundary), not citizenship (resident slots, default paths).

### R11 — Run controller

- **Responsibility:** the loop, as a typed state machine with
  *enumerated* states and transitions (Flight rules FR-1). Fixed round
  structure: survey → gap-audit → acquire → ingest → enrich →
  re-audit, N rounds max, until R3 reports no new gaps for a full
  round or the budget is exhausted (distinct terminal states,
  distinctly reported). Resumable from any phase boundary because the
  ICD artifacts *are* the checkpoints (FR-2) — no separate checkpoint
  machinery. Long runs launch as launchd one-shots per the standing
  convention. Enforces spend caps for search backends and frontier
  keys (C — never model restraint).
- **Class C.** Reuse: the daemon job pattern (`solve`-style job_id +
  status polling) for the surface; enrichment's internal checkpoints
  remain R7's own affair.

## The loop, compressed

```
charter → R1 plan (once)
round (× N max):
  R2 survey (estate first — no network)
  R3 gap audit  ──no new gaps──▶ R8 synthesize → R9 gate+render → DONE
  R4 search (gaps only)
  R5 triage → R6 fetch+ingest → R7 enrich
  budget check ──exhausted──▶ R8/R9, truncation declared → DONE-PARTIAL
(from ANY state: ABORT ▶ R8/R9 over current estate, truncation declared)
```

Fetch-count on repeat/adjacent questions is the observable that
existing-first works: a re-run of an answered question should acquire
approximately zero new sources. That is a benchable number.

## Flight rules

The Apollo posture, stated once: **elegance and bulletproofness are the
same move — remove failure modes rather than armor them.** (The LM
ascent engine had to light once on the lunar surface; the answer was
hypergolics and no turbopump, not a backup engine.) Six rules, each
enforced in code, none remembered (§7).

**FR-1 — Enumerated states, no optional transitions.** The run
controller is a typed state machine; every state and transition is
named in code and in this spec. Nothing "optionally" happens mid-run:
v1 has exactly one enumerated re-plan transition — the re-frame event
(GAP-4, question re-framing): a structural surprise is a typed re-frame
against the same estate, never an ad-hoc branch and never a silently
seeded new run — plus no adaptive round structure and no mid-flight
judgment calls. A loop whose states can all be named can be rehearsed;
one with an optional branch is first observed failing in production.

**FR-2 — Every boundary is an artifact (ICDs).** Every inter-component
payload — plan, gap list, fetch list, evidence window, draft, verdict
set, egress payload + custody proof — is a serialized, versioned
artifact in the run directory, not an in-memory value. Consequences,
all free: components qualify independently against golden fixtures;
any run resumes or replays from any phase boundary (the ICDs *are* the
checkpoints — separate checkpoint machinery for the loop is deleted);
the run directory is the flight recorder — the whole mission
reconstructs from disk, Glassbox taken to its terminus.

**FR-3 — Decisions before launch, execution during.** Every threshold
the run consults — coverage floor before synthesis, spend caps, round
limit N, egress custody policy, judge agreement rule — is frozen into
the charter at launch. Mid-run the system executes rules; it never
sets them. Changing a rule means a new charter and a new run, in a
commit if the rule is a default.

**FR-4 — Go/No-Go gates.** Three polls, each a code check against
charter values, each with both outcomes typed and reported:
before first acquisition (estate surveyed, budget sane), before any
egress (custody proof over the exact payload), before synthesis
(coverage floor met — else the run reports DONE-PARTIAL rather than
synthesizing thin air).

**FR-5 — An abort mode from every state.** The survivable
configuration is always the same artifact: a gated report over the
current estate with truncation declared on page one. Every state —
budget death, daemon death, key revoked, operator cancel — must reach
it. There is no state whose failure yields nothing: even a run that
dies mid-fetch left the corpus richer and the ICD trail resumable.

**FR-6 — Redundancy only at the instrument, dual-string.** Final-report
claims are judged by two decorrelated instruments (per-claim judge +
independent specifics scan, or two judge registers); they must agree
for a pass, and disagreement resolves to **could-not-judge** — never a
tie-breaking third opinion, which would be a third failure mode.
Generators get zero redundancy: n-shot is retry, not redundancy, and
the containment (FR-1/FR-4) is why being wrong there is cheap.

**Containment corollary (models propose, code disposes).** Every model
output in the system is typed *data* — queries, triage picks, drafts,
verdicts — consumed by deterministic code; no model output is ever
control flow. A prompt-injected web page can therefore waste one
round's budget but cannot steer the loop, cross the custody boundary,
or alter a verdict. Injection and exfiltration are contained by
construction, not by detection.

## FMEA — enumerated failure modes

The enumeration is the deliverable: a failure mode absent from this
table is the bug (§18.3 — an `Err` collapsed into a success-shaped
value always starts as an unenumerated failure). Each mode has a typed
representation, a detection point, and a rehearsed response. The gym
deck (below) injects every row.

| # | Component | Mode | Detection | Response |
|---|---|---|---|---|
| F1 | R4 | DDG bot-block / backend 0-results | orchestrator result empty, backend id logged | try next backend in preference order; all dry → gap stays open, recorded `unsearchable` this round |
| F2 | R6 | fetch 404 / timeout / paywall stub | HTTP status; extracted-text-length floor | source recorded absent in manifest with reason; never silently dropped |
| F3 | R6 | fetched page is boilerplate/garbage | extraction yield below floor | chunk not ingested; source marked `low-yield` |
| F4 | R5/R8 | prompt injection in fetched content | (contained, not detected) | FR containment corollary: outputs are typed data; worst case one round's wasted budget |
| F5 | R7 | enrichment fabricates | derived-vs-primary tag + faithful-mode verify (precondition 1) | derived evidence discounted/excluded at gate; fabrication caught by R9 dual-string |
| F6 | R8 | synthesist invents a citation | URL/citation not in evidence window (C check) | citation stripped + reported; claim re-gated without it |
| F7 | R8 | frontier key dies / cap hit mid-synthesis | provider error / spend meter | fall back to local synthesis, report the substitution by name (never silent) |
| F8 | R3/R9 | judge timeout or malformed verdict | typed verdict parse; watchdog | verdict = could-not-judge, never defaulted to pass or fail |
| F9 | R9 | dual-string disagreement | agreement check | could-not-judge → claim lands in Open questions |
| F10 | R10 | payload contains non-public chunk | custody-class scan over exact payload | typed refusal naming withheld chunks; R8 splits local/remote |
| F11 | R11 | daemon death / harness kill mid-run | job status; stale heartbeat | resume from last ICD artifact; launchd one-shot relaunch per convention |
| F12 | R11 | budget exhausted before convergence | spend meters vs. charter caps | DONE-PARTIAL: gated report + truncation declared, never presented as complete |
| F13 | R2 | estate index stale/corrupt | corpus meta validation at survey | loud degradation: run proceeds web-first with the estate absence reported |
| F14 | R7 | circular evidence: a derived chunk (enrichment's own output) becomes the evidence for the claim that produced it | derived-vs-primary tag on every derived chunk; gate eligibility checks the tag | derived evidence discounted at the gate; a claim resting solely on derived support is re-gated against primary evidence |
| F15 | R6/R7 | unstamped derived chunk: a derivation output with no custody record reaches the gate | custody stamped at derivation — lattice join over the inputs, computed at creation (never a model task) | unknown/partial provenance refuses — typed refusal naming the withheld chunk, never a silent pass |
| F16 | R2 | estate-unsearchable reads as "no evidence": an empty or unsearchable estate silently means "nothing exists" | empty-estate precondition is a searchability assert at survey; searchability checked, not assumed | run proceeds web-first with the estate absence reported loud — never an unlabeled "no evidence" |
| F17 | R6 | ingest laundering: fetched content written to the estate without its per-chunk custody/source stamp | custody stamped by the fetcher; ingest asserts the stamp on every write | unstamped write is a loud error — the chunk does not enter the estate silently |
| F18 | R7 | dead-inference enrichment: enrichment silently yields nothing (dead model call) and the round reads as success | enrichment faithfulness asserts; a zero-yield enrich round is an error, never a silent pass | loud degradation: the round's yield recorded and the failure reported by name |
| F19 | R11 | run collisions: two runs against the same run dir / charter race | flock on `<run_dir>/lock` at acquisition; lifecycle recorded in the manifest | second opener refuses — a typed refusal, never a silent second writer |
| F20 | R11 | budget-meter drift: the spend meter and the decider disagree | one decider, one name: the meter is the decider's own record; drift asserted | meter/decider disagreement is a loud error; spend is never trusted from two sources |
| F21 | R11/R2 | stale evidence: estate chunks older than the charter's freshness horizon enter the window | charter freshness horizon checked at survey; stale chunks flagged | stale chunks excluded from the window and reported; fresh search prioritized |
| F22 | R3/R9 | near-duplicate inflation: coverage counts chunks, so five copies of one source look corroborated | coverage counts distinct origins, never chunks — the derivation DAG's distinct provenance components | the corroboration floor (GAP-2): a claim whose support set has <2 distinct origins caps at could-not-judge |
| F23 | R4 | result-SET poisoning: the planted source appears in force (multiple plants, whole-set compromise) | results are untrusted typed data (containment corollary); the gym deck injects sets, not single plants | worst case one wasted round — the corroboration floor keeps any single-origin claim from passing |
| F24 | R1/R2 | mis-framed plan: so broad or unanswerable that no gap can fail it — the gate passes by inaction | plan sub-questions are typed data with acceptance shapes; the coverage key authorable without consulting system output | a plan whose sub-questions are not search-actionable is refused at planning — a typed refusal, never a pass |
| F25 | R5 | systematic triage bias: the ranker excludes a whole class of candidates, and the exclusion is invisible | skip-ledger records every exclusion (a persistent worklist); ε-quota reserves below-cut fetches | bias is auditable from the ledger — every exclusion is on the record, never silent |
| F26 | R10 | boundary bypass: a remote client construction that does not route through the egress boundary | F26 census: every remote client construction routes through the boundary, enforced as a build gate | a bypass is a build failure, not a runtime surprise |
| F27 | R2/R7 | foreign embedding spaces: estate chunks embedded in a different space than the query's get retrieved incoherently | embedding space stamped at ingestion; cross-space retrieval refused or reported | a mixed-space window is refused loudly; mesh sharing of research estates stays behind this (SearchPrivacy::Mesh is a placeholder until it resolves) |
| F28 | R4/R3 | instrument unavailable ≠ could-not-judge: a dead backend or an empty result must not render as an evidence verdict | empty search results are Ok(empty) records, never Err; an empty window never enters the judge | instrument absence reported by name; could-not-judge stays a verdict about the evidence, never the instrument |

**Sim before flight (the gym deck).** The full loop runs against mock
search/fetch backends with the F-table injected — the search-gym and
chaos-monkey precedents, composed. Qualification for the feature is
**all-up** (Mueller's rule): components prove out against ICD golden
fixtures on the ground, but the bench lane flies the integrated loop
through the production code path — no bench-only forks of any prompt,
threshold, or judge (test what you fly; the land-B pattern of the
bench critic importing `CHUNK_JUDGE_SYSTEM` is the template). The
deck's centerpiece is the poisoned-source drill (target P5).

## Methodology gaps — first-class requirements (T1b)

The method document (research/deep-research/METHODOLOGY.md, "The four
gaps") names where the shipped loop falls short of the canon. Three of
the four are first-class requirements of this spec as of T1b — they
enter here, by spec amendment, never by methodology-doc alone. (The
fourth, source appraisal, stays sequenced behind custody at T2.)
Each requirement carries its falsifiable gate; the gates' bars live in
quality/initiative-bars.toml (`dr-corroboration`, `dr-residue`,
`dr-reframe`).

**GAP-2 — Corroboration: the two-source rule as a verdict dimension.
** A claim may pass only if its supporting evidence spans at least two
distinct provenance origins; independence is the derivation DAG's
distinct components — a support set's independence is its source count,
never its chunk count (FMEA F22). A claim whose support set has one
origin — one document, one planted source — caps at could-not-judge.
The floor is C-class (origin extraction + counting, deterministic),
computed in the gate, and verdict-visible on the final claim (the
gate's corroboration record). *Falsifiable gate:* deterministic
fixtures — a single-origin support set downgrades to could-not-judge;
two chunks from one document downgrade; two chunks from two documents
pass unchanged; the adversarial instruments re-run against the floor
with the pre-registered read (§18.6).

**GAP-3 — Epistemic residue: the searched-but-absent section.** Every
query the loop executed is report content: "we looked for X and found
no evidence either way." The report renders a first-class
searched-but-absent section — negative findings, publication-bias
awareness — generalizing the manifest's "what was NOT covered" to a
named section. *Falsifiable gate:* a run whose searches return nothing
renders the section with every query named; a run with hits renders
only the empty-result queries; the red fixture (section absent at
HEAD) lands before the renderer change.

**GAP-4 — Question re-framing: the enumerated re-frame state.** A
structural surprise — evidence that contradicts the charter's framing
— is a typed re-frame event against the same estate: Auditing →
Reframing → Planning, a new plan over the persisted estate, the
re-frame recorded in the manifest. Cheap because the estate persists,
and the variant function (R3's gap sets strictly shrink) still applies
— the estate only grows. This is FR-1's one enumerated re-plan
transition. *Falsifiable gate:* the transition table enumerates
Reframing with its events; golden fixture run-reframe-1 — a re-framed
run re-plans against the same estate and lands the re-frame in the
manifest; icd-schemas.md §13 renders the state.

## Model placement summary

| Component | Class | Local 35B | Cheap/fast local | Frontier (BYO key) |
|---|---|---|---|---|
| R1 Director | G | default | — | eligible |
| R2 Survey | C+I | — (embed slot) | — | never |
| R3 Gap audit | I | judge models | — | **never** |
| R4 Query forming | G | default | fine | pointless |
| R5 Triage | G | fine | preferred | rarely worth it |
| R6 Ingest | C | — | — | never |
| R7 Enrichment | G* | default | tiered | possible, low value |
| R8 Synthesist | G | default | — | **flagship role** |
| R9 Claim gate | I+C | judge models | — | **never** |
| R10 Egress | C | — | — | is the boundary |
| R11 Controller | C | — | — | never |

## Cost model and pre-registered targets

Cloud DR's shape: the frontier model reads everything (order 10^5–10^6
tokens per run at frontier prices), re-spent per question, nothing
persisted. Our hybrid's shape: reading/enrichment at electricity
prices; the frontier sees only R1's plan input and R8's curated window
(order 10^4 tokens in, ~10^3–10^4 out).

Targets below are pre-registered *structure*; the numeric bars get
finalized in a commit when the research question bank is minted, before
any arm runs (§18.6). Initial values are stated so they can be wrong:

- **P1 (cost):** hybrid frontier spend per run ≤ 10% of a cloud-DR
  reference run on the same bank.
- **P2 (honesty):** fabrication rate in the final gated report
  (measured by gate verdicts + an independent human-scored sample) <
  the cloud-DR reference arm. Honesty and coverage scored separately,
  never blended.
- **P3 (compounding):** on the bank's repeat/adjacent-question split,
  round-2 fetch count < 20% of round-1.
- **P4 (local floor):** local-only arm produces a usable report
  (coverage bar set at bank mint) — the feature must not *require* a
  frontier key.
- **P5 (poisoned-source drill):** on gym runs seeded with a planted
  source carrying (a) a confident fabrication and (b) a prompt
  injection: the fabrication is absent from the final report's passed
  claims (gated out or could-not-judged), and the run's control-flow
  trace is identical to the unpoisoned run modulo the wasted round —
  the injection provably steered nothing. 100% of drill runs; this bar
  does not get a noise band.
- **Kill bar:** if the hybrid beats cloud DR on neither honesty nor
  cost, the frontier-key feature does not ship — fix the loop, not the
  chart. If P4 fails after loop iteration, the whole feature waits on
  local synthesis quality; it does not ship as frontier-only.

Bench shape: three arms (local-only, hybrid, cloud-DR reference) on
one bank, run as a lane in the existing bench harness with committed
baselines. Exhaust angle (opt-in, per stance): every hybrid run is
instrument-settled data on a named frontier model's fabrication rate
under a gate it doesn't control — honesty-leaderboard feedstock
(STRATEGY.md, rating-agency seed).

## Preconditions — must land first

1. **Faithful enrichment.** RAPTOR summaries have fabricated
   grounding-eligible content (the "Russian agent Vladimir" finding:
   summarizer invited synthesis, no faithfulness check before persist).
   In this feature the enriched artifacts are the evidence base *and*
   the egress payload — a fabricated summary crossing to a frontier
   synthesist is the worst version of the failure.
2. **Provenance through the gate.** `EvidenceContext` currently drops
   source provenance before judging (chunks flattened to bare text), so
   derived summaries reach the gate indistinguishable from primary
   sources. R3/R9 need the derived-vs-primary and custody tags intact.
3. **Chunk-level custody class** (R6) — small schema addition;
   everything frontier-shaped depends on it.

## What is genuinely new vs. reuse

| New surface | Size | Everything else |
|---|---|---|
| R11 run controller (state machine + ICD artifacts; no bespoke checkpoint layer — FR-2) | the feature's core, still thin — deterministic orchestration over existing seams | search (orchestrator + 3 backends), fetch, ingest, provenance fields, enrichment pipelines + checkpoints, grounding gate + specifics scan, retrieval pipeline, KnowledgeLookup front door, mesh snapshots, bench harness + search-gym/chaos-monkey sim precedents, launchd run pattern |
| R10 egress boundary | small, C, heavily tested | |
| R6 custody stamp | schema field + stamp sites | |
| R9 claim-splitting + report renderer | moderate | |
| R0/R1 charter + plan types | small | |

Net-simplification obligation: R2/R4 must unify with
`KnowledgeLookupTool`'s tier logic rather than sit beside it (one
decider, one name). The feature adds one store (the research corpus is
just a corpus — zero new store kinds), one job type, and one boundary;
it should *delete* the ad-hoc "search then paste" flow as the escape
hatch of last resort once landed.

## Open decisions for the operator

1. **Which frontier roles ship first.** Recommendation: R8 only.
   R1-on-frontier is a follow-up A/B once the bank exists (measurable:
   gap-convergence rounds, plan quality is otherwise vibes).
2. **Egress consent UX.** Per-run ("this run may send public-web
   evidence to provider X, cap $Y") vs. standing opt-in per provider.
   Recommendation: per-run at first — it is also the better demo of
   the stance.
3. **Mesh sharing of research corpora.** Snapshots make research
   estates shareable; deferred until the single-node loop is proven,
   but the provenance/custody schema should not preclude `peer` from
   day one (it is in the custody enum above).
4. **Where the bank comes from.** A public research-question bank with
   known-answer keys is itself Phase-0-shaped work (the assay is the
   launch post).
