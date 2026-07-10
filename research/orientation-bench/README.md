# Orientation-intent bench — smallest spike for the code containment rollup

*Protocol drafted 2026-07-09 from a design conversation. Status: RUN 2026-07-09 —
see FINDINGS.md (G1 failed: +0pts lift; NO-GO on retrieval wiring, GO on
content-assembly + fractal-drift reframings).*

## The question this spike answers

Do containment-rollup summary nodes (file → module → crate, derived from the
already-cached per-function code-intel summaries) earn their place for
**orientation-intent** retrieval — enough to justify building the full rollup
pass and wiring consumers (`project_context`, `brief`, reconcile recall)?

The honest null hypothesis, from our own three RAPTOR experiments (Wikipedia
vanilla +0, Wikipedia `--group-by-article` +0, memory-T3 rank-neutral, SEP
−14pts on pointed QA until additive): **leaves already orient you** — a
navigation question's embedding lands on a function summary in the right
module, and the node adds nothing. The spike must beat that null, not a straw
man. A "no" is a real answer: it kills the retrieval framing cheaply and
redirects the rollup to the derived-vs-asserted (fractal drift) framing, which
doesn't need retrieval lift to be valuable.

## Why the spike is cheap

The leaves already exist: `~/.sovereign/indexes/commonwealth-ai/code_intel_cache.json`
holds 31,719 function enrichments (`meta.file_path`, `body_hash`, `summary`,
`asks`). Spike scope is **corpus-engine only**: 5,886 cached leaf summaries,
249 files, 63 modules. Node generation is ~313 LLM calls (249 file + 63 module
+ 1 crate) — hours on the local slot, checkpointed. No Rust changes; Python
prototype per the phase-tuning playbook (prototype → port only on a "go").

## Pre-registered decision gates (written before any node is generated)

| # | Gate | Threshold | On failure |
|---|---|---|---|
| G1 | Orientation lift, arm C vs A, hit@5 on navigation+structure questions | ≥ +15pts absolute | Tree doesn't earn retrieval wiring; pivot to fractal-drift framing only |
| G2 | Pointed-question guardrail, C vs A hit@5 (mixed-pool policy) | regression ≤ 2pts | Additive-injection becomes mandatory in the port (never mixed-pool) |
| G3 | Node quality, 20 random nodes eyeballed against child evidence | 0 blatant confabulations (asserted specifics absent from children) | Fix prompt, regenerate, re-check — before scaling, not after |
| G4 | Negative controls (answers outside corpus-engine) | report-only: no node in top-3 for out-of-scope questions | Flags over-confident mis-orientation; informs injection gating |
| G5 | Cost | measured tokens/node → full-repo ignition + per-commit steady-state estimate | — (informational, feeds the scale decision) |

Interpretation note, pre-registered: if lift appears only on purpose-shape
questions ("what does module X do") but not navigation/structure — that is the
known "summaries win only when the question is itself coarse" mechanism, and
the scaling decision becomes **intent-gated injection** (as with SEP RAPTOR's
summarize-intent gating), not always-on retrieval.

## The bank (`bank.toml`) — authored FIRST, then frozen

**Ordering is the anti-contamination mechanism**: questions are written before
any node summary exists, so node vocabulary cannot leak into questions
(no-teaching-to-the-test). Sources for authoring: SYSTEM_OVERVIEW, module `//!`
headers, and "what would a week-1 developer ask" — phrased in user vocabulary,
no function names (naturalistic-first). Alex reviews and edits the bank before
freeze; post-freeze edits require a note in FINDINGS.md.

~45 questions, four shapes:

- **N — navigation (~20).** "Where does chunk deduplication happen?" /
  "Which part of corpus-engine decides which files get re-indexed?"
  Gold = *set* of acceptable module/file path prefixes (multi-home allowed).
- **S — structure (~10).** "How is the enrichment subsystem organized?" /
  "What are the major pieces of corpus-engine?"
  Gold = set of module paths a good answer's evidence must come from.
- **P — purpose (~8).** "What does the update module do?"
  Gold = the container path itself.
- **G — pointed guardrail (~10).** Conceptual→symbol questions in the
  CODE_INTEL_CHAT style (user vocabulary, answer is one function).
  Gold = the function's file (line span recorded for the audit).
  These measure displacement, the −14pt SEP failure mode.
- **Negative controls (~5).** Answer lives outside corpus-engine
  ("where is the mesh gossip logic?"). Gold = ∅ within scope.

Scoring is **deterministic** — path-prefix match at the gold's granularity,
hit@5 primary, MRR secondary. No LLM judge anywhere in the primary metric
(unit-of-judgment = unit-of-scoring; the judgment IS a path).

## Arms

All arms share one in-memory pool (numpy cosine), one embedder (daemon `embed`
alias — the production space), same top-k.

- **A (baseline):** 5,886 leaf summary texts (`summary` + `asks`, the
  validated render).
- **B:** A + 249 file nodes.
- **C:** A + file + module + crate nodes (313).
- **D (diagnostic):** nodes only — isolates node quality from blending.

Both retrieval policies measured on C: **mixed-pool** (nodes compete with
leaves) and **additive** (top-k leaves + top-n nodes, displacement structurally
impossible). G2 decides which policy the port must use.

Fidelity caveat (recorded, accepted): the spike pool is summaries-only, not the
production chunk index (no raw code chunks). This biases *against* the null —
if nodes can't beat leaf summaries here, they won't beat leaf summaries + raw
chunks in production. A "go" gets re-validated in-harness at port time
(`bench --synth` with retrieval_audit is the port target).

## Node generation recipe

- **File node:** child function summaries (name + one-line summary each) +
  the file's `//!` header verbatim (if present) + the file path. Prompt forces
  a user-vocabulary orientation summary + 2 asks — mirroring the leaf recipe,
  since summary+asks was the validated retrieval signal. ~200 output tokens,
  temp 0.2.
- **Module node:** child file summaries + `mod.rs` `//!`. **Crate node:**
  module summaries + `lib.rs` `//!`/README.
- Checkpointed to `nodes.json` after every batch (crash-resumable; the
  0-node-is-a-loud-failure lesson applies — a node generated from N children
  that comes back empty fails the run, it doesn't skip silently).
- **Side channel, not scored:** where the derived summary contradicts the `//!`
  header, log the pair to `drift_teasers.jsonl`. Free evidence for the
  fractal-drift arc; explicitly out of scope for the gates.

Model: same class as the leaf enrich used (fast slot via daemon HTTP,
`DISABLE_PEER_INFERENCE=1` for a local, reproducible run).

## Glassbox outputs

`results.json` (per-arm, per-shape hit@5/MRR) + `audit.md` — per question:
top-10 with source tier (leaf/file/module/crate), path, score, and
hit/miss against gold. Every gate verdict must be checkable from `audit.md`
in seconds (evidence-or-it-doesn't-ship, applied to our own bench).

## Steady-state cost estimate (G5)

From `git log --name-only` over the last ~200 commits: touched-files-per-commit
distribution → invalidated nodes per commit = touched files + O(depth)
ancestors. Report ignition (full-repo: ~1,600 file nodes + ~200 module nodes)
and per-commit steady state side by side.

## Order of work (~1–1.5 days)

1. **Author + freeze `bank.toml`** (half day, incl. Alex's review pass).
   Strictly before step 2.
2. `spike.py` — load cache → group by file/module → generate 313 nodes
   (checkpointed) → embed pool → run arms A–D, both policies → score →
   emit `results.json` + `audit.md` + `drift_teasers.jsonl`.
3. Eyeball pass (G3, 20 nodes) + gate table → FINDINGS.md with go/no-go
   and, on "go", the port sketch (rollup pass in `code_intel`, namespaced
   `codeintel:file:<path>` chunks, `granularity` field, injection policy
   per G2).
