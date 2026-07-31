# P5.1 — Budgeted tree-descent answerer vs one-shot top-K (G8)

**VERDICT: G8 answered (report-only). At an equal ~3,000-token evidence budget
on the 14-question summarize banks, LLM tree-descent does NOT beat one-shot
cosine top-K: overall facts-in-answer 31/140 (0.221) descent vs 39/140 (0.279)
one-shot, and the gap starts upstream in evidence assembly — descent's leaf
pools carry a mean 2.07 expected facts vs 3.57 for cosine top-K. Descent wins
narrowly on the main `summarize` bank (19/80 = 0.238 vs 16/80 = 0.200) and
loses decisively on `summarize_obscure` (12/60 = 0.200 vs 23/60 = 0.383),
at 2–3 LLM calls and ~19.1 s/question vs 1 call and ~15.6 s. Routing itself
worked: hop logs show the model picking the on-topic subtree tops (e.g. both
idealism nodes for `summary_idealism`), so the loss is granularity, not
navigation — one subtree's leaves are a worse evidence pool than corpus-wide
chunk cosine at the same budget. Two findings with value beyond the score:
(1) the `--group-by-article` RAPTOR output is a FOREST (42 parentless tops:
2 level-2 + 40 article-level-1), invisible to production because both
consumers fetch-all-then-filter — any future descent consumer must start from
parentless nodes, not max-level; (2) `preferred_speed: Slow` requests fail
outright when the 35B primary is not resident, and the first run's harness
swallowed all 56 such failures via `unwrap_or_default()` — scores were
vacuously zero and hop "descents" were pure first-2 fallback.**

Measured 2026-07-31, M2 Max, daemon on :9741, chat = Qwen3.6-35B-A3B-UD-MTP-
IQ4_NL (temperature 0, thinking off), embeddings = qwen-embedding-0.6b.
Harness `sovereign/crates/sovereign-inference/examples/p51_descent.rs` +
`research/enrichment-spikes/scripts/p51_dump.py` (both committed). Raw logs:
`runs/p51/{hops.jsonl,results.jsonl,run.log}`.

## Question (gate G8)

Nothing in production walks `children_node_ids` — both consumers fetch-all-
then-filter. If a consumer DID descend the RAPTOR tree top-down with an LLM
pick-next-children call per hop under a relevance-call budget
(LazyGraphRAG-style), answering from reached leaves' member chunks, does it
beat one-shot cosine top-K at the SAME evidence-token budget? Report-only:
score delta + complete hop logs; evidence for re-planning P5 after T2.

## Method (exact commands)

```
.venv/bin/python scripts/p51_dump.py --db ~/.svrnmesh/sovereign.db \
  --chunks ~/.svrnmesh/indexes/sep/chunks.lance --corpus sep \
  --banks sovereign/bench/sep/summarize.toml sovereign/bench/sep/summarize_obscure.toml \
  --out-dir data
cargo build -p sovereign-inference --example p51_descent
./target/debug/examples/p51_descent research/enrichment-spikes/data \
  Qwen3.6-35B-A3B-UD-MTP-IQ4_NL research/enrichment-spikes/runs/p51
```

Tree = the SP2-checkpoint-rebuilt 271-node scoped sep tree (224 level-0 /
45 level-1 / 2 level-2 over the 14 bank articles, 4,488 chunks). Both arms
answer with the same model + prompt at the same ~3,000-token evidence budget;
descent additionally spends pick calls (budget 8, observed 1–2/question, 15
total) —
frontier of subtree summaries shown, model picks 2, repeat until leaves.
Score = substring match of the bank's `expected_facts` (10/question) against
answer and against assembled evidence, both logged per row.

## Results

| Bank | Arm | Facts in answer | Mean facts in evidence | LLM calls/q | Wall/q |
|---|---|---|---|---|---|
| summarize (n=8) | descent | 19/80 = 0.238 | 1.75 | 2 | ~19.4 s |
| summarize (n=8) | oneshot | 16/80 = 0.200 | 2.75 | 1 | ~15.8 s |
| summarize_obscure (n=6) | descent | 12/60 = 0.200 | 2.50 | 2–3 | ~18.7 s |
| summarize_obscure (n=6) | oneshot | 23/60 = 0.383 | 4.67 | 1 | ~15.4 s |

- **Evidence assembly is where descent loses.** Per-question
  `facts_in_evidence` ≤ oneshot on 11/14 questions. Committing the whole
  budget to 2 picked subtrees means one mis-pick (or one article whose facts
  are spread across sibling subtrees) forfeits facts that corpus-wide cosine
  scoops up. `summary_recursive_functions` is the sharpest case: descent
  reached the deep level-2 cluster (3 calls) yet landed 0/10 facts in
  evidence vs oneshot's 5.
- **Answer extraction roughly tracks evidence** (mean in_answer ≈ in_ev −1
  both arms); a couple of rows answer facts not in evidence (model prior
  leakage, e.g. `summary_game_theory` descent 1/0) — substring scoring noise,
  same for both arms.
- **Hop logs are complete and picks are real**: 15 pick calls total (13
  questions resolve in one hop — article top straight to leaves;
  recursive-functions takes 2 through its level-2 cluster), zero failed or
  unparseable replies; picks vary by question ("16, 28"-style).

## Findings beyond the score

1. **The tree is a forest.** `enrich raptor --group-by-article` yields
   per-article subtrees; level 2 formed over only one cluster (recursive
   functions). 42 of 271 nodes are parentless (2 level-2 + 40 level-1).
   The probe's first version started descent from `level == max_level` and
   could reach only 5/45 level-1 subtrees — every question except
   recursive-functions was structurally unanswerable, and nothing in
   production would ever notice because both consumers fetch-all-then-filter.
   Fix: start from parentless non-leaf nodes (`p51_descent.rs`), frontier cap
   48 so all 42 tops fit one pick call (~4.4k-token prefill, fine at 35B).
   Any future descent-shaped consumer (or tree-quality lint) must do the
   same, or assert single-rootedness at build time.
2. **Silent inference failure made the first run vacuously zero.** With the
   35B primary not resident, every `complete()` (pick AND answer, 56 calls)
   failed; `unwrap_or_default()` hid it, so results looked like "descent ran,
   facts 0/140" and hop logs showed empty replies with fallback picks.
   Harness now eprintlns every failed call and flushes JSONL per line. The
   same tiny request succeeds via direct `/v1/chat/completions` curl once the
   model auto-loads — worth knowing for any Speed::Slow batch client.

## Consequence for P5 re-planning

Tree-descent as an ANSWERING strategy is not the P5 payoff at this corpus
scale — one-shot cosine over 4.5k chunks is cheaper and better, and it will
take a much larger corpus (where top-K cosine dilutes) or a hierarchical-
navigation-specific task (cross-article synthesis at level 1+) for descent to
pay. The forest finding is load-bearing for any P5 design that assumes "the
RAPTOR tree" is one tree. Hop logs in `runs/p51/hops.jsonl` are the complete
per-hop record if a future re-plan wants to re-score picks against labels.
