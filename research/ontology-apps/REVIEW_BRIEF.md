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
the one-shot's 25.5). Bar R, judged 2026-10-18 on two 20k reads of each of five recipes (generality-prereg.md, last
revision): the author effect is real (recipe means 37, 37, 31.5, 30.5, 28; 2.5% of random deals spread as wide; the
40k control fails at 1.8%), and every recipe clears the floor (lowest 28 against 23.5). Recipes differ in how many
questions they answer, not which: cross-recipe reads swap 2.2 questions, same-recipe 2.0. The 2026-10-16 form's
"fails on agreement" (778461d0e) was its yardstick re-counting the score gap at one read per persona.
Windows are 20,000 characters since 2026-10-17 (determinism-prereg.md): a call writes about as many statements
whatever it is shown, so smaller windows write more of each document. Same-recipe reads of saltgrass-asked went from
26-32 agreeing on 33-34 of 42 to 37 and 37 agreeing on 36 (the one-shot: 37); the held-out recipes agree on 32.6
(29.2), Bar G 30.5; mail kept its deals for about 1.7x the answer tokens. Thinking off and folding two reads were
measured first and failed. A window that writes its claims' field values beside `fields` (8 of 343 saved
windows) now has them read in it: world-s1's 20k read replays 9 -> 12 of 18.
Why deals are still lost, and the solution space, is in `diagnosis/README.md` (2026-10-17): a statement is placed
only by a handle unique to its deal; the state-across-windows decision is taken first.
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
   Fixed by bb16808b5, after those reads.

   **The plan (2026-10-19, operator: "a lean startup level plan").** Target: the default commands on a 64GB machine
   (this M2 Max; Qwen3.6-35B-A3B IQ4_NL, 128k context) make records near DeepSeek's. What the 2026-10-15 reads
   already say (`loop/window_recall.py`, no model call): local attaches 31 of world-s4's 55 gold updates at 20k
   against DeepSeek's 52, and the gap is in the dense windows (window 1: 12/27 against 27/27; window 3: 4/9 against
   9/9; window 2: 15/18 against 16/18). It writes 48 statements to DeepSeek's 103 on about a quarter of the
   reasoning, and 7 of its 10 misses were never written. A world-s4 read takes about 41 minutes.

   The assumption the plan stands on: the loss is per call, a call with many statements to write drops some, so
   calls that each write fewer recover it. It is wrong if a narrow call over window 1 still leaves most of its gold
   unwritten. The innermost ring is that one window, scored per gold update (minutes a trial); a change that wins
   there goes to world-s4 (two reads), then the inner ring. Each step is pre-registered with its bar before its read.
   0. Baseline at HEAD: the daemon on the local model (config swap and restart, peer sessions told first), world-s4
      read twice at 20k. It also checks what a local daemon may not do as a hosted one does: an empty answer under a
      JSON grammar with thinking on (note 068f6b1f), long-call timeouts, forced-choice distributions for RESOLVE.
   1. Window size, the lever that exists (`window_chars`): window 1's documents at 10k and 5k. No code.
   2. If size does not close window 1: a second pass over the same window, shown the statements already written
      and asked only for the rest, which code folds in (the repeated narrow pass). About 150 lines.
   3. Once recall holds: minutes per window and what a 1M-token corpus would take (MTP, thinking budget).
   Pivot: if steps 1-2 leave window 1 under 20 of 27, read it with Qwen3.8-27B (dense, downloaded) to tell the model
   from the design. Stop: if neither model reaches 20 of 27 by any narrow call, the track is model-bound today; the
   numbers are recorded and engineering stops. Done: world-s4 at least 8/15 served and 45/55 attached on two local
   reads (DeepSeek 9/15, 52/55), then the inner ring within 2 deals of DeepSeek's. Spend is machine time; while the
   daemon serves the local model, DeepSeek is off for every session on this machine.

The one-shot reference under the current scorer (c138d09b1) is 68/93 served and 237/328 attached on the worlds,
not the race table's 66/93 and 247/328.

## Rules

- Develop on world-s4. Call a change better only when the other four worlds agree.
- Numbers go in commit messages.

## Deferred until something works

Labelling the hay, a second writer model, a real literary anchor, bars, statistics beyond five
worlds, and a review process.
