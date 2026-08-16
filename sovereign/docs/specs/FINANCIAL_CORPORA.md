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

## §7 The UX contract — reasoned backwards from the desktop (operator, 2026-08-14)

The routing mechanism was designed from the user's journey inward, not from the
router outward. Operator direction: *"Sometimes it's helpful to reason
architecture backwards from the user experience."*

### §7.1 The doorway already exists

`SCHEMA.md:50` documents the `[parameters]` block with THIS use case as its
worked example — "lets a financial journalist (for example) ship one
`sec-filings` recipe and let downstream users plug in their own entity list /
form types / date range" — and `ParameterSpec::kind` "drives both validation of
supplied values and the UI affordance shown to the user". The desktop already
has `corpus_get_recipe_parameters` and `corpus_install_with_parameters`
(`sovereign-desktop/src-tauri/src/recipe_commands.rs`). No new install UI.

### §7.2 The four scenes

1. **Catalog** — user sees "SEC Filings — Single Company" (F3, landed).
2. **Install asks a question, not a config** — "Which company?" -> `AAPL`,
   rendered from `ParameterSpec.kind`, interpolated into `[acquire]`.
3. **Corpus card states what it can answer** — concepts covered, period range,
   as-of filing, and the named limits ("segment figures such as Services revenue
   are not available; SEC's consolidated API does not carry them"). This is F5
   and F6 rendered honestly rather than as a percentage nobody can act on.
4. **Question** — figure with period basis + accession, or a refusal naming what
   IS available.

### §7.3 The generalization: authority is DECLARED, never configured

"Figure-bearing corpus" is REJECTED as a concept — it is a category label, it
overfits, and nearly every corpus contains numbers. The property that
generalizes is three things together:

1. a **typed authoritative store** for a class of assertions,
2. **prose in the same corpus carrying lookalike values** that are not
   authoritative (comparatives, roundings, guidance), and
3. **material harm** when the two are confused.

That is not a financial property. It is the assessor roll (parcel data vs prose
about parcels), a drug formulary, a statute database, a parts catalog.

So the routing predicate is NOT a similarity contest. Today's gate asks "is this
more tool-like than knowledge-like?" against a global exemplar space — which
financial questions can never win, because they legitimately ARE knowledge-shaped
in wording (measured: top_sim 0.9295, and `router/exemplars.toml:345` is
"What's the difference between gross and net margin?"). The right question for a
typed store is **"does this store claim authority over this question?"**, which
the tool answers DETERMINISTICALLY from its own enumerable domain — no
embeddings, no threshold.

Two properties make that safe rather than clever:

- **The failure direction is good.** An over-claiming tool produces an honest
  refusal naming what IS available — already first-class, already audited. It
  does not produce a wrong number. Compare the measured status quo: `$6.08`
  invented against an actual diluted EPS of 7.46.
- **No routing restructure.** `router.rs:360` `intent_is_toolless` excludes only
  `SimpleAction` / `ComplexTask` / `Continuation`, so Knowledge, Comparison and
  Deep queries ALREADY reach the gate. The consult point is correct; only the
  comparison is wrong.

### §7.4 What the user controls, and what they must not

| Decision | Who | Where |
|---|---|---|
| Which company | user | install parameter (affordance exists) |
| Which store is authoritative | recipe author | `[authority]` block, ships with the corpus |
| Whether figures are deterministic | **nobody — it is a contract** | not a setting |
| What it cannot answer | derived, not authored | coverage card (F5/F6) |

**There is deliberately NO "use deterministic figures" toggle.** A switch makes
honesty optional, and whoever leaves it off gets `$6.08` for an EPS of 7.46.
ARCH §7.6 — never ask a model to guarantee what code can enforce — applies to
users too: do not ask a person to remember a guarantee the contract can hold.

The user-visible surface for authority is **visibility, not configuration**.

### §7.5 The design cost, named rather than discovered

The tool needs a pure, cheap `claims(question)` surface the gate can call before
dispatch — a new trait method across tools — plus a deterministic tie rule when
two in-scope stores both claim. That is a real seam, not a threshold tweak.

### §7.6 The epistemic ethos holds here — and it is TWO-SIDED

Operator, 2026-08-14: *"The epistemic ethos holds in this surface just as it
does in other places (honest abstentions, etc)."*

This corpus class is NOT a special honesty regime. It is the system's standing
posture applied to a domain where the cost of confusion is high. The ruler
(`quality/backlog-ruler.toml`) already names both sides, and BOTH bind here:

- **Axis A, Grounded** — "does not hallucinate": no fabricated or wrongly-accepted
  content reaching a user (yardsticks: honesty-when-absent 0.91, 13.1% parametric
  leak, incumbent 2/7 wrong-accepts).
- **Axis B, Responsive** — "doesn't over abstain... the model does respond":
  recovers answers wrongly declined (yardstick: competence-when-present
  0.71/0.80).

**Consequence for F2, and it is a real hole in the bar as first written:** "zero
unattributable numerals" is satisfiable by refusing everything. A tool that
answers nothing has a perfect fabrication rate and is worthless. The zero-
fabrication bar is therefore PAIRED, and neither half stands alone:

- **Honesty half** — zero unattributable numerals across the adversarial set.
- **Competence half** — every question the typed store CAN answer IS answered,
  with basis. A refusal on an answerable question fails F2 exactly as a
  fabrication does.

The judge already has the shape for this: the baseline's `arithmetic-yoy-revenue`
row failed as *"evasion: required value(s) absent — a pass that says nothing
verified nothing"*, which is the competence half catching an abstention that
looked clean. Keep that verdict class first-class and report the two halves
separately, so a change that trades one for the other is visible rather than
netted out (ARCH §18.6 — a judge change reported only in the direction it was
meant to fix).

**Refusals must also be USEFUL, not merely correct.** "I cannot answer that" is
a technically honest abstention and a bad one. The standing form is: what was
asked, why the store cannot support it, and what IS available — which is what
slice 1's refusals already do ("available period end date(s), named not
substituted: [...]").

## §8 The march — the spec IS the definition of done

Operator, 2026-08-14: *"I want us to start marching towards the product
specification that we just clarified -- that's the definition of done."*

Done is §7's four scenes being true for a real user, not six bars closing on
paper. Sequenced so each step is judgeable on its own; each names what it closes.

| # | Step | Closes | Status |
|---|---|---|---|
| M1 | `[authority]` block + deterministic `claims()` + F2 paired proof | F2; makes F5/F6 judgeable at all | IN FLIGHT (`sec-filings-harden`) |
| M2 | `[parameters]` ticker block + install path that is not a repo script | F3 **properly** | NOT STARTED |
| M3 | Coverage card in desktop — concepts, period range, as-of, named limits | F5, F6 on the USER surface | NOT STARTED, UNOWNED |
| M4 | Engine burn-down (`8ac55cf8`, `9c5929be`) | neither — clears debt this program filed | operator: "at the end" |
| M5 | Second filer, then widening | F1 beyond one-filer proof | parked (`sec-filings-mag7`) |

**Named, not scheduled:** dimensional/segment facts from the filing's own XBRL
instance (companyfacts is consolidated-only); 10-Q so a quarter is not a refusal
by construction; amended filings (10-K/A) beyond F6's detection.

### §8.1 Two gaps the bars alone would have hidden

Found 2026-08-14 by reading §7's scenes against the tree, not by reading bar
text:

1. **Scene 2 does not work.** `sovereign-recipes/sec-filings-company/recipe.toml`
   declares NO `[parameters]` block, so no user can install by ticker from the
   desktop. Four recipes already declare one (`scotus-opinions`, `olc-opinions`,
   `email-archive`, `federal-register-presidential`) — the precedent exists and
   the desktop already calls `corpus_get_recipe_parameters`. F3's bar says
   "install by ticker from the catalog surface with no repo script invocation";
   the registry entry alone does not satisfy it. **F3 is NOT met.**
2. **Scene 3 has no surface.** No coverage card exists in the desktop. F5 and F6
   are implemented in-tool, which makes them true for the tool and invisible to
   the user — the same shape as F2's original gap, one layer up.

This is why the journey is the definition of done and the bars are its
instrument. A bar can be satisfied in a harness; a scene cannot.
