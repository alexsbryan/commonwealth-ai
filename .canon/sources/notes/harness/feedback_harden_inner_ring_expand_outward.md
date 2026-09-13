# Harden the innermost subprogram until green, then expand outward ring by ring until the whole real end-user chain is hardened — never pay…

Operator direction 2026-08-18, verbatim: *"harden the subprogram until it's
green then expand outward until the whole chain of real end user UX is hardened
is our methodology."* Preceded by: *"Stop wasting time testing the 95% of this
that we know works just to get to the one section we're praying is going to
work. I'm tired of this lazy methodology."*

Why: on the sec-filings journey, nine attempts each paid 60-90 minutes of
app build + live SEC fetch + ingest to observe a ~40-second segment (question
in, answer out, does it cite its source). Install and ingest were never in
doubt. The one segment that was in doubt had a harness that ran it in seconds
(`sovereign chat ask` via `run_frozen_set.py` + `check-sec-answer-path.py`) and
it had existed the whole time — named in the campaign file I had already read.
The operator's sharper point: a full-stack run launched on hope, when it fails,
tends to end with the feature left default-off "to wither and rot," because
nobody wants to re-pay the cost to return to it.

How to apply: before proposing any expensive end-to-end run, name the rings
and their per-iteration cost, and state which ring the suspected defect lives
in. Harden that ring in its own cheap loop until green. Only then expand
outward, one ring at a time, each green before the next. When asked to run the
full chain, first answer honestly: *is there a mechanism that makes me
confident this returns positive?* If the answer is no, the run is a lottery
ticket and the cheap loop has been skipped. A run is confirmation that an
outer ring wires to an already-proven inner one — never the instrument of
discovery. See [[feedback-ground-in-reuse-before-building]] (principle 11: the
harness usually already exists) and
[[feedback-defaults-ledger-no-withering]] (if it does end default-off, the
flip condition and review-by date are mandatory, in the same commit).
