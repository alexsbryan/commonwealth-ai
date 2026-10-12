# Cite probe (M1 of the reader's second pass): pre-registration, 2026-10-12, before any unit is embedded

Question. Can code alone, with embeddings and no model call, find the unit of a document that shows a claim as
often as Locate's per-line model ranking does (19.7 calls per Ward document), and can embeddings mark furniture
without touching a line gold cites? This decides S0 (sentence units, furniture) and S3 (citation by ranking, then
one forced choice among the top three) of the second-pass reader. No chat call is made.

Baseline (Locate, from the logged whole-text legs, reproduced before this file: scratchpad top3_baseline.py). Per
item, the asked lines of the document ranked by the logged p of the item's kind; a hit when a gold line of that
item is in the top three, ties broken uniformly (expected value). The trace logs p to two decimals, so ties count.
- Ward, item = (document, stage_update): 26 items, .923 (chance .261). commitment: 5 items, 1.000 (reported only).
- uv, item = (document, case_state): 125 items, .944 (chance .822); 95 documents ask three lines or fewer, so the
  informative set is the 30 documents asking more than three: .765 (chance .257).
- GVC, item = (document, gold happening type folded through cdcr/fold_values.json), ranked by p(report): 289
  items, .717 (chance .460); bag_of_events .159 (chance .335).

Instrument (the production embedder, through the daemon's /v1/embeddings, model qwen-embedding-0.6b, resident).
- G1: on 20 sampled Ward chunks, cosine of the endpoint's vector of the chunk's content to the stored chunk vector
  at least .99 (a first look at 6 gave .9955-.9989). uv and GVC store zero vectors for 3072/3152 and 512/743
  chunks (installs copied from runs/scaffold-3sys), so G1 is judged on Ward only and said so.
- G2: 50 units embedded one per request against their batched vectors: cosine at least .9999 each.
A failed gate makes every bar could-not-judge.

Units (S0). The document's chunks joined in order (the reader's body). A paragraph is a run of non-blank lines; its
lines are joined with a space and split where '.', '?' or '!' meets whitespace (the corpus splitter's own rule,
corpus-engine chunkers/sentence.rs:82). A unit maps to the numbered lines it touches. A unit is a candidate when one
of its lines is one Locate asks (quoted passages stay carry, as now). A unit is gold for an item when it touches a
gold line of the item. No rule names a header, a signature or a token.

Furniture (S0). Every unit of every document in the three corpora is embedded once. A unit is furniture when a
unit with cosine at least .95 to it occurs in documents of at least 5 distinct threads other than its own (thread:
Ward thread_id, uv thread, GVC none declared, so each document is its own). Both numbers are fixed here, code
constants, never recipe numbers, and are not tuned after reading.

Queries (S3). The production query instruction (sovereign-contracts embed_quirks.rs, "Instruct: Given a search
query, retrieve relevant passages that answer the query\nQuery: ") over:
- Qv: the item's value as the recipe declares it: Ward stage and uv state, the value's segment of the field's
  description ("won: confirmed, executed or awarded to Enron"); GVC, the declared happening kind value ("a death or
  a killing"); Ward commitment has no closed field, so the kind's description. The value is gold, so Qv measures S3
  given S2 chose right; S2's own accuracy is M2's question.
- Qk: the kind's description alone (value-blind, like Locate's ranking). Reported, no bar.
Units are embedded plain. The particular's words are not in the query (About's output is not joined here).

Arms. U = sentence units, furniture excluded, Qv (the design). Reported beside it, no bar: UF = units with
furniture kept; L = Locate's own asked lines as candidates, Qv (the ranker alone); and each with Qk.

Bars, fixed now.
1. U's top-three hit rate at or above Locate's on every system: Ward stage_update at least 24 of 26; uv's
   informative 30 at least 23 (.765), and all 125 at least .944; GVC's 289 at least .717.
2. Furniture safety: gold units marked furniture at most 1% of gold units per system.
3. Reported with the bars, never hidden: chance per arm (top three of n units), and the mean number of lines the
   top three units cover against Locate's three, so a gain bought by longer units is visible.
Pass 1 and 2: S3 replaces Locate's ranking for these kinds and goes to M3 on sentence units. Miss: not tuned; misses
are classed by shape (gold ranked 4-6; gold ranked far; gold excluded as furniture) and the next tool is chosen by
that class. Furniture is also read by eye on the seat sample (research/ontology-apps/seat/), listed in the output.

## Amendment 1 (2026-10-12, before any unit is embedded): Ward items carry their value

Five of Ward's 31 (document, kind) items hold gold lines of two stage values, so Qv has no single value there. Ward
stage_update items become (document, stage_update, value), each with its own gold lines: 32 items. Locate's
baseline at that grain, recomputed from the trace (its ranking is value-blind, by p(stage_update)): 24 of 32, .750,
chance .215. Bar 1 for Ward reads: U at least 24 of 32. The 26 (document, kind) items stay reported, U counting a
hit when any of the item's values hits, which is Locate's rule.

## Part 2 (M2, 2026-10-12, before any chat call): citing an act by one forced choice over sentence units

Question. Part 1 found embeddings cannot rank an act's sentence (Ward 22 of 32, uv 11 of 30). Given the act's value,
can the model, in one forced choice over the document's sentence units, cite a gold unit at least as often as the
reader's own Locate output does today (a gold line located as its kind), with one to three calls per item instead of
one per asked line? The value is gold, so this isolates citation from the act question (S2); M3 joins them.

Items. Part 1's act items: Ward (document, stage_update, value) 32 and commitment 5; uv (document, case_state, state)
125. Informative: more than three candidate units (Ward 36 of 37, uv 33 of 125).

Baselines (from the logged legs, scratchpad top1_baseline.py, before this section). Reader output, a gold line
located as its kind: Ward stage_update 14 of 32 (.438), commitment 5 of 5; uv 88 of 125 (.704), informative 20 of
33 (.606). Locate's top-1 by p(kind), ties uniform: Ward 12.5 of 32, uv 103.6 of 125, informative 15.6 of 33.
Chance for one unit: Ward .086, uv informative .113.

The question. The declared facts (prefill.rs, as Locate shows them), then the document as its sentence units in
order, one per line, the candidates marked [A] to [Y] (at most 25 per page, in document order) and the rest
(quoted passages) shown unmarked, then the fact, then "which marked sentence shows it, or 0". The fact is written
from the recipe's own words only: "a <subject type> this document is about has <field> '<value>': <the value's
segment of the field description>"; a kind with no closed field states its description. Temperature 0, thinking off,
labels A-Y and 0. Documents with more than 25 candidates are asked page by page; when two or more pages point to a
unit, one final question marks only the pages' picks. A hit is a cited unit touching a gold line of the item.

Wordings. W1 and W2 differ in the system prompt and the question sentence only (W2 a paraphrase); the facts, the
document, the marks and the fact are byte-identical. The wording floor per system is |W1 - W2| on the bar's
measure; a gain over the baseline counts only beyond it.

Gate. 20 items re-asked under W1 outside the cache: the cited unit equals the first answer's on at least 19.

Bars, fixed now.
1. W1 and W2 both at or above the reader-output baseline beyond the floor: Ward stage_update at least 14 of 32,
   uv at least 88 of 125, uv informative at least 20 of 33.
2. Calls per item at most 3 on average per wording, reported with the per-line Locate calls it would replace.
Reported, no bar: agreement between W1 and W2 on the cited unit, and the hit rate where they agree and where they
differ; the share answered 0; the mean lines a cited unit covers (against one line for Locate).
Pass: act-kind citation in M3 is this question, and per-line Locate retires for act kinds. Miss: not tuned; misses
are classed (cited a neighbour of the gold unit; cited 0; cited a far unit) and the next tool chosen by class.

## Amendment 2 (2026-10-12, before any chat call): the same question over physical lines

ec's act pass (in build) cites by one forced choice over the document's lines. Arm L asks M2's question with the
asked lines as the marked candidates (Locate's own set, paged by 25), everything else identical, W1 and W2. Bars 1-2
apply to L as to the unit arm; the two are compared on the same items, a difference read only beyond the larger of
their wording floors. A hit for L is a cited gold line.

## Amendment 3 (2026-10-12, after M2 was read, before any no-none question is asked): citation without none

M2's misses are one class: the answer is 0 (Ward 26-30 of 32, uv 83-92 of 125), 61 of them on one-sentence uv
documents whose only sentence is the act. The act's presence is decided upstream (S2); the citation only points.

- Arm N: M2's unit arm with one change, the 0 label and its clause removed from the system prompt and the question
  (W1N, W2N); labels A-Y only. A document with one candidate is cited by code with no call. Paging as in M2; the
  final question runs whenever two or more pages were asked.
- Bars: M2's bar 1 and 2 unchanged (Ward stage at least 14 of 32, uv at least 88 of 125, uv informative at least 20
  of 33, beyond the floor |W1N - W2N|; at most 3 calls per item).
- Prediction, from M2's diagnostic (renormalised with 0 set aside), written so the read is honest: Ward about 13 of
  32 (at the bar), uv overall about 98-100 (passes), uv informative about 11-12 (misses; 29 of its 33 items are
  'reported' on an issue body, gold quoting one of the sentences that describe the problem, counted and listed).
