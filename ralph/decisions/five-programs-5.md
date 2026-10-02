<!-- ledger -->

**five-programs-5 · 2026-09-23 · fp-4 (CLEAN trip, report-after) · director** — this commit
- Needed: fp-4's §5 CLEAN tripped the control file after the unit landed: the worker invoked `dev-build.sh --clean --gate-only` DIRECTLY, which does not set `RALPH_CLEAN_MB`, so dev-build.sh's 50G default applied instead of the queue's 256G ceiling and the 90G debug target was cleaned (131,645 files, 105.4GiB) mid-campaign. The package asks for clearance only ("nothing blocks fp-4").
- Chose: Clear the control file (kept as `ctl/NEEDS_HUMAN.resolved-clean-20260923.md`, the three-prior-trip precedent). No row edit — fp-4 is `[x]` DONE at ee9333db4 and honest as written. No code change and no escalation: the threshold split is configuration (dev-build.sh owns the 50G default, ralph-check.sh:51 sets the queue's 262144 override), not two deciders of one value, and the failure was procedural — the §5 command invoked outside its wrapper.
- Because: The event already happened and was recovered from (workspace dev rebuild green, 4m37s; smoke passed; target/debug back at 23G this session). Nothing remains for the operator to decide: initiating a clean is moot post hoc, and none of the charter's operator-list items applies — the loss is local dev state, not end-user-observable behaviour. The resolution mandate ("remove NEEDS_HUMAN.md so the campaign resumes") covers the removal outright.

<!-- appendix -->

## five-programs-5 · 2026-09-23 — fp-4's report-after CLEAN trip cleared; loop resumes; no row, code, or threshold change

<details><summary>reasoning, evidence, package</summary>

Package: `ralph/next/five-programs/ctl/NEEDS_HUMAN.md` (kept as `ctl/NEEDS_HUMAN.resolved-clean-20260923.md`; removed as a control file this commit, so the loop resumes). Gate count unchanged by this commit — it touches queue/doc state only, no Rust: **boundary-gate 63**, the count fp-4 left (STATE.md fp-4 row, "65 → 63").

**Package facts reproduced before deciding (principle 4):**

- `scripts/dev-build.sh:105`: `limit_mb="${RALPH_CLEAN_MB:-51200}"` — the 50G default the package names (comment at :83-84 states it in words). `scripts/ralph-check.sh:51`: `clean) RALPH_CLEAN_MB="${RALPH_CLEAN_MB:-262144}" run build 5 ./scripts/dev-build.sh --clean --gate-only` — the 256G queue ceiling; its header comment (:26) says at that ceiling the clean "is a du and never runs". So via the wrapper, 90G would not have cleaned; direct invocation cleaned. Mechanism confirmed.
- fp-4 landed: STATE.md line `[x] fp-4 ee9333db4 — DONE (2026-09-23) … CLEAN TRIPPED — ctl/NEEDS_HUMAN.md`; commits ee9333db4 + c4665ab5 ("ralph: fp-4 done") in `git log`. Working tree clean before this commit.
- Rebuild state: `target/debug` exists at 23G this session, consistent with the package's green 4m37s workspace rebuild after the 105.4GiB removal. Not re-verified by a build here — the loop's own next unit builds first and halts loudly if the package's green claim were false.

**Why no code change.** The tempting fix — make `dev-build.sh --clean` refuse without an explicit `RALPH_CLEAN_MB`, or raise its default — changes behaviour for every caller of dev-build.sh outside this queue and adds scope the charter's strictly-necessary rule forbids. The threshold is not duplicated: one owner (dev-build.sh's default), one queue override (ralph-check.sh's env), which is configuration layering, not the principle-8 smell. The failure mode is a worker skipping the documented §5 path; the lesson lives in this entry and in the kept package, where a future worker will find it.

**Cost accepted, recorded so its owner can find it:** the clean destroyed the warm-build state the build-latency campaign measures against — the reason the queue ceiling is 256G and a clean is normally the operator's call to initiate. That campaign's next measurement will read cold. This entry is the record of why; nothing re-baselines silently.

Loop mechanics: closures counter stays **4** — no closure, none claimed (fp-4 was already counted; the fp-34 precedent). Next served row is the queue's own `current()` from the dep-satisfied open set.

**What would falsify this.** (1) If the loop's next build fails where the package claimed green, the rebuild verdict was wrong and this clearance cleared a broken tree — the halt signature would be immediate and ordinary repair follows. (2) If the build-latency campaign's next report shows a step-change it cannot explain, this entry is the cause; its owner re-mints the warm baseline. (3) If a future worker trips the same direct-invocation clean, the procedural lesson did not hold and the structural fix (refusing `--clean` without an explicit ceiling) becomes the smallest change — that recurrence, not this trip, would justify it.

</details>
