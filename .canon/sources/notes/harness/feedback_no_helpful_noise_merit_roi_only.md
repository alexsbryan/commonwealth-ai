# Don't surface trivia as "would you like me to look at this?" or dress a small finding as dire — the seat assesses merit and ROI, and only…

Operator direction 2026-08-21, on the comaintainer seat filtering worker
output. Two things are being named, and they are opposite failures.

OUT: the overly-helpful pattern — "would you like me to look at this?",
"I noticed this trivial thing, and let me tell you how complex and dire
it is, I think we should work on it." Offering tangents as questions,
inflating a small finding's stakes to justify surfacing it, narrating
what was banked. That is noise and the operator does not want it.

IN: something ragingly relevant to the objective, where **the seat has
already assessed its merit and ROI** and judged it worth interleaving.
Note the load-bearing clause — the SEAT assesses. What reaches the
operator is a judgement already made, not a question passed upward.

Why: the seat exists to hold the forest so the operator does not have
to. A finding relayed as a question moves the triage cost back onto the
operator, which is the one thing the seat is for. And a small finding
dressed as urgent corrupts the signal that makes real interrupts
readable — if everything is dire, nothing is.

How to apply: default is bank it (`co-backlog-producer.sh`, keyed by
what went wrong) and say nothing. The narrow exception that earns an
interrupt: it would change what the current order or campaign should do
NEXT. Then state the finding, the ROI, and the recommendation in one or
two sentences — never as "should I?". Everything else waits for the
close-out heap triage. See [[feedback-ship-code-not-prose]] for the same
discipline applied to written records, and the banking clause in
`.claude/skills/comaintainer/SKILL.md`, which is the worker-side half.
