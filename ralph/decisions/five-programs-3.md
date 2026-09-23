<!-- ledger -->

**five-programs-3 · 2026-09-23 · loop mechanics (no queue row) · director** — this commit
- Needed: the supervisor halted the campaign on "3 iterations without a commit". All three worker iterations died in ~0.7 s: the loop's model is `zai-coding-plan/glm-5.3-flash` (261f1ae6) but `worker_bin` still named `scripts/ralph-claude-shim.sh`, and the claude CLI on this host rejects that model id (`[claude-code:unrecognized_model]`, `result: success turns=1 cost_usd=0` — no-op successes, no commits).
- Chose: the worker binary becomes the loop's builtin — `worker_bin = ""` (ralph.py:950 falls back to `RALPH_OPENCODE_BIN`, then plain `opencode`) and `settings = ""` (the claude settings env is only set when non-empty, ralph.py:945). The tree already held this edit uncommitted; it is verified, not merely accepted: `opencode` 1.18.32 is on PATH and `opencode run --model zai-coding-plan/glm-5.3-flash` answers in the loop's exact invocation shape (ralph.py:995). STATE.md's `[~]` on REVIEW-audit-fp-auto-1 is kept — the audit unit is genuinely in progress (due since 5d5491b7) and is the row the loop serves next.
- Because: 261f1ae6 already took the model decision ("the loop runs on the house model … not claude-opus-5"); a binary that cannot run the chosen model is not a competing decision but an incomplete one, and finishing it is the smallest change that makes the campaign flow. No queue row's premise is touched; no Rust source changes; boundary-gate stays at its re-run count of 68 (target/ralph/five-programs/boundary.log, 2026-09-23 13:07), delta 0. LINT is a never-ran, not a pass (principle 5).

<!-- appendix -->

## five-programs-3 · 2026-09-23 — the loop's worker runs opencode (builtin), because the claude shim cannot run the house model

<details><summary>reasoning, evidence, package</summary>

Package: `ralph/next/five-programs/ctl/NEEDS_HUMAN.md` (removed this commit; no `ctl/STOP` exists — the supervisor had already cleared the old one — and none was touched). Evidence, all reproduced before deciding:

- The three failed iterations are on disk: `target/ralph/five-programs/iter-{1,2,3}.out` (2026-09-23 13:29-13:30), byte-identical failures — the shim's claude CLI starts, connects MCP, then exits on `unrecognized_model` for `zai-coding-plan/glm-5.3-flash` in ~672 ms. The stall detector (`--max-stall 3`) counted three no-op iterations and wrote the package. That is the watched failure.
- The fix was already staged in the working tree (queue.toml: `worker_bin` shim→empty, `settings` claude-json→empty, with a dated comment) — a prior session diagnosed the same mechanism and ended without committing. Verified rather than trusted: `worker_bin: str = ""` is the manifest default (ralph.py:633); `worker_bin()` (ralph.py:950-956) resolves empty → `RALPH_OPENCODE_BIN` → `opencode`; `session_env` (ralph.py:945) sets `RALPH_CLAUDE_SETTINGS` only when `settings` is non-empty; the binary exists (`~/.opencode/bin/opencode`, 1.18.32). Watched success: `opencode run --model zai-coding-plan/glm-5.3-flash 'Reply with exactly: WORKER-SEAT-OK'` → `WORKER-SEAT-OK` — the loop's exact argv shape (ralph.py:995) with the manifest's exact model id (queue.toml `[models]`).
- Charter check: this is loop plumbing, not a row fork — no row premise, no TSV cell, no gate count moves. The supervisor-resolution mandate ("apply the smallest change that makes the campaign flow") covers it, and the model half was already the operator's committed decision. Tagging REVIEW-AFTER anyway because no charter clause names `worker_bin`.

Also landed in this commit, per the tree instruction ("inspect it and continue; commit as you go"): `landing/ring/` — the ring guest page's built deliverable (a separate lane's work, deferred by the previous director's package with "another campaign's deliverable to commit"). Committed separately with its provenance rather than mixed into this one.

**What would falsify this.** (1) A future iteration dying the same way under `opencode run` would mean the fallback is not what the loop invokes (e.g. a stale `RALPH_OPENCODE_BIN` in the supervisor's environment) — the check is the next iter-N.out naming opencode, not claude. (2) If opencode's non-interactive `run` proves unable to honour the queue's permission model (`ralph/claude-settings.json` carried permissions the shim relied on), sessions will surface permission auto-rejects — ralph.py:1020 counts them in the log and warns. (3) If the house model id is later renamed at the provider, every `[models]` cell needs the new id — the failure signature would again be instant, model-named exits.

REVIEW-AFTER: the charter does not name worker-binary plumbing. If a second campaign stalls the same way, the structural fix belongs in `scripts/ralph.py` (refuse a `worker` model the declared `worker_bin` cannot run, at manifest load) — out of the director's reach by the same line that forbids workers touching `scripts/ralph*`.

</details>
