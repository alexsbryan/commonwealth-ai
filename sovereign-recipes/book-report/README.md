# book-report — attach-document benchmark

End-to-end benchmark for the user-facing **attach a document, ask questions**
flow. Drives the same code path as the desktop's `upload_document_asset`
Tauri command, so improvements measured here translate directly to the
in-app experience.

## What it measures

**Two axes, neither sufficient on its own:**

| Axis | Question | How |
|---|---|---|
| Speed | How quickly can the system give a useful answer after attach? | Per-tier *time-to-first-acceptable-answer* against the readiness curve |
| Quality | Are answers anchored in the text, or paraphrased plausibility? | Per-question rubric grading + hallucination detection on every quoted passage |

A model that answers Tier 1 in 4s but fabricates the address-label detail is
worse than one that takes 12s and gets it right. A model that quotes the
Professor's closing image flawlessly but never complicates it is worse than
one that reaches for two earlier passages and earns its anti-canonical reading.

## Why this book

Conrad's *The Secret Agent* (Gutenberg #974) is chosen because:

- **Well-attested motifs at multiple narrative scales** — plot detail (Tier 1),
  monologue inference (Tier 2), cross-chapter (Tier 3), recurring phrase
  (Tier 4), critical-consensus pushback (Tier 5)
- **Widely studied** — the model has training-data signal on it, which is
  exactly what we want: Tier 5 measures whether retrieval beats pretraining
- **Reasonable length** (~85k words) — a single attach completes in tens of
  seconds, not minutes; tight feedback loop for iteration
- **Public domain** — no licensing concerns; reproducible across nodes

## Readiness gates

Document ingestion is not atomic. The asset transitions
`Pending → Indexing → PartiallyReady → BuildingSkeleton → Ready` and the
benchmark fires each tier's questions *at the earliest state where the tier
is plausibly answerable*. Defaults in `bench.toml`:

| Tier | Gate | Why |
|---|---|---|
| 1 | `PartiallyReady` | Plot facts live in chunks; RAG is enough |
| 2 | `PartiallyReady` | Local inference within a retrieved span |
| 3 | `BuildingSkeleton` | Cross-chapter linkage benefits from skeleton |
| 4 | `Ready` | Thematic motif requires multi-passage anchor |
| 5 | `Ready` | Contamination test needs full atlas signal |

The runner records the actual state when each tier fires plus the latency
from attach. That tuple — *(state when fired, ms since attach, quality
score)* — is the load-bearing data point for product decisions.

## Scoring

| Tier | Method | What it catches |
|---|---|---|
| 1 | Mechanical: substring match on `expected_facts` | Missing factual detail |
| 2-3 | LLM-judge with rubric | Failure to articulate inference / cross-chapter link |
| 4-5 | LLM-judge + **hallucination check** | Fabricated passage quotations |

**Judge model: the Sovereign primary, supplied with the reference rubric.**
The judge is *not* reasoning from scratch about literary criticism — the
prompt gives it: (a) the question, (b) the model's answer, (c) the
verified reference passages from `references/<id>.md`, (d) the
`expected_facts` list, and (e) the rubric scale. Its job is the bounded
task "does this answer hit the rubric anchors against this ground truth"
— well within the primary's capability and cheap to run. The trade-off
versus an external judge (GPT-4o) is variance vs. self-contained: we
accept slightly higher variance for the dogfood loop. Re-running the
judge over the same `responses.jsonl` with `--judge-model <external>`
remains available for v2 calibration.

**Scoring scale (Tier 4-5):**

| Score | Meaning |
|---|---|
| 5 | Passage-anchored, correct synthesis, calibrated about uncertainty |
| 4 | Passage-anchored, mostly correct, minor synthesis gap |
| 3 | Partially anchored, correct gist |
| 2 | Paraphrased without anchor, plausible but unverifiable |
| 1 | Confident assertion without anchor |
| 0 | Hallucinated passage OR contamination-trap fired |

Calibrated refusal ("I don't have enough text retrieved to answer this
confidently") gets scored on substance like any other answer — no
separate bonus. If first runs surface that honest-refusal is being
under-scored, we'll revisit; but we won't engineer the rubric ahead of
the data.

**Hallucination detection.** For every block of model output formatted as a
quoted passage (text inside double quotes ≥ 8 words), normalize whitespace
and check substring presence in the source text. Mismatches are flagged
regardless of how plausible the rest of the answer reads. This is the
single most important quality axis for Tier 4-5 — paraphrased "summary"
answers that confabulate plausible-sounding citations are the failure mode
worth surfacing explicitly.

**Tier 5 contamination flag.** A separate check: does the answer hit one
of the canonical critical-consensus phrases in `bench.contamination_traps`
*without* a verifiable passage quotation? Yes → flag and dock score. This
catches the case where the model produces a confident summary of received
opinion when retrieval failed.

## Reference passages

Every question carries `reference_passages = [{ lines = "X-Y", note = "..." }]`
pointing into pg974.txt. These serve two roles:

1. **Ground truth for the grader** — `references/<id>.md` (forthcoming) holds
   the actual quoted text from each line range, so the grader can check whether
   a model's claimed quotation matches what's actually on the page
2. **Sanity check on the rubric author** — if the lines don't say what the
   rubric claims they say, the rubric is wrong, not the model

The bench source is pinned by SHA-256 once first run completes, so a
Gutenberg refresh doesn't silently invalidate the line ranges.

## How to run

```bash
sovereign bench book-report                           # default: direct in-process, earliest-gate dispatch
sovereign bench book-report --tier 1                  # just Tier 1
sovereign bench book-report --no-judge                # mechanical Tier-1 only, no LLM grading
sovereign bench book-report --baseline last           # diff against prior run

# Final confirmation modes — slower, more thorough
sovereign bench book-report --wire                    # drive ingest+ask over /v1/chat/completions
sovereign bench book-report --all-gates               # fire every question at every readiness state
```

**Run modes:**

| Flag | Path | When to use |
|---|---|---|
| (default) | Direct in-process: `DocumentAssetManager::ingest()` + `Runtime::handle_turn()` | Every iteration. Fast feedback, cleanest timings. |
| `--wire` | Over OICP at `/v1/chat/completions` | Final confirmation before shipping a model/atlas change — catches wire-layer regressions the direct path misses. |
| `--all-gates` | Fires each question at every readiness state, not just the earliest plausible | Periodic — once per sprint or before major release — to map the full quality-vs-readiness curve. |

The two confirmation flags compose: `--wire --all-gates` is the full
high-confidence sweep. Both modes default off because they're 5-10×
slower than the iteration path.

Output goes to `~/.sovereign/bench-runs/book-report/<timestamp>/`:
- `timings.json` — per-stage and per-question latencies
- `responses.jsonl` — raw model outputs + provenance + retrieved chunks
- `scores.json` — per-question rubric scores
- `report.md` — human-readable rollup, suitable for sharing

## Versioning

- **v1 (2026-05-20)** — schema + 6 seed questions covering all 5 tiers.
  Runner not yet shipped; bench.toml is reviewable as a contract.
- **v1.1** — fill out to 20 questions at the 3/5/5/4/3 distribution.
- **v1.2** — runner online with mechanical Tier-1 scoring + state-transition
  timing.
- **v1.3** — LLM-judge for Tier 2-5; hallucination detector; full report.
- **v2** — second book (different genre — possibly a non-fiction work) to
  validate that the bench measures system behavior, not Conrad-specific
  retrieval quirks.

## Failure modes the bench surfaces

If the runner reports any of these, the underlying system has a bug worth
investigating, not a tuning knob:

| Symptom | Likely cause |
|---|---|
| Tier 1 fails on `stevie_address_label` after `PartiallyReady` | Chunker dropped the relevant span or embed-quality is degraded |
| Tier 4 quotes appear that aren't in pg974.txt | Synthesis confabulating around weak retrieval |
| `attached_at → first_answer_ms` for Tier 1 exceeds 30s | Ingestion is blocking the chat thread (see Item 4 from the polish sprint) |
| Tier 5 flagged for contamination on every run | Atlas isn't propagating enough text-anchor signal into synthesis |
| LLM-judge variance > 1 point on identical responses | Judge model or rubric prompt is unstable; rerun with a different judge |
