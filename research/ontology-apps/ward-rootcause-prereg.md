# Ward's root causes (pre-registered 2026-10-17, before any replay at the change)

Accepted by the operator 2026-10-17 ("aligned on addressing the root causes comprehensively").

**Finding (note f9142d79).** Over the eight thinking-on reads at adopted code Ward serves 58 of 112 deal-reads (7.25
a read; the one-shot 7.0). The 54 misses: instrument 21 (the record right in substance, or the gold arguable),
embedded mail no stored document holds and no window shows 16, the reader's call 7, RESOLVE across windows 6, and
a splice 4: the sentence widener ran over `own_text`, which had dropped carried lines without a trace, so a
verbatim quote of pasadena/37.'s item 6 widened back across items 2-5 to item 1, and validation, which checks the
raw body, refused it (`validation.rs:355`). 346 of 533 "not an exact passage" refusals in the saved runs are that
splice, every quote verbatim.

## Step 1: the splice

**Change.** `own_text` leaves a blank line where it removed lines, and `sentence_of` already stops at one, so a
sentence widens only within one run of own lines (`window_text.rs`; test
`a_widened_sentence_stops_where_carried_lines_were_removed`, failing before the change on the same refusal).

**Reads.** Replays only, no window call (`ASKER=replay`; RESOLVE asks the daemon only questions it has not
recorded): voice2-ward (40k, `RESTORE=voice2-ward`) and w20-inner (20k, `RESTORE=w20-inner`), the two saved reads
whose prompts HEAD's reader still writes. Each replay's window answers are checked identical to its reference
arm's before it is scored.

**Passes when all three hold.**
1. pasadena-d8 is served in both replays (refused in both references).
2. No deal the reference serves is lost in its replay.
3. Neither replay refuses a claim as "not an exact passage" whose quote is verbatim in the document's own lines.

Reported beside the bars, no bar: refusals by reason before and after; the same count over every saved answer
by the offline port of the widener (`scratchpad ward-dd/refused/sweep.py`), labelled as a model of the code.
