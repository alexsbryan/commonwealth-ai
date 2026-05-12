# Cross-encoder reranker experiment

**Status (2026-05-11):** experimental, **not in the default
retrieval path**. Code is merged into the workspace but every entry
point is opt-in via env vars; defaults preserve baseline behaviour
exactly. The lift on SEP source recall is large enough to justify
keeping the code around, but committing to a new resident model
slot is a system-design question that hasn't been weighed yet — see
the trade-offs section below.

← related: §4.3 (Inference / slots) and §3.3 (embed cross-peer
contract) in `SYSTEM_OVERVIEW.md`.

---

## TL;DR

Built a cross-encoder rerank pass that runs on top of the existing
vector + FTS5 hybrid retrieval. Tested against the
21-question SEP bank and the 20-question Wikipedia bank. Final
empirical-best configuration:

| Bank | Baseline sources | Rerank sources | Δ |
|---|---|---|---|
| **SEP** | 40/66 (61%) | **51/66 (77%)** | **+11 (+28% rel.)** |
| Wikipedia | 29/58 (50%) | 31/58 (53%) | +2 |

The win comes overwhelmingly from one knob — **per-article
aggregation** (one canonical chunk per source after rerank scoring),
not the cross-encoder logits themselves. That distinction matters for
the system-design discussion: most of the lift is structural, not
model-driven.

Cost: ~1.7s/query latency at 50 candidates × 0.6B reranker forward
passes; ~500 MB additional resident weights per process that loads
the slot.

---

## What was tested

The proposal evaluated in
[the earlier ranking exercise](#) singled out a cross-encoder
reranker on top of vector + FTS5 as the lowest-effort retrieval-side
intervention with a measurable scoreboard (the SEP + Wikipedia eval
banks). This experiment runs that intervention end-to-end:

- can a small reranker model (jina-reranker-v3, 0.6B, Qwen3-based)
  lift retrieval quality on the existing eval banks?
- how does it compose with the hybrid fusion path (LanceDB's RRF +
  cosine ordering)?
- what's the failure mode when it underperforms — and is there a
  prompt / structural fix?

The reranker model the experiment uses is
`sovereign/models/jina-reranker-v3-Q6_K.gguf`. The GGUF reports
architecture `qwen3` with special tokens `<|rerank_token|>` (151671)
and `<|score_token|>` (151669) — Jina trained the model to emit a
score-token logit after a `<|rerank_token|>` marker, *not* to use
llama.cpp's BERT-style `pooling_type = Rank` head. The first
implementation attempt used pooled rank and returned all-zero
scores; the working implementation reads the score-token logit
directly from the next-position prediction.

---

## What was built (file map)

Everything is gated; nothing fires unless `SOVEREIGN_RERANK_MODEL_PATH`
is set.

| File | Purpose |
|---|---|
| `corpus-engine/src/types.rs` | `RerankFn` type, `RerankConfig { enabled, candidates_k, min_score, alpha, per_article }` |
| `corpus-engine/src/index/search.rs` | `CorpusIndex::search_with_rerank` — overfetch → cross-encode → min-max-normalised hybrid blend → optional per-article dedup → truncate. Title-prefix on docs (`Title: <t>\n\n<content>`). Falls back to fusion result on reranker error so enabling the slot can never make retrieval worse than baseline. |
| `corpus-engine/src/error.rs` | `Error::Rerank(String)` |
| `sovereign-core/src/traits.rs` | `InferenceProvider::rerank_batch` default-impl returns `NotImplemented` |
| `sovereign-core/src/model_family.rs` | `ModelFamily::Reranker` variant + `RerankQuirks { max_context, max_batch }` |
| `sovereign-core/src/runtime.rs` | `Runtime.rerank_fn` + `rerank_config` fields, `with_rerank` builder method. Both `search_corpus_indexes` paths route through `search_with_rerank`. |
| `sovereign-inference/src/embedded.rs` | `RerankSlot` — loads in generative mode (no pooling, no embeddings), tokenises Qwen3 chat-template prompt fragments at load time, reads logit for `<|score_token|>` at the position after a trailing `<|rerank_token|>`. Slot is held behind `EmbeddedLlamaCpp.rerank_slot: Mutex<Option<Arc<RerankSlot>>>`, installable at runtime via `install_rerank_slot`. |
| `sovereign-inference/src/reranker_standalone.rs` | `StandaloneReranker` — a minimal `InferenceProvider` that holds only a `RerankSlot` (no chat/embed slots). Used when the chat path talks to a remote daemon for chat+embed but the reranker has to live in the same process as the corpus search. |
| `sovereign-tools/src/corpus/mod.rs` | `inference_to_rerank_fn` — mirrors `inference_to_embed_fn` |
| `sovereign-cli/src/daemon_cmd.rs` | Reads `SOVEREIGN_RERANK_MODEL_PATH` at daemon startup and calls `install_rerank_slot` (warns + continues on failure) |
| `sovereign-cli/src/chat_cmd/bootstrap.rs` | Eval-side: builds a `StandaloneReranker`, wires `Runtime::with_rerank`. Reads `SOVEREIGN_RERANK_ALPHA`, `SOVEREIGN_RERANK_PER_ARTICLE`, `SOVEREIGN_RERANK_PROMPT_VARIANT`, `SOVEREIGN_RERANK_MIN_SCORE`. |
| `sovereign-cli/src/eval_cmd/runner.rs` | `run_question` now routes through `idx.search_with_rerank`, passing the runtime's `rerank_fn` + `rerank_config`. When neither is set, behaviour is byte-identical to the prior `search` call. |
| `sovereign-recipes/sep/eval/sep_questions.toml` | Ported from the legacy repo (21 questions, authored 2026-05-04, all expected sources verified against SEP's 1,770-slug catalog) |
| `sovereign-inference/examples/rerank_smoke.rs` | Standalone smoke test — loads the GGUF and scores hand-written (query, doc) pairs |

---

## Results — the tuning sweep

All runs: `sovereign eval run --limit 10` (retrieval-only), 50
candidates pulled from LanceDB, jina-reranker-v3-Q6_K.gguf. Daemon
provides chat + embeddings over HTTP; the reranker loads
in-process in the eval CLI.

### SEP — 21 questions, sources = canonical SEP article slugs

| Config | Sources | Facts | Notes |
|---|---|---|---|
| **Baseline (no rerank)** | 40/66 (61%) | 134/159 (84%) | Production retrieval as-is. |
| Pure rerank, content-only | 34/66 (52%) | 138/159 (87%) | -6 sources. Cross-encoder pulls tangential articles that mention the topic densely over canonical entries. |
| + Title prefix in doc | 38/66 (58%) | 132/159 (83%) | Helps but still -2 below baseline. |
| Hybrid α=0.9, no per-article | 41/66 (62%) | 132/159 (83%) | +1 over baseline. Single-knob sweep peak at α=0.9, monotone-ish curve. |
| **Per-article, α=0.7** ✓ | **51/66 (77%)** | **131/159 (82%)** | **+11 sources, -3 facts.** Kept configuration. |
| Per-article, α=1.0 | 51/66 (77%) | 128/159 (81%) | Same sources, -3 facts vs α=0.7. |
| Per-article, α=0.3 | 51/66 (77%) | 131/159 (82%) | Tied with α=0.7. Sources insensitive to α once per-article is on. |
| + Lean prompt variant | 51/66 (77%) | 129/159 (81%) | No source lift over verbose; -2 facts. Discarded. |

Per-category breakdown at the kept config (per-article, α=0.7,
verbose prompt, title prefix):

| Category | Baseline | Kept | Δ |
|---|---|---|---|
| argument_reconstruction | 12/17 | 15/17 | **+3** |
| comparative | 8/13 | 10/13 | +2 |
| concept_distinction | 2/7 | 4/7 | +2 |
| contested | 5/9 | 8/9 | **+3** |
| dialectical | 6/9 | 7/9 | +1 |
| position_summary | 7/11 | 7/11 | 0 |

Five of six categories improved; the sixth (position_summary) held
even. No category regressed.

### Wikipedia — 20 questions, sanity check

Same config (per_article=on, α=0.7) applied unchanged to wiki:

| | Baseline | Kept config |
|---|---|---|
| Sources | 29/58 (50%) | **31/58 (53%)** |
| Facts | 92/130 (71%) | **98/130 (75%)** |

Modest source lift; clean fact lift. The SEP-tuned config doesn't
regress wiki, which is reassuring — the structural fix is general.

### Why per-article dominates

Pure cross-encoder reranking ranks chunks. SEP's `expected_sources`
score by canonical article slug, not by chunk relevance. A question
about Aristotelian hylomorphism gets 10 top chunks from the rerank
pass — and 8 of them may be from articles like
`medieval-hylomorphism` or `form-matter` (which discuss the topic
densely) rather than from `aristotle-metaphysics` (which discusses
it once, more obliquely). The eval scores zero for the article slug
match even though the answer content is fine.

Per-article dedup collapses each article to its single best chunk
inside the rerank pool before the top-K truncation. The top 10 then
becomes 10 *distinct articles* — the canonical entry gets to compete
on equal footing with the densely-mentioning tangentials.

The trade-off is depth: in the kept config each top result is one
chunk from one article. Questions that legitimately need multiple
chunks from the same source (long-form essay answers) lose some
fact coverage — hence the -3 facts cost on SEP and the unchanged or
positive fact movement on wiki (where chunks-per-article was rarely
the binding constraint).

---

## System-design trade-offs to weigh before promoting

The user explicitly flagged that adding a new resident model slot is
a system-design decision, not a tuning decision. Concretely:

### Resident-weight cost

`jina-reranker-v3-Q6_K.gguf` is ~500 MB on disk and ~600–700 MB
resident with KV cache. Today's slot lineup (Fast + Main + Embed)
already presses against 16 GB on default-profile hardware; adding a
fourth resident slot makes the headroom argument worse. Options
that change the answer:

- demand-load + idle-evict (mirroring the proposed lazy Embed slot)
- keep the reranker out of the daemon's primary slot lineup and only
  load it in eval / batch processes via `StandaloneReranker`
- offer it as an OICP-advertised `x:rerank` capability so a beefier
  peer serves the rerank pass for thinner clients

### Latency cost on interactive paths

The current implementation costs ~1.7 s/query at 50 candidates. The
chat surface's KnowledgeQuery path runs corpus search synchronously
inside the turn, so this lands on TTFT. The SEP lift is large
enough that the time cost is probably acceptable for retrieval-bound
turns — but it would need a runtime gate (skill-level opt-in, or a
"slow corpus mode" flag) to keep snappy turns snappy.

Smaller candidate pools (e.g. 20 instead of 50) would reduce
latency by ~2.5× at some cost to the structural lift. Not measured.

### Mesh contract surface

A reranker that survives is going to want OICP advertisement
(`x:rerank` capability hint) so peer schedulers can route a rerank
call to whichever node has the model loaded. That's a wire-format
addition. Today the reranker is purely local; nothing has been
added to `oicp-types` or the manifest synthesis path.

### The "most of the lift is structural" observation

The cross-encoder logits contribute ~+1 source over baseline (α=0.9
hybrid, no per-article). The per-article dedup contributes ~+10 on
top of that. **The big win is the dedup**, which is a corpus-engine
feature that doesn't need a reranker at all — you could implement
"per-article diversification of the top-K" on the existing hybrid
fusion result and capture most of the lift without paying the model
cost.

This is worth pulling out separately. The right next experiment
might be: re-run SEP with per-article dedup applied to the
*baseline* hybrid fusion output (no rerank), and measure the lift.
If most of the +11 sources survives, the reranker buys very little
and the system-design decision is easy: don't add the slot, add the
diversifier. If the lift collapses without the rerank scores guiding
which-chunk-per-article to keep, the reranker is genuinely
load-bearing.

### Model-protocol coupling

The score-token-logit path is specific to jina-reranker-v3 (and the
m0 lineage that uses similar special tokens). Swapping to a
BGE-style BERT reranker would require either:

- adding a second protocol branch in `RerankSlot` (BERT pooling
  vs. score-token-logit), or
- standardising on score-token-logit and accepting the smaller
  ecosystem of compatible rerankers.

The current code has only one path and asserts the special tokens
exist at load time — incompatible models fail fast rather than
silently producing zeros.

### Cross-corpus consistency

The +11 SEP sources / +2 wiki sources gap is real. The reranker's
fit depends on what "source" means in the eval metric:

- SEP — canonical encyclopedia entry → per-article dedup is the
  match for the scoring metric
- Wikipedia — broader topical article → less benefit from dedup
  because the original fusion already hits these

A reranker that ships as a default knob would need a per-corpus
config story, not a global on/off switch. The current
`RerankConfig` is per-runtime; per-corpus would mean threading
config through `installed_indexes()` or attaching it to recipes.

---

## How to reproduce

```bash
# 1. The reranker GGUF must exist at this path
ls sovereign/models/jina-reranker-v3-Q6_K.gguf

# 2. Daemon up (for chat + embed; reranker loads in the eval process)
target/release/sovereign-cli daemon run > /tmp/daemon.log 2>&1 &

# 3. Baseline run — no env var, default behaviour
target/release/sovereign-cli eval run \
  --bank sovereign-recipes/sep/eval/sep_questions.toml \
  --limit 10 --output /tmp/sep_baseline.json

# 4. Reranker on with kept config
SOVEREIGN_RERANK_MODEL_PATH=$(pwd)/sovereign/models/jina-reranker-v3-Q6_K.gguf \
SOVEREIGN_RERANK_PER_ARTICLE=1 \
SOVEREIGN_RERANK_ALPHA=0.7 \
  target/release/sovereign-cli eval run \
  --bank sovereign-recipes/sep/eval/sep_questions.toml \
  --limit 10 --output /tmp/sep_rerank.json
```

### Env vars (all optional, all default to baseline)

| Var | Default | Effect |
|---|---|---|
| `SOVEREIGN_RERANK_MODEL_PATH` | unset | Loads the GGUF; flips `RerankConfig.enabled = true`. |
| `SOVEREIGN_RERANK_DEDUP_ONLY` | `0` | `1` → overfetch + per-article dedup using fusion scores only (no cross-encoder). The ablation knob. Mutually exclusive with `SOVEREIGN_RERANK_MODEL_PATH` (dedup-only takes precedence). |
| `SOVEREIGN_RERANK_DEDUP_CORPORA` | `sep` | Comma-separated allow-list of corpus IDs eligible for dedup. Empty string = no filter (apply to all). Default `sep` reflects the empirical finding that dedup is corpus-shape-specific. |
| `SOVEREIGN_RERANK_DEDUP_PICKER` | `fused` | Which signal picks the within-article best chunk. `fused` = RRF/blended order (best for SEP). `vector` = cosine-to-query (partially recovers wiki, hurts SEP). |
| `SOVEREIGN_RERANK_PER_ARTICLE` | `0` | `1` → per-article dedup after rerank. **The big lever on SEP.** |
| `SOVEREIGN_RERANK_ALPHA` | `1.0` | Blend weight on rerank vs. fusion. `0.7` is the kept value. Both are min-max normalised inside the candidate pool. |
| `SOVEREIGN_RERANK_CANDIDATES_K` | `50` | Pool size pulled from LanceDB before reranking. |
| `SOVEREIGN_RERANK_MIN_SCORE` | unset | Drop candidates below this raw rerank logit. |
| `SOVEREIGN_RERANK_PROMPT_VARIANT` | `verbose` | `lean` strips the system message + XML tags (no source-recall lift on SEP; -2 facts). |

### Smoke test (no daemon needed)

```bash
cargo build --release -p sovereign-inference --example rerank_smoke
target/release/examples/rerank_smoke
```

Expects relevant docs to score 0.5-1.0 logits above irrelevant ones
across three test queries. If outputs are all-zero, the model isn't
emitting via the score-token protocol (different reranker family).

---

## Ablation: dedup-only (no reranker) — 2026-05-11

Critical follow-up — does per-article dedup *alone* (no
cross-encoder calls, fusion-score-only chunk picker) reproduce the
SEP lift? Implementation: `SOVEREIGN_RERANK_DEDUP_ONLY=1` env var
sets `RerankConfig { enabled: true, per_article: true }` with
`rerank_fn = None`; the search path overfetches and applies dedup
but skips the cross-encoder call entirely. Latency drops back to
baseline (~100 ms search vs. ~2000 ms with the reranker).

| Bank | Baseline | Dedup-only | Reranker + Dedup | Reranker Δ over dedup |
|---|---|---|---|---|
| **SEP sources** | 40/66 | 50/66 (+10) | 51/66 (+11) | **+1** |
| SEP facts | 134/159 | 128/159 (-6) | 131/159 (-3) | +3 |
| **Wiki sources** | 29/58 | **26/58 (-3)** ⚠️ | 31/58 (+2) | **+5** |
| Wiki facts | 92/130 | 86/130 (-6) ⚠️ | 98/130 (+6) | +12 |

### What this changes

The decomposition is **corpus-specific**:

- **SEP** (expected_sources = single canonical article slug): dedup
  captures ~all the win (+10 of the +11). Reranker contributes ~+1.
- **Wikipedia** (expected_sources = broader topical articles, often
  multiple per question): plain fusion-score dedup **regresses**
  baseline by 3 sources / 6 facts. The cross-encoder reweighting
  *within* each article is what makes the canonical chunk win the
  per-article tiebreak — and recover the wiki numbers.

The earlier "the reranker isn't pulling its weight" framing was
correct for SEP but wrong for wiki. Dedup as a default-on baseline
feature would help SEP users and hurt wiki users in roughly equal
absolute amounts.

### Why dedup-only hurts wiki

LanceDB's hybrid fusion returns chunks ranked by RRF — a position-
based aggregation of vector + BM25 ranks. RRF is good at "which
chunks are roughly relevant" but bad at "which chunk best
represents this article for this question." When the dedup pass
walks the top-50 picking the first chunk per article, RRF noise
inside an article means a topical-but-not-summarising chunk can
"win" that article's spot — knocking out the canonical article
entirely when *its* highest-RRF chunk is a tangential paragraph.

The cross-encoder re-scores chunks relative to the query, so the
within-article winner is the chunk that actually answers the
question. That's what dedup-only is missing on wiki, and what the
+5 source / +12 fact reranker delta over dedup-only on wiki
quantifies.

---

## Follow-ups: per-corpus dedup + RRF noise probe

### Per-corpus dedup filter (kept artifact)

`RerankConfig.dedup_corpus_filter: Option<HashSet<String>>` lets
dedup apply to a chosen allow-list of corpus IDs only. Bootstrap
reads `SOVEREIGN_RERANK_DEDUP_CORPORA=sep,…` and defaults to
`{"sep"}` when unset. Empty string explicitly = no filter (apply
to all corpora — original ablation behaviour).

Verified:

| Bank | Filter | Sources | Facts |
|---|---|---|---|
| SEP | `sep` | 50/66 (76%) | 128/159 (81%) (dedup applied) |
| Wiki | `sep` | 29/58 (50%) | 97/130 (75%) (untouched, baseline) |

The filter does what it's supposed to — SEP keeps the dedup lift,
wiki is structurally untouched.

### RRF-noise hypothesis: vector-distance dedup picker

`RerankConfig.dedup_picker: DedupPicker { FusedScore, VectorDistance }`
chooses the signal that picks the best chunk within each source.
Hypothesis: the wiki dedup regression is driven by LanceDB's RRF
noise inside an article — RRF is rank-position-based, so an
article's tangential paragraph can land at higher RRF rank than
its canonical paragraph by quirk. Vector-distance picker re-orders
the candidate pool by cosine-to-query (lower = better) before the
dedup walk, so the chunk whose embedding most resembles the query
represents the article.

| Bank | Baseline | Dedup `fused` | Dedup `vector` |
|---|---|---|---|
| Wiki sources | 29/58 (50%) | 26 (-3) | **27 (-2)** |
| Wiki facts | 97/130 (75%) | 86 (-11) | **91 (-6)** |
| SEP sources | 40/66 (61%) | 50 (+10) | 46 (+6) |
| SEP facts | 134/159 (84%) | 128 (-6) | 129 (-5) |

**Findings:**

1. **RRF noise is *part* of the wiki regression but not all of it.**
   Vector-distance picker recovers about half the wiki loss
   (+5 facts, +1 source over `fused`). Still **below baseline**.
2. **Picker choice is corpus-dependent in the opposite direction
   for SEP.** Vector picker hurts SEP (-4 sources vs. `fused`).
   No globally-better picker exists; the right picker per corpus
   is empirical, not architectural.
3. **There's a deeper cause than RRF noise.** Wiki questions
   legitimately need multi-chunk coverage from a single canonical
   article for fact recall (best wiki dedup setting still loses
   -6 facts vs. baseline). 1-chunk-per-article truncation strips
   that. **Next experiment worth running: cap N chunks per
   article (N=2 or 3) instead of 1.** Mechanical change to the
   dedup walk (`HashSet → HashMap<key, count>`); should preserve
   some multi-chunk depth on wiki while still spreading sources.

The vector-distance picker code stays merged behind
`SOVEREIGN_RERANK_DEDUP_PICKER=vector`, but the default is
`fused` because it's strictly better on SEP (the only corpus
that benefits from dedup today).

---

## Decision

After two ablation rounds, the picture is clear:

- **Dedup is SEP-only by default.** Per-corpus filter shipped
  (env-var-gated, `RerankConfig.dedup_corpus_filter`). SEP users
  get +10 sources, wiki users untouched. **The kept artifact of
  this experiment.**
- **The reranker slot stays experimental.** Resident model slot,
  +1.7 s search latency, mesh-contract work — worth the cost only
  if its residual contribution (+1 SEP source over best dedup,
  +5 wiki sources over best dedup, +12 wiki facts over best
  dedup) is judged big enough relative to those costs.

Next experiments, in order of effort:

1. **Cap-N chunks per article** — replace the 1-chunk-per-source
   dedup with cap-N (try 2, 3). If wiki recovers to baseline-or-
   better with N=2-3, dedup becomes a global default-on with a
   per-corpus N. Cheap to try, no model required.
2. **Vector-distance dedup *combined* with cap-N** — addresses
   both contributors at once (which chunk per article + how many
   chunks per article).
3. **Only after exhausting (1) and (2)** — weigh the rerank-slot
   decision. By that point the reranker's residual contribution
   over the best mechanism-only baseline is the actual size of
   the prize that the slot's costs would have to justify.

The code from this experiment stays merged in but the defaults
stay off. Future-me, when revisiting: re-run the smoke test first
to make sure the score-token protocol still works against whatever
the GGUF in `sovereign/models/` is at that moment — the protocol
is model-specific and a different reranker file would silently
break this path.

---

## Saved JSON reports (local, not checked in)

| File | Config |
|---|---|
| `/tmp/sep_baseline.json` | baseline |
| `/tmp/sep_FINAL.json` | per_article=1, alpha=0.7, verbose |
| `/tmp/sep_rerank_alpha{0.0..1.0}.json` | hybrid alpha sweep |
| `/tmp/sep_perarticle_a{0.3..1.0}.json` | per-article alpha sweep |
| `/tmp/sep_prompt_{lean,verbose}.json` | prompt-variant A/B |
| `/tmp/wiki_baseline.json` | baseline |
| `/tmp/wiki_FINAL.json` | per_article=1, alpha=0.7 |
