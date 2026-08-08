# Native Grounding — decode-rooted calibration to replace the judge stack

**Status:** skunkworks design, branch `skunkworks/native-grounding`. Nothing here is
shipped; every mechanism lands dark behind a flag until its kill/keep gate (§8) is
settled. On merge to main, every dark flag gets a `DEFAULTS_LEDGER.md` row.

**One sentence:** replace the post-hoc LLM-judge control stack (~7,900 non-test LOC,
~35 judge calls per gated longform turn, five string-namespace abstention deciders)
with grounding mechanisms that live where we have privileged access — the inference
stack — so that every anti-hallucination property is **structural** (enforced by
code), **statistical** (a calibrated score with an operating curve), or **trained**
(weights), and none of it is a prompt asking a model to police another model's prose.

---

## 1. The hypothesis

> **H0 (umbrella):** A local-first system that owns its inference engine can match or
> beat the incumbent grounding gate on the chaos-monkey red-lines
> (competence-when-present, honesty-when-absent, hallucination ceiling) at ≥5x lower
> gated-turn latency and with a control surface that is at least 4,000 LOC smaller,
> by moving the intervention from post-hoc prose-judging to: (a) pre-generation
> answerability routing, (b) decode-time evidence-constrained token selection,
> (c) sampling-distribution uncertainty, and (d) mechanical span attribution.

Falsifiable at the top: if the integrated native stack (§8 Phase 5) cannot hold the
committed chaos baselines within the HARD-lane tolerances (`gate.rs:494-575`) on both
banks while cutting p50 gated-turn latency ≥5x, the initiative does not graduate and
the branch is archived with a postmortem note.

Sub-hypotheses H1–H5 in §5, each with its own measurement and kill criterion in §7.

## 2. Why the incumbent is at a local maximum

Six months of working notes describe one failure shape recurring at every layer of
the current stack: *a model reading another model's prose through a prompt*, and the
prompt drifting, diverging across judge models, or trading one facet against another.

The inventory (full delete-list in §9):

- `runtime/grounding/` is ~9,050 LOC (~6,000 non-test). `gate_longform`
  (`grounding/mod.rs:1858`) runs a sequential per-claim audit **twice** (draft +
  rewrite; the "full re-audit is the safety floor" contract at
  `grounding/config.rs:320`), landing at ~35 judge calls per gated longform turn
  (`DEFAULTS_LEDGER.md:848`). Measured medians 22–34s per turn; surgical rewrite
  bought longform back to ~90–103s end-to-end.
- Judge divergence is documented, not hypothetical: `classify_caveat` returned
  opposite verdicts from the 4B vs the 35B on the same frozen answer (note
  `0b747975`); the fix was prompt surgery — the treadmill this design exits.
- Prompt carve-outs trade facets: the honesty carve-out regressed competence
  0.67→0.58 and was reverted (note `dd072a9e`) because the generator cannot
  discriminate present-vs-absent. A prompt cannot grant a capability.
- Abstention is decided in **five** places and rendered in **three**, coupled by a
  string namespace (`meta["action"]`) and a 17-phrase decline substring list
  (`grounding/mod.rs:1577`), plus a 17-opener refusal detector
  (`prompts.rs:775`). Smell-table row one: a match on string ids with many arms.
- The system prompt carries a 166-line, ~2,500-token behavioral plea
  (`prompts.rs:25-190`) paid on every knowledge turn.
- Threshold forks happen silently (the 0.5-vs-0.9 τ divergence, fixed 2026-07-30).

None of this is misdesign in isolation — each piece was the correct next patch. The
entrenchment is architectural: **the model is treated as a black box behind prompts,
so control can only be prose-in, prose-out.** We are not a black-box consumer. We
vendor llama.cpp, we own the sampler, we see every logit.

## 3. Design principles

1. **Structural > statistical > judged.** Prefer an invariant code can enforce
   (span must resolve, prefix must be present); else a calibrated score with a
   measured operating curve; a generative judge only as the escalation tier of last
   resort (and then the already-decided disagreement pattern, note `700bbe09`).
2. **Every score ships with its curve.** No raw threshold enters the runtime without
   a calibration artifact (held-out AUROC, recall at bounded false-alarm budgets)
   checked into the branch. The instrument is validated before the result (§18.4).
3. **One typed verdict, one decider.** The action-string namespace is replaced by a
   typed `GroundingVerdict` (§6) with a compatibility shim, so the five downstream
   consumers keep working while the deciders collapse to one.
4. **Provenance is a type, not prose.** Answers are assembled from typed segments
   (grounded span / parametric / inference). The caveat is a *rendering* of the
   type; measuring it is a field read, not `classify_caveat`.
5. **Glassbox at decode granularity.** Per-token confidence and per-segment
   provenance are surfaced on the wire, not derived after the fact.

## 4. What the stack already gives us (verified surfaces)

The load-bearing primitives exist today; the design composes them.

| Primitive | Where | State |
|---|---|---|
| Lockstep multi-sequence decode | `ModelSlot::generate_sync_batched`, `sovereign-inference/src/embedded/model_slot.rs:3333` | live (FastShort batching); per-seq logit rows tracked (`current_logit_idx`) |
| Raw logits per step | `vendor/llama-cpp-4/src/context.rs:357,385` (`get_logits`, `get_logits_ith`) | live |
| Rust logit-processor hook | `ConstrainedSampler::sample`, `embedded/sampler.rs:126` — mutates the materialized `LlamaTokenDataArray` before the chain; 4 maskers already use it (llguidance, URL, **evidence-id**, non-Latin) | live |
| Shared-prefix KV across sequences | `copy_kv_cache_seq`, `vendor/llama-cpp-4/src/context/kv_cache.rs:47` | live, unused for this |
| Calibrated forced choice (logprob softmax over candidates) | `forced_choice_probs`, `model_slot.rs:659`; wire sentinel `x_forced_choice` (`sovereign-contracts/src/types/completion.rs:332`) | live — the gate's own judges ride it |
| Cross-encoder yes/no margin | `RerankSlot`, `embedded/rerank_slot.rs:95` `YesNoLogit` — `logit("yes") − logit("no")`, Qwen3-Reranker-0.6B, ~22.7 ms/pair batched (SP4) | vendored, **default-inert** (`SOVEREIGN_RERANK_MODEL_PATH`) |
| Second-context-same-weights slot | `ModelSlot::from_existing_model`, `model_slot.rs:1347` (FastShort pattern, `n_seq_max=8`) | live |
| Small-model residency | extras path: `engine.rs:1644 load_extra`, LRU + VRAM budget (`inference.rs:171`) | live |
| Activation read (and write) mid-forward | `tensor_capture.rs` / `tensor_transaction.rs` in `vendor/llama-cpp-4` | vendored, **zero callers** |
| Deterministic witness kernel | `contains_ci` / `gold_match` / `value_present`, `sovereign-eval/src/flywheel/det_checks.rs` | live |
| Threshold-calibration harness | `research/verifier-v0/scripts/{calibrate_threshold,operating_curve,contamination_pass}.py` | research, reusable |

Known contract gaps (Phase 0 closes them): `CompletionRequest`/`Response` has no
`n>1` and no `logprobs`; per-slot `Semaphore::new(1)` means only FastShort holds a
multi-seq context; chaos `rescore` re-invokes the Critic instead of reading the
frozen `violation_prob`, and no offline τ-sweep exists.

**Correction, 2026-08-07 (measured — and it changes Phase 0's shape).** This
paragraph used to end "despite the column being frozen". The column was never
frozen. `violation_prob` existed in the `ResultRow` schema and in the writer
(`chaos_monkey.rs:840`), but **no committed chaos run ever recorded a value**:
across the 15 artifacts in `sovereign/bench/chaos_monkey/results/`, 12 carry the
key on all 468 of their rows and every one is `null`; the other three predate the
field. Zero numeric values anywhere. The cause is structural rather than a bug —
the Critic is consulted only under `--grounding-verify` or `--gv-shadow`
(`chaos_monkey.rs:840`), and no committed run passed either flag. So the
incumbent's operating curve was not sitting in the artifacts waiting to be read:
**Phase 0 must mint the column before it can sweep it**, with one live
`--gv-shadow` run over `secret_agent.toml`. That run is Phase 0's deliverable 0
(§8). Nothing else in this design changes — the τ-sweep reader is exactly as
specified; it simply had no input until now.

## 5. The five mechanisms

### H1 — Answerability routing: a calibrated containment score before any token is generated

**Claim:** a 0.6B cross-encoder margin over (question, chunk) pairs separates
answerable from absent better than both `top_cosine` (the shipped-dark early-decline
signal, known to measure topic not containment) and, per honesty-when-absent
outcome, the incumbent 35B post-hoc gate — at ~200 ms instead of tens of seconds.

**Mechanism.** Activate the rerank slot as an **answerability scorer**:
`answerability(q) = max_i margin(q, chunk_i)` over the top-k retrieved chunks
(k ≤ 8, batched, ~180 ms). The score is calibrated (§7) into three regions:

- `answer` — proceed to generation, evidence-constrained (H3);
- `hedge` — proceed, but the answer is born as `Parametric`-typed segments with the
  structural GK prefix committed (`GK_CAVEAT_PREFIX` as `assistant_prefix`, the
  existing mechanism at `handlers/knowledge_query.rs:503`);
- `abstain` — emit `GroundingVerdict::CannotKnowFromHere` **before generation**; the
  coverage probe / acquisition resolver runs as today.

Insertion point: exactly where `compute_evidence_shape` feeds
`evidence_early_decline` today (`handlers/knowledge_query.rs:541,640`) — the routing
seam already exists and is dark; we are replacing its instrument, not its plumbing.

**Why this can win where top_cosine couldn't:** the reranker is *trained* on
query→passage answerability; cosine is trained on topical similarity. The failure
that killed the early-decline floor ("~0.75 in-topic thin", note on evidence-shape)
is the exact distinction a cross-encoder head learns.

**Escalation:** answerability in an uncertainty band (calibrated, expected ~10–15%
of turns) MAY escalate to one `forced_choice_ab` probe on the primary. This is the
only place a big-model judgment survives in the pre-generation path.

### H2 — Semantic entropy: sampling-distribution uncertainty as the confabulation detector

**Claim:** when the answer's asserted value is absent from evidence, k independent
samples diverge in *meaning*; when present, they agree. The entropy of the
distribution over meaning-clusters (semantic entropy, the Farquhar et al. 2024
line — confabulation detection via entropy over bidirectional-entailment clusters,
not over surface strings) predicts the hallucination label at least as well as the
35B Critic's `violation_prob`, at bounded cost and with zero judge prompts.

**Mechanism.** For turns that pass H1 routing (and as the *replacement* for the
single-claim `verify_grounding` path at `grounding/mod.rs:992`):

1. Sample k=5 **short-form answer values** (not full prose) via one batched
   multi-seq decode (`generate_sync_batched` pattern; shared evidence prefix via
   `copy_kv_cache_seq`, so prefill is paid once), temp ~0.7, ≤24 tokens each — the
   same budget `extract_answer_value` uses today (`value_presence.rs:89`).
2. Cluster the k values by **meaning equivalence**, cheapest instrument first:
   (a) the deterministic kernel (`value_present` normalization, stopword-stripped
   AND-match) merges exact/near-exact values; (b) survivors are merged by
   **bidirectional entailment via the reranker margin** — `margin(a→b)` and
   `margin(b→a)` both above the clustering floor collapses a pair (this is the
   faithful port of the original method's entailment clustering, at ~23 ms/pair
   over at most C(5,2)=10 pairs); embed-slot cosine is the tie-breaker, not the
   decider, because cosine measures topic — the same flaw that killed `top_cosine`
   must not be smuggled into the clusterer.
3. Two statistics over the clusters, both logged, gated on whichever wins its
   calibration (§7.3):
   - **`semantic_entropy = −Σ_c p(c)·log p(c)`** over meaning-clusters — the
     primary. Count-based `p(c) = |c|/k` at k=5; once H5's `logprobs` land,
     `p(c)` upgrades to the sequence-probability-weighted estimate (sum of
     normalized sample likelihoods per cluster), the full Farquhar formulation.
     Entropy sees distribution *shape*: a 3-1-1 split and a 3-2 split have equal
     agreement but different entropy, and that tail structure is where
     hedge-vs-abstain lives.
   - **`agreement = |largest cluster| / k`** — the degenerate cheap statistic,
     kept as the fallback and as a cross-check on the entropy estimate at small k.

   The calibrated threshold on the winning statistic maps to answer / hedge /
   abstain, same three-way as H1. High entropy on an H1-`answer` turn is the
   disagreement signal that triggers the escalation tier.

This targets the documented model bound head-on: the generator "can't tell
present-vs-absent for a specific fact" (note `dd072a9e`) — but its *sampling
distribution* can, without the model ever being asked.

**Cost model:** k=5 × ≤24 tokens on a multi-seq context = one prefill + ~120
lockstep decode steps. Against the current single-claim path it replaces (claim
extraction + up-to-12 forced-choice chunk probes on the 35B), this is strictly
cheaper; against nothing (ungated turns) it is new spend, bounded and flag-gated.

### H3 — Evidence-tilted decoding: contrastive context adherence at token selection

**Claim:** amplifying evidence-conditioned logits against evidence-free logits
(context-aware decoding) measurably reduces fabricated specifics and distractor
uptake *during* generation, shrinking what any downstream check has to catch.

**Mechanism.** Two sequences in one context, lockstep:

- seq 0: full prompt (system + evidence + question) — the normal turn;
- seq 1: evidence-ablated prompt (system + question), sharing the question suffix
  KV where alignment permits (`copy_kv_cache_seq`);
- per step, read both logit rows (`get_logits_ith`), combine
  `z = (1+α)·z_ctx − α·z_noctx` inside a new masker at the existing
  `ConstrainedSampler::sample` hook, sample **once**, feed the same token to both
  sequences (`model_slot.rs:3475-3524` loop, unified sampling).

α is a per-request knob, default from calibration; α=0 is bit-identical to today's
path (the safety rail and the A/B lever in one). The per-step token-level
divergence `KL(p_ctx ‖ p_noctx)` is logged per segment — it is *free provenance
telemetry*: spans where the two distributions agree are exactly the spans the
evidence did not determine (parametric candidates for H4's typing).

**Honest cost accounting:** ~2x decode FLOPs on gated turns, plus the known
full-vocab mask cost (152K entries/step, `sampler.rs:118`) doubled. This buys
deletion of a 35-call judge fan-out; the trade is measured, not assumed (§7).

### H4 — Mechanical attribution: spans resolve or they don't

**Claim:** citation fidelity and longform claim-audit can be scored by span
resolution (verbatim/fuzzy containment in the sealed evidence) plus per-sentence
reranker margins — deterministic, milliseconds — killing the twice-run longform
audit and `verify_grounding`'s free-text claim extraction.

**Mechanism.**

- **Grounded segments carry addresses.** Synthesis output is assembled into typed
  segments (§6). A segment claiming `Grounded{chunk_id, span}` is verified by the
  deterministic kernel (`contains_ci` + the ≥2-word verbatim-phrase shortcut that
  `value_present_in_chunks` already implements). Resolution failure demotes the
  segment to `Unverified` — rendered distinctly, never silently released as
  grounded. The quote-verification guardrail (`quote_verification.rs`) already does
  exactly this for quotation marks; H4 generalizes it to all grounded segments.
- **Longform audit → sentence sweep.** Sentence-split (the lossless splitter at
  `surgical.rs:42` survives), score each sentence against the sealed evidence with
  the reranker (~23 ms/pair, batched); sentences under the calibrated margin with
  fabrication-shaped content (the existing deterministic vetoes at
  `judge.rs:890,974` survive as cheap structural checks) get the surgical
  Delete/Fix treatment (`surgical.rs:227` survives). The 2×(extract + per-claim
  judge + rescan) 35B ladder is deleted.
- **Constrained citation emission.** The `evidence_id_constraint` masker already
  forces emitted source ids to be real; extend it so a `[Source: N]` token sequence
  can only follow content whose sentence-margin cleared the floor — the citation
  cannot syntactically outrun its support.

### H5 — Confidence on the wire: logprobs, agreement, and provenance as first-class fields

**Claim (enabler, not independently gated):** none of H1–H4 can be measured or
trusted glassbox-style without the contract carrying the numbers.

**Mechanism.** Contract additions (`sovereign-contracts/src/types/completion.rs`):
`n: Option<u8>` (bounded multi-sample), `logprobs: Option<bool>` +
`token_logprobs` on the response, and the `GroundingVerdict` + segment array (§6)
on chat responses. The desktop/CLI render provenance from the typed segments —
the user-facing form of family F: *you can always see which words are sourced,
which are the model's, and how sure the system was*.

## 6. The typed contract that replaces the string namespace

```rust
/// sovereign-contracts — one decider's output, everything downstream reads this.
pub struct GroundingVerdict {
    /// Three-way, decided ONCE (H1 routing, revisable only by H2 agreement).
    pub decision: GroundingDecision,       // Answer | Hedge | Abstain
    /// Calibrated answerability from the containment scorer (H1). 0..1.
    pub answerability: f32,
    /// Semantic entropy over meaning-clusters from the k-sample gate (H2), when
    /// run. 0 = unanimous; log(k) = full divergence.
    pub semantic_entropy: Option<f32>,
    /// Largest-cluster fraction (the degenerate cheap statistic). 0..1.
    pub agreement: Option<f32>,
    /// Which mechanism decided (glassbox: every decision names its decider).
    pub decided_by: DeciderId,             // Router | AgreementGate | Escalation | Structural
    /// Per-segment provenance of the released text (H4).
    pub segments: Vec<AnswerSegment>,
}

pub struct AnswerSegment {
    pub text_range: Range<usize>,
    pub kind: SegmentKind,                 // Grounded { chunk_id, span } | Parametric | Inference | Unverified
    pub margin: Option<f32>,               // reranker sentence margin, when scored
}
```

**Compatibility shim, named and single:** `verdict.to_gate_action() -> &'static str`
emits the legacy action strings so `epistemic.rs:85`
(`gate_action.starts_with("abstained")`), the ledger holdings, the gap-check card,
the chaos scorer's typed-verdict parity path (`score.rs:215`), and the GK rescue all
keep functioning unchanged during the transition. The shim is the *only* writer of
action strings on the native path; deleting it at graduation is the final cutover.
`GateOutcome.claims` (the ledger's holdings basis) is fed from `segments` — a
grounded segment is a holding with a real address, which is strictly more than the
incumbent gives the ledger today.

## 7. Measurement — how each claim gets to say it won

### 7.1 Datasets and contamination discipline

Three roles, never mixed; the split is fixed **now**, before any tuning:

| Role | Data | Why |
|---|---|---|
| **Calibration** (fit thresholds, α, k) | Flywheel-mined probes (`flywheel/generators/corpus.rs:145` `held_out_witness` machinery) over the **SEP substrate** — 1,770 `sep-<slug>` atlases carrying 59,100 `Claim` atoms — plus `brothers-karamazov-book-1` (installable in 3s from HF) as the literary minority; thousands of (question, chunks, answerable?) pairs, labels from the fairness contract, zero hand-tuning against shipped banks | volume + label mechanics already exist |
| **Development / held-out** | `saltgrass.toml` + `saltgrass_compound.toml` | carries `superseded_trap` / `partially_present`, the hard cells |
| **Test — touched only at phase gates** | `secret_agent.toml` (43 probes) + its committed baselines | the SSOT the CI gate already trusts |

**Substrate correction, 2026-08-08 (measured — this row used to name wikipedia
as the volume source).** Wikipedia cannot feed the calibration role and never
could: its atlas is 800 MB holding 1,773,106 atoms, and a full scan finds every
one of them to be `Entity`. There are **zero `Claim` atoms**, so the
claim-mining substrate this path is built on does not exist there. Producing
them is an enrichment question, not a miner question, and it is not in Phase 1.

The volume comes from SEP instead, and its shape needed one fact establishing
before it could: the 1,770 `sep-<slug>/` directories are **atlases only** — not
one carries a `chunks.lance`, and only 2 of the 1,770 source articles survive on
disk. Their passages live together in a single `sep` corpus of 187,967 chunks
keyed by `source_doc_id`, with exactly 1,770 distinct values, one per atlas
(`flywheel::passages::chunk_store_for` is the one name for that mapping).

Two consequences the miner is built around, both measured on this host:

- An atom's evidence `chunk_id` (`sec_0002`) is **not resolvable** here.
  `CorpusIndex::resolve_sections_to_chunks` keys on a `section_id` in chunk
  metadata; across all 42 installed corpora that have a chunk store, 146,596
  chunks carry non-null metadata and 39 carry `section_id` — none in a corpus
  this initiative mines. Passages are resolved by **verbatim anchor
  containment** instead, and a claim whose quoted fragment is found in no real
  passage is dropped and counted (30.5% of SEP claims), never attached to the
  nearest-looking chunk.
- The delivered set is **4,207 pairs from 1,346 articles** (median 4 per
  article), contamination-clean against all three banks. The literary family is
  19 pairs — bk-book-1's entire ceiling at 13 `Claim` atoms — which is too thin
  to carry a per-family curve, and the H1 report says so rather than printing
  one.

`research/verifier-v0/scripts/contamination_pass.py` runs against every calibration
set before use (a contamination report for chaos already exists —
`findings/contamination_report_chaos.json` — the pattern is proven).

Every calibration artifact (operating curve JSON, chosen threshold, fold spread) is
committed next to the code that reads it. A threshold with no committed curve fails
review — principle 2.

### 7.2 Instrument validation before results (Phase 0 exit criteria)

- **Offline τ-sweep exists and reproduces:** a `rescore`-family subcommand reads
  frozen `violation_prob` from transcripts (no Critic re-invocation) and reproduces
  the live gated verdicts at τ=0.9 exactly, then emits the full τ curve from one
  artifact. This retires a documented gap and gives us the incumbent's operating
  curve — the thing H1/H2 must beat — from runs we already have.
- **Scorer determinism floor:** chaos `rescore` on a frozen transcript is
  bit-stable across 3 repeats for every mechanical facet (it is today; we pin it
  with a test so the new facets inherit the requirement).
- **Latency harness:** per-turn stage timings (route / synth / verify) land in the
  transcript sidecar so latency claims are measured per-probe, not estimated.

### 7.3 Per-hypothesis gates (kill criteria are the point)

**H1 (routing).** Offline, no generation needed: score (question, retrieved_chunks)
pairs against bank labels.
- *Metrics:* AUROC answerable-vs-absent; honesty-recall at false-alarm budgets
  {5%, 10%, 20%} (the verifier-v0 convention); macro BAcc across banks.
- *Beat:* `top_cosine` on the same pairs (the incumbent signal), by ≥0.10 AUROC;
  and the end-to-end incumbent honesty-when-absent on dev at equal-or-better
  competence proxy (fraction of answerable turns routed `answer` ≥ 0.95).
- *Kill:* if the reranker margin AUROC < top_cosine + 0.05 on calibration data,
  H1 dies before any runtime integration and we fall back to training the 4B for
  this head (verifier-v0 continuation) as the router instead.

**H2 (semantic entropy).** Offline against frozen + fresh transcripts with labels.
- *Metrics:* AUROC of `semantic_entropy` AND `agreement` vs hallucination label
  (per `is_hallucination`, `score.rs:281`), reported side by side — the gated
  statistic is whichever wins held-out, and if entropy does not beat agreement by
  ≥0.02 AUROC the cheaper statistic ships (complexity must pay). Compare both on
  the same probes to the Critic's frozen `violation_prob` AUROC. Once H5
  `logprobs` land, re-run with probability-weighted `p(c)` and report the delta
  vs count-based — the upgrade is kept only if it moves the curve.
- *Beat:* within 0.05 AUROC of the Critic at <20% of its per-turn judge cost, or
  better than it at any cost.
- *Kill:* if k=5 agreement cannot separate the saltgrass fabrication cases the
  Critic catches, the escalation tier stays permanent (H2 degrades to a triage
  filter in front of the incumbent single-claim verify, still deleting the
  longform ladder via H4).

**H3 (evidence-tilted decoding).** Full chaos runs, α ∈ {0 (control), 0.5, 1.0},
3 seeds each, dev banks.
- *Metrics:* competence, hallucination_rate, distractor_evasion,
  grounding_fidelity (all existing lane metrics — `gate.rs:494` picks up finite
  additions non-breakingly); decode tok/s.
- *Beat:* hallucination_rate and distractor uptake improve vs α=0 with competence
  within lane tolerance (0.15) and decode throughput ≥ 0.45x baseline.
- *Kill:* competence regression beyond tolerance at every α that helps
  hallucination → CAD dies; H4's sentence sweep carries the load alone. (This is
  the *expensive-to-run, cheap-to-decide* experiment; it runs after H1/H2 are
  settled so the runs double as integration soaks.)

**H4 (mechanical attribution).** Rescore-first: replay frozen transcripts through
the span resolver + sentence margins.
- *Metrics:* agreement rate with the incumbent longform audit's per-claim verdicts
  (from fresh instrumented gate runs on dev); citation_fidelity and
  grounding_fidelity deltas; per-turn audit wall-time.
- *Beat:* ≥0.90 verdict agreement on claims the incumbent judged with high margin
  (|vp − τ| > 0.2), audit time ≤ 2s p50 (vs ~35 calls × per-call latency), and —
  the measurement dividend — `classify_caveat` usage in the chaos scorer drops to
  zero (caveat becomes a segment-type read).
- *Kill:* if span/margin scoring disagrees with the incumbent on >25% of
  high-margin claims *and* hand-adjudication (20-claim sample, the `score-answer`
  seam at `chaos_monkey.rs:1307` makes this one command per claim) sides with the
  incumbent, mechanical-only attribution is insufficient and H4 keeps a
  forced-choice escalation rung for contested sentences.

**H0 (integration, Phase 5).** The headline A/B: incumbent stack vs native stack,
full chaos on dev + (once, at the end) test, 3 seeds.
- Competence, honesty, hallucination within/better than committed-baseline
  tolerances on `secret_agent`;
- p50 gated-turn latency ≥5x better on longform-class turns, ≥2x on short;
- zero LLM-judge calls on the happy path (escalation tier reserved for the
  calibrated uncertainty band, target <15% of turns);
- chaos scorer LLM dependence reduced to `judge_correctness` fallback only
  (`caveat_present` structural, `asserted_value_grounded` via H2 clusters +
  det kernel, `violation_prob` retired in favor of `answerability` /
  `semantic_entropy`, which the lane gains as new HARD metrics).

### 7.4 Noise handling

HARD verdicts come only from deterministic facets (span resolution, routing on
frozen pairs, rescore replays). Anything involving fresh generation runs 3 seeds
and reports the band; RUNBOOK §6 noise-band semantics apply unchanged. The τ-sweep
tool (7.2) exists precisely so incumbent-vs-native curves come from the *same*
frozen artifacts wherever possible.

## 8. Phasing — aggressive, but every phase ends at a gate

| Phase | Builds | Ends when | Est. scope |
|---|---|---|---|
| **0 — Instruments** | **mint the `violation_prob` column** (one live `--gv-shadow` chaos run — see the §4 correction: no committed run ever recorded one); contract fields (`n`, `logprobs`); offline τ-sweep reader; stage-timing sidecar; calibration-set miner over brothers-karamazov + wikipedia; contamination pass | 7.2 exit criteria green; incumbent operating curve in hand | small, pure addition + one live run |
| **1 — Router (H1)** | rerank-slot answerability scorer + calibration; dark wiring at the early-decline seam; `GroundingVerdict` type + shim | H1 gate settled on dev | the first real win or the first real kill |
| **2 — Agreement (H2)** | batched k-sample value decode on a multi-seq context; clustering; calibrated three-way | H2 gate settled | needs Phase 0's `n>1` |
| **3 — Attribution (H4)** | segment typing in synthesis assembly; span resolver; sentence-margin sweep; structural caveat | H4 gate settled; chaos scorer runs judge-free on caveat/citation facets | biggest delete unlock |
| **4 — Tilted decode (H3)** | two-seq CAD lane on the primary (own `n_seq_max=2` context); KL telemetry | H3 gate settled across α grid | the expensive experiment, last for a reason |
| **5 — Integration (H0)** | native pipeline behind `SOVEREIGN_NATIVE_GROUNDING=1` end-to-end; incumbent untouched and default | H0 A/B on dev, then one test-bank run | the verdict |

Phases 1–3 are independently valuable and independently killable; the ordering
front-loads cheap offline verdicts (routing and rescore replays need no live model
runs) before anything that costs soak time. H2 and H4 can proceed in parallel once
Phase 0 lands.

**Residency plan:** router + embedder = 0.6B + 0.6B alongside the 35B primary —
within the extras budget on the 20GB-card profile (`models.toml:73`), but the 64GB
SIGTERM incident (note `b57b0cd5`) says: Phase 1 includes a capacity check via the
existing `capacity.rs` fit gate before the rerank slot loads, and the bench runs
pin residency the way `run_live_pinned` already does.

## 9. Deletes ledger (the ratchet this plan is funded by)

Nothing is deleted until H0 graduates; this is the *funded target*, phase-tagged.
Survivors are named too — deleting them would be whack-a-mole in reverse.

| Component | LOC (non-test) | Fate | Phase |
|---|---|---|---|
| `gate_longform` ladder + batched triage + rescan (`mod.rs:1858-2660`, `judge.rs` longform prompts) | ~1,400 | **delete** → H4 sentence sweep | 3→5 |
| Single-claim verify path (`mod.rs:992-1246`, `judge.rs:142-397`) | ~700 | **delete** → H1 route + H2 agreement | 2→5 |
| `classify_caveat` + caveat prose classification (bench + runtime) | ~150 | **delete** → structural segment type | 3 |
| Decline recognition zoo: 17-phrase `answer_declines`, `released_pure_decline`, refusal-opener list, `REFUSAL_RETRY_*` | ~250 | **delete** → decision is typed upstream; refusal-retry obsolete when abstention is a verdict, not prose | 5 |
| Retry machinery (retry floor, retry system notes, re-verify) | ~400 | **delete** → hedge is a first-class decision, not a failed answer re-rolled | 5 |
| `verify_grounding` 2-stage Critic + `violation_prob` | ~300 | **delete** → `answerability` + `agreement` | 5 |
| Anti-fabrication block of `KNOWLEDGE_SYNTHESIS_SYSTEM` (~60 of 166 lines, ~1k tokens/turn) | prompt | **shrink** — behavioral pleading replaced by structural channels | 5 |
| 8-surface `GateSurface` profile matrix + most of the 18 env flags | ~400 | **collapse** to the native flag set | 5 |
| `citation.rs` + `citation_attribution.rs` quote-then-answer path | ~1,800 | **absorb** — span resolution is its generalization; verbatim verifier survives inside H4 | 3→5 |
| Surgical rewrite core (`surgical.rs` splitter, best_match, Delete/Fix) | ~330 | **keep** — H4 reuses it as-is |  |
| Deterministic vetoes (`judge.rs:890,974`), numeric audit, quote verification | ~700 | **keep** — already structural, already cheap |  |
| Ledger/verdict/probe/acquisition (`epistemic.rs`, `collaboration.rs`, `acquisition.rs`) | ~900 | **keep** — consumers, not police; fed better inputs |  |

Net target: **≥4,000 LOC deleted**, judge-prompt count in the runtime from ~8 to 0
(escalation tier: 1 forced-choice template), env-flag count in the grounding family
from 18 to ≤6.

## 10. Risks, named

- **The reranker head may not transfer** from passage-relevance to
  answer-containment on our corpora. That is why H1's gate is offline, first, and
  cheap — and why the kill path (train the 4B head; verifier-v0 data pipeline
  exists) is written down before we start.
- **Agreement can be confidently wrong** — k samples can collapse onto the same
  parametric attractor (the `import_conversations` top-1 attractor precedent, note
  in epistemic closeout). Mitigation: H2 is measured against exactly those
  saltgrass fabrication cells, and CAD (H3) exists to decorrelate samples from
  parametric memory; the escalation tier is the backstop.
- **CAD may tax fluency or competence** — the α=0 bit-identity rail and the 3-seed
  α-grid keep this an evidence question, not a belief.
- **Segment assembly changes the synthesis prompt contract** — the model must emit
  markable structure. Mitigation: we already force structural emission elsewhere
  (llguidance, evidence-id constraint); worst case the segmenter is the sentence
  splitter + span resolver with zero model cooperation (attribution degrades
  gracefully from claimed to inferred).
- **Two banks are small** (43 + saltgrass). The flywheel miner is the volume
  answer for calibration, and test-bank touches are rationed to phase gates. Bank
  expansion (a third corpus bank via the installable brothers-karamazov) is Phase 0
  collateral, not a dependency.
- **Skunkworks drift vs main** — the branch clobbers freely, but `epistemic.rs:85`
  and the chaos scorer parity path are moving on main (peer sessions active).
  The shim (§6) is deliberately the *only* coupling surface; rebases stay cheap.

## 11. Out of scope, deliberately

- **Abstention LoRA / R-tuning** on the primary — the endgame if H2 shows the
  sampling distribution knows more than the greedy path, but training the primary
  is a different risk class; it gets its own design when the H2 data exists.
- **TensorCapture attention-provenance lens** — powerful and vendored, but a
  research instrument until H4's cheaper span resolution is proven insufficient.
- **Multi-node verification on the mesh** — nothing here precludes it; nothing
  here waits for it.
