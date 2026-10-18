# A record's lifecycle as typed statements, and the window size as a lever (pre-registered 2026-10-18)

Asked by the operator 2026-10-18 ("all this should be configurable then if it's contestable based on sources -- give
the recipe owner some levers, but provide the best batteries included default. Let's add the code to make 20k
stronger"). Diagnosis: `diagnosis/README.md`, "On today's reader": 20k windows write more of each document, and on
world-s1 what they newly write is statements the recipe has no stage for (an act under a won deal), and a rollover's
end when the deal's history is in another window.

**Change.**
1. `[enrichment.ontology] window_chars`: the window size, a lever with today's 20,000 as the default, refused
   outside 2,000..=40,000 (40k is the largest window read, and it fits the 128k call whatever the script).
2. For each entity type whose attribute a protocol fold derives from one claim kind, the method supplies two
   statement kinds the reader is shown beside the recipe's (`<type>_replaced`, `<type>_act`). Code applies what they
   mean.
   - `<type>_replaced`, the record is replaced, rolled into or superseded by another. By default it is a kind of
     the fold's claim kind, so the recipe's own rules give it its state, read from the recipe's values. Lever
     `replaced = "<state>"` on the protocol block: the reader is not asked, and the fold moves it there.
   - `<type>_act`, something done under the record as it stands. By default no rule names it, so it moves
     nothing. Lever `acts_under = "<state>"` makes it a move.
   - Lever `lifecycle = false` supplies neither.
   - The supersession line of `window_prompt.md` goes.
   Expected size ~150-250 lines and ~200 of tests.

**Code first.** Tests that fail before the change: the levers parse and refuse out of range; the two kinds are in
the policies and the prompt of a recipe with a protocol fold, absent with `lifecycle = false` or without a fold; an
act after a won report leaves the record won; a replaced statement moves its record to the state its rules or its
lever give; the manifest is cut at the recipe's size.

## Reads

The window prompt changes, so the reads are fresh: world-s1, world-s3, ward-story at 20k (`ASKER=daemon`, `INIT=1`).
Budget: about 0.6M output tokens with thinking (today's 20k reads: 248k, 249k, 99k). Controls are today's reads:
leand-0 (world-s1, world-s3), with two fresh RESOLVE passes (leand-nz1, leand-nz2); nest-w1 and nest-w2 (ward-story).

## Passes when all hold

1. **Target.** At least 3 of world-s1's five losses to 20k are served: blueville-d1, kestrel_ridge-d2 (acts
   under), halwick-d1, coyoteville-d1, coyoteville-d2 (rollovers). The controls serve 0, 2 and 2 of them; today's
   reader at 40k serves all five.
2. **Mechanism.** On world-s1, no statement quoting a nomination is a stage_update with a stage other than won
   (control: 2 of 2, lead and proposal).
3. **Guard.** world-s1's other 13 deals: at least 11 served (controls 12, 12, 12). world-s3 at least 26 of 27
   (controls 27). ward-story at least 6 of 14 (controls 6-8).
4. **Contract.** C1-C6 pass; corpus-engine's tests pass.

Reported beside the bars, no bar: every `deal_act` and `deal_replaced` statement written, with its quote; deals by
lifecycle class (`diagnosis/by_reader.py`); attached updates; answer tokens per window.

**After the bars.** Passed: world-s2, s4, s5 are read once (no world below today's read by more than the 40k
reader's own read-to-read change, 2 deals), and saltgrass-asked once, because the prompt line goes for every recipe
(Bar G, `generality-prereg.md`). Failed: the result and its diagnosis by class, and the change reverted unless one
part passes on its own.
