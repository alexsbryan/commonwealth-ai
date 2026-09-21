# 002 — rg-loop recovery

**What it tests:** can the model break out of an in-context-reinforced
loop on a failing command?

**Captured from:** real smoke 2026-05-13. Turn N had emitted `rg
'oicp-v0.2' --files` and gotten back `rg: oicp-v0.2: IO error: No
such file or directory`. The model's next turn (this fixture) should
try a DIFFERENT strategy — but in practice 4/10 runs at T=0.7 emit
the exact same failing command.

**The diagnosis:** codex resends the full conversation each turn,
so by turn N+1 the model sees its own previous identical emissions
as in-context examples. The sampler has a strong attractor on the
exact tokens.

**Pass criteria:**
- args parses as JSON
- `args.cmd` is NOT exactly `rg 'oicp-v0.2' --files` (the loop string)

**Loose enough that:** any pivot counts — `find` variants, `cat`
to read the closest file directly, `rg 'v0.2'` without `oicp-` prefix.
**Strict enough that:** copy-paste loops fail.

**Empirical history (10 replays, daemon state at fixture creation):**
- T=0.0: 0/10 pass
- T=0.3: 5/10 pass
- T=0.7: 8/10 pass
- T=1.0: 9/10 pass

**Investment #14** (T=0.7 pin) targets this fixture specifically.
**Investment #15** (anti-rep nudge) is the next lever — should push to
10/10.
