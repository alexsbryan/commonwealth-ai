<!-- ledger -->

**five-programs-9 · 2026-09-24 · fw-1 (close at −1) · director** — this commit
- Needed: fw-1 halted with no executable half left (ctl/NEEDS_HUMAN.md, session 6) and asked whether the wave closes at −1 against its −8 target; fw-2 and fw-3 both `depends [fw-1]`, so the queue cannot move while fw-1 is `[~]`.
- Chose: CLOSE fw-1 as `[x] 49578c1e2` (its last code commit). Its residue already lives on its own rows, each carrying its own gate: fp-42 (parked on the C-toolchain cost, five-programs-8), fp-9 (membership bootstrap + LAN advertisement have no serving owner), fp-47's publish half (where the `cwth/app/0` claim registry lives), fp-54's guest-stamp re-mount. Those rows stay `[ ]` unchanged; the wave is not held open for any of them.
- Because: fw-1 is a WAVE row whose header already says the fp-44/45/46/47/42/9 rows "remain as verification stubs" — folding/splitting rows is the charter's call, and a wave held `[~]` for halves the director cannot decide stalls the whole queue (`Queue.current()` returns the first ACTIVE row regardless of deps, scripts/ralph.py:462-470) while deciding nothing. The −8 target was minted before the census; the census (SCOUT FINDINGs on fp-9/47/54, five-programs-8 on fp-42) is the input, and it says 7 of the 8 edges need an operator answer, not more cutting.

<!-- appendix -->

## five-programs-9 · 2026-09-24 — fw-1 closes at −1 (63 → 62); its operator-gated halves stay on their own rows

<details><summary>reasoning, evidence, package</summary>

Reproduced before deciding (director, 2026-09-24, toolbox host):

- `cargo xtask boundary-gate` from corpus-engine/ → `boundary-gate FAILED (62 violation(s))`, EXIT=1. fw-1 session 1 recorded 63; the one counted closure is fp-54's flip at 24070aeb8. `sovereign-daemon/Cargo.toml:26` names `commonwealth-rail-core` only — the daemon→commonwealth-rail edge is gone.
- 49578c1e2 exists on `cut` (fp-45's serve half, 17 files); 6bb5b6090 is its rustfmt. Tree clean at 7bfb5cc34.
- Readiness after the mark (Queue parsed by scripts/ralph.py, fw-1 treated done): ready = fw-2, fw-3, fp-54, REVIEW-mint-fp-core-dial, fp-7, fp-8, fp-9, fp-14, fp-47. The serial loop serves fw-2 next (file order, STATE.md:107).

What the close does NOT decide: none of the four operator forks. fp-54, fp-9 and fp-47 have their deps met and sit below fw-4/fp-55 in file order; when the loop reaches one, its worker will find the recorded SCOUT FINDING and halt with the fork already written — that is the honest state (the charter's BLOCKED-with-reason; there is no BLOCKED mark, ralph.py:47). Minting a HUMAN- gate for them now would be a second row where the existing finding already does the job.

Refused alternatives:
- Hold fw-1 open for one of the four halves: each needs an operator answer (new auth machinery for guest stamps; a membership-bootstrap owner; the claim-registry home; a C toolchain in the lifted cw-rails binary). Holding the wave blocks fw-2/fw-3, which need none of them.
- Rewrite the −8 target to −1 retroactively: the row keeps its target and progress lines as written; the shortfall is recorded here, not erased.

Falsified if: fw-2 or fw-3 turns out to need one of fw-1's uncut halves (a port or fold consumer that only exists after fp-9/47/54/42's dial) — then the dependency was real, and the row that hits it should depend on the specific stub row, not on a reopened fw-1.

</details>
