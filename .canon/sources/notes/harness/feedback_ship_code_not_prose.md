# ~98% of a session is code with shorthand comments; a written record is produced on ACCOMPLISHMENT, never on attempts, refusals, or…

Operator direction 2026-08-14, after the audit-economy order produced ~400 lines
of committed narrative (D3-B refusal 86, gap analysis 113, D5 report 93, plus
prereg and D0/D1 docs) for one order — most of it about paths NOT taken.

**Target: ~98% of what a session produces is code, with at most shorthand
comments. Record when we actually accomplish something.** Scratchpads are fine;
the repo is not the scratchpad.

Why: "documentation is not a result." The chronic failure mode this corrects
is *well-documented near-misses* — a session that refuses three candidates,
writes three careful reports, and ships nothing reads as productive in the log
and is not. The prose also accumulates: a repo carrying 18 wrong turns in essay
form is harder to navigate than one carrying the code plus the data.

How to apply:
- A REFUSAL ships its DATA (verdicts jsonl, curve, render fingerprints) plus 1-3
  lines in the commit body. Not a prose report.
- A verdict to the operator or seat goes in the MESSAGE — numbers, bars,
  pass/fail — not a committed document.
- KEEP: pre-registrations (bars must exist before data or the verdict is not
  honest), test names, corrections that lead with what was wrong, and comments
  where the next reader would otherwise misread the code.
- CUT: any document restating what the commit message, the test, and the data
  already say.

Does NOT weaken [[feedback-creation-closure-loop]] (closure still ships with
creation) or ARCH §1.1 (a subsystem change still updates its SYSTEM_OVERVIEW
entry) — those are records of things that LANDED. It targets narrative about
work in progress and work abandoned.

Related: [[feedback-report-actual-metric-comparisons]],
[[feedback-quantified-pitches]], [[feedback-net-simplification-plans]].
