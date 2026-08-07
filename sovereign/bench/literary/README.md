# literary — atlas extraction against hand-authored goldens

Scores a resolved literary atlas (entities, events, states, relations, claims,
questions) against a golden set of expected AND forbidden atoms. Two banks:

| Bank | Corpus | Installed everywhere? |
|---|---|---|
| `bk-book-1` | `brothers_karamazov` — Brothers Karamazov, Book I | **yes**, since 2026-08-07 |
| `dubliners-3` | `dubliners-3` — three stories from Dubliners | no, optional |

`bk-book-1` is the bank `scripts/sovereign-ci-bench.sh` gates on
(`ENRICHMENT_CORPORA`). `dubliners-3` is deliberately excluded there because it
isn't installed on most boxes.

## Getting the corpus

```bash
svrn corpus install brothers_karamazov
```

That restores a 237 KB prebuilt snapshot from
`svrnmesh/brothers-karamazov-index` — 41 chunks + FTS + vectors + the resolved
atlas — skipping both the ingest and the LLM enrichment. Then:

```bash
svrn bench all --filter literary/bk-book-1
```

To verify the source text's provenance instead of trusting the upload, or to
re-derive it from Project Gutenberg:

```bash
scripts/setup-literary-corpus.sh --verify-only   # re-derive + sha-check
scripts/setup-literary-corpus.sh                 # derive, check, install
```

## Why this bank ships a prebuilt atlas

**An enrichment lane is a regression gate only if every box scores the same
artifact.** The committed baseline under `baselines/bk-book-1/` was minted from
one specific atlas; scoring a locally-extracted atlas against it measures your
model, not a regression, and every box would report a different number.

So the default path ships the reference atlas and the lane diffs against it.
A box that wants to measure **its own** model runs:

```bash
svrn bench all --filter literary/bk-book-1 --rebuild
```

which shells `svrn enrich build brothers_karamazov` to re-extract in place. Its
numbers are about that box's model and should not be expected to match the
committed baseline. That path needs the source document present locally —
run `scripts/setup-literary-corpus.sh` first, which also repairs the
`source_path` a restored snapshot inherits from the publisher's `$HOME`.

## Why the corpus is Book I and not the whole novel

The golden is built around **leakage anti-tests** — assertions that content
from *later* books must not appear:

- `forbidden_event_atoms` "Mitya's trial": *"The trial happens in Book XII.
  Book I makes no reference to it. Surfacing 'the trial' from Book I would mean
  the extractor leaked context from outside the section."*
- `forbidden_relation_atoms` Alyosha/Lise/Grushenka/Katerina: *"Book I
  introduces no female love interest for Alyosha."*

Enrich the full novel and each of those inverts — the extractor gets penalised
for correctly reading the text. The scoping has to live in the **source
document**, not in a flag, because the weekly `--rebuild` tier shells
`enrich build <corpus_id>` with no chapter selection
(`sovereign-cli-llm/src/bench_cmd/all.rs::rebuild_corpus`).

## History

Before 2026-08-07 this bank's corpus existed on exactly one machine, enriched
from a personal `~/Downloads/Brothers_Karamazov.txt`. Everywhere else the lane
found no corpus and reported `1 stale`, which `sovereign-ci-bench.sh` grades
`PASS(warn:setup)` — clearing the HARD gate. The lane was not failing on other
boxes; it was **measuring nothing and reporting green**.

The atlas that machine held also did not match its own golden: it covered
Book I chapters I-IV plus a Book II chapter, and omitted chapter V ("Elders").
Re-extracting over the correct 5 chapters moved `person_atoms` 7 → 10/10,
`concept_atoms` 0 → 1, `state_atoms` 1 → 2, with zero forbidden hits.

Two `expected_event_atoms` entries were also matching on inflected forms
(`"died"`, `"elopement"`) where the file's own marriage entry declares
stem-matching as policy (*"Match 'marri' covers married/marries/marriage/
marrying"*). An extractor narrating in the present tense ("Adelaïda **dies** in
Petersburg") scored zero on a semantically correct extraction. Those two are
now stems (`"die"`, `"elope"`). The three event misses that remain
(Fyodor's second marriage, Sofya's death, Alyosha entering the monastery) are
genuine extraction gaps and are still scored as misses.
