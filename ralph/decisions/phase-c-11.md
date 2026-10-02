<!-- ledger -->

**phase-c-11 · 2026-10-02 · pc-knowledge-gym-noresults · seat, reverting its note and reopening it for the operator** — this commit
- Needed: pc-knowledge-gym-noresults-both-directions read 7fb5bfd10's empty-result note in the direction it was not meant to move (61c978b07, n = 9 per arm, deployed primary, load 4.7-7.6): 05 1/9 -> 9/9, but a turn whose first lookup comes back empty and whose rephrase would find the answer recovers 9/9 without the note and 0/9 with it. A narrowed note that says only what the code knows recovers 9/9 and fails 05 0/9. The supervisor parked the split as operator-only (05's bar was ruled at the ship gate, phase-b-113).
- Chose: revert 7fb5bfd10 on cut, reopen pc-knowledge-gym-noresults and park it with the table and three options (keep the note; move 05's bar to one rephrase; a structural one-rephrase bound, a new row). The tree is main's behaviour again: recovery intact, 05 failing on its lookup count exactly as at main.
- Because: without the note 05 already answered honestly and failed only on lookups (2 > 1); the note bought that one lookup by turning every wrong-words first query into a false "missing". That is the suppressed-correct-answer trade AGENTS.md's working style rules out ("Quality over the metric"), and the charter's rule that a measure miss reopens its code row. Which trade to make is the operator's; the interim tree should not carry a measured recall loss into the merge.

<!-- appendix -->

## phase-c-11 · 2026-10-02 — the empty-result note is reverted; 05's bar and rephrase recovery go to the operator

<details><summary>reasoning, evidence, package</summary>

The revert touches only 7fb5bfd10's four files (executor.rs's note and its trace field, the new test file and its mod line, functional.rs's visibility); no later commit touched them or uses what they added (`git grep EvidenceProbeInference|EMPTY_RESULT_NOTE|absence_stated` outside them is empty). The recovery reading used a named substitute: fixture 01 with a scratch mock whose first lookup returns no evidence and later ones the fixture's rows; no headless real-corpus driver exists for the executor path. 07-11 run `raw` and cannot see the note. Falsified if: a real-corpus reading shows rephrase recovery after an empty first lookup is rare enough that the note's honesty gain outweighs it; that reading is the operator's option (c) row's.

</details>
