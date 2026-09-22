<!-- ledger -->

**the-link-5 · 2026-09-22 · tl-2-offline-leg-exports · director** — this commit
- Needed: the supervisor dispatched resolution attempt 3 of 4 on the same NEEDS_HUMAN the-link-4 already resolved ("further resolution sessions of this dispatch cycle: read the-link-1/2/4 in `ralph/DECISIONS.md` and stop — do not re-verify"). The only unblocking action remains `git push`, the charter's operator line.
- Chose: confirm the halt; no row edit, no re-verification. The package stays with its ask unchanged (options, costs, recommendation: push now). Attempt 4 reads the-link-1/2/4/5 and stops.
- Because: nothing landed since the-link-4 (only the supervisor's dispatch-log commit 5160a01d3); the venue facts re-checked in one command each (setsid absent, Mach-O build, no `ralph/STOP`); removing the package would re-dispatch a row that cannot run here — the stall-burn the-link-2 documented at 07:15/07:28.

<!-- appendix -->

## the-link-5 · 2026-09-22 — attempt 3 confirms the halt; the fork stays the operator's push

<details><summary>reasoning, evidence, package</summary>

**The fork.** Same fork as the-link-4, re-presented by the supervisor's
retry loop: (a) push so the Halo can run tl-2's DEMO checks — operator-only
(charter "Leave these for the operator: Pushing…"; AGENTS.md: never push
without the operator); (b) a host-local way to run the room demo — none
exists, the-link-1 declined faking one; (c) edit the row to lie — forbidden.
(a) is the fork and it is not mine to take. the-link-4 decided this state
this morning; re-deciding it is the added scope the charter forbids.

**Evidence reproduced this session (2026-09-22, cheap checks only, per the
package's own instruction not to re-verify):**

- `git log --oneline e8bf6c69c..HEAD` → only `5160a01d3` (the attempt-2
  dispatch-log commit). No campaign work since the-link-4.
- `git rev-list --count origin/main..HEAD` → 43 (was 41 through af264fe3;
  the delta is ralph housekeeping commits, no product change).
- `ls ralph/STOP` → absent. The halt is the package, not an operator STOP.
- `command -v setsid` → absent; `file target/debug/sovereign-cli` → Mach-O
  64-bit arm64. the-link-1's venue findings still hold.
- `ralph/next/the-link/STATE.md:56` → the row still parks `[ ]` with the
  verbatim Halo completion path (the-link-1) intact.
- the-link-1 (c2616bf51), the-link-2 (235c737c1), the-link-4 (e8bf6c69c)
  read in the ledger; none of their premises has moved.

**The decision.** Confirm and stop. `ralph/NEEDS_HUMAN.md` stays in the
tree; its §"Director resolution" now names the-link-5 in the read list and
§c's commit count is corrected 41 → 43. The recommendation stands: push
now; the Halo runs the row's three verbatim commands and `ralph-mark.sh`
closes the row there; the demo the operator direction of 2026-09-21
requires becomes runnable today. If the push will not happen soon, the
campaign stays stopped — that is the honest state.

**What would falsify this decision:** a push landing and the Halo closing
the row (success — the package is then removed per its §Resume); setsid or
an ELF toolchain appearing on this host (reopens the-link-1); a commit
touching `scripts/ring-room-demo.sh` or the row's build spans landing
without a new verification (reopens the-link-4's evidence).

**The worker's package:** `ralph/NEEDS_HUMAN.md`, kept in the tree — its
ask (§"Director resolution" and §c) is the operator-facing record and is
unchanged in substance.

</details>
