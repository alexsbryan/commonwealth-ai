<!-- ledger -->

**the-link-7 · 2026-09-22 · REVIEW-audit-the-link · director** — e9d0c4f85 · 49b664b2e · this commit
- Needed: The audit body was done and committed (5d44de59f) but the row stayed `[~]` on its two named checks, both red on causes the campaign's diff does not touch. TESTALL 13701/1: `every_journey_cites_a_doc_that_exists` — mesh-offers-catalogue cites `docs/internal/RING_APPLICATIONS.md`, gitignored per-host (`.gitignore:67`), absent here, cited since a3bd715f5, an ancestor of BASE. PREPUSH arch-gate [hard]: `runner.rs` past slack and the approach band +446. The package posed both as operator forks; the tree contradicts that framing — a313c9c18 (the runner.rs growth) is ON origin/main, the branch's `origin/main..HEAD` diff has zero `.rs` changes, and the 2074 pin was already 45 lines stale (the file was 2119 when 5bd896d8f froze it).
- Chose: (1) The file: SPLIT, never re-pinned — the trailing `degraded_router_tests` module (100 test-only lines) moved to `runner/degraded_router_tests.rs` under `#[path]`, the `router.rs`→`router/posture_tests.rs` precedent; runner.rs 2133→2037, back under its pin (e9d0c4f85). (2) The band: re-pinned AT origin/main from a detached worktree per the 2026-09-21 direction's recipe, `approach_band.txt` ONLY — `oversized.txt`/`instruction_surface.txt` deliberately not copied back, because a file pin is never raised (49b664b2e). (3) TESTALL's red: recorded foreign per this queue's own precedent (A51 rr-2, A59 ring-guest — the identical red on this host), row closed on the campaign's share; restore-or-rename of the per-host doc stays the operator's (third report).
- Because: The direction makes the file-ceiling fix "never re-pinned, never an operator question" and prescribes the origin/main re-pin for ratchets in arrears; the charter's decide-list covers "fixing the code the gate names". The TESTALL red is not a ratchet and its fix (restore or rename a per-host doc this campaign never cited) is outside the charter's leave-list's spirit to guess at — closing on the campaign's share with the red honestly recorded is ARCH 5's four-verdict close, and it is exactly what A51 and A59 did.

<!-- appendix -->

## the-link-7 · 2026-09-22 — the audit's two red gates: one split, one re-pin at origin/main, one foreign red recorded

<details><summary>reasoning, evidence, package</summary>

**The fork.** The package offered: maintain the per-host doc locally, or bless
the foreign-red disposition (TESTALL); lane-owned split or baseline re-pin
(PREPUSH). The 2026-09-21 operator direction (AGENTS.md, definition of done)
post-dates the precedents and decides the PREPUSH fork outright: a file past
its ceiling "is SPLIT … never re-pinned, never an operator question", and any
other ratchet failure is re-pinned at origin/main, never absorbed from a
working tree. What remained was execution plus the TESTALL disposition.

**Why the split takes the tests, not the field.** a313c9c18's +14 is a struct
field (`SynthSnapshot.gate`), its doc comment, and three plumbing lines — a
field cannot move. The direction's remedy ("move what you added into a sibling
file") is applied to the nearest coherent test-only mass: the trailing
`degraded_router_tests` module, 100 lines, whose move is behaviour-preserving
by construction and follows the repo's worked precedent
(`sovereign-core/src/router.rs` → `router/posture_tests.rs`, `#[path]` so
names are unchanged). runner.rs lands at 2037, under the 2074 pin, so the
file ratchet greens WITHOUT touching any pin.

**Why the band re-pin is honest.** Measured in a detached worktree at
origin/main (4d733f98e): 206 files / 203149 lines, +446 over the 5bd896d8f
pin, banked by the grounding lane (value_presence.rs entered the band;
judge.rs, inner.rs grew), the f26 egress census split, the harness, and
the-link's own banked rows (deep_link, ring_cmd) — the same lanes
REVIEW_FINDINGS §the-link attributes. This branch adds no `.rs` lines outside
two band-invisible files (runner.rs > 1200, the new test file < 800), so the
fresh pin describes this tree exactly. `--tighten` was not run: banking the
split's shrinkage is optional polish, not needed for green (strictly
necessary).

**Why the TESTALL red is recorded, not fixed.** The test's own sentence
offers "rename the citation or restore the doc". Restoring means authoring a
per-host handoff doc whose content lives on the Mac; renaming means editing a
shipped contract citation the ring-apps lane owns — both change observable
contract surfaces beyond anything this row states, which the charter reserves.
A51 (rr-2) and A59 (ring-guest) both closed audits on this identical red. It
is the third report to the operator, per A59's own counting.

**Evidence (reproduced this session):**
- `git merge-base --is-ancestor a313c9c18 origin/main` → yes; `git diff
  --numstat a313c9c18..origin/main -- runner.rs` → empty; numstat 5bd896d8f..a313c9c18
  → +14; `git show 5bd896d8f:…runner.rs | wc -l` → 2119; pin `oversized.txt:74` → 2074.
- `git diff --numstat origin/main..HEAD -- '*.rs'` → empty (branch adds no code).
- Split: scoped sovereign-lint clean (`--all-targets`, 2 crates); sovereign-cli-llm
  1151/1151 green; runner.rs 2037 lines.
- Re-pin: worktree `arch-gate --update-baseline` → "206 files / 203149 lines";
  this tree `cargo xtask arch-gate` → exit 0; `scripts/ralph-check.sh prepush`
  → exit 0, "all gates passed" (size-gate + deletion-manifest advisory-failed as
  designed; concept-gate declared could-not-judge).
- TESTALL red: `--package sovereign-cli --filter
  every_journey_cites_a_doc_that_exists` → 0 passed / 1 failed, "mesh-offers-
  catalogue cites `docs/internal/RING_APPLICATIONS.md`, which does not exist" —
  the same sentence the audit recorded.
- rail-core diff EMPTY (invariant c3fed9c3); regression sets C2/C3/rr-1
  untouched; no D1-D4 surface touched by any of the three commits.

**What would falsify this decision:** a test red traceable to the split (none
observed; the moved module's three tests run green in the new file); the band
pin drifting from this tree again without a code cause (would mean an agent
re-ran --update-baseline on a dirty tree — the recipe forbids it); the
per-host doc resolving on a fresh clone (it cannot — `.gitignore:67` ignores
`docs/internal/` wholesale, so the red is per-host by construction and stays
the operator's to restore or rename).

**Worker's package (NEEDS_HUMAN, inline):** unit REVIEW-audit-the-link; audit
body DONE at 5d44de59f (falsifier's reading, the twelve, REUSED report,
rail-core EMPTY); row held only on the two reds; commands and outputs quoted
in the package match this session's reproductions (arch-gate's two ✗ lines;
TESTALL's one failure).

</details>
