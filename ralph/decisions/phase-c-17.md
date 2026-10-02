<!-- ledger -->

**phase-c-17 · 2026-10-02 · pc-partial-decline-verdict-measure · director** — this commit
- Needed: the measure read a MISS in both directions (6ce67adbc). Reopen the code row and with what fixtures; whether the 35B present-killer-weapon flip is a false positive; where the zero-chunk finding goes; how the measure row closes.
- Chose: reopen pc-partial-decline-verdict on three corrected shapes of `declines_asked_fact` (a contrast that continues the decline, a GK signpost with no value, an absence statement read anywhere in the text), with the 14 misses and the chat-ask false positive verbatim as fixtures; 4e8f9cfb1 stays. present-killer-weapon is not a fixture either way. The measure row is `[x]` at 6ce67adbc; the re-read is the split pc-partial-decline-verdict-measure-2, alone. The zero-chunk finding is filed below phase-c's cut as pc-zero-chunk-decline-verdict, not to phase-d.
- Because: the row's proof says a miss reopens it and never re-tunes the bar; the misses and the false positive each trace to the one decider (principle 8), and the fix describes shapes rather than adding the two uncovered strings. The killer-weapon label agrees with what the prose claims of the retrieved sources; the unsignposted aside is the claim gate's to judge (principle 12). The zero-chunk verdict is a bug at a site the decider does not reach, the sibling of pc-complex-task-decline-verdict, not architecture; moving it above the cut is the operator's.

<!-- appendix -->

## phase-c-17 · 2026-10-02 — the partial-decline MISS reopens its row on the decider's shape

<details><summary>reasoning, evidence, package</summary>

Reproduced from the lane's raw (now target/ralph/phase-c/pc-partial-decline-verdict-measure/phase-c/pdm/, copied from the lane worktree; hash 0b2a7bb26, load per run in 6ce67adbc's body):

- ledger-table.txt: 35B check4 5 turns, CannotKnowFromHere 2 (abstained_decline 1, released 1), Grounded 1, Unverified 2; 4B check4-firmonly 10 turns, CannotKnowFromHere 7 (5 flipped), Unverified 3. Matches the package.
- runs/35b-cm-r2.transcripts.jsonl present-killer-weapon: "The provided passages do not state that Winnie Verloc kills Adolf Verloc ... Not covered here: The actual killing scene (which involves a carving knife) does not appear in these specific retrieved passages." The passages quoted in the same answer show her handling the carving knife.
- absent-room-thursday in the six firm-bank transcripts: gate_action None, 0 chunks, every answer "I do not have access to your organization's internal schedule ...", verdict Unverified per ledger-table (Unverified/- once per firm run).
- decline.rs `declines_asked_fact` at this tree: strips the GK caveat, vetoes on any remaining "from general knowledge", takes the first DECLINES or ABSENCE_STATEMENTS hit anywhere in the text, vetoes on any ANSWER_PIVOTS after it. Each miss class follows from one of those lines.

The lane branch was merged (merge pc-partial-decline-verdict-measure) and ralph/lanes/pc-partial-decline-verdict.done removed with the reopen, as at phase-c-15, so a fresh lane does not read as finished.

Falsified if: the reopened decider, judged on the same sandbox, flips a full answer to cannot_know_from_here at the chat-ask rate or worse, or the 14 fixtures pass but check 4 at 35B stays near half (then the misses are not the decider's and the row's premise is wrong again).

REVIEW-AFTER: present-killer-weapon is read as the label agreeing with the prose. If the operator reads a parenthetical parametric specific as an answer the user would act on, it becomes a false-positive fixture for the reopened row.

</details>
