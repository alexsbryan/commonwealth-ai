<!-- ledger -->

**five-programs-1 · 2026-09-22 · fp-0 CLEAN trip report · director** — this commit
- Needed: the fp-0 CLEAN gate tripped (target/debug 288G ≥ the 256G ceiling; 307.3GiB removed, rebuilt 4m45s, green) and the loop stopped on the trip report; the package asked whether the build-latency campaign re-baselines, and how the CLEAN trip should reach a human.
- Chose: (1) trip report accepted, campaign resumes (NEEDS_HUMAN removed this commit). (2) The trip is report-after by design — dev-build.sh couples the du and the clean in one invocation (scripts/dev-build.sh:105-118), so no worker can see "288G" without causing the clean; the campaign addendum now says so (§0 standing facts) instead of the base row's unactionable "say so in NEEDS_HUMAN if the gate trips". (3) No re-baseline of the build-latency campaign is taken by this campaign: bl banked its numbers 2026-09-17 and is not a live ralph queue (no ralph/next/bl-*); its numbers are historical measurements of the pre-clean tree, its own protocol re-warms the target when it resumes, and the event + the 4m45s post-clean cold figure are recorded for its next session (memory + sovereign note).
- Because: the ceiling's protective premise — that it guards a live warm-measurement campaign — is currently moot, and the overnight loop exists precisely to not stall on a sleeping human for a self-healing hygiene event (a clean costs one 4m45s rebuild and nothing else). The 256G threshold itself stays the operator's call; this decision changes only what the row truthfully tells the worker. dev-build.sh is left untouched: its `--clean` is the operator's own explicit verb, and a confirm gate there changes shared tooling behaviour (charter: operator).

<!-- appendix -->

## five-programs-1 · 2026-09-22 — the fp-0 CLEAN trip is report-after by design; no bl re-baseline; campaign resumes

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/five-programs/ctl/NEEDS_HUMAN.md (removed this commit). Every fact in it was reproduced:

- `scripts/ralph-check.sh clean` (host) → exit=2, refuses before any du ("no C toolchain"), names the toolbox form — the clean verb is a builtin (scripts/ralph-check.sh:51) with RALPH_CLEAN_MB=262144; five-programs' queue.toml declares only boundary+compile.
- `dev-build.sh --clean --gate-only` (toolbox): the du and `cargo clean` are one conditional (scripts/dev-build.sh:105-118); no confirm path exists. When it fires, the build-after-clean runs (the 2026-09-16 comment explains why: the clean removes the binaries the loop's checks call).
- The worker's trip: 288G ≥ 256G → "Removed 182602 files, 307.3GiB" → full workspace rebuild green 4m45s / 1140 crates + smoke. Verified aftermath: target/debug is now 15.9G.
- fp-0 itself: 475f0bc0e + 4fab978c2 touch only ralph/next/five-programs/STATE.md; queue row fp-0 marked [x]. Boundary gate re-run today: 79 violation(s), unchanged, as doc-only commits predict.
- The base CLEAN row (ralph/PROMPT.base.md:112, rendered into the worker prompt) says "a clean is the operator's call, say so in NEEDS_HUMAN if the gate trips" — unactionable at run time given the atomic du+clean; the worker's NEEDS_HUMAN was necessarily a trip report. The row's protective purpose (the 256G ceiling exists so a clean cannot destroy the build-latency campaign's warm numbers) is currently moot: bl banked 2026-09-17, no live bl queue.

The correction lands in the five-programs addendum §0 (this commit), not in ralph/PROMPT.base.md or dev-build.sh:

- PROMPT.base.md's CLEAN row and the sibling campaigns' verbatim copies are ralph-wide surface, beyond this charter — REVIEW-AFTER: operator folds the same correction into PROMPT.base.md (and lets re-mints pick it up) rather than hand-syncing eight sibling copies.
- A confirm gate in dev-build.sh (the package's other named option) changes the operator's own explicit `--clean` verb and every campaign's CLEAN behaviour — operator territory under the charter.

What would falsify this: a future trip whose clean destroys something a rebuild does not restore (e.g. bl re-arms and holds measurements in the tree) — then report-after is wrong and the operator-first confirm design becomes right; or bl's resumed comparisons shown to depend on pre-clean target state — then the 2026-09-17 banked numbers need a re-baseline against the post-clean tree (post-clean cold full workspace: 4m45s / 1140 crates, debug profile, treesitter+dev-tools).

</details>
