# Defaults Ledger — capabilities shipped dark, and what flips them

**The failure mode this file exists to stop:** an initiative proves a
capability works ("zero delta on both banks!"), ships it behind a flag
or non-default mode "until X", and then X never happens — the work
withers, provably good code sits dark forever, and six months later
nobody remembers the flip condition or whether it was ever met.
(Poster child: the cluster-score blend shipped 2026-05-22 at
`cluster_weight=0.0` "pending bench plan" and sat dark for ten weeks
before this ledger existed.)

**The contract:**

1. Any push that ships a capability default-off or dark **adds a row
   in the same commit** — with a falsifiable flip condition, which
   plan item or run settles it, and a review-by date.
2. When the condition is met (or refuted), the row **moves** to
   Graduated or Rejected — it never silently disappears.
3. A row past its review-by date is not noise: it is the signal. Any
   session touching that area raises it to the operator — flip it,
   kill it, or re-date it with a reason. "Still waiting" without a
   named blocker is not a valid state.

Cross-references: env flag defaults live in `quality/env-flags.toml`
(this ledger records *why* a default is what it is and what changes
it, not the mechanics); decisions with full context live in the notes
store (ids cited per row).

---

## DARK — proven or plausible, awaiting a named condition

### Cluster-score blend — `SOVEREIGN_DOC_CLUSTER_WEIGHT` (default 0.0)
- **Shipped:** 2026-05-22 (note `54869b19`), byte-identical baseline
  at weight 0.0, opt-in only. Spec:
  `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`.
- **Proof so far:** pattern descends from the SEP rerank atlas_weight
  blend (sources 40→65 of 66); the blend itself never measured.
- **Flip condition:** T1 P0.4 ablation lane (`bench enrichment-ablate`)
  shows the blend moves a bank metric; the lane's knob matrix names
  this flag explicitly.
- **Settled by:** T1 P0.4 (plan step 6). If P0.4 reports the banks
  cannot separate it, move this row to Rejected — that verdict routes
  to P3.1 golden authoring (T2).
- **Review by:** 2026-08-14.
- **Registry gap:** closed 2026-07-31 — declared in
  `quality/env-flags.toml` (with `SOVEREIGN_DOC_CLUSTER_POOL`) as part
  of the P0.4 lane landing.

### EvidenceCheck frame + evidence-shape early-decline
- **Shipped:** 2026-07-21, dark.
- **Proof so far:** top_cosine established as TOPIC signal, not
  answer-containment (~0.75 in-topic-but-thin) — the floor needs
  calibration before the frame can gate anything.
- **Flip condition:** floor calibration soak separates
  "in-topic-thin" from "answerable" without raising false declines.
- **Settled by:** unowned — no current T1 item covers it. If no
  tranche claims it by review date, kill or re-scope.
- **Review by:** 2026-08-14.

### Cross-encoder reranker slot
- **Shipped:** dark (note `10a1b08d`).
- **Cost of on:** ~500MB resident, ~1.7s/query at k=50, OICP wire
  work for peer routing.
- **Flip condition:** residual contribution (+1 SEP source, +5 wiki
  sources, +12 wiki facts) survives after cap-N chunks-per-article +
  vector-distance dedup are measured *combined* — the cheap fixes
  must fail to close the gap before the expensive slot earns it.
- **Settled by:** unowned.
- **Review by:** 2026-09-01.

## REJECTED — measured no; do not re-litigate without new evidence

### Demand-plan fan-out — `SOVEREIGN_DEMAND_PLAN_FANOUT` (off)
- **Verdict:** 2026-07-19 A/B — net-neutral answer quality at 2–3x
  retrieval latency. Flag stays off; `env-flags.toml` records it.
- **Re-open only if:** a bank exists that separates multi-hop recall
  (P3.1 golden-authoring, T2). A flat-recall bank cannot exonerate it.

### Acquisition gate armed at 0.45
- **Verdict:** 2026-07-20 — `import_conversations` is a top-1
  attractor at that threshold; arming it misroutes.
- **Re-open only if:** the attractor is fixed and the threshold
  recalibrated against the post-fix distribution.

### Speculative decoding (classic draft)
- **Verdict:** 2026-05-12 — net-negative on this hardware; KV-rollback
  hand-port costed at 2–4 days for nothing the llama-server harness
  doesn't provide.
- **Re-open triggers:** recorded in
  `sovereign/docs/archive/SD_EXPERIMENT.md` §closure.

## INTENTIONAL OPT-IN — off is the designed end state, not a debt

### RSS hard limit — `SOVEREIGN_RSS_HARD_LIMIT_MB` (off)
- Self-SIGTERM is only safe under a supervisor that restarts the
  daemon (2026-07-18). Soft-warn is on. This row exists so nobody
  "fixes" the default.

## GRADUATED — the pipeline completing, for the record

### Extractive summary mode default for memory corpora (T1 P1.1)
- **Flip condition met 2026-07-31, same day it was written:** the
  production seam held parity on the sep banks — both arms rebuilt
  through `enrich raptor` at identical 14-article scope, |B−A| =
  −0.0125 on summarize (band ±0.025), 0.0000 on obscure (band
  ±0.0167), r1–r3 deterministic, rawindex guard 0.0000 both banks
  (`research/enrichment-spikes/runs/prodAB/`).
- **Default flipped:** memory corpora (vault notes, imported
  conversations via `build_folder_tiered_provider`; memory-pool trees
  via `mem_atlas`; the vault-wide theme synthesis) now build
  EXTRACTIVE trees. Attached documents keep abstractive — now
  verifier-gated (T1 P1.2, same push). `enrich raptor` CLI default
  remains abstractive with explicit `--summary-mode`.
- **Registry/env:** no env flag — the default is code-level policy at
  the memory-corpus construction sites, provenance-stamped per node.

### Caller-directed prefix-cache pin
- Dark → 180-min soak (restore p90 29ms, TTFT 173→66s) → **default-on
  2026-07-21** after stale-pid sweep + byte-capped LRU. The template
  this ledger holds every dark row to.

### RAPTOR grounding — `SOVEREIGN_RAPTOR_GROUNDING`
- Default **on**, status shipped (`env-flags.toml`). Summary nodes as
  virtual chunks earned the default.
