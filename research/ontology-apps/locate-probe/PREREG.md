# Locate probe: pre-registration (2026-10-12, written before any arm was asked)

Question. Cause 1 of the 2026-10-12 quality ledger (runs/quality-ledger-20261012/assessment.md): on all three
systems most READ losses are lines the reader asked and answered "none", where the line implies the fact rather than
stating it (Ward 18 of 22, uv 26 of 27, GVC most of 169). Does another question shape recover them without losing the
lines read right or flagging more unmarked lines? The probe changes only the question; line classing, the document
view and the facts are the reader's.

Population (whole-text run legs, runs/whole-text/ours-{ward,uv,gvc}; one run each).
- Ward: every asked line of the documents holding the latest message of the 47 current tune deals. Positive: a line
  holding a gold stage_update quote (stage_update, its stage) or a gold commitment quote (commitment).
- uv: every asked line of the documents holding a tune gold case state. Positive: a line holding the quote (case_state,
  its state).
- GVC: lines of the 78 read documents. Positive: a line holding a gold event mention (report). A line whose every
  mention is a pronoun or demonstrative is tagged reference-only and reported apart, never dropped.
- Unmarked: an asked line no gold item marks, sampled to the positive count per system (seed 20261012). Gold may be
  incomplete (Ward's has 127 uncertain labels), so a flagged unmarked line is listed for reading, and still counted.
- GVC is capped at 100 lost and 50 hit positive lines (seeded), for cost. A positive line the reader does not ask
  (quoted passage) is outside cause 1: counted, not probed.
- Classes, from the reader's logged distribution: a positive is hit (logged argmax is its kind) or lost; an unmarked
  line is clean (logged argmax none) or flagged.

Arms (forced choice, temperature 0, thinking off, labels A-Y and 0, the reader's facts and numbered document).
- C: the reader's Locate question as passes.rs locate_question renders it. Re-asked, it is the control and the gate.
- V: one label per declared value of a kind's closed field (a kind with none keeps one label); P(kind) is the sum of
  its labels, its value the argmax among them. Ward and uv only (GVC's report declares no closed field: V = C).
- Y: one yes/no question per declared kind; a line states every kind whose yes beats no.
- T: one yes/no question, "is line n about a particular <subject type>?", per decided subject type; a separation
  measure, read beside V and Y.

Gate (validates the instrument before any arm is read). C's argmax equals the logged argmax on at least 95% of
lines and the mean |p(0) re-asked - p(0) logged| is at most 0.05, per system. A system that fails the gate is
could-not-judge for every arm.

Metric. Mean log probability of the right outcome over the system's probe lines (positive: its kind; unmarked:
none; for Y the product over kinds of the right yes/no), beside counts: lost recovered, hits lost, unmarked flagged.

Bars, per arm (fixed now).
1. On every system where the arm is defined: mean log p(right) at or above C's.
2. On at least two systems: at least 25% of lost positive lines recovered.
3. On every system: hits lost at most 10% of hits, and unmarked flagged up at most 5 points over C.
4. V only: value accuracy on positives V reads as their kind at or above Choose's logged accuracy on the same lines.
An arm meeting 1-3 (and 4 for V) is promising and goes to an end-to-end leg on the same inputs. An arm that misses is
not tuned; its remaining losses are classed by distribution shape (right label rose but stayed second; confident
other label; split between adjacent values) and the next tool is chosen by that class.
