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

## Step 2: embedded mail as messages of their own (pre-registered 2026-10-17, after eb07f225a, before any read)

**Step 1's result.** All three bars passed (6e8850e6d): voice2-ward 7 -> 8 and w20-inner 6 -> 7 on replay, both
gaining pasadena-d8, losing nothing; "not an exact passage" refusals 1 -> 0 and 4 -> 0.

**Change (eb07f225a, agreed invariants).** Every line has one owner; a message's date and author are its own
header's; its identity is who said what; the thread is the mail format's, not the recipe's (step 2b, not yet built).
ward-story's 37 files are 59 documents, 22 of them embedded messages.

**Instrument change, decided here.** `loop/score.py` matches a statement citing an embedded message against the
gold updates filed at any message carrying it (GOLD_SPEC files a quoted statement at its first carrier by date),
the carriers read from the index's own documents (`embedded_in`, each segment's `message`). On every saved read the
scores are unchanged apart from the judge stamp (none had an embedded document).

**Reads.** ward-story re-ingested at eb07f225a; two fresh reads (`ASKER=daemon`, `INIT=1` re-cut at 20k, then the
second on the first's cut), HEAD's reader. The worlds are not re-ingested in this step.

**Passes when all three hold.**
1. tep-d4 is served in both reads (11 reads of 11 missed; the one-shot 2 of 2).
2. tep-d6 is served in at least one read (0 of 11; the one-shot 1 of 2).
3. Ward's mean over the two reads is at least 7 of 14 (the step-1 replays at HEAD's reader: 7 and 8).

Reported beside the bars, no bar: pasadena-d1 (its gold names one deal where the recipe's identity rule names two,
and the one-shot serves it 0 of 2), every Ward deal's served count beside the 11 earlier reads, windows and answer
tokens, and claims that cite an embedded message, with their date basis.
