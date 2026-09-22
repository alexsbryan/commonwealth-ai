<!-- ledger -->

**the-link-6 · 2026-09-22 · REVIEW-DEMO-the-link-run · director** — this commit
- Needed: The row's expected table ("three `tl-*` PASSED", exit 0 — STATE.md:60, HANDOFF.md:63-64, the-link.toml:21) contradicts the instrument tl-2-offline-leg-exports built, which prints `tl-link-carries-its-couriers` and `tl-dial-measured` as UNCONDITIONAL COULD-NOT-JUDGE naming their proof homes (ring-room-demo.sh:2349-2355, comment :2312-2317 citing ARCH 5), so exit 0 is unreachable. All five checks ran to completion on the Halo: rr-1 at its A38 baseline exactly (answer 0.8 / plug-in 0.0 on `c_answer_names` are the two expected reds, exit 1); both cold room runs read `tl-checkpoint-verifies` 1.0 with all four legs true and `created_unix` strictly inside the cut windows, the three forgeries refused by name, everything else PASSED, no FAILED clause.
- Chose: Option 1 of the package — accept the instrument's table as the honest reading. Correct the predicate (the-link.toml:21) to name `tl-checkpoint-verifies` PASSED with the two designed CNJs; align STATE.md:60 and HANDOFF.md:63-64; mark REVIEW-DEMO-the-link-run `[x]` on these runs (logs: `target/ralph/tl-rr1-regression.log`, `tl-room-run-1.log`, `tl-room-run-2.log`).
- Because: The order implies the fix — Demo §3 names `ralph/DECISIONS.md` as the dial bar's proof home and Demo §2's proof is the builder/parser/QR round-trip, i.e. the test suite; neither asks the room run to re-run them. Forcing it would embed cargo in a demo whose contract is "measures binaries, never the tree" (HANDOFF step 3) and add a second row where one correction suffices (charter: strictly necessary). ARCH 5: CNJ is an unmeasured bar making no claim, never a pass — printing PASSED would read 1.0 on kindness, the bars' own goodhart language. the-link-1 already recorded "demo exit 4 … is the four-verdict rule working, not a failure", and tl-2 closed on exactly this table (f6fb719e9; its acceptance, STATE.md:59, required only the six rr-2 and five rg PASSED).

<!-- appendix -->

## the-link-6 · 2026-09-22 — the demo's two designed CNJs are the pass shape; the row's expected table was the stale premise

<details><summary>reasoning, evidence, package</summary>

**The fork.** The built instrument and the row's expected table disagree, and
the campaign predicate cannot be read as written. Two options were on the
table: correct the three documents to match the instrument, or add a build
row giving the two unmeasured bars in-demo legs and rerun both cold room
runs.

**Why the correction is the covered fork.** The charter's "fixing a row
whose premise the tree contradicts, when the order already implies the fix"
— the order step is O §Demo: §3 states the dial item AS the decisions
appendix ("`ralph/DECISIONS.md` carries the dial entry … No product code
gates on any of it") and §2's claim is a builder/parser/QR round-trip, the
exact clauses tl-2's cargo tests prove. The room run exercises neither
surface: it mints no `at=`/`iroh=` link (D1/D2 keep `at=` to digest marks
and couriers on the join link's param) and measures no wasm build. The
instrument says exactly this, by name, in its CNJ reasons.

**Why not the build row.** It fails "strictly necessary" three ways: a
second row where one correction does; cargo inside a demo that "measures
binaries, never the tree" (HANDOFF step 3), reading tree state in-run; and
two more cold room runs (~1 h of podman) to produce PASSED the floors never
asked the run to produce. It also moves the verdict in the direction the
change wanted — ARCH 7's tell.

**Evidence (reproduced this session):**
- `scripts/ring-room-demo.sh:2312-2317` (comment, ARCH 5) and `:2349-2355`
  (the two unconditional `row(..., None, …)` calls).
- `scripts/lib/demo_verdicts.py:17-19` — exit 1 if any FAILED, 4 if any
  COULD-NOT-JUDGE, 0 when every bar PASSED; "an unmeasured bar makes no
  claim and is never a pass".
- `target/ralph/tl-room-run-1.log:62-64`, `tl-room-run-2.log:62-64` —
  `tl-checkpoint-verifies` value 1.0, legs a-d all true, run1 created_unix
  1790098351 in cut 1790098311..1790098375, run2 1790098624 in
  1790098584..1790098648; verify exit 0 printing marks; flipped → "step 2
  (admit) … signature"; truncated → "step 3 (digest) … marks disagree —
  actor …"; same-seq pair → "used one sequence number twice (#12) — the
  document forks"; the two CNJs with their proof-site reasons.
- `target/ralph/tl-rr1-regression.log` — answer 0.8 FAILED, doc 1.0, film
  1.0, plug-in 0.0 FAILED on `c_answer_names`, nothing-typed 0 PASSED: the
  A38 baseline unmoved (DECISIONS A38; the same shape the rr-2 close
  recorded).
- Consistency: the-link-1 (DECISIONS.md:8108-8111) already expected the two
  CNJs and called exit 4 "the four-verdict rule working, not a failure";
  tl-2's close f6fb719e9 accepted exactly this table under an acceptance
  line that required only six rr-2 + five rg PASSED beside the tl rows.
  STATE.md:60 / HANDOFF.md:63-64 / the-link.toml:21 are the three documents
  left behind.

**Nothing reopens tl-2 or tl-3:** their proofs exist and passed — 13,701
tests green on the Mac (HANDOFF.md:13-14), the dial entry in DECISIONS.md
(the-link-3). The regression sets (C2, C3, rr-1 baseline) are untouched;
rail-core untouched; no product file touched by this resolution. The empty
`the-link-6.md` template a prior session minted but never filled is
repurposed for this entry, so the ledger stays dense.

**What would falsify this decision:** a run where `tl-checkpoint-verifies`
reads below 1.0, a FAILED clause on any rg/rr-2 bar, or a CNJ whose reason
no longer names its proof site — any of those reopens the row. And if a
future order gains a demo leg that mints an `at=`/`iroh=` link or moves the
dial numbers into the run's reach, the two bars become measurable and the
predicate must name them PASSED again — this correction then reads as the
stale premise.

</details>
