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

## Revisions

**2026-10-18, after a smoke read of three of world-s1's target windows (sec_00004, sec_00007, sec_00008; 114k
output tokens), before any measured read.** The replaced kind's description now says the replaced record has ended
and asks the stage it ends in (e271fac85): under the first wording halwick-d1's rollover was written replaced with
stage negotiating, under the second, lost. Every nomination in the three windows was written as an act under
(8 of 8). The bars are unchanged; halwick-d1's window informed the wording, so a bar 1 that passes on halwick-d1
alone is reported as such.

## Result (life-inner, fresh 20k at 2e3ea8585; 623k output tokens, thinking included, chars/4)

**Fails: bar 3 fails on world-s3 and ward-story; bars 1, 2 and 4 pass.** Bar 1: 4 of world-s1's five losses served
(blueville-d1, kestrel_ridge-d2, coyoteville-d1, -d2; controls 0, 2, 2); halwick-d1 is not, so the wording its smoke
window informed did not carry it. Bar 2: world-s1's six statements quoting a nomination are all acts under (control:
2 of 2 lead or proposal). Bar 3: world-s1's other 13, 12 served (controls 12); world-s3 25 of 27 (controls 27);
ward-story 5 of 14 (controls 6-8). Bar 4: C1-C6 and the changed crates' 2752 tests pass. Inner ring pooled 46 of 59
against 45-48. Diagnosis by class:
- The replaced kind leaves the end state to the reader, and it read won in 2 of 4: halwick-d1's "being rolled into a
  new arrangement" and velica-d1's "being replaced by a new Cal 01 deal" won, coyoteville-d1's and velica-d2's lost.
- The act-under kind swallows stage moves on real mail: ward-story wrote 17, among them tep-d3's "working to close a
  gas transaction" (gold negotiating) and pasadena-d8's "Sale to Enron at SoCalBorder ... at $2.68" (gold won); on
  the worlds it took every nomination (9 of 9 on world-s1 and world-s3) and "book it", "roll it" (no gold update).
Neither part passes alone, so the kinds' default is reverted to today's reader (no kinds supplied, the prompt's
supersession line back) and they stay as levers a recipe opts into; `window_chars` stands (no change at its
default).
