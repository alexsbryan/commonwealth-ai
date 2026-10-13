# Ontology layer: the lean plan (2026-10-13)

The longer version, with instruments, validity checks and bars, is in git at 1977b2eac. It is deferred until
something works.

## Goal

One recipe and the default commands turn a corpus into typed, cited records. It must work on mail and on a novel,
and scale past 1M tokens. The yardstick is a model that reads the whole corpus at once (the "one-shot"). On five
generated mail worlds with an answer key it gets 66 of 93 deals right. The worlds are in
`research/ontology-apps/world/` and the scorer is `score_ward.py`.

## What we could be wrong about

The percentages are my estimates, not measurements. Each row says what being wrong would cost, and the cheapest
check that would tell us.

| Belief | How sure | Cost if wrong | Cheapest check |
|---|---|---|---|
| The pipeline's narrow passes cap quality, not the model | ~75% (one run each, on Ward's 14 deals) | We redesign when a bigger model was the fix | world-s4: pipeline against the one-shot (running); step 4 |
| Cast-first is the right architecture | ~40% | Weeks of cast machinery for a design that loses | Race it against three alternatives on the same world (step 2) |
| The local model is good enough for the winning design | ~50% | Hosted legs or a bigger local model: a product and cost decision | The winner on local against hosted (step 4) |
| Generated mail ranks designs the way real mail does | ~60% | We tune to MiMo's prose | The top two designs on Ward too; the order must agree (step 2) |
| What wins on mail wins on a novel | ~50% | Two designs, or a redesign later | A small novella in the race before choosing (step 3) |
| The ontology syntax can say what a novel needs (relationships, events, themes) | ~60% | A gap in the core, bigger than any architecture choice | Write the novel's recipe and run `recipe validate`, before generating anything (an hour) |
| The winner at 50k tokens holds at 1M | ~50% for any one design | The 50k winner collapses at scale | Step 5; prefer designs whose call count grows linearly |
| The scorer measures what a user needs | ~70% | We chase quote-matching artifacts | Hand-read 20 scored disagreements per design (step 2) |
| The one-shot is a fair yardstick | ~80% | Low: it scores 71%, so a design can beat it | – |

## Steps

1. **Baseline.** The pipeline on world-s4 is running.
2. **The race on world-s4.** Four throwaway scripts, about 300 lines in all, share one model-call helper and the
   existing scorer. DeepSeek reads first, because this step is about the architecture, not the model.
   - **A. Cast first.** List the names in every message, group them into a cast, then read each message with the
     cast. Run it once with the key's cast, to give its ceiling.
   - **B. Windows, then merge.** Read each thread or folder whole, which fits easily. Then one pass merges the
     records across windows.
   - **C. Notes, then one read.** One short structured note per message, then one call over all the notes.
   - **D. Rolling store.** Read the messages in date order, with the current record list in the prompt, and update
     it.

   Score each on all five worlds and on Ward. Keep the designs that come within about 15 points of the one-shot
   and that rank the same way on Ward. If none comes close, stop and rethink before building anything.
3. **A small novel.** First write and validate its recipe. Then generate one novella of about 20k tokens, with
   about 10 characters, relationships that change, and an answer key. Run the survivors on it.
4. **The local model.** Run the survivors on Qwen. The gap to DeepSeek tells us whether the local model is enough.
5. **Scale.** Run the best one on a 1M-token world or novel series.
6. **Product.** Build only the winner into the default commands, reusing what exists. For example, the reader
   already has a cast list (`SeedEntities` in `cache/seed.json`), switched off for document reading at
   `runner_support.rs:41`. Delete the race scripts.

## Rules

- Develop on world-s4. Call a design better only when the other four worlds agree.
- Numbers go in commit messages.

## Deferred until something works

Labelling the hay, blind recipes, a second writer model, a real literary anchor, bars, statistics beyond five
worlds, and a review process.
