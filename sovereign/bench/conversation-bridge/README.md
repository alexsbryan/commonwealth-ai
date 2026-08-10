# Conversation-bridge bench — does the entity graph actually bridge?

> **STATUS: DESIGN, NOT BUILT.** No bank, no parser fields, no runs. This file
> is the spec. Delete this banner when `questions.toml` exists and the first
> variance run has landed.

The whole-game test for the **GLiNER entity layer**: on questions whose answer
requires two or more conversations linked only by a shared entity, does the
`chunk_entities` → `conv_entity_graph` → PPR-rerank path surface the second
conversation — over a RAPTOR-only baseline? The gate is **retrieval lift on
bridged questions**, never mention-F1 or entity counts in isolation (no metric
overfitting; same discipline as `cross-corpus/README.md` and
`feedback_whole_game_quality`).

Sibling of `cross-corpus/` — that bank tests the *meta-atlas* bridge across two
corpora; this one tests the *entity* bridge inside one corpus.

## Why this bench has to exist

The question was already asked once, with the wrong instrument, and the answer
was reported as settled. It was not. Per-question re-analysis of the 2026-08-03
L0 runs (`scratchpad/l0-ablation/*.json`, notes `93ea772d` / `675d6388`):

| bank | fact | rigid source | loose source |
|---|---|---|---|
| obsidian | 9.36 → 9.36 (0) | 8.00 → **5.00** | 10.00 → 10.00 (0) |
| conversations | 8.75 → 8.75 (0) | 8.50 → **8.00** | 9.50 → 9.50 (0) |

Deleting every entity row changed which document was cited on **4 of 24
questions** — three obsidian questions swung rigid source 1.00 → 0.00 — while
both fact totals and both loose-source totals were unchanged. "Loose gap zero"
meant *a judge that accepts any surface form of a correct citation could not
separate the arms*. It did not mean the arms behaved the same.

Two structural reasons the existing banks cannot answer this:

1. **`obsidian` is 12 of 12 single-source.** Categories are `concept_lookup`
   (5), `argument_reconstruction` (4), `numerical_fact` (3), and every question
   has exactly one `expected_sources` entry. There is nothing to bridge, so the
   mechanism has no way to express itself.
2. **`cross-corpus` has the right shape and the wrong corpora.** 8 questions,
   4–6 sources each — but it is SEP × Wikipedia, and GLiNER writes **zero**
   `chunk_entities` rows for either. It tests the meta-atlas bridge.

`conversation-private` at least carries the right *categories* (`entity_recall`
×3, `cross_conv_synth` ×2) — and notably, the single conversation-side question
that moved under ablation was `cross_energy_economics`, a `cross_conv_synth`
question, rigid source 1.00 → 0.50. n=1, no power, but not a random draw. That
is the signal this bench is built to resolve.

Also note the null was weak on its own terms: at n=12, a 10/12-vs-10/12 tie has
a confidence interval of roughly 0.55–0.98.

## Corpus and feasibility

Driver corpus: **`conversations-anthropic`** — the real archive on this machine.

| | |
|---|---|
| conversations (`conv_skeletons`) | 576 |
| `chunk_entities` rows | 68,464 |
| distinct entity surface forms | 20,830 |
| conversations carrying entities | 574 |
| `conv_raptor_nodes` (level 0) | 1,184 |

Bridge candidates, by how many distinct conversations an entity spans
(`score>=0.5`, surface length ≥4):

| span | entities | usable? |
|---|---|---|
| 1 conversation | 16,503 | no — nothing to bridge |
| **2–3 conversations** | **2,192** | **ideal** |
| 4–8 conversations | 646 | usable, weaker discrimination |
| 9–30 conversations | 227 | marginal |
| 31+ conversations | 24 | too common — retrieved anyway |

319 `Person` entities span 2–4 conversations with ≥4 mentions. The pool is
ample; question authoring is the cost, not candidate scarcity.

## Bank design

40 questions. Category mix chosen so each *bridging* category is independently
powered rather than pooled (see Power):

| category | n | what it tests |
|---|---|---|
| `entity_bridge` | 20 | answer needs 2+ convs linked ONLY by a shared entity |
| `entity_recall` | 8 | "everything about P" — recall across all P's conversations |
| `bridge_negative` | 6 | entity-adjacent but answer is in ONE conv; graph must not drag in noise |
| `positive_control` | 3 | second conv reachable ONLY by entity bridge, by construction |
| `cosine_control` | 3 | single-source, high lexical overlap; both arms must tie |

### The load-bearing control: lexical-overlap gating

A bridge only tests the entity graph when **cosine cannot already find the
second document**. If the second conversation shares distinctive vocabulary
with the question, embedding search retrieves it regardless and the entity edge
is redundant — the question measures nothing.

Authoring procedure, per `entity_bridge` question:

1. Pick a candidate entity spanning 2–3 conversations (query above).
2. Compute the centroid-embedding cosine between the two conversations
   (embeddings are already in `chunks.lance`). **Keep only pairs below a
   pre-registered threshold τ** — record the value as `cosine_gap`.
3. Author a question whose answer requires a fact from *each* conversation.
4. **Lexical-leak check:** the question's non-entity tokens must not contain
   distinctive terms from conversation B. If they do, cosine finds B on the
   text alone and the question is void. The shared entity should be the only
   path from the question to B.
5. Record both conversations in `bridge_sources`, and split `expected_facts` so
   at least one fact is reachable only from B.

Step 4 is what makes this a bench rather than a demo. It should be mechanically
checkable at authoring time, not left to judgement.

### Schema additions

New fields on `Question` (`eval_cmd/bank.rs:77`) — parser work required:

```toml
[[questions]]
id           = "..."
category     = "entity_bridge"
question     = """..."""
bridge_entity  = "..."            # NEW — the linking entity
bridge_sources = ["conv-A", "conv-B"]  # NEW — BOTH must be retrieved
cosine_gap     = 0.31             # NEW — recorded control, authoring-time
expected_facts   = [...]          # existing; must span both sources
expected_sources = [...]          # existing; keeps cross-bank comparability
notes            = "..."          # existing
```

`expected_facts` / `expected_sources` are retained unchanged so this bank's
answer-level numbers stay comparable with every other bank.

## Metrics — three levels

The existing axes stay; the new ones sit *below* them, at the level the
mechanism actually operates on.

**Level 1 — retrieval (new, primary).** The entity graph acts on ranking, so
measure ranking. No LLM judge, cheap, high information per question, immune to
generation noise.

- `bridge_recall@k` — fraction of `bridge_sources` present in top-k retrieved.
  **The primary endpoint.** A refinement of the existing
  `score_sources(expected, retrieved)` (`eval_cmd/score.rs:43`), restricted to
  the bridge set and evaluated at fixed k.
- `bridge_mrr` — reciprocal rank of the *hardest* required source (lowest
  cosine to the query). Catches ranking lift that `recall@k` rounds away.
- `distractor_rate` — fraction of top-k that are entity-linked but carry no
  expected fact. Guards the failure mode where the graph adds noise.

**Level 2 — answer (existing, unchanged).** `fact_score`, `source_score`,
`loose_source_score`. Kept for continuity with every other bank, and because
the L0 lesson is that rigid and loose must be reported *separately* — a rigid
delta with a loose tie is a real behavioural change, not a scoring artifact.

**Level 3 — connection quality (new, judge-scored).** The dimension no current
axis captures: did the answer actually *relate* the two sources, or merely cite
both? Binary + evidence span, following the `JudgeSourceDetail` pattern
(`score.rs:490`). Precedent for non-RAG axes already exists in
`EssayReadinessScore` (`score.rs:753`).

## Arms — dose–response, not binary

| arm | entities | `SOVEREIGN_CONV_PPR_WEIGHT` | isolates |
|---|---|---|---|
| A0 | present | `0` | entity path off entirely |
| A1 | present | `0.25` (production default) | production |
| A2 | present | `0.5` | dose up |
| B1 | **deleted** | `0.25` | RAPTOR-only graph (the L0 ablation) |

- **A1 vs B1** — GLiNER's contribution. The question.
- **A1 vs A0** — whether the entity path contributes at all.
- **A2 vs A1** — dose–response. A real mechanism responds monotonically to its
  own weight; a fluke does not. This is the strongest single piece of evidence
  the design can produce, and it costs one extra arm.

Ablation method: delete the corpus's `chunk_entities` rows (reversible,
snapshot first — the method validated 2026-08-03 against a known cold-build
number). **Do not** conclude anything build-side from it; a row-level ablation
observes retrieval only. That blind spot is what hid the typed-extension person
seeds last time.

## Controls

- **Positive control (3 q).** Second conversation reachable *only* by entity
  bridge. If A1 does not beat B1 here, the harness is not exercising the
  mechanism and **the entire run is void** — report `could-not-judge`, never
  "no effect". (ARCH §18.4: validate the instrument before the result.)
- **Negative control (3 q).** Single-source, high cosine. Both arms must tie.
  A difference here means run-to-run noise, not signal.
- **Variance first.** Run A1 three times before any comparison and report the
  run-to-run spread. One run is not a measurement (ARCH §18.5). If spread
  exceeds the effect being claimed, the bench cannot answer the question.

## Pre-registered hypothesis and falsifier

Written before the first comparison run, and not edited after.

> **H1.** On `entity_bridge` questions, `bridge_recall@10` is higher in A1 than
> in B1 by ≥ 0.15 absolute, and A2 ≥ A1.
>
> **Falsifier.** If `bridge_recall@10` differs by < 0.05 between A1 and B1 on
> `entity_bridge` while the positive control passes, the entity layer does not
> bridge in production, and the GLiNER ingest pass has no retrieval
> justification on this corpus.

The falsifier firing is a **result**, not a failure — it converts a held
deletion into a funded one, and it is the outcome the roadmap's subtraction
directive is actually asking for.

## Power — and its honest limit

Paired design (same questions both arms), continuous endpoint in [0,1].
At n=20 with an assumed paired sd of ~0.3, the bench has roughly 80% power to
detect a **0.20** absolute difference in `bridge_recall@10`.

It is **not** powered for subtle effects: detecting 0.10 would need ~70
questions. State this in the result. A null from this bench licenses "no large
bridging effect", never "no effect".

## Privacy and file layout

`conversations-anthropic` is a real personal archive. Follow the established
split (`.gitignore:69-82`):

```
sovereign/bench/conversation-bridge/          # public — this README + fictional scaffold
sovereign/bench/conversation-bridge-private/  # GITIGNORED — the real bank
```

Add `sovereign/bench/conversation-bridge-private/` to `.gitignore` **in the
same commit** that creates it. The public scaffold stays fictional and will
score ~0 by construction — that is intended, and matches `bench/conversation/`.

## Run protocol

```bash
# 0. snapshot the entity rows before any ablation
sqlite3 ~/.svrnmesh/sovereign.db \
  ".dump chunk_entities" > scratchpad/bridge-bench/chunk_entities.sql

# 1. variance: three identical A1 runs, no comparison yet
for r in 1 2 3; do
  SOVEREIGN_CONV_PPR_WEIGHT=0.25 sovereign eval run \
    --bank sovereign/bench/conversation-bridge-private/questions.toml \
    --prod-pipeline --loose-source-judge --isolate \
    --out scratchpad/bridge-bench/A1-r$r.json
done

# 2. arms A0 / A2, then B1 after deleting rows for this corpus only
# 3. restore, verify row count + quick_check
```

`--isolate` **is** correct here (unlike `cross-corpus`): the bridge under test
is inside one corpus, so retrieval must be sealed to it. `--prod-pipeline` and
`--loose-source-judge` together, per the 2026-08-03 fix at `7480f697`.

## Cost, and when to abandon

Authoring 40 questions against a private archive is the real cost — call it a
day of focused work, and it cannot be delegated to a subagent (the archive must
not leave this machine). Compute is minor: 4 arms × ~40 questions, plus 3
variance runs.

**Abandon if** the variance run shows run-to-run spread above ~0.15 on
`bridge_recall@10`. At that point the endpoint is too noisy for the effect size
this corpus can produce, and the honest move is to say so rather than author 40
questions the bench cannot score.
