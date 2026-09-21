# 005 — Write under full Codex context

**What it tests:** can the model execute a clear, simple write task
when the system prompt is the FULL 20.7KB codex 0.130 verbose
prompt + 1 (post-filter) tool?

**Same task as 001** (write `pub fn answer() -> u32 { 42 }` into
`src/lib.rs` via `apply_patch` heredoc) — but with the real codex
ambient context dropped in. This isolates the "is codex context the
problem?" question from "is the task hard?" — the task is the same
trivial task that 001 hits 10/10 on.

**Why it matters:** 001 at 343 prompt tokens proves the pipeline
works in isolation. 005 at ~21K prompt tokens proves (or disproves)
whether the model can ALSO perform under realistic conditions. If
001 = 10/10 and 005 = 0/10, the bottleneck is context handling.

**Pass criteria:**
- args parses as JSON
- `args.cmd` contains `apply_patch`
- `args.cmd` contains `*** Add File: src/lib.rs`
- `args.cmd` regex-matches `pub fn answer`

**Expected (hypothesis):** lower than 001's 10/10 — model gets
distracted by codex's bookkeeping instructions even when the user
ask is unambiguous. The delta vs 001 quantifies the "20K context
cost" empirically.
