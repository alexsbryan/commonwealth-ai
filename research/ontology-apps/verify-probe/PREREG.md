# Verify probe: pre-registration, 2026-10-12, before any question is asked

Question (outside review via ec): citing an act with no "none" (cite-probe M2-N) turns a wrong act answer, even one
both wordings agree on, into a cited field. A quote shows where the model looked, not that the sentence entails the
value. M2 showed a yes/no "does it show this" rejects true acts, so the rejection path needs its own measure.

Pairs: every agreed act the act pass emitted on the runs/act legs (ec, 0f4f9b92d; `act-pairs.jsonl`, written by
runs/act/score.py before this file): Ward 71 (35 right, 36 wrong against gold), uv when its leg lands. A pair is the
act's value and the lines its citation touches (first_line..last_line of the leg's reader lines).

Question: Choose's own form (ONTOLOGY_METHOD §Reading), asked of the cited lines alone: the field's name and whole
declared description, its declared values as labels A.. and "not stated" as 0, one forced choice, temperature 0,
thinking off; two wordings differing only in the system prompt and the question sentence. No document, no facts, no
other line: what the value rests on is only what was cited.

Verdict: a pair is verified when the argmax equals the act's value. Read per wording and under both-must-agree.
Gate: 20 pairs re-asked outside the cache give the same argmax on at least 19.

Measures per system: true pairs kept (right pairs verified), false pairs rejected (wrong pairs not verified), apart for
wrong pairs on messages gold gives no particular; beside them, the argmax's spread (which value or "not stated").
Bar, fixed now: verification is worth building when, on each system with pairs, it keeps at least 80% of true pairs
and rejects at least 50% of false pairs under one rule (one wording or both-agree) chosen here as both-agree.
A miss is not tuned; it is classed (a true pair lost to "not stated"; a true pair lost to a neighbouring value; a
false pair kept because the cited lines do state the value, which is gold's or the act's error, read by eye).
