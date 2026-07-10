# Orientation-bench spike — findings (run 2026-07-09)

**Verdict: NO-GO on retrieval wiring (G1 failed, exactly as the pre-registered
null predicted). GO on the two reframed uses: rollup nodes as orientation
*content* (not retrieval keys) and the derived-vs-asserted fractal-drift arc.**

Protocol: README.md. Bank: bank.toml (frozen before generation; no post-freeze
edits). All artifacts under `out/` (nodes.json, results.json, audit.md,
drift_teasers.jsonl, run.log).

## Run parameters

- Pool: 5,886 leaf summaries + 274 rollup nodes (245 file / 28 dir / 1 crate),
  corpus-engine scope.
- Node generation: `fast` = Qwen3.5-9B-MTP (109 file + 24 dir nodes),
  `primary` = Qwen3.6-35B-A3B-MTP (136 file + 4 dir + crate — prompts over the
  FastShort 6,000-char gate). 0 parse warnings, 0 prompt overflows, 0 empty
  completions. Embeddings: Qwen3-Embedding-0.6B (1024-d), the production space.
- Wall time: ~31 min generation + embed + score, single local box.

## Gate results

| Gate | Threshold | Result | Verdict |
|---|---|---|---|
| G1 orientation lift (C vs A, hit@5, nav+structure) | ≥ +15pts | **+0.0pts on every shape** (nav .778→.778, structure .375→.375, purpose .875→.875) | **FAIL** |
| G2 pointed displacement (C vs A, hit@5) | ≤ 2pts regression | 0pts (.7→.7) — but 3/7 scored guardrails lost a rank (G01 1→2, G02 3→4, G06 2→3); MRR .383→.308 | PASS with a visible warning |
| G3 node quality | 0 blatant confabs in 20 | 0/20 — summaries track child evidence; quality is genuinely good | PASS |
| G4 negatives | report-only | 1/5 flagged: X02 (out-of-scope question) pulled `enrichment/open_questions.rs` node into top-3 | noted |
| G5 cost | informational | median commit = 5 .rs files → ~14 node re-summaries ≈ 80s background; p90 ≈ 46 ≈ 5min. Full-repo ignition ≈ 1,800 nodes ≈ 3h once | fine |

## Full arm table (hit@5)

| shape | A (leaves) | B (+file) | C (+all) | D (nodes only) |
|---|---|---|---|---|
| navigation (18) | .778 | .778 | .778 | .611 |
| structure (8) | .375 | .375 | .375 | **.500** |
| purpose (8) | .875 | .875 | .875 | .625 |
| guardrail (10) | .700 | .700 | .700 | .700 |
| negative (5) | 0 flags | — | 1 flag | — |

MRR moved slightly *down* in C (nav .657→.602): non-gold nodes crowd above
gold items without themselves being answers.

Reporting caveats: structure-shape MRR prints 0.0 in results.json — the scorer
never assigns a rank for the coverage-style structure rule; read it as N/A.
`C_additive` guardrail (.8) is hit-within-7, not comparable to the hit@5 columns.

## Interpretation

1. **The null held, fourth time in a row.** Leaves already orient: a
   navigation question's embedding lands on a function summary inside the
   right module without any tree. This now matches Wikipedia-vanilla (+0),
   Wikipedia group-by-article (+0), and memory-T3 (rank-neutral). For pointed
   AND orientation retrieval, summary nodes do not outrank leaves in this
   embedding space. The mechanism is the memory-T3 finding again: node sims
   sit below leaf cosines (a centroid can't beat its own best member).
2. **The one positive retrieval signal is structure-shape under D** (nodes-only
   .500 vs leaves .375): when the *question itself is coarse*, nodes carry
   real signal — but only when leaves aren't in the pool to crowd them. That
   is an argument for **intent-gated node lookup** (a "orient me" tool path
   that queries the node tier directly), NOT for mixing nodes into general
   retrieval.
3. **Displacement pressure is real even at +0 lift** (G2 rank slips). Mixing
   nodes into the shared pool costs a little and buys nothing. Do not do it.
4. **Node content passed quality review cleanly** (G3 0/20). The tree is a
   good *artifact* — the failure is using embeddings over it as a *retrieval
   index*.

## Decision + follow-ups

- **Do not** wire rollup nodes into `code_search`/KQ retrieval. This closes
  the "RAPTOR for code chunks" question with data; do not revisit without a
  new mechanism (e.g., a reranker feature, per the rerank-atlas-weight
  precedent — that lever lifted SEP when pool-widening alone did nothing).
- **Do** consider the rollup as *content assembly*: `project_context` / `brief`
  answering orientation intent by serving the crate/module/file node for the
  named scope (a lookup by path, not an embedding search — retrieval isn't
  the bottleneck the spike measured). The D-structure signal and G3 quality
  both support this.
- **Do** pursue the fractal derived-vs-asserted arc: 245 (header, derived)
  pairs are already captured in `out/drift_teasers.jsonl` — the input to a
  file/module-level capability-reconcile extension.
- Bank + harness are reusable: bank.toml stays frozen as the orientation eval
  for any future mechanism (reranker, intent-gated lookup, PPR-style graph
  blend).
