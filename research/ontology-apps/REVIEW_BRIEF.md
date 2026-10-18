# Ontology layer: the lean plan (2026-10-13)

The longer version, with instruments, validity checks and bars, is in git at 1977b2eac. It is deferred until
something works.

## Goal

One recipe and the default commands turn a corpus into typed, cited records. It must work on mail and on a novel,
and scale past 1M tokens. The yardstick is a model that reads the whole corpus at once (the "one-shot"). The test
is five generated mail worlds with an answer key (93 deals; `research/ontology-apps/world/`), scored by
`loop/score.py`, and every arm runs through `loop/loop.sh` (it replaced `run_world.sh`).

## Where it stands (2026-10-16)

The window reader is the product's one reader (5f2711847; the passes reader deleted 2026-10-16). Worlds: 86/93
served against the one-shot's 68/93 (arms structural-fresh and structural-fresh-s1, the diagnosed set). Ward: 8/14,
tied with the one-shot. Saltgrass, held out (`generality-prereg.md`): Bar G passes (the ledger recipe's 28.5 against
the one-shot's 25.5), Bar R fails on 7 of 10 recipe pairs, on agreement (778461d0e).
Windows are 20,000 characters since 2026-10-17 (determinism-prereg.md): a call writes about as many statements
whatever it is shown, so smaller windows write more of each document. Same-recipe reads of saltgrass-asked went from
26-32 agreeing on 33-34 of 42 to 37 and 37 agreeing on 36 (the one-shot: 37); the held-out recipes agree on 32.6
(29.2), Bar G 30.5; mail kept its deals for about 1.7x the answer tokens. Thinking off and folding two reads were
measured first and failed. A window that writes its claims' field values beside `fields` (8 of 343 saved
windows) now has them read in it: world-s1's 20k read replays 9 -> 12 of 18.
Who says it (b34ee699b, ebdf1eab5, voice-prereg.md) is kept on the cut line (operator, 2026-10-16): it passed
its three target bars and missed the mail guard by one deal (31 against 32, note dcdfeb8f); the first change found
later that jumps the objective and conflicts with it takes precedence, and this is the first thing to cut.

## How we develop (operator, 2026-10-13)

- **The system and the model are separate tracks.** Build the system on the fastest model that is reasonably
  capable; today that is DeepSeek Flash. Once the system works, engineer it down to leaner models.
- **No one-way doors against small models.**
  - Every call's prompt, reasoning and answer fit in 128k tokens: a 64GB machine runs a capable local model (Qwen 35B-A3B MoE or Qwen3.8 27B) at that context (operator, 2026-10-14; it was 32k). The bound is tunable: be expansive within it until the quality objective is met, then engineer down.
  - No call's input grows with the size of the corpus.
  - Nothing depends on a feature only a hosted model has.

  A design that only works by eating context is out, however well it scores.
- **The writer is not the reader.** MiMo writes the worlds, so it never reads them.

## The race, 2026-10-13

History, tables at c73b91408: four architectures read with DeepSeek Flash; windows served 76/93 against the
one-shot's 66/93, windows cut by date tied windows cut by folder, and Ward's rerun put single-run noise there at
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
2. ~~Product~~ (done: the window reader is the default commands' reader, 5f2711847).
3. ~~Saltgrass~~ (done: blind recipes through the same commands, scored by its question banks; above).
4. **The model track.** Engineer the system down to the local model. Filed 2026-10-15 (Qwen 35B-A3B, thinking
   on, product window reader, world-s4; `runs/loop/local35b-s4-think{,-w20k,-w10k}`): 3/15 at the then-default 40k-char
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

Labelling the hay, a second writer model, a real literary anchor, bars, statistics beyond five
worlds, and a review process.
