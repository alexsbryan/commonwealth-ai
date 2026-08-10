# Stream B corruption harness — substrate survey + design (2026-07-29)

Grounding for VERIFIER_V0.md §3 Stream B / milestone M2, from a workspace
survey (Explore sweep) done while the M0 baselines ran. Everything below is
file:line-cited so the M2 session can start building without re-discovery.

## What exists (reuse before building)

**Substrate — chunks and typed entities:**
- Saltgrass text in-repo: `sovereign/bench/chaos_monkey/corpora/saltgrass-ledger.txt`
  (~8.3k words) + `corpora/SALTGRASS_FACT_LEDGER.md`. NOT installed under
  `~/.svrnmesh/bench-corpora/` on this machine — recipe
  `sovereign-recipes/chaos-saltgrass/recipe.toml` must be ingested first.
- Secret Agent text local at `~/.svrnmesh/bench-corpora/chaos-secret-agent/`
  (Gutenberg #974; chunking paragraph/2048/overlap 256, enrichment off, per
  `sovereign-recipes/chaos-secret-agent/recipe.toml:32`). Not in-repo.
- Entity-typed extractions ALREADY EXIST for entity-swap corruptions:
  `out/chaos-secret-agent.questions.json` (typed `section_extraction`) and
  `out/chaos-secret-agent.named-clusters.json`.
- Real (question, answer, evidence, label) tuples for calibration:
  `sovereign/bench/gap_check/bank.toml` (harvested from a live chaos run) and
  frozen run transcripts in `sovereign/bench/chaos_monkey/results/*.jsonl`.

**Corruption machinery (~20% of the spec taxonomy exists):**
- `sovereign-eval/src/mechanism_fidelity/perturb.rs` — seeded metamorphic
  perturbation engine (PerturbKind/Variant/expected_sign/apply), numeric-only
  today; the scaffolding (seeded StdRng, expected-sign contract) is the part
  to reuse.
- `sovereign-eval/src/mechanism_fidelity/classes/attribution.rs` — deterministic
  negate (:51) / reframe (:58) / with_distractor (:65) template transforms;
  crude but model-free.
- `sovereign-eval/src/flywheel/` — the intended plug-in seam:
  `Generator` trait (mod.rs:50) + `registry()` (:59) registers only
  `CorpusGenerator`; probe.rs:3 names "I2 adversarial" as a source but no
  adversarial generator exists. **That empty slot is where the corruption
  generator belongs.**
- `sovereign-eval/src/flywheel/redteam.rs` — `AnswerTransform` sabotage
  catalogue with capture/replay driver (`bench_cmd/redteam.rs`) and judge
  cache — the harness ergonomics to copy. (It corrupts answers to fool the
  gate; Stream B corrupts claims against evidence — sibling, not overlap.)
- Deterministic corruption-site checkers already in production:
  `grounding/value_presence.rs:37` (`AssertedValue`, `value_present_in_chunks`
  :152) and `grounding/citation.rs:378` (truncated/altered numeric guard).

## Five places the spec's assumptions don't hold (M2 pre-work)

1. **`extract_claim_list` has no callable surface.** It is an LLM call,
   `pub(super)` in `sovereign-core/src/runtime/grounding/judge.rs:375`, sole
   call site `grounding/mod.rs:1478`; no CLI verb, no HTTP endpoint. Smallest
   fix: a `pub` wrapper + a `svrn bench` stdin/stdout verb (the shape of
   `chaos_monkey.rs:1234 score-answer` is the right seam pattern). Until
   then, no script can produce claims "in the production register".
2. **No OCR-garbling generator exists anywhere** — all OCR hits are the real
   ingest pipeline. Entity swap, number/date perturbation (textual), true
   negation (beyond a "categorically false" prefix), and cross-chunk chimera
   also don't exist. These get written fresh, deterministic + seeded.
3. **The fairness contract doesn't check what M2's gate implies.**
   `ChaosBank::validate` (sovereign-eval/chaos_monkey/question.rs:182-236)
   proves witness presence/absence + id uniqueness; it does NOT verify a gold
   keyword occurs in the corpus, and knows nothing about claim/evidence pairs
   or construction labels. M2 needs a corruption-site contract: for every
   generated case, mechanically re-check the corruption at its site
   (value_presence / citation guards / string witness) — extend
   `flywheel::case::validate_fairness` rather than invent a third contract.
4. **Corpus bytes are not uniformly available.** Secret Agent: local-only
   (recipe + setup script fetch). Saltgrass: in-repo but not installed.
   Harness runs need both installed corpora (their ids are the provenance).
5. **Secret Agent's bank has no Distractor/ProvenanceTrap exemplars**
   (reserved v2, secret_agent.toml:10-12); only Saltgrass carries provenance
   quotes. Distractor-absorption corruptions have hand-written exemplars only
   on Saltgrass (+ `bench/attached_doc/meridian_postmortem.toml` as the
   adjacent-doc source).

## Proposed architecture (one implementation, two consumers)

Corruption core as the missing **flywheel adversarial `Generator`** in
`sovereign-eval` (Rust, deterministic, seeded) — NOT a python sandbox script:
the fairness/validation machinery, typed probes, and failure classes it must
integrate with all live there, and the flywheel registry is an explicit
empty slot waiting for it. Two consumers:
- **Eval probes** (flywheel path, existing `Probe`/`Oracle` types) — feeds
  M2's "helpful on internal banks" arm and the M1/M3 hard-negative loop.
- **Training pairs**: a `svrn bench` export verb renders the same generated
  cases to Stream B JSONL (claim, evidence window, constructed label,
  corruption kind, site witness). Teacher labeling (chosen=35B, rejected=0.8B,
  discard-on-verdict-mismatch) stays a research/verifier-v0 python step — the
  label is fixed by construction before any teacher writes a word.

Taxonomy → mechanism mapping:

| Spec corruption | Source material | Mechanical site check |
|---|---|---|
| entity swap | named-clusters typed entities (same-type swap) | swapped surface form present in claim, absent at site |
| number/date perturbation | perturb.rs family + regex mining | `value_present_in_chunks` = false for perturbed value |
| negation/modal flip | new (real rewrite, not prefix) | polarity marker at site + original asserted |
| cross-chunk chimera | two chunks, fused claim | each fragment grounded in a different chunk, conjunction in neither |
| OCR/date garble | new: confusion-table garbler (0/O, 1/l, date digits) | garbled form fails `citation.rs` numeric guard |
| distractor absorption | meridian/holdout adjacent docs | absorbed fact present only in the distractor doc |
| unsupported-but-plausible | teacher-written addition | `value_present_in_chunks`/witness = false |

Hard-grounded half (timidity red line): paraphrase (reframe upgraded),
multi-hop-within-window, unit conversion — the last needs the numeric-audit
tolerance rules from `citation.rs` so a correct conversion isn't scored as a
perturbation.

## Build order for M2

1. Install saltgrass corpus; verify both corpus ids resolve.
2. `extract_claim_list` seam (pub wrapper + CLI verb) — small sovereign-core
   change, gates: lint --full + tests + callers/blast pre-flight on the
   grounding module.
3. Corruption generators (entity swap, number/date, negation, chimera, OCR,
   distractor) as the adversarial flywheel Generator + corruption-site
   validation contract.
4. Export verb → Stream B JSONL; teacher labeling script in research/.
5. Volume run: 20-40k pairs, 50/50 balance, contamination pass over the
   generated stream (same 13-gram machinery, external test sets).

Open question for Alex: does the corruption core live in `sovereign-eval`
proper (this doc's recommendation) or start in research/ python for iteration
speed and port later? The Rust path costs more up front but inherits the
fairness contract, seeded reproducibility, and the flywheel registry for
free — and avoids writing the taxonomy twice.
