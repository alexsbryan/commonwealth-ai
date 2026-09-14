# Operator requires every default-off/dark ship to get a row in sovereign/DEFAULTS_LEDGER.md (same commit) — flip condition, settling plan…

Operator directive (2026-07-31): stop the "proved it works, shipped it
dark until X, X never happens" withering pattern. Every capability
shipped default-off or dark must be logged in the document of record,
`sovereign/DEFAULTS_LEDGER.md`, in the same commit.

Why: The operator observed repeated cycles of "we proved it worked
great!" followed by a caveat ("off by default until such-and-such")
after which the work withers — the flip condition lived only in a
session summary nobody re-reads. Example that motivated it:
SOVEREIGN_DOC_CLUSTER_WEIGHT shipped 2026-05-22 at 0.0 "pending bench
plan", dark for ten weeks.

How to apply: When shipping anything default-off/dark, add a
ledger row with: falsifiable flip condition, which plan item settles
it, review-by date. Flips/rejections move the row to
Graduated/Rejected — never delete. When touching an area whose row is
past review-by, raise it: flip, kill, or re-date with a named blocker.
Related: [[demo-value-every-push]] — same operator instinct: work must
land in a user-visible, accountable form, not evaporate into process.
