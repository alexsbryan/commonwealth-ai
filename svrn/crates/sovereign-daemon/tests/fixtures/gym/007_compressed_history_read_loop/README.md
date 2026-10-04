# 007 — Compressed-history read-loop

**What it tests:** the read-loop attractor REAL codex sessions hit
after ~5 turns, when frontdoor's distiller-style block summarization
kicks in and the LLM-generated narrative recaps the read pattern
("explored directory structure", "cat on the spec file succeeded
three times", "No actual source files have been written yet").

**Why fixture 006 wasn't enough:** 006 had a hand-authored short
user message and a handful of raw reads. Read-attractor's trim
removed the raw reads and got the model to pivot 5/5. But REAL
codex history compression collapses turns into a narrative that
self-reinforces the read pattern. Trimming raw tool_calls leaves
the summary intact — model attends to it and continues reading.

**Captured from:** `~/.sovereign/codex-sessions/raw/resp_1778719695030.input.json`
— the smoke run on 2026-05-13 where codex did 19 consecutive turns
of reads against the fresh oicp-types smoke and never pivoted to
apply_patch.

**Pass criteria:**
- args parses as JSON
- `args.cmd` contains `apply_patch` AND a `*** Add File:` for either
  `Cargo.toml` or `src/lib.rs`
- `args.cmd` does NOT contain any read-shaped command
  (cat/find/ls/rg/head)

**Why both Cargo.toml and src/lib.rs accept:** the task at this point
in the smoke has the agent still figuring out which deliverable to
write first. Either is a valid pivot.

**Empirical baseline (pre-fix):** 0/N — model continues with `find`
or `cat`, matching the failure mode observed in the live smoke.

**Investments that should move this fixture:**
- Replace / trim the compressed-history user message when
  read-attractor fires
- Anti-read narrative bias in the distiller prompt (prevent the
  summary from emphasising reads)
