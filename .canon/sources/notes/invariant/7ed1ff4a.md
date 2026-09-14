# INVARIANT 4dd9c7ab, FIFTH OCCURRENCE (2026-08-21) — AND THE MECHANISM WAS NEW. In a shared checkout, git diff --name-only IS A…

INVARIANT 4dd9c7ab, FIFTH OCCURRENCE (2026-08-21) — AND THE MECHANISM WAS NEW. In a shared checkout, `git diff --name-only` IS A PEER-CONTAMINATED LIST, and any script that WRITES files must take an explicit path list.

WHAT HAPPENED: nc-18's scripted import-repair pass iterated over `git diff --name-only` to find files needing a fix. In this shared checkout that command returns the PEER's dirty files too. It hoisted one function-local `use std::sync::Arc;` into the header of two files belonging to nc-17:
  sovereign/crates/sovereign-contracts/src/traits.rs        (was inside `complete_stream_with_finish`)
  sovereign/crates/sovereign-inference/src/embedded/model_slot.rs (was inside `mod queue_gauge_tests`)
Blast: two lines, one per file. No commit was made; both were uncommitted working-tree state.

WHY THIS IS NOT COVERED BY THE FIRST FOUR OCCURRENCES. Those were all about the INDEX and the COMMIT: `git add -A`, a bare `git commit` taking the whole index, and (occurrence four) a worker sweeping a peer's uncommitted doc hunks. Every existing remedy — `git commit --only -- <paths>`, the private index, `git show --stat` after every commit — operates at commit time. THIS ONE NEVER REACHED THE INDEX. A helper script whose INPUT was the whole dirty set wrote into peer files directly. No commit-time guard can catch that, because the damage is done in the working tree before staging is considered.

THE RULE: any script that WRITES must take an explicit allowlist of your own paths. Deriving the list from `git diff --name-only`, `git status --porcelain`, or any other whole-tree query is the defect. nc-18's corrective was to drive every scripted pass off an explicit list of its 68 files.

HANDLED WELL, and worth copying: the worker SELF-REPORTED before the seat asked, repaired both files, named the exact blast radius, and flagged the risk window (had the peer committed in between). It also caught its own bad first restore — it anchored `model_slot.rs` on the wrong `Ordering` import — and corrected it, which is why it ran a verification grep at all.

SEAT VERIFIED THE REPAIR rather than relaying it: `git diff` on each file returns ZERO added `use std::sync::Arc;` lines, and neither file appears in any recent commit — the last commits touching them are 7ec40482 and d74d5e79, both long predating this wave. The risk window closed clean.

DETECTION FOR THE VICTIM SIDE: before committing, `git diff -- <your file> | grep '^+use '` and ask whether you wrote each one. A stray header import is the cheapest possible symptom of this class and the only one visible without a peer telling you.
