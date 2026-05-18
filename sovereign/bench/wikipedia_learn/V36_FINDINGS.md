# v36 noise-floor augmentation — findings, methodology, next-iteration handoff

**Date:** 2026-05-18  
**Branch:** `llama-cpp-4-mtp`  
**Commits:** 17cdf8e (vendored llama-cpp-4 + initial v36) → revert (this commit)  
**Baseline retained:** **v28** (`threads-v28-allthreads.json`)  
**Status:** **v36 reverted**. v28 stays as cross-thread high watermark. Marathon-thread fix logic kept in commit history; reservation-only redesign deferred to next iteration as **option C** below.

---

## 1. Background — what we were trying to fix

The marathon thread (`computing_history`, 12 turns) has long shown
turns where retrieval went wrong despite title-expand correctly
naming the right Wikipedia title for the question.

- T3 "What did she contribute that was genuinely new?" — title-expand
  named `["Ada Lovelace", "Charles Babbage"]`. Fan-out pulled
  Lovelace chunks. But the bare query has no proper-noun token (just
  "she", "contribute", "genuinely"). The noise-floor at
  `drop_no_overlap_chunks` then dropped every Lovelace chunk because
  none contained "contribute" or "genuinely" verbatim. The synth had
  no Lovelace material in the prompt and either refused or pulled
  the answer from parametric.

This is the canonical "anaphoric + sparse-token query" pattern. The
marathon thread's design exposes it deliberately. The hypothesis was
that title-expand had already done the hard semantic work (resolving
"she" → Ada Lovelace), so the noise-floor should respect that
resolution and let the named-title chunks through.

## 2. Designs explored

### v33 (rejected, 2026-05-17) — protected-title bypass

Bypass the noise-floor entirely for chunks whose title matches a
title-expand title. Implementation:
`drop_no_overlap_chunks_with_protected` with a `title_lower == p`
short-circuit returning `true`.

Outcome: Marathon T3 retrieval correct (Lovelace chunks survive),
but marathon T6/T7/T8 collapsed to empty visible answers. Forensic
trace showed: bypass kept ALL chunks from a protected article —
biographical / family / education / WWII / legacy. RRF (title-
anchored from title-expand fan-out) ranked them high. They displaced
on-topic chunks at the `KQ_MERGED_LIMIT=20` cap. Synth context
became bloated and off-topic, Fast slot think-collapsed (reasoning
chars present, visible answer empty).

Reverted in same session. Function kept in tree under a `TEMPORARILY
DISABLED` comment.

### v36 (this commit, rejected) — token augmentation

Don't bypass the per-chunk check; augment its query-token set with
the tokens of each title-expand title. So for marathon T3 the
survival tokens become `{contribute, genuinely, ada, lovelace,
charles, babbage}`. Lovelace lead/bio chunks pass because they
contain "Lovelace". Off-topic noise chunks (no overlap) still drop.
Uniform per-chunk rule, no whole-article bypass.

Single-thread result (marathon only, vs v35):
- fact 0.612 → 0.730 (+11.8 pt)
- src 7/12 perfect → 11/12 perfect
- T3 went from think-collapse (rc=1215, ac=0) to graceful no-info
  refusal (rc=0, ac=151). T4/T10/T11 went from broken to full
  multi-paragraph answers. T6/T7/T8 stayed clean (no v33-style
  collapse) — augmentation does not bloat context the way bypass
  did.

13-thread result (vs v28 baseline, `threads-v36-allthreads.json`):
- aggregate `fact_recall` 0.648 → 0.623 (−0.025)
- aggregate `source_recall` 0.558 → 0.649 (+0.091)
- 5 threads improved on fact, 6 regressed, 2 unchanged

Per-thread Δfact (sorted):

| Thread | Δfact | Δsrc |
|---|---|---|
| newton_einstein_compare | +0.25 | +0.10 |
| nonexistent_entity_boundary | +0.12 | +0.12 |
| ambiguous_name_boundary | +0.08 | −0.10 |
| computing_history (marathon) | +0.04 | +0.17 |
| french_revolution_causal | +0.04 | +0.08 |
| scientific_revolution_synthesis | +0.00 | +0.33 |
| atomic_bombing_contested | +0.00 | +0.00 |
| columbus_legacy_contested | −0.10 | +0.10 |
| wwii_causal | −0.10 | −0.10 |
| einstein_1905 | −0.13 | +0.00 |
| industrial_revolution_synthesis | −0.14 | +0.07 |
| darwin_origin | −0.15 | +0.33 |
| buddhism_christianity_compare | −0.24 | −0.10 |

The 13-thread regression on `fact_recall` (and concentration of
biggest hits on info-rich, well-formed queries) made v36 a net
negative as a default. Reverted.

## 3. Root cause — why augmentation regressed info-rich threads

Forensic comparison on `darwin_origin` T1 "What was his most famous
voyage?" — retrieved chunk distribution from the bench JSON:

**v28 (bare noise floor):** 28 chunks across **15 articles**

```
4x Charles Darwin
4x Falkland Islands wolf
4x sep::darwinism
4x Robert Gray (sea captain)
2x Royal Navy
1x Tierra del Fuego
1x sep::origin-descent
1x Death of James Cook
1x Pedro Álvares Cabral
1x Amerigo Vespucci
1x French Polynesia
1x Exploration of the Pacific
1x HMS Endeavour
1x Italy
1x San Diego
```

**v36 (augmented noise floor):** 14 chunks, **12 from a single article**

```
12x Charles Darwin
1x sep::evolution-before-darwin
1x sep::darwinism
```

Mechanism:

1. Title-expand names "Charles Darwin"; fan_out adds K chunks from
   that article.
2. Augmented noise floor adds `{charles, darwin}` to the survival
   token set. **Every chunk whose title is "Charles Darwin" now
   passes by definition**, regardless of whether it contains
   "voyage" or "famous". Chunks the bare noise-floor would have
   thinned (Darwin's bio sections lacking voyage tokens) now all
   pass.
3. `cap_chunks_per_article(MAX=10)` caps Darwin chunks at 10.
4. `reserve_chunks_per_entity` pins 3 Darwin chunks past the cap.
5. `truncate(KQ_MERGED_LIMIT=20)` keeps the top 20.
6. `expand_from_dominant_source` then inspects the post-truncate
   set, sees Charles Darwin dominates, and adds **more** Charles
   Darwin chunks. Runaway.

In v28 the bare noise-floor was the diversity check that kept
secondary articles like Tierra del Fuego and Falkland Islands wolf
alive: those chunks contain "voyage" or "Darwin" in content even
though they're titled differently. v36 didn't remove those — it
just stopped culling weak in-Darwin chunks, which then crowded out
the diverse pool at the cap.

The same pattern explains the other regressions. Each named the
title-expand article as dominant and replaced citation diversity
with monoculture from one Wikipedia article. Where the v28 answer
cross-referenced 3–5 articles for a fact, v36 had a single article
to lean on and missed the per-source factoid (Galápagos, coal, 1859,
Wehrmacht, etc.).

The marathon thread didn't regress because its turns are designed
around chains where the title-expand article *is* the answer source
(Babbage, Lovelace, Turing, von Neumann, Transistor, Intel). For
those, monoculture is correct.

## 4. Why v33 (bypass) and v36 (augment) both lost at the noise-floor layer

Both designs treated the noise floor as the place to express
"title-expand named this; protect it." But the noise floor's job is
diversity-via-substring-relevance. Per-chunk relevance and per-
source preference are different concerns. Pushing source preference
through a per-chunk relevance filter inevitably either over- or
under-protects.

Title-expand should affect:

- **Retrieval composition** (already does: `fan_out_decomposed_queries`)
- **Reservation past the truncate** (already does:
  `reserve_chunks_per_entity` with `COMPARISON_PER_ENTITY_RESERVE=3`)

It should NOT affect:

- The noise-floor substring rule, which exists to thin the hybrid
  RRF residue.

## 5. What stays committed from this session

- **Vendored `llama-cpp-4` + workspace `[patch.crates-io]`** —
  reintroduces the `with_n_seq_max` builder retired upstream in
  0.2.x. Daemon's `n_tokens == 0` batched-prefill error is fixed by
  this alone. `vendor/llama-cpp-4/src/context/params.rs` carries a
  sovereign-provenance comment block at the addition.
- **FastShort `build_ctx_params` + EmbedSlot `build_params`** call
  `with_n_seq_max(...)` so multi-sequence batched decode actually
  has capacity.
- **`SOVEREIGN_FORENSIC=1`-gated instrumentation** at:
  - `Runtime::audit_pipeline_stage` (per-chunk dumps per pipeline
    stage)
  - `embedded.rs::generate_sync_batched` empty-tokens warn
  - `embedded.rs::generate_sync_batched` pre-decode batch-state warn
  
  Error-path enrichment `(batch_n_tokens, n_requests)` stays always-on (cheap, only constructs on error).

- **Doc-comment in `runtime.rs::drop_no_overlap_chunks`** preserving
  the v33/v36 design history and pointing to this file.

## 6. What's reverted

- `drop_no_overlap_chunks_with_protected` function — removed.
- KQ + DeepQuery call sites — back to bare `drop_no_overlap_chunks`.

Function is gone, but the v33 `_with_protected` callers and the v36
augmentation logic are recoverable from this commit's parent
(`17cdf8e`).

## 7. Further research — option C (reservation-only, recommended)

The principle: title-expand is a **retrieval-and-reservation** signal,
never a **filtering** signal. Implementation candidates:

### C1. Increase `COMPARISON_PER_ENTITY_RESERVE` for title-expand titles

Currently 3. The KQ path reserves 3 chunks per title-expand title
past the `KQ_MERGED_LIMIT=20` truncate (`runtime.rs:9176-9183`).
Title-expand chunks should always be in the synth prompt; bump to
5 or 6 *only for the title-expand reserve call*, leaving the
ComparisonQuery reserve at 3.

**Risk:** the marathon T3 case has the bare noise-floor *already*
killing the Lovelace chunks before they reach reserve. Increased
reservation slot count doesn't help if there's nothing to reserve.

### C2. Reorder: reserve title-expand chunks before noise-floor

Move the title-expand reservation step ahead of the noise floor.
Mark the K best Lovelace chunks (by RRF rank) as "pinned" before
`drop_no_overlap_chunks` runs. Noise floor then operates on the
remainder.

**Risk:** synth context still has 3 reserved Lovelace chunks even
when the bare query doesn't reference Lovelace at all. For
marathon T3 this is exactly what we want (the question *is* about
Lovelace by anaphora). For other queries where title-expand named
a tangentially-related title, this may seed the synth with
irrelevant context.

### C3. Pin specific chunk_ids from title-expand fan-out

Most surgical. `fan_out_decomposed_queries` returns the chunks it
added. Track their `(corpus_id, chunk_id)` and exempt those exact
chunks from the noise-floor — not by title-match. Other chunks
from the same Wikipedia article (pulled by hybrid RRF) face the
normal noise-floor rule.

This separates "chunks title-expand decided to include" from
"chunks that happen to share the title" — the failure mode that
sank both v33 and v36.

**Recommended starting point: C3.** It expresses the actual
invariant ("the chunks I intentionally fanned out should reach
synth") without per-article side effects.

### Evaluation

Whichever C-variant lands, the validation gate is:

1. **Marathon T3** — fact_recall and src_recall both > 0.33 with
   non-empty visible answer (rc > 0, ac > 500). Current v28
   baseline: f=0.33 s=0.00.
2. **13-thread** — `fact_recall` ≥ v28's 0.648 (no cross-thread
   regression). `source_recall` ≥ v28's 0.558.
3. **`bench all`** — no regressions on single-shot surfaces.
4. **`--judge-trials 3`** — variance check; sub-5pt single-judge
   deltas are within noise (see `feedback_bench_three_views.md`
   and `project_title_expand_v28.md` for the noise-band derivation).

The v36 single-thread marathon delta (+11.8pt) was a real signal
above the noise band. The 13-thread regressions on info-rich
queries are also above noise. Any C-variant should preserve the
marathon win without exporting it to the rest of the bank.

## 8. Methodology notes (for the next iteration)

- **Always run the full 13-thread bank**, not single-thread marathon.
  v36's marathon-only result was misleadingly positive.
- **Cite the retrieved-titles distribution per failing turn**. The
  monoculture pattern would have surfaced after the first failing
  thread had we looked at the per-article histogram. The
  forensic-T3 log doesn't dump this by default; consider adding
  per-article histograms to the `post_merge` audit event so
  retrieval shape regressions get caught at trace time, not at
  bench time.
- **Multi-judge (`--judge-trials 3`) is the canonical scorer**.
  Substring fact-scoring has ~17pt single-trial inflation per
  `project_title_expand_v28.md`. The marathon T3 "graceful refusal"
  observed in v36 (correct system behavior, but no expected facts
  in the response) shows the substring scorer also under-counts
  honest behavior. Bench fixture authors: consider adding
  `expected_refusal_markers` to thread turns where retrieval is
  expected to come up empty.
- **`retrieved` field in bench JSON includes post-expansion chunks**,
  so it overstates the per-article count vs the post-truncate set.
  The audit event `post_merge` (in `runtime.rs:9189-9227`) is the
  source of truth for what enters the synth prompt before the
  expander runs.
