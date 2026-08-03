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
- **Shipped:** dark (note `10a1b08d`). **Wired into the daemon-server
  and desktop Runtimes 2026-08-03** (T1 A2) — until then the `svrn
  chat` CLI was the only surface that installed one, so both shipping
  surfaces ran baseline fusion and `SOVEREIGN_PPR_EXPAND` logged "lane
  dark" for want of the same `rerank_fn`. Still opt-in via
  `SOVEREIGN_RERANK_MODEL_PATH`; the row stays DARK until the A/B.
- **Cost of on:** the ~500MB / ~1.7s-per-query figures below are
  SUPERSEDED and were measured on the broken jina GGUF. SP4
  (2026-07-31, note `d43fb03b`) adopted the official
  `Qwen3-Reranker-0.6B-Q8_0` GGUF: 639MB, **22.7ms/pair batched**
  (~470ms for top-20), 2.57ms/pair on short titles. The
  `jina-reranker-v3-Q8_0.gguf` finding that read as "rerankers are
  unusable" was a conversion defect in that one artifact — it dropped
  the scoring head — not a property of the capability.
- **Superseded cost figures:** ~500MB resident, ~1.7s/query at k=50,
  OICP wire work for peer routing.
- **Flip condition:** residual contribution (+1 SEP source, +5 wiki
  sources, +12 wiki facts) survives after cap-N chunks-per-article +
  vector-distance dedup are measured *combined* — the cheap fixes
  must fail to close the gap before the expensive slot earns it.
- **Settled by:** unowned.
- **Review by:** 2026-09-01.

## REJECTED — measured no; do not re-litigate without new evidence

### GLiNER2 as the vault/conversation extractor — `SOVEREIGN_GLINER_MODEL_ID` (stays `gliner_small-v2.1`)
- **Shipped and settled the same day, 2026-08-03.** The row was written
  in the morning against a flip condition — "holds the vault bar at a
  lower time-to-enriched AND no per-label typing regression" — and the
  afternoon's run refuted **both halves**. Recording it rather than
  deleting it, because the seam it rode in on is staying.
- **Verdict, on all 3,175 obsidian vault chunks**, both backends through
  the production `LabeledEntityExtractor` seam
  (`sovereign-gliner/examples/typing_audit.rs`, artifact
  `research/enrichment-spikes/findings/typing_audit_obsidian.json`):
  - **Time: 881.9 s v1 vs 893.2 s GLiNER2 — no speedup, marginally
    slower.** The 2.52× was real but is a property of the chunk-length
    distribution, not the model (sep p50 761 chars; vault p50 1,808).
    v1's gline-rs stack batches 8 texts per call and amortises; GLiNER2
    is one graph call per text. Note `dc2e4b5d`.
  - **Typing: worse, not fixed.** Mention-level Person accuracy 96.9%
    (v1) vs 81.8% (GLiNER2) on the vault oracle; 99.7% vs 67.3% on sep.
    `Ostrom` — the vault's anchor entity — is `Person` ×6 /
    `Organization` ×6 under GLiNER2. `Work` becomes a catch-all for
    ordinary noun phrases: 16,053 `Work` mentions to v1's 632, 47% of
    its entire output. Note `f42cf7ec`.
- **What is NOT rejected:** the residency finding (GLiNER2 is ~4.8×
  lighter, note `3f47d12e`) and the seam itself. The knob stays — it is
  how anyone re-tests this — and P2.1's steps (b)–(d) were never
  evaluated.
- **Re-open only if:** a GLiNER2 checkpoint or label/threshold
  configuration demonstrably stops `Work` absorbing common noun phrases,
  scored **per mention** on `bench/gliner/` oracles; or a target corpus
  with sep-shaped chunk lengths makes the throughput win real AND typing
  holds. Both halves, not either.

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

### Caller-directed prefix-cache pin — `SOVEREIGN_PREFIX_STATE`
- **Default ON 2026-08-03**, opt out with `=0`. Genuinely flipped this
  time — `env_enabled()` now defaults true.
- **This row was FALSE for thirteen days and that is the lesson.** It
  claimed "default-on 2026-07-21" when the flip had never happened:
  `BATCHED_GATE_VERIFY.md` *recommended* flipping after two hardenings,
  those hardenings landed, and the row recorded the recommendation as
  executed. A false GRADUATED row is worse than no ledger, because it
  is trusted. Nothing parses this file's review-by dates (T1 B2 is the
  gate that would have caught it).
- **Earned by:** controlled A/B through the production answer path,
  `svrn bench enrichment-ablate sovereign/bench/obsidian/questions.toml
  --prefix-state --reps 2`, on `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`:

  | arm | reps | mean wall | fact ratio |
  |---|---|---|---|
  | off | 901.7s, 835.2s | 868.4s | 0.4736 |
  | on  | 671.1s, 667.0s | 669.0s | 0.4597 |

  **1.30x, −199s per rep**, against an OFF-arm spread of 66.5s — the
  delta is 3x the noise. Arms proven distinct by pin telemetry: OFF
  `LEARNED=0 HIT=0`, ON `LEARNED=28 HIT=86`. Reproduces the 2026-07-21
  result (1.35x, 786.3s → 584.5s) on HEAD.
- **The earlier "worth ≈0" result was never a contradiction.** The
  2026-07-12 A/B measured ONE synthesis prefill; the pin's only
  consumer is the grounding gate, which issues ~35 judge calls per turn
  each re-prefilling the same evidence. Two workloads, not two answers.
- **Open caveat, stated rather than buried:** the quality delta is
  −0.0139 mean fact ratio (~1 fact in 60). That is below the ablation's
  0.02 separation floor and reports as NOT SEPARABLE, but it was
  IDENTICAL in both reps — a small reproducible difference, not noise.
  If restore is bit-exact it should be zero. Settle it by checking
  restore bit-exactness, not by adding reps (the eval is deterministic
  per arm, so more reps of the same config cannot move it).
- **Model scope:** measured on `qwen35moe`. The pin's value scales with
  prefill cost, and `prefix_cache_gate` vetoes ordinary partial-KV
  reuse on both `qwen35moe` and dense `qwen35`, so on those the pin is
  the ONLY caching available. On a small primary the win will be
  smaller and the ~64KB/token state cost proportionally larger; the
  byte-capped LRU (`_MAX_MB`, default 2048) is what bounds it.
- **Instrument:** `svrn bench enrichment-ablate --prefix-state` is
  committed and is the template for any daemon-side knob. The original
  harness (`scratchpad/arm_runner.py`) never was.

### RAPTOR grounding — `SOVEREIGN_RAPTOR_GROUNDING`
- Default **on**, status shipped (`env-flags.toml`). Summary nodes as
  virtual chunks earned the default.
