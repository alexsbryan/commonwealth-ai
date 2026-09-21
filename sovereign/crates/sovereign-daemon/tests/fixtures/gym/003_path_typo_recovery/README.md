# 003 — Path-typo recovery

**What it tests:** does the model invent typo'd absolute paths
(`tos-experiment` missing `a`, `example.dev` with stray dot)
when its context includes the correct path repeatedly?

**Captured from:** real smoke 2026-05-13 where the model had read
the spec from the CORRECT path (`/Users/user/dev/atos-experiment-oicp-types/...`)
multiple times, then suddenly started using
`/Users/user/dev/tos-experiment-oicp-types/...` (missing `a`).
The pattern persisted across many turns.

**The diagnosis:** at T=0.7 the model's tokenizer can drift one
character on long paths; once a typo'd path appears in the
in-context history, subsequent turns reinforce it. Role-conditional
sampler (Inv #16) addresses content-byte drift inside JSON strings.

**Pass criteria:**
- args parses as JSON
- `args.cmd` does NOT contain any known typo variant

**Pre-fix baseline:** at T=0.7 with global sampler, the typo
appeared in ~30% of follow-on runs. With Inv #16 role-conditional
sampler (Content greedy), drift inside strings drops significantly.

**Note:** this fixture's pass criterion is loose by design — model
might use relative paths (`./Cargo.toml`) which is fine. The bad
behavior is specifically inventing the typo'd absolute path.
