# Calibrated decisions — the data strategy

Written 2026-09-23, against `main` @ 22a41304c. This is a plan, and no data has
been collected under it. Volume targets are proposals, and each is revised once
its yield check (D1) has been measured.

Sibling of `CALIBRATED_DECISIONS_PREREG_20260922.md` and
`CALIBRATED_DECISIONS_MODEL_20260923.md`. The model doc says what the decision
model is for: one model, the same weights on every client, judged on this
stack's decisions rather than on public leaderboards. That goal sets the
priorities here. Data that teaches this stack's distributions, and banks that
measure them per genre, come before public benchmark coverage. This doc says what it learns from and what it is judged against.
Provenance rules are `VERIFIER_V0.md` §3's (Constructed > Mechanical >
StrongModelJudged; never train on agreement alone), and every rule there holds
here too.

Licences below were read from Hugging Face API tags and repository licence
files on 2026-09-23. A licence marked *unverified* is not used until someone
reads its card.

## What the verifier arc already taught about data

From `research/verifier-v0/findings/`:

- **Held-out banks are the binding constraint, not training volume.** Every
  positive result in `VERIFICATION_SCALING_AXES.md` §13–21 died within an hour
  of being written, and every negative held. The instrument's noise exceeded
  the effects being chased: 7.2% per-fact resampling noise (§12), ±1 fact of
  run-to-run drift (§17), and 15–21 items per class (§21). A bank too small to
  see the effect makes any amount of training data unjudgeable.
- **Volume follows substrate.** Stream B cases ≈ 2 × claims that pass the
  grounded gate (`M2_STREAM_B_VOLUME.md` §2). `--n` and the corruption kinds
  change the mix, never the total.
- **The substrate is narrow.** 89% of Stream B is SEP, one encyclopedia
  (`M2_STREAM_B_VOLUME.md` §1). §19 showed that a detector's ranking reverses
  when document granularity changes, from SEP's large articles to a code
  corpus's small files. A model calibrated on one genre is calibrated to that
  genre.
- **Statistics that pass while the mechanism does nothing.** §21 counts four:
  an absolute bar with no control, a bar uninformative by construction,
  accuracy on imbalanced data, and a threshold fit on the held-out set. Every
  bank here prints its confusion matrix, and every rule is frozen before its
  held-out set is touched.

## Rules

1. **Evaluation banks are sized before any training row is collected.** Each
   decision's held-out bank is sized from the smallest effect that would change
   a decision, against the measured noise. As a floor, ±5 points at 95% on a
   rate near 50% needs about 380 items per class.
2. **Held-out means never trained on, and frozen first.** The same applies to
   calibration banks, the gate's holdout, and every benchmark test split below.
   `research/verifier-v0/scripts/contamination_pass.py` is extended to every
   new evaluation set, and its collision count goes on the eval card.
3. **No genre above 25% of the constructed substrate.** Report calibration per
   corpus, never pooled alone.
4. **Every label carries its provenance tier and its source.** Labels that
   depend on a model (self-knowledge) also carry that model's digest, and are
   relabelled when that model changes.
5. **Letters are permuted uniformly in training data.** This keeps the model
   from learning position bias that the prereg's rotation check would then
   have to catch. N-way items match production window sizes, up to 13 options.
6. **Licence tier decides the use** (below). A set whose licence cannot be
   verified is not used.

## Teacher roles

A stronger model's part in this data is mostly to label, rarely to write. The
three uses, best first:

1. **Soft labels on decision items already constructed.** The teacher reads an
   S1 or S2 item's state and question and outputs a distribution over the
   options. The student trains on the construction label, which is the anchor,
   plus the teacher's distribution, which adds graded uncertainty ("B, but C is
   close") that a 0/1 label cannot carry. Two conditions:
   - the teacher is calibrated first (Platt scaling on the fit split), and the
     calibrated distribution is what is distilled; otherwise the student
     inherits the teacher's miscalibration;
   - calibration is always scored against construction labels, never against
     the teacher. Teacher distributions are StrongModelJudged, the lowest
     provenance tier.
2. **Top-K logits over natural text the teacher did not write** (S6). This
   improves the student's language model in general. It helps decisions only
   indirectly, which is what the model doc's F2 tests.
3. **Text the teacher writes** (reasoning traces, synthetic claims). This is the
   weakest use: StrongModelJudged provenance, and decision-format rows do not
   need it. Stream B uses a teacher only where ORPO needs a written chosen
   response.

## Licence tiers

| Tier | Use | Sets |
|---|---|---|
| **Train** | training and evaluation | permissive or share-alike: VitaminC, HotpotQA, MuSiQue, 2WikiMultihopQA, QASPER, SQuAD 1.1/2.0, NQ, TyDi QA, WiCE, RAGTruth (MIT repo), RAGBench, HalluGuard-76k, PopQA (MIT repo), EntityQuestions, SelfAware, Mr. TyDi, MMLU-Redux 2.0, MMLU-Pro, FreshQA. For S6: OpenThoughts3-1.2M (Apache-2.0), OpenR1-Math-220k (Apache-2.0; traces from DeepSeek-R1, MIT), and outputs of our own Qwen3.8-27B teacher (Apache-2.0, read from its config) |
| **Train, with care** | training, with the caveat recorded in the run manifest | C2D/D2C MiniCheck, RAGBench and UltraFeedback Binarized (MIT, but labels or responses generated by GPT-4-class models; check the provider's terms before training a competing model on them). FineWeb-Edu (ODC-By; the page text itself is of unknown copyright). Share-alike sets: derived data we redistribute stays share-alike |
| **Eval only** | measurement, never training | LLM-AggreFact (CC BY-ND) |
| **Operator call** | as ruled; S6 sets out the options for Qwen3.8-Max-50k | Qwen3.8-Max-50k's traces (licence "other"; Alibaba Cloud terms on hosted-model outputs; 24.3% of rows from non-commercial sources). The rest of this row: internal measurement at most, if the operator rules non-commercial research use covers it: FaithBench, HaluBench, CRAG, RGB, FaithEval, AbstentionBench, MS MARCO / TREC DL (and BEIR's MS MARCO subset), ANLI and DocNLI (ANLI-derived), SciFact abstracts, NewsQA, Qualifire grounding-benchmark |
| **Unverified** | not used until its card is read | PRM800K (OpenAI's release; the Hugging Face mirror's card is to be read), TriviaQA data, MultiRC, MuSiQue's Hugging Face mirrors (use the CC BY 4.0 repo directly), the 2026 LettuceDetect span set |

FaithBench is on the verifier's eval card today (`VERIFIER_V0.md` §1). Its
CC BY-NC-SA licence puts it in the operator-call row, and that ruling covers
both documents.

## The six sources

### S1. Public labelled sets, converted to decision format

These are converted by a script, not ingested through a recipe. The `parquet`
extractor keeps one content column and drops structured label fields.

| Decision | Train | Conversion |
|---|---|---|
| Claim support (A supports / B refutes / C not enough) | VitaminC (370k), RAGTruth (15k), HalluGuard-76k (verdict only), WiCE (3.5k) | one passage plus claim, three-way. VitaminC's contrastive pairs are the main value: the same claim against minimally different evidence |
| Which passage supports (A…N, or none) | HotpotQA (~90k), 2WikiMultihopQA (~167k), MuSiQue (20k answerable), QASPER (~5k Qs) | the gold and distractor paragraphs become the options; "none" is built by removing the gold paragraph |
| Evidence sufficiency (A/B) | MuSiQue-Full (~40k, paired answerable/unanswerable), SQuAD 2.0 (130k), NQ null answers | the pairing in MuSiQue-Full is the useful part: the same question with and without its evidence |
| Span pointing (which sentence) | SQuAD 1.1/2.0, NQ short answers, TyDi GoldP | the answer's character span is mapped to a sentence index, and that sentence becomes the correct option |
| Relevance (A/B per passage) | HotpotQA and MuSiQue distractors, Mr. TyDi | MS MARCO is excluded by licence |

That is roughly a million labelled rows across four decisions before anything
is constructed locally. It is also mostly Wikipedia, which is why S2 exists.

Every one of these is almost certainly in Qwen's pretraining data. That does
not hurt training signal, but it means none of their public test splits is a
clean evaluation (see "Evaluation banks").

### S2. Constructed labels over recipe-installed corpora (Stream B, widened)

`svrn bench verifier harvest --corpus <id>` opens any installed corpus by id
(`sovereign-cli-llm/src/bench_cmd/verifier.rs:199`), and
`generate_cases` (`sovereign-eval/src/flywheel/generators/adversarial.rs:435`)
works on its output. So any corpus a recipe installs feeds Stream B without new
code. Corpora with an atlas also supply the entity clusters that entity-swap
corruptions need.

Two changes to Stream B:

1. **More decision types from the same harvest.** Each harvested window already
   knows its claim's source chunk and span, so the same export can emit
   which-passage items (the source chunk among the window's other chunks, or
   removed for "none"), sufficiency items (the window with and without the
   source chunk), and span items. Claim-support cases stay as they are.
2. **No teacher for the decision format.** The 22.7 h Stream B run was teacher
   labelling for ORPO's chosen responses (`M2_STREAM_B_VOLUME.md` §8). A
   decision row is a state, a question, and one label token fixed by
   construction, so it needs no teacher. The cost left is harvesting, which
   extracts claims through the daemon and has not been priced on its own. The
   D1 yield check measures it.

**The corpus matrix.** A 25% cap per genre means at least five genres. Existing
recipes cover most of them:

| Genre | Existing recipe | New recipe (TOML only) |
|---|---|---|
| Encyclopedic | `sep`, `wikipedia`, `wikipedia-simple` | — |
| Legal | `us-code`, `scotus-opinions`, `olc-opinions`, `federal-register-presidential` | Common Pile CAP caselaw subset |
| Financial / regulatory | `sec-filings-company`, `proxy-company` | `eloukas/edgar-corpus` (public domain) |
| Government reports | `crs_reports` | `launch/gov_report` (CC BY 4.0) |
| Technical Q&A | `stackexchange` (CC BY-SA) | — |
| Scientific | `openalex` (abstracts only) | Common Pile PMC commercial-use subset, excluding its CC BY-ND articles, since corruption edits are adaptations |
| Code and docs | `codebase` | — |
| Literary | `gutenberg-work`, `federalist-starter`, `brothers-karamazov-book-1` | — |
| OCR-garbled | `uap-blue-book-scans` | — |
| Email | `enron-sample` | — (PII: operator call before it enters training) |
| Tabular atoms | `sf-assessor-roll` | — |

New recipes copy `ingest/crates/sovereign-recipes/crs_reports/recipe.toml` (about 25 lines:
the `huggingface_dataset` acquirer with the `parquet` extractor). The Hugging
Face acquirer sends no `HF_TOKEN`, so gated sets go through `bulk_download`,
which does.

Private corpora (obsidian, conversation) never enter this stream. Their
evaluation role is S5's.

### S3. Manufactured traffic, labelled by outcome

Decisions that no construction can label (whether a retrieval step
contributes, corpus choice, early exit) need the pipeline's own outcomes.
Traffic from a single operator will not reach training scale, and the product
has no phone-home by design (`VERIFIER_V0.md` §6.1), so the traffic is
manufactured:

- Install the `wikipedia` recipe. Run the full pipeline offline, every step on,
  over questions from HotpotQA, MuSiQue, PopQA and EntityQuestions, whose
  answers and source articles are known.
- Log every decision the pipeline makes, the step that introduced each chunk
  (R2), per-step time (R1), and the gate's per-claim support.
- Evaluate policies offline over the logged runs, as §16 did from a single
  k=80 pass. One fully instrumented run labels every policy over the same
  decisions.

It also fixes the uncovered-regime bank. §16.1 named acquisition routing on a
mixed corpus as the test that matters, and §18–21 ran it at 15–41 items per
class. With known source articles, the ablated regime is built at scale:
exclude each question's source articles from the installed index. §18 found
that an encyclopedia leaks through cross-references, so the ablated label is
"source removed", not "fact absent". It is reported in its own column.

Cost is the open number. At about a minute per turn with the gate on, 10k turns
is about a week of Halo time. Price it from a 100-turn pilot before committing.

### S4. Self-knowledge, labelled by each primary's own outcome

Run each primary that clients ship (Qwen3.5-4B, 9B and 35B-A3B, per
`models.toml`) directly, with no retrieval, over PopQA (14k, stratified by
entity popularity) and EntityQuestions (~176k, long tail). Label each item by
whether that primary's answer was right under the existing bench scorer. That
is Track A's ground truth at scale, three times.

The labels describe that model's knowledge, not the world's, so there are
three label sets, one per primary. Each label carries the generator's digest,
and a generator change invalidates its set. Three primaries triple the prefill
cost; the 4B and 9B are cheaper per item than the 35B-A3B, so the total is
under three times the 35B-A3B's cost.
TriviaQA and NQ-open are left out: they are the most-pretrained sets, and
PopQA's popularity strata are what make the calibration curve informative.

### Probe data: activations, not new labels

Probes appear in two places in the model doc: the self-assessment probe on each
primary (the one per-profile piece of the design), and F1's arm (d), a probe on
the embedder. Both train on labels from S1–S4. What they need besides the
labels is the model's last-position hidden state for each item, cached once per
model digest. That changes three things:

- **Cost is prefill, not training.** Each item costs one prefill of its state
  and question. On the development Halo the 35B-A3B prefills about 780 tokens
  per second, about 20k items a day at 3k tokens per item. The 4B, 9B and
  embedder are faster. Fitting a probe on cached activations takes minutes.
  Items that share a state share its prefill.
- **Rule 4 extends to activations.** A cached activation set is stamped with
  its model's digest and is invalid when that model changes. Evidence-bound
  labels survive a change of model; the activations do not.
- **Fewer rows are needed.** A linear head over a frozen representation
  needs thousands of items per decision, not the hundreds of thousands a
  fine-tune uses. So the evaluation banks bind even harder: a probe fits
  quickly, and it overfits a small fit split just as quickly.

Private corpora can feed probe training locally. The activations and the probe
never leave the machine, which the model-training streams cannot say.

### S5. The journal, later

The grounding journal (`VERIFIER_V0.md` §6.1) is Stream C. It joins at v1 as
the spec already says, and it is the only source of real-traffic distribution.
The local operator can also use it to refit τ per mesh (§6.1's local
self-calibration). Its current volume is unknown; `svrn journal grounding
stats` reads it.

### Not a source: LLM autorater labels

The Sufficient Context work (Joren et al.) released no labels, only the
autorater prompt in the paper. Reproducing its rater would give
StrongModelJudged labels, the lowest provenance tier. Construction by removal
(S1's MuSiQue-Full pairs, S2's with-and-without windows) labels the same
decision mechanically, so the autorater is not used for training. It can be an
evaluation comparator.

### S6. The distillation corpus (for F2)

F2 in the model doc asks whether distilling the base helps decisions. Its data
is mostly open, and the one artifact generated here is the teacher's top-K
logits (role 2). No public set of Qwen3.8-27B logits at this scale was found.

**The pilot mix, 5M tokens**, scaled from a 50M target of:

| Share | Tokens at 50M | Source | Why |
|---|---|---|---|
| 40% | 20M | public recipe-installed corpora from S2's matrix, excluding the held-out genre | the text the decision model will actually read; logits over it adapt the student to this stack's genres at no extra cost |
| 20% | 10M | FineWeb-Edu, a streamed random slice (the full set is 5.84 TB) | general language |
| 20% | 10M | OpenThoughts3-1.2M and OpenR1-Math-220k | reasoning |
| 20% | 10M | S1 and S2 decision items, rendered as text | decision-shaped states |

This departs from a general-distillation mix on purpose. A generic recipe would
put most of the budget on web text, reasoning, code and chat. The goal here is
this stack's decisions, so most of it goes to this stack's text.

**The SFT warm-up**, for F2's SFT arms: 25–50k traces subsampled from
OpenThoughts3-1.2M (1.2M rows: 850k maths, 250k code, 100k science) and
OpenR1-Math-220k (about 94k problems in the default split, several verified
traces each). No teacher generation is needed for the pilot.

**The readability probes**, F2's secondary readout: UltraFeedback Binarized
(about 61k chosen/rejected pairs) and PRM800K (about 800k human step-level
correctness labels), once its card is read. The same frozen linear probe runs
on every checkpoint. The primary readout remains this stack's banks.

**Storage.** Top-64 logits with their indices, plus the out-of-top-K mass, are
about 384 bytes per token: about 2 GB at 5M tokens and about 20 GB at 50M.

**Qwen3.8-Max-50k** (`r0b0tlab/qwen3.8-max-distillation-50k`). 49,772 traces
from the hosted Qwen3.8-Max: 16,597 maths, 14,057 code, 11,690 reasoning,
7,302 instruction, 126 tool use. It deserves more than a dismissal. It comes
from the strongest Qwen, one we cannot run. It spans the same four domains as
S6's reasoning slice. And it carries a per-row `source` field.

What its card says (read 2026-09-23):

- Licence `other`, "not cleared for unrestricted commercial/model-training
  use". It cites Alibaba Cloud's Product Terms §4.48(d)(v), which restrict
  using Model Studio, its hosted models, or their outputs to train or develop
  products or services that compete with Alibaba Cloud or its affiliates,
  unless expressly authorised.
- At least 12,106 rows (24.3%) come from non-commercial sources: Evol-Instruct-Code
  (CC BY-NC-SA 4.0) and SciQ (CC BY-NC 3.0). The rest come from sources such as
  MetaMathQA, NuminaMath-CoT, OrcaMath, GSM8K, MATH, CodeAlpaca, MBPP,
  HumanEval, CommonsenseQA, ARC, OpenBookQA, Tulu 3, Dolly and IFEval.
- Answers were not independently verified. The quality score (mean 8.59 of 10)
  is described as "a heuristic, not a factual-correctness judgment".
- The card does not restrict use to research.

Whether a local-first product "competes with Alibaba Cloud" is a legal
question, not a technical one, and it is the operator's. Three uses short of
training on the traces are available whatever the ruling:

1. **Its prompts, re-answered by our own teacher.** Filter by `source` to the
   permissively licensed upstreams, drop the Max outputs, and run our
   Qwen3.8-27B over the prompts. The prompts' licence follows their upstream,
   not Alibaba's terms. This keeps the curation and discards the encumbered
   part.
2. **A teacher-strength comparison.** On the same prompts, compare Max's traces
   against our teacher's: agreement, and answer correctness where it can be
   checked. It prices how much a stronger teacher would be worth before anyone
   pays for one.
3. **A held-out comparison set** for F2's secondary readout.

If the operator clears training on the traces, F2 adds the SFT-Max arm on the
non-NC rows only. It answers whether a stronger teacher's text beats our
teacher's logits at a fraction of the cost.

## Evaluation banks

Held out from all training, frozen before any model is scored on them, and
sized by rule 1.

| Decision | Public | Own |
|---|---|---|
| Claim support | LLM-AggreFact (eval only), RAGTruth test | Stream B held-out split, per corpus |
| Which passage | HotpotQA and MuSiQue hidden tests (leaderboard submission) | S2 held-out, per corpus |
| Sufficiency | MuSiQue-Full hidden test, SQuAD 2.0 hidden test | S3's ablated regime, at scale |
| Span | SQuAD 2.0 hidden test | S2 held-out |
| Self-knowledge | SimpleQA-Verified (MIT, 1k items, label noise cleaned), FreshQA (refreshed weekly, so it resists contamination) | S4 held-out strata; Track A's bank |
| Off-distribution (the FaithBench rule) | FaithBench, if the operator rules it admissible | one whole genre from the corpus matrix held out entirely: never harvested for training, only for evaluation |

Holding out a whole genre is what makes "calibrated" mean more than
"calibrated on genres it trained on". Its absence is how §19 happened.

**Public benchmarks are the off-distribution check, not the target.** The
decision model is judged on this stack's banks. Public test splits confirm it
does not collapse outside them. It is not tuned to top them.

**ECE never stands alone.** A near-chance model that spreads its probability
evenly over its options scores near-zero ECE while being useless. Every
comparison reports log loss and Brier with its resolution term, with ECE beside
them.

**Deferred: a general-calibration comparison with Jev.** An earlier draft of
this doc made MMLU-Redux 2.0 and MMLU-Pro "the Jev comparison". It was removed
when the goal was set to one model on every client, and the reasoning is kept
here for whoever reopens it:

- MMLU-Pro measures stored knowledge. No model that loads on every client
  approaches Jev's reported 84.6%, so a small model's number beside Jev's
  compares parameter counts, not calibration. A knowledge benchmark could only
  be run on the primaries, which are not the decision model.
- FINAL-Bench is the one comparison that could still matter: Jev's figure
  there (0.7350 verifier AUC) is reported, as is 0.7289 for a frozen
  Qwen3.5-27B with a probe. Both come from an outside analysis and are
  unverified. It is worth running if it measures a decision this stack needs,
  and its licence and items are read at D0 either way.

## Order

| | Deliverable | Gate |
|---|---|---|
| **D0** | Licence ledger committed beside this file (each set, tier, card URL, date read). Converters for S1's train sets. The contamination pass extended to every evaluation set. Evaluation banks sized and frozen | every set in use has a verified tier; every bank meets rule 1 or is marked too small to judge |
| **D1** | Yield check: about 150 windows harvested from each new corpus, as `M2_STREAM_B_VOLUME.md` §3 recommends | grounded-gate pass rate per corpus, and harvest seconds per window, both measured |
| **D2** | The widened Stream B export (which-passage, sufficiency, span) over the corpus matrix, capped at 25% per genre | every generated case passes the fairness contract (`question.rs:191-233`); the held-out genre is untouched |
| **D3** | S4 self-knowledge labels for each shipped primary, and cached activations for the self-assessment probes and F1's embedder arm | the model's digest is stamped on every label and every activation set |
| **D4** | A 100-turn S3 pilot | cost per turn measured; a go or no-go on the full run from that number |
| **D5** | The full S3 run, if D4 says go | — |
| **D6** | S6: the 5M pilot mix assembled, the teacher's top-K logits and out-of-top-K mass generated, the SFT subsample drawn, and Qwen3.8-Max-50k's prompts filtered by `source` | the held-out genre is absent from the mix; every file carries the teacher's digest |

D0 and D1 are cheap and run in parallel. Nothing trains before D0's banks are
frozen.

## Open for the operator

1. Whether non-commercial evaluation sets (the operator-call row, FaithBench
   included) are admissible for internal measurement.
2. Whether Enron enters training at all, given its PII.
3. Whether GPT-4-derived training rows (C2D/D2C, RAGBench) are acceptable
   under the provider's terms.
4. Which genre is held out entirely as the off-distribution set. Legal is the
   strongest candidate: it is large, public, far from encyclopedic prose, and
   the recipes already exist.
5. Volume targets per decision, fixed after D1's yield numbers, not before.
6. Whether Qwen3.8-Max-50k's traces may be used for training (S6). Its prompts
   and the comparison uses do not wait on this.
