# PRE-REG: a custom ontology, and RAPTOR, against bare RAG — the experiment behind the demo

Drafted 2026-09-17, before any corpus is acquired, any truth is derived or any
arm is run. Pattern: `research/prompt-climb/PRE-REG-obsidian-2026-09-09.md`.
**Status: DRAFT. The bars are proposed, not ratified.** The ratifying commit is
the timestamp the demo shows.

Serves the `epistemic-index` campaign (`quality/campaigns/epistemic-index.toml`).
None of its bars EI1–EI6 compares against bare retrieval, so this study proposes
bar `EI7-ontology-reach`, to be registered by the operator.

## Claims under test

1. **Custom ontology.** A domain expert writes a short declaration of how their
   field thinks. The system builds a map in those terms. On the kinds of
   questions the theory predicts, answers get better than bare RAG and better
   than the generic atlas a user gets without writing one. The declared
   primitives are visibly on the path that fed the answer.
2. **RAPTOR.** On long literary works, summaries answer whole-story questions
   better than bare RAG and better than the walk without summaries, and cost
   nothing on questions a single passage answers.

A gain that shows up outside the predicted kinds falsifies the theory, even if
the average rises.

## The filter every element passed

The audience is technically literate skeptics: partners, the foundation, the
product team. An element stays only if it does one of three things:

- (a) produces something they see;
- (b) answers an objection they will raise;
- (c) validates the instrument that (a) and (b) rest on.

Anything else is a diagnostic, deferred until a result needs explaining. Four
objections shape the design:

| Objection | Answered by |
|---|---|
| "You cherry-picked." | bars fixed before any data; showcase questions picked by a rule; a lookup kind where no gain is predicted, shown on stage; the audience draws a live question from the frozen bank after hearing the measured odds |
| "You graded your own homework." | an outside domain reader, blind, in bar 4 |
| "The model already knew." | closed-book arm on the scoreboard |
| "Just retrieve more." | deep-pool bare arm |
| "Why write an ontology? Any graph would do." | generic-atlas arm |

## Prior evidence, and why it does not settle this

- **Ontology on vs off: every comparison so far is a null.**
  - SEP walk on = off at 153/158 (55b3ca0b5).
  - Rung 6 scored 0.902 vs 0.916, inside the run-to-run spread.
  - Wikipedia scored 111 vs 113 of 130.
  - EI1: "the atlas walk contributed nothing" (00e5a71f9).

  All of these sit in the lookup kind, where SEP bare retrieval already reaches
  97%. They also ran before eb3681d1e fixed the question classifier and the
  unembedded atoms.
- **The one large judge gap is not on the production path.** It is 52% vs 32%,
  n=25 (`sovereign/bench/HISTORY.md:53-61`), and its truth set was derived from
  the atoms themselves.
- **RAPTOR is unproven.**
  - A "+8/+10pts theme coverage" claim has no artifact behind it (55ad1d573).
  - The knob matrix measured −0.0125, under its own noise floor.
  - 9 of 23 misattributions trace to summary content.
  - In June, the model dismissed accurate summaries and answered from memory.
- **Counter-signal: bare retrieval with a deeper pool beat the walk plus the
  reranker** (93.0% vs 88–89%, a32d98d0b).

## Theory, in one paragraph and one table

Bare RAG scores every chunk against the question on its own. The top 20
survive the merge (`sovereign/crates/sovereign-core/src/runtime/prompts.rs:366`),
and at most 24,000 characters reach synthesis
(`sovereign/crates/sovereign-core/src/runtime/formatters.rs:39`), which is
about 12 passages. That imposes two limits:

- **Capacity:** no more than about 12 pieces of evidence.
- **Independence:** a passage that matters only in combination with another
  one earns nothing for it.

Structure changes both. A walk selects passages through typed links instead of
by similarity. Rendered atoms and summaries pack many facts into one retrievable
unit. The atom-enum path renders atoms
(`sovereign/crates/sovereign-core/src/runtime/retrieval/atom_enum.rs:69`), and
RAPTOR nodes do the same for whole-story arcs.

| Kind (audience words) | What it covers | Why bare RAG fails | Prediction |
|---|---|---|---|
| **K0 Look it up** | one passage answers | it doesn't | tie (control) |
| **K1 List them all** | enumeration, counts, absence | recall ≤ window / set size; absence is in no passage | custom ontology wins |
| **K2 Connect the dots** | bridges, aliases | the far passage never names what was asked | custom ontology wins |
| **K3 What changed, who disagrees** | superseded terms, contested claims | sides and versions are scored apart, unmarked | custom ontology wins |
| **K4 The whole story** | arcs and themes across a long work | sees about 1/25 of a 300-chunk novel | RAPTOR wins |

**Kinds are computed, never labelled by the author.** Every gold item is
attested by exact or regex match over the corpus chunk text, not through
retrieval, and unattested items are dropped and counted. The rules:

- **K0:** exactly one attesting passage.
- **K1:** more than 12 attesting passages.
- **K2:** at least one attesting passage that does not name the entity the
  question asks about.
- **K3:** the gold derives from a contested pair or from an original and its
  amendment.
- **K4:** no single chunk holds 80% or more of `answer1`'s content words.
- **Literary K0:** a single chunk does hold 80% or more of them.

A question that fails its kind's rule is dropped and counted.

## Implementation reach — checked 2026-09-17, before any arm

A declared primitive that never reaches an answer cannot be credited or
narrated (principle 6).

- **These reach answers today:**
  - Declared types and attributes are rendered into the atom text that the walk
    and atom-enum retrieve (`render_atom_entry` in
    `corpus-engine/src/enrichment/atlas/context.rs`).
  - Identity keys merge atoms at build time.
- **These do not reach answers:**
  - **`patterns`** (for example circular flow) are detected at build and written
    to `atlas/pattern_findings.json`
    (`corpus-engine/src/enrichment/atlas/writer.rs:501-503`). Nothing in
    sovereign-core reads that file.
  - **`change` (supersession)** feeds build-time tension folding
    (`corpus-engine/src/enrichment/atlas/analysis/tension_fields.rs:90-101`).
    None of the 14 edge kinds (`corpus-engine-vocab/src/edges.rs`) is a
    supersedes edge.
  - **Declared tension.** The `tension` row walks `Tension` and `OpposesIn`,
    while a declared corpus emits `Involves` and `Grounds`
    (`sovereign/docs/specs/EPISTEMIC_INDEX.md:434-443`).
- **Row and budget limits:**
  - The walk has five rows (`corpus-engine-vocab/src/ontology/navigation.rs:57-61`).
  - `lookup` walks a single hop.
  - `enumeration` keeps 12 atoms (`DEFAULT_BUDGET`, `:252`), so on K1 only
    rendered atoms can beat the capacity limit.
- **Summaries** reach retrieval only through the `thematic` and `unfiltered`
  rows (`:346`, `:498`).

**Consequence for sequencing.** The ANS corpus runs first on today's path. Its
primitives are types, attributes, identity keys and graded attribution claims.
Its K3 is narrated only if check A3 shows reach. Hollinger and Virco lean on
patterns, supersession and declared tension, so they are built only after the
product rung (Order of work, stage 1b) lands.

## Corpora and held-out truth

Truth never enters a corpus. Figures below come from the 2026-09-17 scouting
and are re-verified at acquisition. A figure that fails is corrected under
Deviations, never silently.

| Corpus | Text | Truth (held out) | Kinds | Primitives it narrates | Licence → recipe |
|---|---|---|---|---|---|
| **ANS numismatics** | American Numismatic Society TEI monographs, ~19 works (Newell's Alexander hoards and mint studies, Thompson, Troxell 1997) | CoinHoards/IGCH NUDS, PELLA, Seleucid Coins Online (ODbL); nomisma.org labels (CC BY) | K0 K1 K2 K3 | `hoard`, `mint`, `ruler`, `coin` with Price-number identity; `attribution` graded die-link / hoard-context | CC BY-NC 4.0 → `scope="local"`, `mesh_sharing=false`; `query_sharing` set per showing |
| **Hollinger International** | FY1999–2003 10-Ks, proxies, 8-Ks | Breeden Special Committee report (2004), SEC complaint v. Black, Delaware Chancery opinion | K0 K1 K2 K3 | `counterparty` role, `payment` events, circular-flow `pattern` | SEC public |
| **Virco–PNC credit agreement** | 2011 original + amendment exhibits | conformed copies, 8-K Item 1.01 summaries, 10-K debt footnotes | K0 K1 K3 | `defined_term`, `obligation` (deontic), `change` / supersession | SEC public |
| **Literary** | 3 Project Gutenberg books from NarrativeQA's test split (github.com/google-deepmind/narrativeqa, Apache-2.0), each ≥ 150 chunks | `answer1`; the Wikipedia plot summary is held out as the oracle context and the faithfulness reference | K0 K4 | RAPTOR summaries | public domain (US) |

**Corpus notes.**

- **ANS truth handling.**
  - Strip the TEI's semi-automated IGCH and Price links before ingest.
  - IGCH summarises these same books, so a later revision by Troxell or
    Thompson counts as K3, not as a system error.
  - K3 truth has no structured source. Two annotators work independently from
    the named passages, the operator adjudicates, and the result is frozen
    before any arm.
- **Stage book.** Several recognizable NarrativeQA titles are probed with the
  closed-book arm on 10 questions each. The first to score below 0.5 is the
  stage book. The other two books are drawn by a seeded script across length
  terciles, with the same screen.
- **Enron is excluded.** The installed slice has 8,829 chunks. LJM appears in
  16 of them, Chewco in 4, Raptor in 3, and Southampton, Rhythms and Talon in
  none (counted 2026-09-17), so truth derived from the Powers Report would be
  unattested.
- **PsyTAR is excluded.** AskaPatient's terms bar showing post text. The
  patient-community template appears on stage as a declaration only.
- **EDGAR contact.** EDGAR requires a User-Agent contact. Scripts read it from
  an environment variable; it is never written as a literal.

## The authored ontology

The arc is authored on camera, so its record is data.

- **Authoring path.** Declarations start from
  `sovereign-recipes/_templates/ontology-v1/<domain>/recipe.toml` and use the
  product's own steps: `svrn recipe new --ontology`, edit, `svrn recipe validate`.
  They are adapted from the domain and the corpus's table of contents only,
  before any question exists. Gaps known now: the numismatics template has no
  `hoard` type, and the due-diligence template has no control or partnership
  relation.
- **Recorded and committed at freeze:**
  - the author;
  - wall-clock authoring minutes;
  - declaration line count;
  - the `validate` output.
- **Freeze.** The manifest of every arm records sha256 of the recipe and of
  `atlas/ontology.json`. The comparison refuses arms whose hashes differ from
  the frozen pair. An edit after freeze is a deviation and re-runs every arm for
  that corpus.

## Arms — one shape for every corpus

Each arm runs as its own process, three times:
`svrn eval run --bank <b> --synth --prod-pipeline --isolate --limit 30 --format json`.

| Arm | Ontology corpora | Literary |
|---|---|---|
| **closed-book** | synthesis with no retrieval | same |
| **bare** | `SOVEREIGN_ATLAS_GROUNDING=0 SOVEREIGN_ATOM_ENUM=0 SOVEREIGN_ATOM_ENUM_OVERVIEW=0` | same |
| **bare, deep pool** | bare with `--limit 80` | same |
| **ablation** | generic atlas: the same corpus built with no `[enrichment.ontology]` (the `custom_atlas` v0 path, `corpus-engine/src/recipe_ontology/docs/v0.md`), walk and atom-enum on | the walk over an atlas copy with no `Summary` atoms |
| **full** | custom ontology, walk and atom-enum on (`quality/env-flags.toml:248`, `:304`, `:311`) | the same copy after `svrn enrich summary-atoms` |
| **oracle** | bare, against a corpus holding only the attesting passages | the held-out plot summary as the only context |

- **Closed-book** needs the study's one Rust addition. No flag in
  `sovereign/crates/sovereign-cli-llm/src/eval_cmd/mod.rs` suppresses retrieval
  (checked 2026-09-17). Questions that closed-book answers (judge ≥ 0.5 on all
  three runs) are excluded, and their count is shown on the scoreboard.
- **Literary ablation and full** are two states of one build. That is the
  defaults ledger's RAPTOR A/B method (`sovereign/DEFAULTS_LEDGER.md`). Copy ids
  must not collide under any evidence-parent prefix (the raptor-subset
  precedent, d3560efa2).
- **Default flip before the demo.** If the full ontology arm wins with
  `SOVEREIGN_ATOM_ENUM=1`, that flag's default flips, with a `DEFAULTS_LEDGER`
  row, before the demo presents it as the product.

## Checks, before any delta is read

Each check has four verdicts: passed, failed, could-not-judge, never-ran. The
pilot plants a fault for each one and watches it go red.

**Instrument**

- **I1.** Closed-book returns an empty `retrieved[]`.
- **I2.** Every arm is what it says it is.
  - Bare has no atlas, atom-enum or raptor sources.
  - Full's walk fired: atlas-sourced entries on at least 50% of questions, and a
    summary appended on at least 50% of K4 questions. Otherwise that arm is
    never-ran, not a null.
- **I3.** Ablation and full differ only in the named thing. Their manifests show
  identical `chunks.lance` and differing atlas hashes.
- **I4.** Noise band = the bare arm's max − min per kind over three runs, floor
  0.05. The judge is reported beside the keyword view, since it flips on 37% of
  facts across trials (note 485f9f05).
- **I5.** Oracle scores at least 0.8 on K0 and K2. Below that, the truth set or
  the scorer is broken. On K1 and K4 the oracle is reported as the window
  ceiling.
- **I6.** Build census:
  - `ontology.json` and `atoms_ann.lance` present;
  - atom count per declared type;
  - edge count per edge kind;
  - pattern-finding count;
  - `Summary` atom count.

**Arc.** These gate what may be narrated on stage, not the retrieval claim.

- **A1.** The authoring record exists and was committed before the bank was
  written.
- **A2. Typed reach.** Recall of each declared type against the truth
  vocabulary, in the `sovereign-recipes/wessex-hoard/truth.json` shape. A type
  under 0.5 is not named on stage.
- **A3. Primitive reach.** For each K1–K3 question, whether a declared type,
  attribute, grade, pattern finding or supersession appears on the evidence
  path (the walk's fetch ledger, persisted per question by stage 0a, and source
  tags). A primitive is narrated for a kind
  only if it reaches at least 50% of that kind's questions on that corpus.

## Metrics

All come from the existing eval JSON
(`sovereign/crates/sovereign-cli-llm/src/eval_cmd/runner.rs:205-211`). Each gold
member is one `expected_facts` entry
(`sovereign/crates/sovereign-cli-llm/src/eval_cmd/bank.rs`).

- **Member recall:** `synth.judge_fact_score`, plus the `fact_score` keyword
  view. Literary questions score against `answer1`, equally for every arm.
- **Fabricated members** (new, deterministic): names from the corpus's truth
  vocabulary that the answer states as members but that are not gold. Names
  outside the vocabulary are not counted, and that limit is stated.
- **Contradictions (K4):** answer statements that the judge marks as
  contradicting the held-out plot summary.
- **Diagnostics, kept in the JSON and never bars:** evidenced recall
  (`chunks_fact_score`), the walk row fired, and whether a summary was appended.

## Bars — proposed, fixed at ratification

Each bar is the mean of three runs, pooled per kind across the corpora that
carry that kind. A kind needs n ≥ 20 pooled questions or its verdict is
could-not-judge. Banks hold 10 questions per (corpus, kind) carried.

1. **Doesn't hurt lookups (K0).** Full ≥ bare − band, on every corpus.
2. **Wins where predicted (K1–K3 for the ontology, K4 for RAPTOR).**
   Full − max(deep-pool bare, ablation) ≥ max(0.15, 2 × band). Both deltas are
   always reported: "beats RAG by x" and "beats the generic graph by y".
3. **Doesn't make things up.** Per corpus, full ≤ bare on fabricated members
   (K1–K3) and on contradictions (K4).
4. **People prefer it.** 10 answer pairs per corpus: full against whichever of
   deep-pool bare or ablation scored higher. Order is randomized, the arm is
   hidden, and both answers are rendered in one format with citations reduced
   to numbered references.
   - The operator reads every corpus. Full must be preferred in at least 7 of
     10, in at least 3 of the 4 corpora (literary counts as one).
   - One outside domain reader reads the same pairs for the stage domain,
     independently: a securities lawyer for Hollinger, or a numismatist for
     ANS. Full must be preferred in at least 7 of 10.
   - The reader is recruited before any arm runs, because the operator can
     recognise the system's style and that weakens the blinding.

   A metric win paired with a loss on either reading fails.

**Claim rule.** A kind is claimed only if bars 1, 2 for that kind, 3 and 4 all
pass. The demo shows only claimed kinds, plus the K0 tie. A predicted kind that
no arm moved is reported with its implementation-reach annotation.

## What the study must emit, so the demo is a rendering

`sovereign/bench/sep_atlas/map-conversion-rung6/compare.py` is extended; no
second scorer is written. It emits:

- **Three-way side-by-side per question** (bare, generic, custom; for books,
  bare, walk, RAPTOR). Each shows gold members marked found, missed or
  fabricated, citations, and the evidence path, with declared types and grades
  named.
- **One scoreboard:** kind × arm with noise bands, the closed-book exclusion
  count, n, and the ratifying commit's date.
- **The rule-picked showcase:** the median-gain question per (corpus, claimed
  kind), plus one K0 tie.
- **Blind-preference tallies** and the authoring records.
- **The map shot:** one continuous recording per ontology corpus, in three
  moves.
  1. The generic atlas (the ablation arm's build).
  2. The custom atlas of the same corpus, coloured by declared type.
  3. The showcase answer's evidence path lit across that map, drawn from the
     persisted fetch ledger.

  It is captured with `sovereign/crates/sovereign-desktop/playwright.demo.config.ts`
  by extending `sovereign/crates/sovereign-desktop/tests/e2e/real/numismatics.real.spec.ts`.
  The path is replayed from the study's own data, not computed live.
- **The deck, for pick a card:** for each claimed kind, the frozen bank minus
  closed-book exclusions, each question carrying its measured win count (full
  vs the stronger baseline, over three runs). The presenter states the odds
  for the kind before the draw.
  - The draw runs through a live runner: one question, bare and full, as two
    CLI invocations side by side.
  - A live result is never scored into a verdict. When it differs from the
    measured result, the difference is said out loud.

## Order of work

- **Stage 0a — harness.**
  - the closed-book flag;
  - a run wrapper that writes the manifest (declaration hashes, arm
    environment, commit);
  - the walk's fetch ledger persisted per question in the eval JSON. Today the
    path (seeds, edges followed, fetched evidence) is only a tracing event
    (`atlas-grounding: fetch ledger` in `atlas_grounding.rs`), and the JSON's
    `atlas_navigation` is an embedding top-K snapshot that never enters the
    prompt (`sovereign/crates/sovereign-cli-llm/src/eval_cmd/runner.rs:1133-1135`);
  - the fabricated-member scorer;
  - the compare.py outputs above, and the live runner behind pick a card.

  Cargo work goes through `scripts/with-cargo-lock.sh`, one worker at a time.
- **Stage 0b — Map keyed by declared type.** Today `AtlasNode` carries no
  declared type (`sovereign/crates/sovereign-tools/src/atlas_view/subgraph.rs:47-54`),
  colours come from a fixed 11-kind table
  (`sovereign/crates/sovereign-desktop/src/lib/components/atlas/atomKinds.ts:85-100`),
  and the node cap favours argument and question atoms (`subgraph.rs:116-124`).
  The Map also gains one input: a set of atom ids to highlight, which the map
  shot feeds from the ledger. That needs no change to the core or the wire.
- **Stage 0c — pilot on chaos-secret-agent.** It is installed, with both the
  walk and 19 `Summary` atoms. Run I1–I6 and watch each planted fault go red.
  The pilot is instrument evidence only.
- **Stage 1a — ANS.**
  1. Acquire and strip the TEI links.
  2. Derive truth.
  3. Author the declaration and record it (A1), then freeze it.
  4. Attest and write the banks.
  5. Build the custom and generic atlases (launchd one-shots, logs monitored).
  6. Run the census, then the arms.
- **Stage 1b — product rung, in parallel: declared primitives reach retrieval.**
  Pattern findings, supersession and declared tension. The design extends
  `sovereign/docs/specs/EPISTEMIC_INDEX.md`. The rung is pre-registered as
  mechanism, and its bar is A3 reach on a fixture corpus, never answer quality
  on these banks. It lands before any Hollinger or Virco bank is written.
- **Stage 2 — Hollinger, then Virco**, through the same steps as 1a.
- **Stage 3 — literary.** Select the stage book, draw two more books, build,
  copy and project summaries, run the arms.
- **Stage 4 — close.** The report, the demo outputs, the `ATOM_ENUM` default
  decision, and the verdict row for `EI7-ontology-reach` if registered.

## Layout and licensing

The directory is `research/ontology-retrieval/`, with one subdirectory per
corpus. Each holds:

- the acquisition, truth and attestation scripts;
- the recipe and the authoring record;
- the banks;
- runs (manifests, arm JSONs, verdict jsonl).

Corpora and derived truth live in a gitignored data directory that the scripts
regenerate. ODbL-derived truth carries share-alike if published; the scripts
that derive it do not. Each recipe records its SPDX id in `license`.
Restricted-text corpora set `scope`, `mesh_sharing` and `query_sharing`
(`corpus-engine/src/recipe.rs:789-816`). The side-by-side renderer reads
`query_sharing`: when it is false, the renderer withholds snippets and says so.

## Non-goals (diagnostics, deferred until a result needs explaining)

- A selection-only vs representation split.
- RAPTOR summary text vs summary seeding.
- A length-dose curve.
- A wrong-ontology arm.
- PsyTAR as a measured corpus.
- Any tuning of the walk, declarations, prompts or thresholds against these
  banks. A change of that kind starts a new study.

## Deviations

(Appended as the study runs: date, what changed, why, which arms re-ran.)
