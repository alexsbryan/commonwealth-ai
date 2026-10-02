# 006 — Write with read-loop pressure

**What it tests:** the read-loop attractor we saw in real smokes —
the model "keeps reading" instead of committing to writing, even
when explicitly told the reading is done.

**Setup:** full codex system + 4 prior turns of (read-spec / read-
features-md) + a final user message that explicitly says "stop
reading, write src/lib.rs now."

**Why it matters:** real smokes never reach the write phase because
the model gets stuck in 10+ turns of reading the same files. This
fixture compresses that pressure into one fixture: same in-context
"I've been reading for a while" signal, then an explicit pivot
directive.

**Pass criteria:**
- args parses as JSON
- `args.cmd` contains `apply_patch` AND `*** Add File: src/lib.rs`
- `args.cmd` does NOT contain another read command (`cat`, `head`, `rg`)

**Strict on "more reads"** because that's the failure mode we
actually saw — model claims "let me just read one more thing"
endlessly.

**Hypothesis:** without intervention, model continues reading.
Investments that could move this fixture:
- Stronger anti-rep / cycle-detection
- Explicit "commit threshold" — when N reads have happened, force a
  synthetic message demanding write
- Tighter task framing in the user message (already strict here as
  the empirical baseline)
