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

3. **Flexibility.** The win is a property of the mechanism, not of one domain:
   it replicates across corpora whose declarations differ. Coins share no type
   with either agreement corpus; the credit agreement and consumer fine print
   start from the same `contracts` template, and the record shows how many
   lines changed to move from one to the other. Each declaration is written by
   one person in under an hour in the same recipe language.

A gain that shows up outside the predicted kinds falsifies the theory, even if
the average rises.

## The filter every element passed

There are two readers (operator, 2026-09-18). The **skeptic** is technically
literate — partners, the foundation, the product team — and checks the
instrument. The **layperson** is a general audience in 2026 and has to feel the
result without a gloss. The study is built for the skeptic; the stage is chosen
for the layperson; and the stage shows nothing the study did not measure. An
element stays only if it does one of three things:

- (a) produces something they see;
- (b) answers an objection they will raise;
- (c) validates the instrument that (a) and (b) rest on.

Anything else is a diagnostic, deferred until a result needs explaining.

**The stage-domain rule.** The stage corpus is a measured corpus, chosen for
relatability among those that pass the three stage probes (Corpus notes). The
other corpora appear as scoreboard rows, and those rows are the flexibility
evidence. Relatability pulls against the closed-book screen — a famous subject
is one the model already knows — so the stage wants a familiar subject with
unfamiliar detail: something everyone has touched and nobody has read.

Five objections shape the design:

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
Its K3 is narrated only if check A3 shows reach.

**Fine print is the stage domain, and it splits at the same line** (operator,
2026-09-19; it replaces Hollinger, which replaced ANS on 2026-09-18). The
`contracts` template (`sovereign-recipes/_templates/ontology-v1/contracts/recipe.toml`)
declares `defined_term` and `obligation` (deontic); the author adds `service`,
`data_type`, `recipient` and `purpose`. Types and attributes are rendered into
atom text and reach answers today, so fine print's K0, K1 and K2 run on
today's path, in parallel with ANS. Its K3 (what a policy update changed) waits
for the product rung (stage 1b), as does Virco, which is supersession
throughout.

## Feasibility spikes — before ratification, and what they may not touch

Four spikes run before the bars are ratified (operator, 2026-09-19). They may
change which corpus is used, how banks are sized, and whether a kind is
attempted at all. They may not set or move a bar's threshold, and none of them
runs an arm against a study bank — no study bank exists yet. Each result is
recorded under Deviations with its data under `spikes/`.

1. **K1 ceiling.** On an installed atlas corpus, one enumeration question over
   a type with 30 or more atoms: how many members reach the prompt, and how
   many reach the answer. The enumeration path takes the top 16 atoms
   (`SOVEREIGN_ATOM_ENUM_TOPK`) and its chunks still pass `KQ_MERGED_LIMIT` and
   `MAX_KNOWLEDGE_CHARS`, so the full arm's ceiling on "list them all" may sit
   near 16 whatever the ontology declares. If it does, K1 needs a product rung
   as K3 does, and the study says so before it is built.
2. **Fine-print data.** Five services: the share of ToS;DR quotes that
   re-anchor into Open Terms Archive text, the overlap of the two archives, and
   real K1 set sizes.
3. **Extraction census.** A throwaway adaptation of the `contracts` template
   over three services' policies: atom count per declared type. This lets the
   author see extraction before the on-camera authoring, so that record
   measures adaptation, not first contact, and says so.
4. **Closed-book screen.** Ten detail questions to the synthesis model with no
   retrieval.

## Decision tree after the spikes

Written 2026-09-19 at 14:45 PDT, after spikes 1, 2, 4 and the local 108-section
census, and BEFORE the full 449-section census on the rented pod returned. The
thresholds for that census are fixed here so its result selects a branch
instead of inspiring one. None of these is a bar; they decide what is built and
attempted, not what is claimed.

| # | Question | Evidence in hand | Branch |
|---|---|---|---|
| D1 | Is the enumeration mechanism worth a study? | Spike 1: 1/40 → 20/40 members named | **Go.** Settled. Capacity only; quality is what the study measures. |
| D2 | Can K1 arms run on today's path? | Spike 1: at top-K 40 every merge slot is a virtual chunk | **No, until passages hold a reserved share of the merge.** Without it K1 arms are never-ran, not nulls. |
| D3 | Does declared extraction support fine-print K1 over `data_type`? | Local: Spotify 10/10 against its own table, LinkedIn weak | **Full census decides.** `data_type` recall against each service's own published list ≥ 0.7 on at least 2 of the 3 services → go. Otherwise fine print loses K1 and D6 applies. |
| D4 | Are `recipient` questions attempted? | Local: Spotify 4/13 by name; "partners" absorbed four distinct recipients | **Full census decides.** Spotify recall by name < 0.5 → not attempted until the head-noun merge is fixed and re-censused. ≥ 0.5 → attempted. |
| D5 | Are obligation and K3 questions attempted on any declared corpus? | Local: deontic filled on 2.5% of 320 claims | **No, until the fill rate is ≥ 70% on a re-census.** This is an engine defect (the prompt's claim example omits the attribute), so it blocks Virco and fine-print K3 alike. |
| D6 | Does fine print keep the stage? | Spikes 2 and 4 passed for detail questions | Keeps it if D3 is go. If D3 fails after one fix round, **ANS takes the stage**, as already written; no new candidate is scouted. |
| D7 | Does the quantisation matter? | Local ran IQ4_NL, the pod runs Q6_K | On the 108 sections both runs cover, if any declared type's atom count differs by more than 2× → the quant is part of the model freeze and is chosen deliberately. Otherwise either serves. |
| D8 | Where do the arms run? | Pod: 4.7 s a section, no refusals, $0.72/h. Shared local daemon: 20–30 s, 9 of 108 sections refused | **On a rented pod, one per corpus,** if the pilot's per-question synth latency there is at most half the local figure. Otherwise local, in a reserved window. |

**The product rung the spikes require (lane X of the stage-0 queue).** Each is
pre-registered as mechanism, judged on a fixture and a re-census, never on a
study bank:

- required before any arm: the passage reservation (D2); the deontic attribute
  shown in the extraction prompt's claim example (D5); document and service
  context carried into the extraction prompt, not the section title alone
  (18–33% of atoms had no service);
- required for within-service K1: the 15-entities-a-section cap, which
  Spotify's data table hit;
- conditional on D4: the head-noun merge;
- deferred unless check A2 shows a declared type under 0.5: the 83 junk
  `person` atoms and the 56% orphan rate.

**The spike services are a development set.** Reddit, Spotify and LinkedIn
have now shaped both a declaration and the engine fixes. They stay in the
corpus, their questions are reported as their own scoreboard row, and they are
excluded from every bar. The re-census that gates D3–D5 runs on three services
that were not in the spike, chosen by the corpus rule.

**Ratification waits for:** lane X landed, the re-census meeting D3 and D5, and
the pilot. If the re-census fails after ONE fix round, the failing question
family is dropped from the study and named in Non-goals; there is no second
round.

## Corpora and held-out truth

Truth never enters a corpus. Figures below come from the 2026-09-17 scouting
and are re-verified at acquisition. A figure that fails is corrected under
Deviations, never silently.

| Corpus | Text | Truth (held out) | Kinds | Primitives it narrates | Licence → recipe |
|---|---|---|---|---|---|
| **ANS numismatics** | American Numismatic Society TEI monographs, ~19 works (Newell's Alexander hoards and mint studies, Thompson, Troxell 1997) | CoinHoards/IGCH NUDS, PELLA, Seleucid Coins Online (ODbL); nomisma.org labels (CC BY) | K0 K1 K2 K3 | `hoard`, `mint`, `ruler`, `coin` with Price-number identity; `attribution` graded die-link / hoard-context | CC BY-NC 4.0 → `scope="local"`, `mesh_sharing=false`; `query_sharing` set per showing |
| **Consumer fine print** (stage domain) | Terms of service and privacy policies of about 60 consumer services, from the Open Terms Archive versions repos (`contrib`, `pga`, `vlopses-us`, `genai-contrib`): the current version, plus the version pairs K3 needs | ToS;DR approved points (case label + quoted span, per service), fetched from its public API; Open Terms Archive commit diffs for K3 | K0 K1 K2 now; K3 after stage 1b | `defined_term`, `obligation` (deontic), `service`, `data_type`, `recipient`, `purpose` | text: each company's copyright, republished in full by both archives; database ODC-By 1.0; truth CC BY-SA 3.0 → `scope="local"`, `mesh_sharing=false`, snippets as short quotations |
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
- **Fine print is on stage because everyone has agreed to it and nobody has
  read it.** That is the familiar-subject, unfamiliar-detail shape the
  stage-domain rule asks for, and a member of the audience can name a service
  they use. Scouted 2026-09-19 (figures re-verified at acquisition):
  - **Text.** Open Terms Archive keeps one git commit per version, as cleaned
    markdown, and is live (pushed 2026-09-19). Google's privacy policy has 50
    versions since 2020, Zoom's terms 515 since 2021.
  - **Truth.** ToS;DR's API serves 10,290 services and 269 cases; each point
    ties one case to one document with `quote_text` and a status. Case 220
    (targeted third-party advertising) has 1,797 points, case 166 (sharing
    with non-essential third parties) 3,294, and case 504 (profiling or AI
    training) 54 approved services, among them Reddit, X, Google, Slack,
    Spotify and Discord.
  - **Which services.** Picked by a rule, not by hand: services present in
    both archives, ranked by approved ToS;DR points, taken until the corpus
    reaches about 9,000 chunks (the size of the installed Enron slice, which
    has built). The rule favours the services people know, because those are
    the ones volunteers annotate.
  - **The truth is incomplete, and two metrics say so.** ToS;DR is
    crowd-sourced: a service with no point for a case is unlabelled, not a
    "no". So K1 recall is scored against the labelled set only, and bar 3's
    fabricated-member count cannot be read off the vocabulary on this corpus:
    every non-gold service an answer names is checked against the policy text
    by the two annotators, blind to arm, and counts as fabricated only if the
    text does not support it. Case 504 mixes profiling with AI training and is
    filtered to AI-training quotes by the same pair, before any arm.
  - **K2 and K3 truth.** K3 has a structured source: the archive's diffs
    (LinkedIn 2024-09-19 adds "develop and train artificial intelligence (AI)
    models"; Zoom 2023-08-07 adds the consent sentence). K2 has none: joins
    through a defined term or a referenced policy (Reddit's user agreement
    defers to its Public Content Policy) are written by two annotators from
    the named passages and adjudicated by the operator, as ANS K3 is.
  - **Three stage probes**, run at acquisition, before any fine-print bank is
    written, each result recorded under Deviations:
    1. **Truth leak.** No ToS;DR text, point or rating enters the corpus, and
       no Open Terms Archive memo does.
    2. **Attestation drop.** ToS;DR quotes point into its own crawl, so each is
       re-anchored by whitespace-normalised match into the archive's text;
       the unmatched are dropped and counted. If K1 or K2 falls under 20
       surviving questions, the kind pools with ANS and the stage says so.
    3. **Closed-book screen.** The AI-training changes made headlines. Ten
       questions per kind through the closed-book arm; a kind scoring 0.5 or
       more is not staged from this corpus.

    If the probes leave no stageable kind, ANS takes the stage and the outside
    reader changes with it; that is a deviation, written before any arm.
- **OPP-115 is excluded as a measured corpus.** Its expert span annotations are
  the best truth scouted, but its policies date from 2009–2015 and its terms
  are research-only, non-commercial. **Hollinger is excluded**: it was the
  stage for one day, and a lay audience has no stake in a 2003 newspaper
  fraud. The due-diligence template's circular-flow pattern is therefore not
  exercised by this study.
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
  `hoard` type, and the `contracts` template has no `service`, `data_type`,
  `recipient` or `purpose`. It does have what K2 leans on: `defined_term`
  carries a `ref` attribute to its `agreement`, and the language has a relation
  primitive (`kind = "relation"` with `from`/`to`, `contracts/recipe.toml:96`).
  (Corrected 2026-09-19: commit 0f93fd34e said the language had no relation
  primitive. That was read off the due-diligence template alone and is wrong.)
  Whether a declared relation or `ref` reaches an answer is not assumed: check
  A3 measures it, and K2 is narrated through one only if it does.
  The fine-print declaration is the one authored on camera, and its record
  includes the line diff against the Virco declaration (claim 3).
- **Recorded and committed at freeze:**
  - the author;
  - wall-clock authoring minutes;
  - declaration line count;
  - the `validate` output.
- **Freeze.** The manifest of every arm records sha256 of the recipe and of
  `atlas/ontology.json`. The comparison refuses arms whose hashes differ from
  the frozen pair. An edit after freeze is a deviation and re-runs every arm for
  that corpus.
- **Model and host.** The manifest also records the synthesis model id, the
  judge model id and the host. The comparison refuses a corpus whose arms
  differ on any of the three: the closed-book exclusion set is a property of
  one model, so arms run on two models do not share a bank.
  - **Synthesis and the in-loop judge: the Qwen MoE MTP** (operator,
    2026-09-18: iteration speed decides). Its exact id and context are taken
    from the pilot's manifest and written here at ratification.
  - **Proposed, open: the Qwen3.8 27B re-judges the frozen answers once, at the
    gate.** It is the fleet's most accurate model and too slow for the loop. No
    `eval run` flag re-judges a saved run today, and the judge prompt lives in
    Rust, so this needs a `--judge-only <eval.json>` mode over the ONE existing
    judge, never a second judge in python. If it is built, bar 2 is read on the
    27B's verdicts and the MoE's are reported beside them; if it is not, the
    MoE's verdicts stand and bar 4 (people) is the check on the judge.

## Arms — one shape for every corpus

Each arm runs as its own process, three times:
`svrn eval run --bank <b> --synth --isolate --format json`.

(Corrected 2026-09-18, before ratification. The draft command also passed
`--prod-pipeline --limit 30`. Under `--synth` both are dead: the dispatcher
takes the synth branch first and never reads `prod_pipeline`
(`sovereign/crates/sovereign-cli-llm/src/eval_cmd/mod.rs:872-890`), and
`run_bank_synth` is not passed `a.limit` (`mod.rs:883`). The synth lane drives
the full chat pipeline, so it is the production path without the flag.)

| Arm | Ontology corpora | Literary |
|---|---|---|
| **closed-book** | synthesis with no retrieval | same |
| **bare** | `SOVEREIGN_ATLAS_GROUNDING=0 SOVEREIGN_ATOM_ENUM=0 SOVEREIGN_ATOM_ENUM_OVERVIEW=0` | same |
| **bare, deep pool** | bare with `--limit 80` | same |
| **ablation** | generic atlas: the same corpus built with no `[enrichment.ontology]` (the `custom_atlas` v0 path, `corpus-engine/src/recipe_ontology/docs/v0.md`), walk and atom-enum on | the walk over an atlas copy with no `Summary` atoms |
| **full** | custom ontology, walk and atom-enum on (`quality/env-flags.toml:248`, `:304`, `:311`) | the same copy after `svrn enrich summary-atoms` |
| **oracle** | bare, against a corpus holding only the attesting passages | the held-out plot summary as the only context |

- **Deep pool has no knob today (open, found 2026-09-18).** `--limit 80` does
  nothing under `--synth` (above). What reaches synthesis is fixed by two
  constants, `KQ_MERGED_LIMIT = 20` (`prompts.rs:366`) and
  `MAX_KNOWLEDGE_CHARS = 24000` (`formatters.rs:39`). An arm that answers "just
  retrieve more" has to raise both, through one declared env knob
  (`quality/env-flags.toml`, status `experiment`), and its ceiling is the synth
  model's context. The multiplier is fixed at ratification together with the
  model; until then this arm is never-ran, not a null.
- **Closed-book** is a flag over an existing seam, not new machinery:
  `TurnMode::Naked` (`sovereign-contracts/src/types/turn.rs:418-424`) already
  bypasses retrieval, router, gate and atlas, and `collect_turn` already takes
  a `TurnMode` (`runner.rs:1669-1676` passes `Grounded`). No flag in
  `sovereign/crates/sovereign-cli-llm/src/eval_cmd/mod.rs` selects it
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
carry that kind. A kind needs n ≥ 20 pooled questions **after** closed-book
exclusions and kind-rule drops, or its verdict is could-not-judge. Banks hold
15 questions per (corpus, kind) carried, and 20 for K2.

K2 is sized apart because only ANS and fine print carry it. At 10 per cell it
pooled to exactly 20 before any exclusion, so one excluded question made the
kind unjudgeable, and a fine-print corpus that fails its probes left it at 10.
At 20 per cell, ANS alone clears the floor with no exclusions and the pair
survives a 50% drop. The other kinds pool across three corpora or three books
and survive a 55% drop at 15.

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
     independently: a privacy lawyer or consumer-rights advocate, for fine print.
     Full must be preferred in at least 7 of 10.
   - The reader is recruited before any arm runs, because the operator can
     recognise the system's style and that weakens the blinding.

   A metric win paired with a loss on either reading fails.

5. **It replicates (flexibility).** Bar 2 pools, so one strong domain can
   carry a kind. For each claimed kind, full − max(deep-pool bare, ablation) is
   greater than zero in EVERY ontology corpus that carries the kind. It is a
   sign test and is read as one: per-corpus n (15–20) is too small for a
   margin, so this bar can refuse the flexibility claim but cannot by itself
   establish a per-domain effect size. Fewer than two corpora carrying a kind
   is could-not-judge. The scoreboard shows the per-corpus deltas beside the
   pooled one whether or not the bar passes.

**Claim rule.** Claim 3 is made only for kinds that pass bar 5. A kind is claimed only if bars 1, 2 for that kind, 3 and 4 all
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
  as a demo beat under `sovereign/crates/sovereign-desktop/tests/e2e/demo/`.
  (Corrected 2026-09-18: `numismatics.real.spec.ts` never opens the Map, and the
  real config keeps video only on failure.)
  The path is replayed from the study's own data, not computed live.
- **The deck, for pick a card:** for each claimed kind, the frozen bank minus
  closed-book exclusions, each question carrying its measured win count (full
  vs the stronger baseline, over three runs). The presenter states the odds
  for the kind before the draw.
  - On the stage corpus the audience member may first name a service they use;
    the draw is then from that service's questions in the frozen bank, and the
    odds stated are that kind's.
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
  on these banks. It lands before any K3 bank or any Virco bank is written.
- **Stage 1c — fine print on today's path, in parallel with 1a.** The three
  probes first (Corpus notes), then the steps of 1a for K0, K1 and K2. The
  declaration is authored on camera. Acquisition is about 12–16 hours: a
  rate-limited crawl of the ToS;DR API (there is no dump), shallow clones of
  four archive repos, service-name reconciliation and quote re-anchoring.
- **Stage 2 — fine print K3, then Virco**, after 1b, through the same steps as
  1a. Adding K3 questions to a frozen fine-print bank is a new bank file, never
  an edit to the frozen one.
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
