# 001 — Write-stage baseline

**What it tests:** the cleanest possible "write a file via apply_patch
heredoc" path. Minimal system prompt + 1-tool catalog + clear task
("create src/lib.rs with `pub fn answer() -> u32 { 42 }`").

**Why it exists:** sanity floor. If this fixture stops passing 10/10
runs, the pipeline has regressed in a fundamental way — grammar,
sampler, or response shaping is broken.

**Bench history:** 10/10 perfect at the time of fixture creation
(2026-05-13, pipeline state Inv #0/1/3/11/12/14/15/16). Each run
emits an apply_patch heredoc that adds `src/lib.rs` with the
expected body.

**Pass criteria:**
- args parses as JSON
- `args.cmd` contains `apply_patch <<'EOF'`
- `args.cmd` contains `*** Add File: src/lib.rs`
- `args.cmd` contains `pub fn answer`

**If it fails:** the regression is in the inference adapter,
sampler chain, grammar lock, or routing layer — NOT in
context-handling. Bisect with the bench replay rig.
