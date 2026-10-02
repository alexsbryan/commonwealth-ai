<!-- ledger -->

**phase-c-15 · 2026-10-02 · pc-gk-rescue-fabrication-measure · director** — this commit
- Needed: the measure read a MISS (b07f61eb2). Reopen the code row, and with what premise; keep or replace the rescue prompt's private-records clause; whether the synth-path twin moves above the cut; how the measure row closes.
- Chose: reopen pc-gk-rescue-fabrication with two corrected premises: (1) the known probe takes the zero-chunk branch, which reads the existing `question_is_situation_deictic` and declines without the GK prefix; (2) the 4B's Halvorsen release on gk_rescue's path is the entity clause's to explain, census first, the prompt clause stays but is not the guard. The measure row is `[x]` at b07f61eb2 and the re-read is the split `pc-gk-rescue-fabrication-measure-2`, alone. pc-gate-gk-exemption-deictic stays below the cut with the measurement attached. Two findings filed below the cut: pc-chaos-monkey-gk-blind, pc-node-entry-v1-suffix.
- Because: the row's outcome names the user-visible failure the kit found, and that failure's code site is the zero-chunk branch, so the row's premise ("gk_rescue releases it") was false, not its scope; the predicate the fix minted is the one decider (principle 8). A prompt clause the 4B broke in 2 of 3 is not structural (principle 10). Moving a row above the line is the operator's by the charter.

<!-- appendix -->

## phase-c-15 · 2026-10-02 — the gk fabrication MISS reopens its row on the zero-chunk branch

<details><summary>reasoning, evidence, package</summary>

Reproduced from the lane's raw transcripts (now target/ralph/phase-c/pc-gk-rescue-fabrication-measure/phase-c/gkm/runs/*.transcripts.jsonl, hash e2f387097, n=3 per cell, load per run in b07f61eb2's body):

- "Which conference room is booked for the Thursday partners' meeting?": `retrieved_chunks: []` and `gate_action: null` in all 12 firm-corpus runs, so gk_rescue never ran on it. Invented room or time: 4B before 1/3 ("Conference Room B"), after 1/3 ("the Boardroom"); 35B before 3/3 ("102", "Room 4B", "102."), after 2/3 ("Room B", "Room B (2nd floor)"). The other 4B answers are prefix-forced non sequiturs ("2017 was the year ..."). The branch (runtime/handlers/knowledge_query.rs:463) commits GK_CAVEAT_PREFIX at :548 for every question except the lost-corpus disclosure. `question_is_situation_deictic` (runtime/anchoring.rs:330) already matches the probe ("the" + weekday).
- "Which conference room is the Halvorsen Marine kickoff meeting held in?": `gate_action: gk_rescue_released` at 4B before 3/3 with a room in 1 ("Conference Room ..."; the other two "2026 is in the future"), after 2/3, both naming a room ("Conference Room 4B", read in after-4b-firm-r1); 35B released a plain decline 6/6. `rescue_precondition_met` already refuses an entity-anchored question (gk_rescue.rs:46), so entity anchoring was false on the sandbox's unenriched corpus.
- Other direction: world-general probes answered in every arm (b07f61eb2).
- "What is our paralegal billing rate?" over the unrelated corpus: `released` with the GK prefix 3/3 per arm per model; the content is a public range at 4B and a description of the passages at 35B, with no firm value. That is the twin row's path, and it stays below the cut.
- chaos-monkey: after-35b-firm-r1.log prints honest 8/8, blatant-confab 0.00, VERDICT PASS over a run whose transcript has the invented room with `pass: false`; runs/invalid/after-4b-empty-r1.log scores 8/8 honest on 8 turns that all failed with 400. Filed as pc-chaos-monkey-gk-blind. The re-read row says to read transcripts, not the aggregate.

Removed ralph/lanes/pc-gk-rescue-fabrication.done along with the reopen: a fresh lane worktree from `cut` would carry it, and the pool takes the marker as the lane being done.

Recommendation for the operator: lift pc-gate-gk-exemption-deictic above the cut with the reopened row; it edits the same file and the same predicate.

Falsified if: after the reopened fix, the zero-chunk branch with the deictic decline still releases an invented room on the conference-room probe (then the branch is not the cause), or world-general zero-chunk questions lose their answer (then the predicate is too wide for this branch).

REVIEW-AFTER: widening the row's outcome from "gk_rescue" to "a knowledge turn" is read here as correcting a false premise, not as adding scope; the operator may read it otherwise.

</details>
