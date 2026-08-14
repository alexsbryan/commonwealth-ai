# Financial corpora — a company's filings answer with their basis, or refuse

Pre-registration. Operator direction 2026-08-14: *"This is a program. This is a
generalizeable feature meant to ship to users. We need to productize it — it's
not just a cute demo."*

## §1 The product thesis

A user names a public company and asks it questions. Figures come back with the
period they belong to and the filing they came from. When the corpus cannot
support a figure, it says so and names what it does have — it never approximates
from a near neighbour, and it never puts two different 12-month windows in one
column without saying they differ.

## §2 What already exists (commit 174bdd33, order sec-filings-template slice 1)

`sovereign-recipes/sec-filings-company/` — a template recipe materialized per
issuer as `sec-cik<10-digit>`, AAPL proven: 10-K prose (95 parts) + XBRL
companyfacts figures (20 fact files, 24 tags across 20 canonical concepts).
`scripts/sec_facts.py` renders and is the one decider; `scripts/check-sec-corpus.py`
judges from retrieval only. B2 10/10 and B4 3/3, with the judge watched failing
on tampered controls (exit 2 on a confident number for a negative control, exit 1
on a falsified value).

## §3 Why that is not yet a product — the three gaps, verified

1. **The honesty rule is not on any user path.** `check-sec-corpus.py` enforces
   typed-fact-or-refuse. `grep` for it and for `parse_fact_line` across every
   `.rs`, `.toml`, `.ts` and `.svelte` in the repo returns nothing outside
   `scripts/`. A user asking through chat gets ordinary retrieval and synthesis.
   The demo's integrity lives in its test harness.
2. **No enrichment.** `recipe.toml:77` states it outright. The original request
   was ingest AND enrich.
3. **Not installable without the repo.** Absent from `registry.toml`;
   `on_demand = true`, materialized by a bash script in `scripts/`.

And one source limit, structural: **companyfacts is consolidated-only.** Its
fact objects carry `start/end/val/accn/fy/fp/form/filed/frame` and no dimension
axis, so segment figures (Apple's Services revenue) cannot be typed from it —
even though the number is in the 10-K prose already ingested. Today's refusal of
`services_revenue` is honest about our typing, not about the source.

## §4 The six bars (F1-F6)

Declared 2026-08-14, before the work. All are judged AT AAPL SCOPE first
(operator: *"Build it for the AAPL filing then let's sync again"*); widening to
more filers is a later order and does not change these definitions.

- **F1 no-special-casing** — the template carries nothing Apple-specific, and a
  filer it cannot serve fails BY NAME. Provable at one filer: the concept map,
  period logic and selection rules are data or general code; a CIK with no 10-K
  in window produces a named failure, not a stack trace or a silent empty corpus.
- **F2 honesty in the answer path** — the typed-fact-or-refuse rule reaches the
  user. The frozen B2/B4 sets, answered through the product surface rather than
  `check-sec-corpus.py`: figures carry period basis and accession; unavailable
  concepts and out-of-range periods refuse with what IS available named.
  **F2 is the headline. Without it the other five decorate a demo.**
- **F3 installable without the repo** — present in `registry.toml`; a user
  installs a company corpus by ticker without running a repo script.
- **F4 enrichment** — a question about why a figure moved returns the filing's
  own explanation, attributed to it, alongside the figure. Never a manufactured
  explanation the filing does not contain (the proxy-company honesty rule).
- **F5 coverage is visible** — the corpus states what it can and cannot answer
  for this company. 24 of 503 tags are mapped today; a product that answers 5%
  of concepts while looking omniscient is the failure this bar prevents.
  Asked-but-unmapped concepts are counted, so coverage grows by evidence.
- **F6 freshness** — a corpus knows its as-of filing and refuses or flags
  periods after it, including when an amended filing (10-K/A) supersedes the
  figure it holds. A superseded number answered confidently is the `fy` trap in
  another costume.

## §5 Standing invariants inherited from slice 1

- Writer and judge stay on opposite sides of the bar. Collapsing them makes the
  bar unable to fail (ARCH §18.1).
- `fy` is the REPORTING FILING's fiscal year, not the period's. Fiscal year comes
  from the fact's own end date; identity is `(concept, start, end, unit)`; `fy`
  and `frame` are never consulted for selection.
- Preregs are hand-read from filing text, never from companyfacts — companyfacts
  is the system under test.
- Unmapped concepts are REPORTED by name, never defaulted to a near neighbour
  (ARCH §18.3).

## §6 The fabrication-free design (operator direction: design this before building)

**The guarantee, stated as the machine enforces it: the model must never
ORIGINATE a number.** Every figure a user sees is either a datum read from the
fact store or a value COMPUTED in Rust over named facts, with its derivation
emitted. Anything else is flagged before the user sees it.

### §6.1 This machinery already exists — reuse, do not invent (ARCH §19)

Survey done 2026-08-14, before designing:

- `sovereign/crates/sovereign-core/src/runtime/numeric_audit.rs` — built for the
  SF-LVT demo and its module doc states this exact objective. Three layers:
  **L1** the tool emits pre-cited figures plus a `derivation`; **L2** the
  synthesis prompt forbids model-originated numbers and surfaces the derivation;
  **L3** `uncited_numerics(answer, cited, raw_values)` is the deterministic
  backstop — after synthesis, every figure must match BY VALUE something the
  tool emitted, in formatted (`$1.48B`) or exact (`$1,477,806,471.00`) form.
- `sovereign/crates/sovereign-core/src/quote_verification.rs` — verbatim-span
  verification with demotion of unverified quotes. Handles curly quotes,
  markdown emphasis, whitespace folding, and rejects spliced composites.
- The wiring seam is a JSON CONTRACT, not a special case:
  `handlers/complex_task.rs` harvests any tool step whose `StepOutput::Json`
  carries `cited_figures`, `derivation`, `reproduce`; `json_numeric_leaves`
  collects the raw side. The precedent tool is
  `corpus-engine/src/enrichment/atlas/analysis/parcel_analytics.rs`, routed from
  `sovereign/crates/sovereign-core/src/router.rs:2066`.

So F2 is: **emit the existing contract from a sec-facts tool.** It is not a new
guarantee architecture.

### §6.2 The design

1. **Facts are the only source of figures.** Identity `(concept, start, end,
   unit, accession)`. Derived from XBRL only. **Numbers in prose are NOT facts**
   — a prose numeral may be a comparative, a rounded restatement, or guidance.
   Prose is for explanation (F4), never for figures.
2. **A `sec_facts` analytics tool** beside `parcel_analytics`, emitting
   `cited_figures` + `derivation` + `reproduce` + raw numeric leaves. Routed as
   `parcel_analytics` is.
3. **Derived quantities are computed in Rust, never by the model.** "R&D as a
   share of revenue" is a division over two named facts with the formula, both
   inputs and the result in the derivation trace. A model doing arithmetic is a
   model originating a number.
4. **L3 backstop runs on every answer** carrying figures. Non-empty
   `uncited_numerics` blocks or flags — it does not warn quietly.
5. **F4 explanation rides `quote_verification`.** Management's reason for a
   change is a verbatim span from the filing or it is demoted. Never paraphrased
   — a paraphrase of a number is a new number.
6. **Refusal stays first-class** (proven in slice 1): unavailable concept or
   out-of-range period refuses and names what IS available.

### §6.3 The gap this design must close — MEASURED, not assumed

`numeric_audit` is **scoped to `$…` and `…%` tokens; bare integers are NOT
audited** (module doc, deliberate — so it does not false-positive on "in 2024"
or "874 parcels"). Financial answers are full of bare figures: `416,161`
(millions), EPS `7.46`, `34,550`. **Under today's auditor those are unaudited,
so the guarantee has a hole exactly where this corpus lives.**

Two candidate closures, to be decided with a measurement rather than a
preference:
- (a) **Render unit-qualified always** — the tool never emits a bare numeral;
  every figure carries its unit (`$416,161 million`, `$7.46`). Local to this
  corpus, no shared-code change, but it only holds for figures WE render — a
  model can still emit a bare number of its own.
- (b) **Extend the auditor's token scope** for figure-bearing turns. Closes the
  hole properly, but `numeric_audit` is shared with the native-grounding answer
  path — a CROSS-INITIATIVE SEAM requiring coordination, not a unilateral edit.

Recommendation: ship (a) immediately since it is free, and treat (b) as the real
fix, sized by measuring how many bare numerals actually appear in answers to the
adversarial set. Do not skip (b) because (a) makes the demo look clean.

### §6.4 How F2 is proven — the adversarial fabrication set

A frozen set built to INDUCE fabrication, with the judge watched failing first
(slice 1's method): a segment figure companyfacts cannot carry (Services
revenue); a period outside the filing range; a concept with a tempting near
neighbour (`Revenues` vs `RevenueFromContractWithCustomerExcludingAssessedTax`,
gross margin vs gross profit); a question requiring arithmetic; a question whose
answer exists only in prose as a comparative.

Bar: **zero unattributable numerals across the set.** Not "low" — zero. One
fabricated financial figure shown to a user is a product-ending event, so the
bar is not a rate.
