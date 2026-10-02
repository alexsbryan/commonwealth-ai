<!-- ledger -->

**phase-c-20 · 2026-10-02 · pc-partial-decline-verdict-measure-2 · operator, closing on the second re-read** — this commit
- Needed: measure-2 (b43e0e20f) read a narrower MISS: on-prem check 4 pooled 35B 8/8 (was 4/8 at 6ce67adbc, 1/3 before the row) and 4B 12/13 (10/13, 0/25); every fixture phase-c-17 named reads correctly live and the q2 trailing-caveat false positive is gone (0/40, 0/16). Two shapes remain, one each way: a hedge-then-answer full answer reads cannot_know_from_here (4B chat-ask q1, 1 of 40), and a decline whose general-knowledge caveat sits mid-text reads unverified (4B absent-paralegal-bare 3/3, unchanged from before the row). The row's rule says a miss reopens it.
- Chose (operator, asked by the seat with both options priced: "Close, file residue"): pc-partial-decline-verdict stays [x]; measure-2 closes at b43e0e20f as the reading it is; the two shapes and the 35B decline-plus-GK Grounded label go below the cut as pc-partial-decline-residue with their transcripts as fixtures.
- Because: a third round costs ~2.5 h (code plus a 2 h reading alone) for one false label in 40 turns and one miss that predates the row; the operator ruled the residue a cleanup, not a blocker. The seat stopped the director's resolution session (an operator STOP, 18:11Z) so it would not reopen the row against the ruling; it had already merged the readings (379e56f1e).

<!-- appendix -->

## phase-c-20 · 2026-10-02 — the decline verdict closes on its second re-read; the residue is cleanup

<details><summary>reasoning, evidence, package</summary>

Causes, from b43e0e20f's findings: `pivot_answers`' value test (a fresh number or a capitalised mid-sentence word) misses a lowercase or code-span answer after " but " (sovereign-core runtime/grounding/decline.rs); `strip_gk_caveat` (repair.rs:58) keeps only the text after a mid-text "from general knowledge:", so `declines_asked_fact` (decline.rs:155) never sees the opening decline. Fixtures, kept in the main tree: target/ralph/phase-c/pc-partial-decline-verdict-measure-2/phase-c/pdm/runs/4b-chatask-r3-q1-warmup.txt (answers) and .../runs/4b-firm-r1.transcripts.jsonl (declines). The 4B gk_rescue "1945-06-23" release to a conference-room question (1 of 3) is pd-anchor-unenriched's case.

</details>
