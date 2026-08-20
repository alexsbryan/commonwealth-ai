# Canaries — the only thing here that checks judgment

Schemas check form. Parsers check shape. Consumers check that the output
is loadable. **None of them checks whether the verdict was right**, and
for R4 that is the entire job.

A canary is a known-bad input whose only correct answer is named in its
frontmatter. It runs alongside real work — one extra call, ~3s — and if
the role gives the forbidden answer the run **HALTS**. The real output is
not recorded as trustworthy, because the instrument that produced it just
failed a question with a known answer.

This is ARCH §18.1 made continuous. The alternative considered and
rejected was a 46-episode bank: it measures better and it is not running
while you work, which means the instrument can rot between measurements
and nothing says so.

## Format

```
---
role: R4
forbid_verdict: approve
why: <one line — what makes the correct answer the correct answer>
---
<the input the role is given, verbatim>
```

`forbid_verdict` is the answer that must NOT come back. Not the answer
that must — a model may legitimately say `revise`, `escalate` or
`could-not-judge` about the planted defect, and demanding one exact
string would make this a brittle string match rather than a judgment
check.

## Watched to fail

`scripts/co-role.py R4 --canary-only` runs just the canary and prints
what came back. Point `forbid_verdict` at the answer the model actually
gives and the harness must HALT — that is how you confirm the canary can
fire at all, rather than trusting that it would.

## R6

R6 has no canary yet, and this file says so rather than implying six
roles are covered. The R4 shape does not transfer directly: R6's answer
is per-item liveness, so its canary needs a known-alive item whose
evidence is absence-shaped, and picking one that stays known-alive as
the tree moves is the unsolved part. Named, not silently absent.
