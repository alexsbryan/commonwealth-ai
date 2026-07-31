# SP3 — Judge throughput for the faithfulness lane

**VERDICT: G3 numbers recorded (informational gate). Fast 4B: 7.2 s/node →
73 min/obsidian-corpus, ~22.4 h at sep scale. Primary 35B: 9.4 s/node → 95 min /
~29.2 h — only 1.3x the fast tier, because the workload is prefill-bound and the
MoE's active params keep decode cheap. AND the 35B is a materially better judge:
decisive support (max_support p50 0.99 vs the 4B's diffuse 0.68) and 2x the
claims extracted per node. Recommendation: primary-tier judging by default;
judge-now for corpora ≤ ~1.5k nodes (≤ ~3.3 h), sample 10-15% above.**

## Question (sizing doc §1)

What does judge-scoring one corpus's summaries cost? $/corpus in minutes known →
judge-now vs wait-for-verifier decision; P1.2 sampling-rate default.

## Method actually run

Harness: `sovereign-inference/examples/sp3_judge_probe.rs` (committed).
Validity gate honored: provider is `SplitInferenceProvider` (README G3 — the
`x_forced_choice` structured-output envelope reaches the daemon only via its
`response_format: json_schema` path; the harness PANICS if a reply is not a
calibrated `{"A":p,"B":p}` distribution, so a silently-invalid run cannot
complete). Protocol replicates production `runtime/grounding/judge.rs`
standalone: `extract_claim_list` template (max 4 claims, temp 0, no thinking)
then per-claim forced-choice support over member chunk passages (2,400-char cap,
12-chunk cap, early exit at support ≥ 0.95).

Corpus: `obsidian-vault-959ee8a8f330` — 608 RAPTOR nodes (606 L0), member texts
resolved from chunks.lance via `scripts/sp3_dump_nodes.py` (6,193/6,193 chunk
ids resolved; the reingest-drift check SP2 flagged is moot here — zero missing).

```
.venv/bin/python scripts/sp3_dump_nodes.py --db ~/.svrnmesh/sovereign.db \
  --corpus obsidian-vault-959ee8a8f330 \
  --chunks ~/.svrnmesh/indexes/obsidian-vault-959ee8a8f330/chunks.lance \
  --out data/sp3_nodes_obsidian.jsonl
cargo run -p sovereign-inference --example sp3_judge_probe -- \
  data/sp3_nodes_obsidian.jsonl <model_id> runs/sp3/<tier>/results.jsonl [limit]
```

Fast tier ran the corpus END-TO-END (608/608 nodes). Primary tier ran a 60-node
sample and extrapolates (the plan's sample-extrapolation clause; a full 608-node
35B pass is hours of machine time that buys no additional information).

## Cost table

| Metric | fast Qwopus3.5-4B (608 nodes, end-to-end) | primary Qwen3.6-35B (60-node sample) |
|---|---|---|
| claims/node (raw / excl. failed extractions) | 1.58 / 1.72 | 3.28 (0 failures) |
| calls/node | 12.86 | 12.40 |
| s/node (mean / p50) | 7.23 / 6.18 | 9.40 / 7.04 |
| claim-extract ms (mean) | 1,100 | 2,399 |
| forced-choice ms/call (mean) | 507 | 624 |
| chunks checked/claim (mean) | 7.5 | 3.5 |
| **min/corpus @ obsidian 608** | **73** | **95** |
| **min/corpus @ conv-anthropic 1,262** | **152** | **198** |
| **min/corpus @ sep-scale 11,181** | **1,347 (~22.4 h)** | **1,752 (~29.2 h)** |

The near-parity is structural: both tiers pay mostly prompt prefill (600-token
passages, 1-token forced-choice replies), and the 35B is an A3B MoE. The 35B
checks HALF the chunks per claim (early exit at ≥ 0.95 fires constantly) while
extracting 2x the claims — more verdicts per node at similar call count.

Reliability during the fast run: 7,818 calls, 107 retried (1.4%), 53 hard-failed
after 3 attempts (0.7%) — residue of the daemon fast-slot Metal-OOM incident
(below). 40 nodes (6.6%) lost their claim extraction to those windows; the raw
claims/node under-counts accordingly (conditional value alongside).

## Verdict quality snapshot (fast tier)

Fast 4B: 959 claims scored; 85.3% supported at the 0.5 threshold; `max_support`
p10/p50/p90 = 0.44 / 0.68 / 0.80 — DIFFUSE. Early-exit rarely engaged; verdicts
sit in the noise-accumulation zone the grounding gate's rescue floor exists for
(judge.rs:302-311).

Primary 35B (60-node sample): 197 claims; 89.3% supported; `max_support`
p10/p50/p90 = 0.46 / 0.99 / 1.00 — DECISIVE. This matches the production
expectation that genuine support measures ~0.99. The tier choice is therefore a
verdict-quality knob first and a cost knob second: at 1.3x cost the 35B produces
calibrated-confident verdicts and twice the claim coverage.

## Stream B seed

Every scored tuple appended as `(member_chunks, claim, verdict, max_support)`
JSONL — `sovereign/bench/faithfulness/obsidian_fast_seed.jsonl` (959 rows,
converter `scripts/sp3_streamb.py`). This is the faithfulness lane's seed format
per the sizing-doc decision.

## Operational finding (rides the memo, mirrors a note)

Mid-smoke the daemon's fast slot hit a Metal GPU OOM
(`kIOGPUCommandBufferCallbackErrorOutOfMemory`) when the 35B primary became
resident alongside fast+embed; llama.cpp then wedges the backend permanently
("backend is in error state ... recreate the backend to recover") and EVERY
subsequent fast-slot decode 503s until daemon restart. Judge batch runs at
P1.2 scale must either pin single-slot residency or treat 503-bursts as a
restart signal, not a retry case.

## Consequences

- **Judge-now vs wait-for-verifier:** judge-now (primary tier) for corpora up to
  conversation scale (≤ ~1.5k nodes ≈ ≤ 3.3 h). At sep scale either tier is an
  overnight batch (22-29 h) — viable once, not per-reindex; wait-for-verifier
  (or sampling) is the steady-state answer there.
- **Judge-model default: primary 35B, not fast.** The 1.3x cost premium buys
  decisive support scores (p50 0.99 vs 0.68) and 2x claim coverage. Use the fast
  tier only when the primary slot is contended.
- **P1.2 default sampling rate:** 100% at ≤ 1.5k nodes; 10-15% stratified above
  (sep at 12.5% ≈ 3.7 h primary ≈ 2.3x the full-obsidian cost).
- **Stream B seeds:** `sovereign/bench/faithfulness/obsidian_fast_seed.jsonl`
  (959 rows) + `obsidian_primary_sample_seed.jsonl` (197 rows). The two tiers'
  verdicts on the SAME 60 nodes also give a free inter-judge agreement probe for
  the verifier lane.
- **P0.3 correction confirmed in practice:** the real judge seams are
  `extract_claim_list` + `forced_choice_ab` (runtime/grounding/judge.rs), and
  the `SplitInferenceProvider` envelope requirement is LOAD-BEARING — a naive
  /v1/chat/completions client would have produced a plausible-looking but
  invalid run. P0.3's visibility-promotion line item should carry that.
