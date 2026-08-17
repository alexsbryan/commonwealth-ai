# dr-verdict bar — the restated kill bar, verbatim from quality/initiative-bars.toml

Source: `quality/initiative-bars.toml`, id `dr-verdict` (the file is the
authority; this page is a verbatim mirror for the landing review).

## The bar (frozen, unedited)

> **ship iff P4 AND P2 AND P1**: the local-only arm clears the coverage
> floor (P4); the hybrid's fabrication rate beats the cloud-DR reference
> (P2), measured between arms with pre-registered n and cluster-adjusted
> CI; and the P1 cost arm beats its reference, measured against a NAMED
> PROXY arm — never 'cloud DR' (the reference is a proxy and says so).
> **Cheapness is never a pass**: the original kill bar ('beats cloud DR
> on neither honesty nor cost') ships on cost alone by construction —
> electricity wins even for an empty report.

Declared 2026-08-13 (`derives_from`: DEEP_RESEARCH.md cost-model kill
bar; PLAN.md §1 kill bar REFUTED as written, §4 T2).

## Transitions

### on = "2026-08-13", to = "declared"

by = "research/deep-research/PLAN.md §6, approved unedited (directive 0411b48f)"

### on = "2026-08-17", to = "failed" (landing)

by = """Measured on the frozen DRB subset (n=10, pre-registered 2026-08-16, both arms run 2026-08-17, judge pinned local Qwen3.6-35B-A3B-MTP-UD-Q6_K). P4 passed — carried from t1h (63/72 coverage floor, not re-measured). P2 FAILED — hybrid-arm pooled fabrication 0.3571, cluster-bootstrap 95% CI [0.2564, 0.4554] vs the perplexity-Research reference 0.1737 (failed: CI lower >= reference, interval does not straddle). P1 met — max(arm mean cost) $0.000573/task < $1.45 proxy (o3-deep-research, frozen in drb/p1-cost-reference.md). Ship requires P4 AND P2 AND P1; P2 failed, so the bar is failed. Bar text unedited; the full record is in research/deep-research/adversarial/pre-registration.md (EXECUTION RECORD — T2b DRB arms)."""
