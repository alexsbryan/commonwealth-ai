# SP2 — Extractive-vs-abstractive RAPTOR parity (G2)

**VERDICT: PARITY — GATE PASSES WITH ZERO DELTA. |Arm B − Arm A′| = 0.0000 on
aggregate fact_score and source_score for both banks, against a band of
fact ≤ 0.025 (summarize) / ≤ 0.0167 (obscure), source exact-equal. The T1
kill-point does NOT fire; P1.1's extractive SummaryMode is viable at equal
coverage on retrieval-quality grounds alone.**

Answered 2026-07-31. All artifacts under `research/enrichment-spikes/runs/`
(`arm0/`, `armA/`, `armB/`, `armAprime-enrich.log`, `armB-compute.log`).

## Results

Prod-pipeline surface (`--prod-pipeline --isolate --limit 30`), aggregate per
run = mean over questions of |fact matched|/(matched+missing) and
source_score.ratio; scorer `scripts/score_runs.py`. All three replicates
per arm per bank were IDENTICAL (the surface is deterministic; same as Arm 0).

| Bank | Arm 0 (orphan, 11,181 nodes) | Arm A′ (abstractive, 271 nodes) | Arm B (extractive, 271 nodes) | \|B−A′\| | Band | Gate |
|---|---|---|---|---|---|---|
| summarize (n=8) | fact 0.6250 · src 1.0 | fact 0.6125 · src 1.0 | fact 0.6125 · src 1.0 | 0.0000 · 0.0000 | ≤0.025 · exact | **PASS** |
| summarize_obscure (n=6) | fact 0.7833 · src 1.0 | fact 0.7833 · src 1.0 | fact 0.7833 · src 1.0 | 0.0000 · 0.0000 | ≤0.0167 · exact | **PASS** |

Raw-index no-regression guards (one per arm per bank, `--limit 30`, no
`--prod-pipeline`): A′ and B both scored summarize 0.6625 / obscure 0.7500,
source 1.0 — identical to each other and within the original dated band
(README G2 original fixtures). As pre-registered, a raptor swap cannot move
raw-index scores; the guard confirms the rebuilds broke nothing.

## Validity checks (why zero delta is a real result, not a stale cache)

- The daemon serving the Arm B benches booted AFTER the swap (see incident
  note below), so no in-memory pre-swap state could survive. SQL rows and the
  rebuilt lance were verified extractive on disk before the runs
  (`sqlite3 … substr(summary,…)`; meta row_count 271).
- **The swap demonstrably changed retrieval:** per-question retrieved pools
  differ between A′ and B on 1/8 summarize + 1/6 obscure questions (r1 vs
  r1), i.e. Arm B's re-embedded extractive summaries shifted node selection —
  yet all 14/14 matched-fact sets are identical. The parity is in outcomes,
  not in frozen inputs.
- Mechanism: RAPTOR grounding's contribution to these banks flows dominantly
  through `evidence_chunk_ids` (identical across arms by construction — only
  `summary` + `summary_embedding` were swapped). Summary text affects node
  *selection*; the scored evidence is chunk-level. At equal coverage the
  selection differences were too small to move any fact.

## What Arm B was

Per node (plan step 4): member texts via `direct_member_chunk_ids` (level 0)
/ `evidence_chunk_ids` (levels 1-2, where direct is NULL) from
`chunks.lance`; sentence-split (≥40 chars); sentences embedded
(`qwen-embedding-0.6b` via daemon `/v1/embeddings`); ranked by cosine to the
node's untouched `centroid_embedding`; top sentences to the node's own
abstractive summary length (avg 420 chars → extractive avg 520, stop-at-≥
overshoot); source order restored; re-embedded → `summary_embedding`.
Compute: `scripts/armb_extractive.py` → `runs/armB/armb_nodes.clean.jsonl`
(271 nodes). Write path: `sovereign-store/examples/armb_write_nodes.rs` via
the public `SqliteStateStore::save_conv_raptor_nodes` (atomic per-conv
delete+insert, correct f32-BLOB encoding) — no raw rusqlite. Then
`sovereign enrich raptor-index sep` (destructive rebuild from SQL rows).

## Coverage note (Arm A′ vs Arm 0)

A′'s scoped 14-article tree (271 nodes) scored −0.0125 on summarize vs the
11,181-node orphan (0.6125 vs 0.6250), identical on obscure. Informational
only (Arm 0 is not a gate input; coverage differs). Consistent with Wave 0's
ablation finding: the orphan lance contributes ≈ +1 fact on 1/8 summarize
questions.

## Exact commands

```
# Arm A′ build (predecessor session):
sovereign enrich raptor sep --titles-file data/sp2_bank_articles.txt --group-by-article --force
# (raptor-index ran automatically at the end of the retrofit; log runs/armAprime-enrich.log)

# Benches, per arm (runs/arm{A,B}/run-benches.sh):
sovereign eval run --bank sovereign/bench/sep/<bank>.toml --prod-pipeline --isolate \
  --limit 30 --format json --output runs/<arm>/<bank>-r{1..3}.json
sovereign eval run --bank sovereign/bench/sep/<bank>.toml --limit 30 --format json \
  --output runs/<arm>/<bank>-rawindex.json   # raw-index guard

# Arm B swap:
.venv/bin/python -u scripts/armb_extractive.py --db ~/.svrnmesh/sovereign.db \
  --chunks ~/.svrnmesh/indexes/sep/chunks.lance --corpus sep --out runs/armB/armb_nodes.jsonl
cargo build -p sovereign-store --example armb_write_nodes
target/debug/examples/armb_write_nodes ~/.svrnmesh/sovereign.db runs/armB/armb_nodes.clean.jsonl sep
sovereign enrich raptor-index sep
```

## Restore (obligation satisfied)

sep returned byte-identical from `backup/` on 2026-07-31: daemon stopped →
lance + meta copied back (`diff -r` clean on both) → `DELETE FROM
conv_raptor_nodes WHERE corpus_id='sep'` (baseline zero rows restored) →
daemon restarted. Meta back at source_version 1780886017 / 11,181 rows.

## Secondary corpora (report-only) — DEFERRED

`conversations-anthropic` (1,262 nodes) + `obsidian-vault-959ee8a8f330`
(608) exercise conversation-bench + briefing signposts. Deferred past the
primary gate: sep answered the G2 question with zero delta, these are
pre-registered as report-only (no gate), and each costs another full
swap/bench/restore cycle on live (non-orphan) rows where a member-chunk-id
drift check must pass first. If P1.1 planning wants the conversation-surface
datum, the harness is reusable as-is (`armb_extractive.py --corpus <id>` +
the same writer + `enrich raptor-index`). Book-report stays dropped (attaches
fresh per run; extractive variant arrives with P1.1's real SummaryMode).

## Incidents (process notes)

- **Daemon stop/start race cost ~1h:** after the swap's index rebuild, a
  `daemon stop && daemon start` race let a late SIGTERM land on the NEW
  daemon at 02:03:01; it exited 0 and launchd did not respawn a
  clean-exiting job. An unbounded health-wait loop masked the death. Lesson
  applied: bound daemon waits (300s) and check process liveness, not just
  the port; confirm full stop before starting.
- **Concurrent-writer torn line:** the first armb compute run was wrongly
  presumed dead (pgrep false negative) and a resume run was started
  alongside it; both finished and interleaved one torn JSONL line. All 271
  nodes validated intact from the remaining lines
  (`armb_nodes.clean.jsonl`); torn line discarded. Determinism of the
  compute makes either writer's row equivalent.
- `source_version` does not change on a swap that preserves `created_at`
  (it derives from row timestamps), so any version-gated index cache would
  not see a same-timestamp content change. The daemon restart made this moot
  here, but it's a freshness-gate blind spot worth knowing about
  (consistent with Wave 0's orphan-liveness finding).

## P1.1 / P1.2 implications

- Extractive summaries at equal coverage are score-parity on the sep
  summarize surfaces — P1.1 may choose extractive-first (cheap, faithful by
  construction, no LLM summarization cost: A′'s abstractive retrofit cost
  1,539s of 35B time for 14 articles vs ~7 min of embed-only time for the
  extractive swap) with abstractive as an upgrade, not a prerequisite.
- The faithfulness contract stands regardless (parity here is about
  retrieval quality, not about summary truthfulness — extractive is
  trivially faithful; abstractive still needs P1.2's verification lane).
- Sizing-doc §1 SP2 row: confidence Low → High for the "extractive is not
  worse at equal coverage" premise on document corpora; conversation-surface
  premise remains at its prior confidence (secondary corpora deferred).
