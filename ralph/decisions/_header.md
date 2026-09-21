# ralph director decisions

One entry per decision, four lines each: what forced it, what was chosen, why, and the
commit. The reasoning in full, the evidence and the worker's package are in the appendix
of the same id, folded. One decision, one commit, so `git revert <sha>` undoes
exactly it. `FLAG` marks a reading of a bar or a clause the operator may revert.

**This page is generated. To add a decision, run `scripts/ralph-decisions.py new
<campaign>`, fill the file it prints, and re-render with `--write`.** One decision, one
file under `ralph/decisions/`, so two campaigns running at once add two files instead of
colliding on one — and ids are `<campaign>-<n>`, which cannot collide across branches
the way a single counter did. The `A1`-`A75` series is closed: it is frozen verbatim in
`_archive-ledger.md` and `_archive-appendices.md`, and its numbers are cited from
`quality/campaigns/`, `research/` and commit bodies, so none of them is ever reused or
reassigned. `A69`-`A75` were written as `A35`-`A41` on a branch and renumbered when it
merged; that merge is what minted this arrangement, and `ledger-1` records it.
