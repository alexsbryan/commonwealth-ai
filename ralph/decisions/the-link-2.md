<!-- ledger -->

**the-link-2 · 2026-09-22 · tl-2-offline-leg-exports · director** — this commit
- Needed: the loop's picker (`scripts/ralph.py` `current()`) returns the first `[~]` row unconditionally, so with tl-2 parked `[~]` on Halo-bound checks it re-dispatched the same unrunnable unit every iteration (log: 06:52, 07:11, 07:15:57, 07:28:57 — stalls 1 and 2 burned) and would until attempt 4 killed the campaign.
- Chose: flip tl-2 `[~]` → `[ ]` (not `[x]`) and move `tl-3-dial-measured` above it, so "first ready `[ ]` row" is tl-3 — the-link-1's own flow note. No `scripts/ralph*` edit; the ei7 files stay uncommitted (live peer session).
- Because: the queue grammar has three marks (ralph.py:398-401) and `[~]` is always picked; the only honest not-picked state is `[ ]` with the wait named in the row, which the row's text already carries verbatim (decision the-link-1, the Halo path). `ralph-mark.sh` accepts `[~]` OR `[ ]` (`^- \[[~ ]\]`), so the Halo completion path is unchanged.

<!-- appendix -->

## the-link-2 · 2026-09-22 — tl-2 waits as `[ ]`, tl-3 first; the picker's file order now matches the-link-1's flow note

<details><summary>reasoning, evidence, package</summary>

**The fork.** The package (ralph/NEEDS_HUMAN.md, removed by this commit) asked
the queue to "route this host to tl-3 while tl-2 waits" and said the mechanism
was the director's call. Its §c1 (make the picker skip tl-2) and §c2 (leave
tl-2 `[~]`) are in tension: `Queue.current()` returns the first ACTIVE row
unconditionally — scripts/ralph.py:463-466 — so a `[~]` row cannot be
skipped, only re-dispatched. The grammar (ralph.py:398-401) offers exactly
three marks.

**Evidence — every package claim reproduced (2026-09-22):**

- `git log --oneline -3` → `c2616bf51` (the-link-1) atop `4f08eafdc` (tl-2's
  code): nothing remains to build on this host for tl-2.
- `ralph/log-the-link.txt` → dispatches at 06:52:56Z, 07:11:27Z, 07:15:57Z
  ("no commit this iteration (stall 1/3)"), 07:28:57Z ("stall 2/3"), then
  NEEDS_HUMAN and this resolution session.
- `scripts/ralph.py:463-470` (read): first ACTIVE row wins; else first
  PENDING row with deps met, in file order. tl-2 is line 55, tl-3 line 56 —
  with tl-2 `[~]` OR `[ ]`-above-tl-3, the picker lands on tl-2 either way;
  hence the flip AND the reorder are one fix.
- `scripts/ralph-mark.sh` (read): rewrite regex `^- \[[~ ]\] ${unit} —
  depends` — the Halo's mark lands over `[ ]` exactly as over `[~]`. §c2's
  concern (the ruled completion path) is untouched by the flip.
- `work_in_flight --scope=.../summary_verify.rs --match_mode=file` →
  observation from session `608c3fc9`, node-37f17554b6c4ff29,
  node_is_self=true, confidence=recent: the ei7 dirty files
  (`summary_verify.rs`, `raptor_atlas.rs`, `summary_verifier_instrument.rs`)
  are a live session's mid-flight work. The dispatch preamble's "commit it as
  you go" is rightly NOT applied to them; committing them bakes a half-state.

**Options and why the smaller one won:**

- Runner change (a fourth "parked" mark or a skip rule in scripts/ralph.py):
  peer-observable tool behaviour shared by every campaign; the charter's
  "Decide these" names rows and queue mechanics, not the runner — and the-link-1
  already declined tool edits on exactly this ground (the macOS `demo-bg`
  hazard). Not strictly necessary once the queue encodes the wait. Rejected.
- tl-2 → `[x]`: dishonest (the demo never ran) and explicitly forbidden by
  the package itself. Rejected.
- tl-2 → `[ ]` + tl-3 above it: two reversible row edits in the one file the
  charter puts under the director. The row text already states the whole
  Halo-bound completion path with the decision cite, so a future worker
  dispatched on it (after tl-3 lands) reads why there is nothing to commit
  here and §6-stops with an honest package instead of inventing work.

**Known tail, accepted:** after tl-3 lands, the picker reaches tl-2 (`[ ]`,
deps met), the worker cannot commit, three stalls halt the loop, and the
supervisor packages it — converging to "operator: push" within its
resolution budget. That halt is the honest terminal state the-link-1 already
documented ("after tl-3 the queue is honestly exhausted pending the Halo");
pre-solving it would need new machinery or a falsified dependency. Not worth
it (charter: strictly necessary).

**What would falsify this decision:** `Queue.current()` changing to skip
ACTIVE rows (then `[~]` becomes parkable again and the flip can revert), or
the Halo path landing a mark on a `[ ]` row failing (it cannot today — the
regex above — but a ralph-mark change would reopen this).

</details>
