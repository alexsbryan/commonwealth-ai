# Ontology layer: plan and instrument, for review and as the live plan (2026-10-13)

This brief is written for reviewers who have not followed the work. It is also the plan we work from.

- Each item carries a status: DONE, RUNNING, PROPOSED (needs the operator's OK), APPROVED (OK given, not started)
  or OPEN (the operator's decision).
- A status changes only in a commit that cites its evidence: a run directory, a commit or a measurement row.
- Every bar is PROPOSED until the operator sets it, and it must be set before the data it judges exists. Once
  set, it moves into `quality/campaigns/ontology-layer.toml` and is read from the measurement rows, not from this
  prose.

The 2026-10-12 brief is kept as `REVIEW_BRIEF_2026-10-12.md`. Read its sections 2 and 4 for where the layer
stood and what its four repairs taught. This brief covers what changed on 2026-10-13:
- a ceiling test that located the bottleneck;
- a design target that has to scale;
- a generated instrument that can tell designs apart;
- a literary leg.

## 1. The objective and the design target

**The objective** is unchanged (`quality/campaigns/ontology-layer.toml`). A user with a first-principles
understanding of their domain, and competence in the ontology syntax, writes one recipe. They run the CLI's
default commands and get typed, cited records on unlike systems. The records come out very similar whoever wrote
the recipe, and the recipe holds no number measured on data and no mechanism switch.

**The design target** was added by the operator on 2026-10-13:
- The layer scales past 1M tokens with a bounded number of model calls. Calls grow with documents plus clusters,
  never with pairs.
- It organizes what it reads in a RAPTOR-like way, so the reading can be piped and fed at scale.
- It builds a whole-corpus sense of who is who (the cast) before reading any one document. In the operator's
  words: "You can't just take a chapter and reconstruct all the relationships in the book and the plot."
- Its fidelity is judged against a reader that attends to everything at once.

**The literary requirement** was added the same day. The layer must work on a literary source no model has seen:
"characters, plots, themes, relationships all expressible in the ontology system", through the same default
commands.

**The kill condition** is unchanged. The work is not worth continuing if it turns into tuning to datasets, so
every dataset below is an instrument.

## 2. What changed on 2026-10-13

### 2.1 The ceiling test: the pipeline is the bottleneck, not the model

We ran three arms on Ward, one run each (`runs/ceiling/`, local to this host).

| Arm | Records | Notable | Wall time |
|---|---|---|---|
| Our pipeline, local Qwen 35B-A3B | 30 deal records | – | 1,661 s |
| The same pipeline, DeepSeek Flash as the engine | 12 records | one record held four TEP deals; 2,030 calls | 1,984 s |
| One-shot: DeepSeek Flash given the recipe and every message in one call | 19 records | 0 false merges; every quote verbatim | 131 s |

The DeepSeek arm became possible with f74bbffe5: a hosted vendor now answers a forced choice through its
logprobs, with thinking off.

A stronger model inside the same pipeline traded splits for merges. So the pipeline's narrow passes cap the
result, not the model's capacity. The one-shot read is the fidelity reference wherever a corpus fits in one call.
It is never the design, because it does not scale.

This overtakes the 2026-10-12 brief's plan (§6 there: message-level questions asked in several wordings). Two
reasons:
- Its first step went into the statement-against-record forced choice, which turned out to be a knife edge where
  wording changes fit noise.
- The ceiling test shows that better questions inside narrow passes are not where the loss is.

### 2.2 The failures that keep recurring

A week of experiments (2026-10-06 to 10-12) kept hitting the same five failures:
- **(a)** identity is decided before the system knows what the particulars are;
- **(b)** values are read apart from identity, so a change of state reads as a new particular;
- **(c)** linking is greedy and depends on document order;
- the reading unit is wrong (a sentence or a line, where the answer lives in a message or a thread);
- a party is named only in the prose.

Where we stood on 2026-10-12 (brief §2, our recipe):
- Ward: 0 of 47 current deals at the right stage;
- uv: 19 of 44 cases;
- GVC: 50 of 977 event mentions on a record of their type.

### 2.3 Ward cannot separate designs

Ward's scored folders hold 14 deals. The 95% intervals of 6 of 14 and 10 of 14 overlap. On Ward, the same output
scored 6 by quote matching and about 10 by hand, so the scorer's variance equals the effect we want to see.

## 3. The instruments

**I1. Generated mail worlds. DONE (built 2026-10-13; the code is not yet committed).** The code is in
`research/ontology-apps/world/`: `build_world.py`, `compare_runs.py` and `run_world.sh`.

Code plans each world from a seed:
- companies, some of them renamed or acquired partway through;
- people, each with a persona;
- deals, each with stable identity fields and dated changes:
  - stage moves;
  - price corrections ("it's .58, not .65");
  - renegotiation without a stage change;
  - term changes;
  - regress, stall and revive;
  - rolled deals;
  - contact turnover and trader handoffs.

A writer from another model family (MiMo) writes the mail. The planned facts become the key, each with its exact
quote. Every setting is drawn per world from a range and recorded in the world's manifest:
- the number of companies;
- deals per company;
- the mix of reference forms (name, alias, nickname, misspelling, implicit);
- forward, internal and distractor rates;
- noise;
- the amount of hay;
- the timeline;
- lifecycle shapes.

The hay is real Ward mail. Five worlds now exist (`~/.svrnmesh/bench-corpora/world-s1` to `s5`):

| World | Deals | Stage updates | Commitments | Renamed companies |
|---|---|---|---|---|
| s1 | 18 | 71 | 8 | 1 |
| s2 | 12 | 42 | 7 | 3 |
| s3 | 27 | 88 | 21 | 4 |
| s4 | 15 | 55 | 12 | 2 |
| s5 | 21 | 72 | 4 | 4 |
| Total | 93 | 328 | 52 | 14 |

All 380 key quotes are verbatim in their messages; a second checker re-checked this independently of the
generator. Each world shares 4.3-5.7% of its word 5-grams with the other worlds. The first generation's
templating ("apologies for the delay" 47 times) was found by that same measure and removed before these five were
written.

**I2. Generated novellas for the literary leg. PROPOSED** (see §4).

**I3. One scorer per shape. DONE for deals and stages.**
- `score_ward.py` scores both arms with one matcher: the pipeline's atlas, and a one-shot's records through
  `--records`. On Ward, the second form reproduces the one-shot's own matcher exactly: 10 found, the same 4
  omitted, the same split, 0 false merges.
- `compare_runs.py` pairs two runs deal by deal and update by update. It reports a bootstrap interval clustered
  by world, and a breakdown by the key's difficulty tags.
- The scorer still lives in a gitignored working directory; it moves next to the generator when that is
  committed.

**I4. Real data is a check, never a target.**
- The three layer fixtures (mail, issues, news) and Ward stay.
- A real literary anchor joins them (§4).
- uv and GVC gold stay held out.

**I5. The reference: the one-shot reader** (`runs/ceiling/oneshot.py`, DeepSeek Flash).
- Its output cap is the API's own ceiling of 393,216 tokens. At 32k, its reasoning (17-26k tokens) cut the JSON
  short on two of five worlds, and the Ward one-shot had used 30.3k of the 32k.
- It runs on every world that fits in one call. Above that, gold alone judges.

**Discipline.** These rules keep the instrument from becoming the thing that gets tuned:
- Develop on one world, read every change on the other four, and accept on sealed seeds generated only for the
  acceptance run.
- Read every change on the three layer fixtures as well, as ONTOLOGY_METHOD's invariant 5 already requires.
- Five worlds detect a paired difference of about 15 points. That is enough for the architecture decisions in §6
  (0.9× against 0.7× of the reference), and deliberately not enough to see fine tuning.
- About 250 deals (roughly 13 worlds) would resolve 10 points.

## 4. The literary leg. PROPOSED

**Why.** The cast-first thesis is a thesis about novels. A literary corpus is also the most unlike system
available:
- its clock is narrative, not the calendar;
- people go by many names;
- relationships change;
- events have causes;
- themes are interpretive.

It is also the natural 1M tier: a novel series is long, and its cast is the point.

**The recipe.** A blind author writes it from a domain description through the five-question interview, which
tests "whoever wrote the recipe". It declares at least four types:
- character: an entity, one person across given names, surnames, titles, nicknames and epithets;
- relationship: a state between two characters, on the narrative clock;
- event: a plot beat with its participants, place, narrative position and cause;
- theme: a claim, with the scenes that bear on it.

The ontology-v1 literary template
(`ingest/crates/sovereign-recipes/_templates/ontology-v1/literary/recipe.toml`) already declares three things:
character, inner_state on the narrative clock, and reading. If the blind recipe needs something the syntax can't
say, that is recorded as a method gap.

**The source.** A novella generated from a seed, so no model has seen it and the key is exact by construction.
- Code plans:
  - the cast: 15-40 characters, with 2-5 names each;
  - the relationships and their changes: marriage, rivalry, betrayal, reconciliation, death;
  - the plot: events with participants and causes, told out of order, with flashbacks, an unreliable narrator
    and changes of point of view;
  - themes, planted as motifs in named scenes.
- A writer writes the chapters.
- Each planned fact is marked and checked verbatim, as in the worlds.
- Two sizes: about 50k tokens, where the one-shot fits, and about 1M tokens (a series).

**The real anchor.** A public-domain novel with existing character gold. Candidates are BookCoref and LitBank;
their licences still have to be checked. Character names are swapped consistently, so the reader can't answer
from the model's memory. The anchor checks the generator: a design that ranks first on generated novellas must
rank first here too.

**Scoring.**
- Cast: character recall, and alias B³.
- Relationships: F1 over the pair, the type and the narrative interval.
- Events: event identity B³, and participants.
- Themes: whether each planted theme is covered and its scenes cited. The interpretive remainder is read by the
  operator on a sample, because themes are a matter of judgement.

**The path.** The leg runs through the ontology default path, not the old `literary` pipeline. Today, `enrich
init --from-corpus` under a new id silently falls back to `literary` with no ontology. That is a default-path
defect, and fixing it is part of this leg.

## 5. The hypothesis being tested: cast-first (not approved)

Every call is bounded, and calls grow with documents plus clusters.

| Level | What it does | Reuses | Removes |
|---|---|---|---|
| L0 Ingest | Canonical text, segments and threads. Header fields seed people and companies. | extractors, line classes, `mentions::seed` | – |
| L1 Mention inventory | Each document is read whole for the particulars it mentions, each with a quoted span. A GLiNER pass adds recall. Nothing is linked yet. | whole-document reading, `sovereign-gliner`, quote alignment | (a); the wrong unit |
| L2 Cast | Mentions are blocked by declared keys, thread and embedding. Within a block, one call clusters about 9 mentions at a time. Clusters merge hierarchically, guarded for precision. | HDBSCAN or ANN blocking, header records as anchors, a RAPTOR-style bounded hierarchy | (c); the party named only in prose |
| L3 Read with the cast | Each document is read whole, with its retrieved cast entries in context. Each mention links to an entry or to "new". Values are read as time-stamped, cited facts. | the `SeedEntities` prompt shape, the quote-is-its-span check | (a), (b) |
| L4 Refresh and fuse | New particulars go back through L2, and each record's values are folded from its facts. | `write_fold`, the stage protocol | (b) |
| L5 Serve | Records, `atoms_lookup` and atlas-query, with summaries for navigation. | `corpus-engine-atlas-reader/src/records.rs`, corpus-mcp, RAPTOR summaries | – |

## 6. Experiments and proposed bars

| # | Question | Run | Proposed bar | Status |
|---|---|---|---|---|
| X0 | Is the instrument valid enough to judge designs? | Today's arms on the worlds: the one-shot on all five; the local pipeline on world-s4, then the rest; the hay labelled with Ward's own gold (D); cast and value scoring (E) | The worlds rank the arms as Ward does; a human spot check of 30 needle facts finds no key errors | RUNNING |
| X1 | Does a cast help? (the upper bound) | The reader given the gold cast in context, on the worlds, with the local model and with DeepSeek | Records reach at least 0.9× the one-shot. Below 0.7×, a cast alone is not enough and the architecture goes back to the operator | PROPOSED |
| X2 | Can the cast be built? | L1 and L2 against the gold cast: set-wise, pairwise and greedy clustering | Cast recall ≥0.95; alias B³ ≥0.9; set-wise beats greedy by ≥10 points of pairwise F1, with the interval excluding 0 | PROPOSED |
| X3 | Does it hold end to end, and at scale? | L0-L4 at 50k against the reference, then at 1M against gold, with calls and seconds per document | ≥0.85× the reference at 50k; under 10 points lost from 50k to 1M; seconds per document under the operator's ceiling | PROPOSED |
| X4 | Where does model capacity matter? | Local and hosted models swapped one level at a time | descriptive | PROPOSED |
| X5 | Does it work on unlike systems? | The literary leg (I2 and the anchor), the three fixtures, Ward | No regression against today's pipeline on any; the literary anchor within reach of published cast-first results | PROPOSED |
| X6 | Does it hold whoever wrote the recipe? | Blind recipes through the interview, for the world family and for the literary leg | The author gap (B³ between two authors' records, over units both place) at the operator's bar | PROPOSED |

**Checkpoint.** After X1 and X2, if either fails its bar, the architecture goes back to the operator. There is no
third repair round on the same design.

**X0 so far: the one-shot reference, 2026-10-13.** DeepSeek Flash ran once per world, about 2 minutes and 30k
output tokens each (`runs/ceiling/worlds/`).

| Measure | One-shot |
|---|---|
| Gold deals with any record | 89 of 93 |
| Served exactly (one record, nothing else merged in, right current stage) | 66 of 93 (71%) |
| Splits | 4 |
| False merges | 1 |
| Stage updates attached to their own deal | 247 of 328 (75%) |

- The weakest world is s2: 9 of 12 deals served and 19 of 42 updates attached. That world drew the heaviest
  misspelling weight.
- Every one of the one-shot's unmatched records in s2 (16) and s5 (12) is a real Ward deal in the hay, which the
  key does not label yet. D fixes that.
- The local pipeline's baseline on world-s4 is RUNNING (`world-s4/runs/20261013T183858Z`).

## 7. Next, in order (each needs its own OK)

1. **Finish X0's baseline.** World-s4 is RUNNING. The local pipeline on s1, s2, s3 and s5 is PROPOSED, at about
   6 hours of local daemon time.
2. **D. Label the hay with Ward's gold.** About 30 lines in `build_world.py`, and it regenerates only the keys.
   PROPOSED. It does three things:
   - completes the key, so precision becomes measurable;
   - adds real deals to every world;
   - gives every run a real-against-generated split. That split is a partial check of whether the generated
     worlds rank designs the way real data would.
3. **E. Score the cast and values over time.** About 60 lines in `score_ward.py`. PROPOSED.
   - It covers aliases, renames, employers, price corrections and term changes.
   - X1 and X2 need it, and it makes the planted changes count.
4. **I2. The novella generator**, reusing the world generator's planner, writer and checker, plus the real
   anchor. PROPOSED.
5. **X1**, scoped as a change to the read path's product code. PROPOSED.

## 8. Open for the operator

1. The bars in §6, and the seconds-per-document ceiling.
2. Whether hosted models may serve as production legs for some levels (X4), or only as references.
3. Writer families. MiMo is the only writer today, and a second family is needed to rule out writer bias.
4. The literary real anchor, and who writes the blind recipes.
5. Whether uv and GVC gold may be used to validate the instrument, or stay sealed.
6. Who owns the human audit of the key.
7. Committing the world generator and the scorers. They are uncommitted or gitignored today.

## 9. Questions for reviewers

1. **Instrument validity.** Can generated worlds, written by one model family over real hay, rank architectures
   the way real data would? What evidence would convince you? Our candidates:
   - the known-groups check in X0;
   - the real-against-generated split from labelling the hay;
   - a second writer family;
   - the literary anchor.
2. **Alternatives to cast-first.** Which would you test against cast-first in X1-X3? Candidates we have not
   tested:
   - hierarchical summaries alone (RAPTOR);
   - community extraction over an entity graph (GraphRAG);
   - a one-shot read per thread, followed by a merge;
   - iterative refinement over a growing record store.
3. **Resolution.** Is a 15-point resolution the right granularity for architecture decisions? Is it sound to
   withhold the resolution that fine tuning would need, on purpose?
4. **The literary leg.**
   - Which real anchor has character gold and a usable licence?
   - How should themes be scored without a human in every loop?
   - Is a generated novella too clean to stand in for real fiction?
5. **The bars.** Are 0.9× and 0.7× of a one-shot reference, cast recall of 0.95, and at most 10 points lost at 1M
   the right shape of bar for these decisions?
