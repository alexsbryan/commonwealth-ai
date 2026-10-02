<!-- ledger -->

**phase-c-19 · 2026-10-02 · pc-rails-journal-linear · director** — this commit
- Needed: the pool halted with "merge conflict merging ralph/pc-rails-journal-linear"; the main tree held the uncommitted closure of pc-partial-decline-verdict (STATE.md `[x]`, DECISIONS.md regenerated with phase-c-18), staged, with no MERGE_HEAD.
- Chose: commit that closure (22d0bb9e1), complete the merge by hand (b25bcfac8; clean, no conflicting hunk), mark the row `[x]`, and make the pool check its done commit: a failure now halts naming the unit and git's first line, instead of surfacing at the next merge as a conflict.
- Because: with the index committed, `git merge --no-ff ralph/pc-rails-journal-linear` applied cleanly (9 files, 737+/194-), so the only thing stopping it was the dirty index; `scripts/ralph.py` ran the done commit unchecked after each merge. The new test fails without the check on exactly the observed package ("merge conflict merging ralph/dm-b") and passes with it; the 164-test suite is green.

<!-- appendix -->

## phase-c-19 · 2026-10-02 — a failed done commit was reported as the next lane's merge conflict

<details><summary>reasoning, evidence, package</summary>

Evidence: `git status` at session start showed only `M  ralph/DECISIONS.md` and `M  ralph/next/phase-c/STATE.md`, both staged, matching what `_regenerate_decisions` and `queue.set_status` + `git add` leave before the `ralph: <unit> done` commit; `0838218d1 merge pc-partial-decline-verdict` was HEAD and no `ralph: pc-partial-decline-verdict done` followed it (the earlier 0b2a7bb26 is the row's first close, before it was reopened). The merge of the rails lane after committing the index went through with no conflict. Why the done commit itself failed was not recovered: no pool log on this host records git's stderr, and `scripts/commit-msg.sh` accepts the subject (rc=0, tried by hand). A concurrent git operation on the main tree (an index.lock) is the likely cause; the halt now prints it.

Falsified if: a pool halt with "merge conflict" recurs while the merge applies cleanly on a clean index; then something else dirties the index between merges.

</details>
