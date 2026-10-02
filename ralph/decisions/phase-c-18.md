<!-- ledger -->

**phase-c-18 · 2026-10-02 · pc-partial-decline-verdict · worker** — this commit
- Needed: the reopened row names absent-professor-realname ("The passages do not answer: real, legal name.") as a coverage miss to be fixed by the shape the absence phrases share; the census found that text is the citation multiquote's own render, which also follows correct answers.
- Chose: leave "answer" out of the decider's telling stems; pin both professor and present-shop-street as texts that must NOT read as declines; correct the row's premise rather than flip three full answers. 13 of the row's 14 misses are fixed.
- Because: citation.rs:683 renders "The passages do not answer: <part>" only beside a part it grounded. In the same 4B sa reading the render follows a correct answer of the asked fact in present-shop-street ("Brett Street"), prov-mother-almshouse and present-asst-commissioner, all Grounded; covering the shape flips those three (about 7% of the 4B sa turns), past phase-c-17's own falsifier (the chat-ask rate, 1 in 24). The text cannot say which part was asked; the multiquote knew, and that is where a fix would read it.

<!-- appendix -->

## phase-c-18 · 2026-10-02 — the multiquote's "do not answer" render is not read as a decline

<details><summary>reasoning, evidence, package</summary>

Measured with a Python port of old and new decider over all 185 turns of the pdm reading (lane target/ralph/phase-c/proto.py; raw in the main tree's target/ralph/phase-c/pc-partial-decline-verdict-measure/phase-c/pdm/runs). With "answer" as a telling stem, eight 4B sa turns in that render read as declines: absent-professor-realname (right), present-anarchists-parlour, present-bomb-target, prov-michaelis-apostle, distract-explosion-victim (the declined part is the asked one: arguably right), and present-shop-street, prov-mother-almshouse, present-asst-commissioner (the asked fact was answered: wrong).

Falsified if: a multiquote turn whose grounded part is NOT the asked fact is common enough on the next reading (pc-partial-decline-verdict-measure-2) that the professor-shaped miss outweighs the three answered ones; then the fix belongs at the multiquote, which knows its parts, not in the text decider.

REVIEW-AFTER: whether the multiquote should carry which part was the question's own, so the ledger can read a declined asked part structurally (principle 10). Not this row's lift.

</details>
