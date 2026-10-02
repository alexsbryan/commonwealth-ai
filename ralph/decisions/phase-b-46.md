<!-- ledger -->

**phase-b-46 · 2026-09-28 · pb-ingest-dial-tools-local → the finish marker is repaired; no premise changes · director** — this commit
- Needed: the loop refused to dispatch -local because its row "lacks '- finish:'". phase-b-45 had rewritten the line as `- finish (rewritten by phase-b-45; …):`, and the dispatch check (`Queue.unmet_requirements`, scripts/ralph.py:562) looks for the literal substring.
- Chose: move the colon to read `- finish: (rewritten by phase-b-45; the −1 premise was false) retires …`. No other text changes. The census, trial, PROOF and LIFT stand as phase-b-45 wrote them.
- Because: charter "a false row premise" does not apply, since the premise is intact and only the marker was malformed. The smallest change that lets the campaign flow is the one-character move. Boundary gate: EXIT=1, 23 violations (`cargo xtask boundary-gate` from corpus-engine/, this session). This commit touches no Rust.

<!-- appendix -->

## phase-b-46 · 2026-09-28 — pb-ingest-dial-tools-local's finish marker was malformed by phase-b-45

<details><summary>reasoning, evidence, package</summary>

Reproduced this session at f72a2cffb:
- `Queue(STATE.md).unmet_requirements(<-local>, ("- finish:", "- trial"))` returns `['- finish:']` on the committed tree and `[]` after the edit.
- The same check, run over every open row, reports no other row missing a marker, so the next dispatch will not halt on the same slip.

What would falsify this:
- The loop halts on -local again with a dispatch-requires message. That would mean the check reads something other than the row block.

Prevention: a director rewrite that annotates a marker line puts the annotation after the colon (`- finish: (rewritten by …)`), never before it.

</details>
