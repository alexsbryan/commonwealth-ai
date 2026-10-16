# Ontology layer: the lean plan (2026-10-13)

The longer version, with instruments, validity checks and bars, is in git at 1977b2eac. It is deferred until
something works.

## Goal

One recipe and the default commands turn a corpus into typed, cited records. It must work on mail and on a novel,
and scale past 1M tokens. The yardstick is a model that reads the whole corpus at once (the "one-shot"). The test
is five generated mail worlds with an answer key (93 deals; `research/ontology-apps/world/`), scored by
`score_ward.py`.

## How we develop (operator, 2026-10-13)

- **The system and the model are separate tracks.** Build the system on the fastest model that is reasonably
  capable; today that is DeepSeek Flash. Once the system works, engineer it down to leaner models.
- **No one-way doors against small models.**
  - Every call's prompt, reasoning and answer fit in 128k tokens: a 64GB machine runs a capable local model (Qwen 35B-A3B MoE or Qwen3.8 27B) at that context (operator, 2026-10-14; it was 32k). The bound is tunable: be expansive within it until the quality objective is met, then engineer down.
  - No call's input grows with the size of the corpus.
  - Nothing depends on a feature only a hosted model has.

  A design that only works by eating context is out, however well it scores.
- **The writer is not the reader.** MiMo writes the worlds, so it never reads them.

## The race, 2026-10-13 (DeepSeek Flash, one run each, `runs/race/`)

Four architectures were read with the same model and scored by the same scorer. On world-s4 and s1:

| Design | Deals served |
|---|---|
| Windows | 28/33 |
| Cast-first with the key's cast | 22/33 |
| One-shot | 22/33 |
| Cast-first with a built cast, notes, and rolling | Behind, or cut short when the DeepSeek balance ran out; not pursued |

"Windows" reads each folder whole, up to about 24k tokens, then one call merges the records across windows.

Windows against the one-shot on all five worlds, and on Ward (real mail):

| | Windows | One-shot |
|---|---|---|
| Deals served, 5 worlds | 76/93 (82%) | 66/93 (71%) |
| Stage updates attached, 5 worlds | 294/328 (90%) | 247/328 (75%) |
| Ward deals served | 7/14 | 8/14 |
| Ward updates attached | 18/35 | 13/35 |

Updates attached is better with windows on all six corpora. Served is better on three worlds and one deal worse
on two.

On world-s4 with the local model, Qwen 35B-A3B with thinking off:

| Run | Deals served | Updates attached | Time |
|---|---|---|---|
| Windows, DeepSeek Flash | 14/15 | 48/55 | 4 min |
| Windows, local, thinking on | 9/15 | 31/55 | 83 min |
| Windows, local, thinking off | 5/15 | 27/55 | 6 min |
| Today's pipeline, local | 1/15 | 27/55 | 43 min |

With thinking off the local model folds a company's deals into one record (four false merges); with thinking on it
makes none, but it cites fewer statements (43 quotes against DeepSeek's 101), so updates stay unattached. Each call
thinks for 7-9.5k tokens. With thinking on, the daemon's JSON format empties the answer (note 068f6b1f), so those
runs ask for JSON in the prompt only.

**Windows cut by date** (one date-ordered stream, folder names hidden, 15 calls against 67) tie windows cut by
folder: 78/93 served against 76, 284/328 attached against 294. The folders were not doing the work. Ward fits in one
date window, so it cannot answer this; its rerun (5/14 against the one-shot's 8/14) puts single-run noise on Ward at
about three deals.

## What we could be wrong about

The percentages are my estimates, not measurements.

| Belief | How sure | Cost if wrong | Cheapest check |
|---|---|---|---|
| Windows wins because of the design, not because Ward foldered his mail by counterparty | Checked: date cut ties folder cut on the worlds | – | – |
| The merge holds once it works in capped blocks (its input grows with the corpus today, which breaks the rule above) | ~50% | Windows doesn't scale | Cap is structural; quality at 1M waits for a big real inbox |
| What wins on mail wins on a novel | ~50% | Two designs, or a redesign | Saltgrass (key: `bench/lanes/chaos_monkey/corpora/SALTGRASS_FACT_LEDGER.md`) |
| The ontology syntax can say what a novel needs | ~60% | A gap in the core | Write saltgrass's recipe and run `recipe validate` (an hour) |
| Generated mail ranks designs as real mail does | ~65% (Ward roughly agrees) | We tune to MiMo's prose | Ward alongside every run |
| The scorer measures what a user needs | ~70% | We chase quote-matching artifacts | Hand-read 20 disagreements per design |
| Cast-first is the right architecture | ~25% (lost the race) | – | – |

## Steps

1. ~~Thinking on locally~~ (done: 9/15). ~~Windows cut by date~~ (done: ties the folder cut).
2. **Product.** Build windows into the default commands, reusing what exists, with the merge in capped blocks so no
   call grows with the corpus. Measure with `run_world.sh` on the five worlds and Ward. No new test world (operator,
   2026-10-13: the instruments in hand are enough).
3. **Saltgrass** through the same commands: write the recipe, `recipe validate`, score against its fact ledger.
4. **The model track.** Engineer the system down to the local model. Filed 2026-10-15 (Qwen 35B-A3B, thinking
   on, product window reader, world-s4; `runs/loop/local35b-s4-think{,-w20k,-w10k}`): 3/15 at the default 40k-char
   window, 5/15 at 20k, against DeepSeek's 9-10/15. The losses are per call, not the design: the model plans a
   window's claims in its reasoning and drops some writing one long answer (9 of 38 on the densest window), and
   gold-document recall rises as windows shrink (67%, 75%, 86% at 40k, 20k, 10k). Window size is the first knob.
   The passes pipeline served 1/15 locally, so retiring it closes no door. One system defect found here: one
   malformed number (`"value":.02`) fails the strict JSON parse and loses the whole window (2 of 2 samples at 10k).

The one-shot reference under the current scorer (c138d09b1) is 68/93 served and 237/328 attached on the worlds,
not the race table's 66/93 and 247/328.

## Rules

- Develop on world-s4. Call a change better only when the other four worlds agree.
- Numbers go in commit messages.

## Deferred until something works

Labelling the hay, blind recipes, a second writer model, a real literary anchor, bars, statistics beyond five
worlds, and a review process.
