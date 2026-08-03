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


### GLiNER2 on the ingest path (`SOVEREIGN_GLINER_MODEL_ID`)
- **Shipped:** 2026-08-03, dark. The `LabeledEntityExtractor` seam
  routes either generation into `GlinerChunkExtractor`; the selector
  defaults to v1 (`gliner_small-v2.1`), so nothing changes until the
  env var names `gliner2-base-v1-onnx`.
- **Proof so far:** backend correctness proven against the real 795 MB
  graph (7.29 chunks/s, 2.44 GB RSS); 2.52× faster and ~4.8× lighter
  than v1 on an isolated 50-chunk fixture, three runs per arm, quiet
  box (notes `abc4fb34`, `3f47d12e`). **Quality is unproven** — the
  50-chunk eyeball found GLiNER2 typing BonJour and Sosa as `Work`
  where v1 said `Person`, so P2.1's "fixes type-collapse by extracting
  types jointly" claim is currently contradicted by the only evidence
  that exists.
- **Flip condition:** on the obsidian vault lane, GLiNER2 holds the
  established bar (58/68 facts, 8/12 sources) at a lower
  time-to-enriched than 29m32s — AND per-label typing does not regress
  against v1 on the goldens. Either half failing keeps it dark.
- **Settled by:** P2.1 (`ENRICHMENT_ROADMAP.md` §P2.1), the run this
  row was written to gate.
- **Review by:** 2026-08-17.

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

### Cluster-score blend — `SOVEREIGN_DOC_CLUSTER_WEIGHT` (stays 0.0)
- **Verdict:** 2026-07-31, per this row's own settling condition — the
  T1 P0.4 knob matrix (`bench enrichment-ablate`, 3 sep banks × 3
  reps, artifact `sovereign/bench/ablation/2026-07-31-sep-knob-matrix.json`)
  reports the banks CANNOT separate it: Δ = 0.0000 on every bank,
  zero rep spread. In fact NO knob separated — even
  `SOVEREIGN_RAPTOR_GROUNDING=0` moved only −0.0125 on summarize,
  under the 0.02 floor. Dark since 2026-05-22; settled in one night
  once the lane existed.
- **Honest scope note:** the sep banks do not exercise the
  attached-document search path the blend lives in — this is "the
  current banks can't see it", not "the blend does nothing". Both
  readings route the same way:
- **Re-open only if:** P3.1 golden authoring (T2) produces a bank that
  exercises attached-doc retrieval with cluster-structured answers —
  the same routing as the demand-plan rejection.
- **CODE DELETED 2026-08-01.** Both env vars, the blend branch in
  `attached_document_search.rs`, and the now-unreachable
  `blend_by_cluster_score` / `min_max_normalize` helpers with their 10
  tests are gone; the registry entries in `quality/env-flags.toml` are
  replaced by a tombstone pointing here. A Rejected verdict that leaves
  the code running is the withering pattern this ledger exists to stop —
  the verdict and the deletion belong in the same week, not the same
  hypothetical future tranche. Enrichment knob count **12 → 10**, the
  first movement on the `ENRICHMENT_ROADMAP.md:348` complexity ratchet.
  Recovery for the re-open case: `git show <this commit>^` — the
  rationale survives in `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`.

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

### `SOVEREIGN_SKIP_MOTIFS` / `vault-report --no-motifs` → **deleted**
- **Lifespan: 2026-08-02 to 2026-08-02.** Shipped dark in the morning
  as an ablation arm; the code it ablated was deleted the same day. The
  knob is gone with it — this row is the record, not a live default.
- **What it proved.** Motif extraction was **22.3m of a 52m03s cold
  vault build — 42.8% of time-to-enriched** (330 notes,
  `~/.sovereign/bench-runs/vault-report/1785678945/`), and its output
  table `conv_motifs` had one INSERT, two DELETEs and **no reader
  anywhere in the workspace**. The briefing-signposts claim at
  `conv_tiered_provider.rs:232` traced to `CONV_TIERED_PORT.md:385`,
  which is future tense and was never built for the conv/vault side.
- **The measured result** (three cold builds + `eval run
  --prod-pipeline`, obsidian vault, sweeper paused):

  | config | wall | speedup | facts | sources |
  |---|---|---|---|---|
  | motifs + GLiNER | 52m03s | 1.00x | 58/68 | 8/12 |
  | **motifs off, GLiNER on** | **29m32s** | **1.76x** | **58/68** | **8/12** |
  | motifs off, GLiNER off | 14m15s | 3.65x | 58/68 | 5/12 |

  Motifs-off matched the full build **per question exactly** on facts
  (6,6,5,3,4,6,5,4,3,6,6,4). Run-to-run variance on one build was zero.
- **Resolution: deletion, not a flip.** `build_folder_artifacts` now
  calls `build_raptor_nodes_with_checkpoint`, which has no motif
  concept in its return type — the pass cannot be re-enabled by
  setting anything. `save_conv_motifs` and `ConvMotifRow` are deleted.
  The `conv_motifs` table and its purge DELETE are retained so existing
  databases still shed their legacy rows.
- **Untouched:** the attached-document path keeps its motifs
  (`asset_motifs`, read by `list_asset_motifs` for the doc briefing).
- **Notes:** `3f47d12e` (the result), `e10bf96e` (the no-reader
  census), `de25ebe9` (why the confirmation arm was cancelled),
  `0b8b6cae` (sweeper contamination), `d39af2dc` (the 68/68 correction).

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
