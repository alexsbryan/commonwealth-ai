# Header-reconcile spike — findings (run 2026-07-09)

## v2 run (same day; three taxonomy fixes; `out-v2/`)

**Both v2 gates FAIL. Precision got worse, not better (22% vs 43%), and the
known-real regression set recalled 1/5. Prompt-level fixes are exhausted; the
bottleneck is the 35B one-shot judge, not the harness.**

Numbers: 225 headers → 801 claims (466 corr / 288 not-ev / 44 contradicted) →
45 candidates → 23 machine-shipped → **5 real on manual source verification**
(sha256 again, plus four NEW reals v1 missed):

- `pipeline/atom_normalizer.rs:19` — header: default chain is "AnchorSnapProcessor today"; `default_chain()` registers two processors (`:104`).
- `pipelines/literary.rs:13` — header: "Phases 3/5/6/7 are scaffolded with stub compose/parse methods"; all four are fully implemented (`:212+`). The stub claim outlived the code.
- `pipeline/validation.rs:4` — header: reads "phase 3/5/6/7 caches"; `from_cache_dir` reads 3/5/6 only.
- `safety.rs:9` — header: oversize download "triggers callback"; code returns `Option<String>` warning, no callback.

Gate verdicts:

| Gate | Result |
|---|---|
| V1 precision (0 phantoms) | **FAIL** — 18/23 shipped were phantoms (22%) |
| V2 recall (5/5 known-reals) | **FAIL** — 1/5 strict (multi.rs resurfaced only as a mislabeled silent-growth) |
| V3 polarity conflicts | guard caught 6, but "unclear" classifications leak: 2 polarity-flip phantoms still shipped (checkpoint.rs, builder.rs — their own `code_shows` confirm the header) |

Recall-loss diagnosis (all four): section_cache-lookup died at **decomposition**
(claim simply not extracted this run); watch.rs-ownership died at
**classification** (capability→cross_cutting flip, gated out); multi.rs and
tensions.rs died at **adjudication leniency** (partially-true claims — wrong
exception list, stale enumeration — rounded to corroborated).

New phantom classes v2 introduced: **paraphrase entity-swap** (spans.rs —
decomposer rewrote `section_id` as `chunk_id`, manufacturing the mismatch),
stretched-generalization, and miscites. The modulators stub-context stripping
**persisted despite the explicit prompt rule** — the decomposer still splits
the intended-behavior bullets from the "**v1 stub**" paragraph three bullets
below. Long-range qualifier association is the persistent 35B failure.

**The load-bearing insight: run-to-run union and intersection.** v1+v2
together found 9–10 distinct real drifts; each single run found ~5 (≈50%
recall of the union). But the intersection does NOT purify: the recurring
phantoms (atoms.rs "in this step", field_engine's own exception clause, the
modulators stubs) recur across independent decompositions — they are
*correlated model misreads*, not sampling noise. Majority-vote-across-runs
therefore can't fix precision here; a **stronger judge** or a **human
precision layer** can.

Conclusion after two runs: the reconcile is a good RECALL device (real drift
exists, ~4% of headers across the union, all ten-second fixes) and a bad
autonomous PRECISION device at 35B. The two shippable shapes, in order:
(a) human-in-loop — 45 candidates per full-repo-crate run is minutes of
review; (b) test the 122B as the verify-stage judge only (~45 calls, the
agent-bench "judge MUST be 122B" precedent) — requires deliberately loading
the heavy model, so it's an operator decision, not a default.

---

## v1 run (original findings below)

**Verdict: the hypothesis survives falsification — real, act-on-able header
drift exists (6 findings below, each source-verified). The automated pipeline
does NOT yet meet the precision bar: it shipped 14, of which 8 were phantoms
killed only by the manual pass. G1 passes (pending Alex's ten-second test);
G2 fails for the pipeline as-is, with a clear, fixable phantom taxonomy.**

Inputs: 225 corpus-engine `//!` headers; evidence = body-hash-pinned function
summaries; models: Qwen3.6-35B-A3B (all three LLM stages). Artifacts in `out/`
(claims.json, verdicts.json, verify.json, verify.json.v1-buggy-framing,
report.md, run.log).

## Base rate (the number this spike existed to measure)

744 claims decomposed from 225 headers → 445 corroborated / 271 not_evidenced /
27 contradicted (3.6%) → 28 candidates → 14 shipped by the adversarial
verifier → **6 survived manual source verification (5 clear + 1 borderline)**.
≈ **2.7% of headers carry real drift** — low, but not zero, and every real one
is a genuine reader-misleads. The house does occasionally burn.

## The six real findings (each verified against source by hand)

| # | File | Header asserts | Source shows |
|---|---|---|---|
| 1 | `enrichment/atlas/section_cache.rs:6` | cache key = `sha256(section_text+prompt_version+model_id)` | `blake3::hash` at `:33` (header even says blake3 at `:21` — internally inconsistent) |
| 2 | `enrichment/atlas/section_cache.rs:12` | interface `cache.lookup(section_id, key)` | `lookup(atlas_dir: &Path, key: &str)` at `:45` |
| 3 | `enrichment/domains/multi.rs:3` | "Domain methods use todo!() except id/name" | `skeleton_storage()` implemented at `:76` (returns `LanceOnly`) |
| 4 | `update/watch.rs:20` | "`CodeWatcher` owns the notify watcher and the tokio task. Dropping…" | `WatcherHandle` owns the task + Drop-abort (`:57`); `CodeWatcher` holds only config (`:64`) |
| 5 | `enrichment/atlas/analysis/tensions.rs:4` | "three cheap deterministic signals" incl. embedding top-K | `select_candidates` (`:156`) composes FIVE signals; embedding top-K is a separate pub fn (`:245`) not among them |
| 6 (borderline) | `enrichment/pipeline/typed_schemas/source_recovery.rs:24` | renders `QuoteSpan`s into the block | `render_source_recovery_block(excerpts: &[&str])` (`:81`) — deliberately avoids `QuoteSpan` (documented at `:61`) |

Every one is a ten-second fix (update the header line) and every one would
mislead a reader who trusted the header — #1 anyone computing keys externally,
#4 anyone reasoning about drop semantics, #3 anyone extending MultiDomain.

## Gate verdicts

| Gate | Result |
|---|---|
| G1 value (≥5 act-on-able) | **PASS pending Alex's ten-second test** — 5 clear + 1 borderline |
| G2 precision (0 phantoms shipped) | **FAIL for the automated pipeline** — 8/14 machine-shipped findings were phantoms; 0 phantoms survive the manual layer. Machine precision 43%. |
| G3 claim-class discipline | Classes themselves were assigned well (rationale/history correctly gated); the failure is **context-stripping**, see below |

## Phantom taxonomy (all 8 diagnosed — this is the fix list)

1. **Decomposition context-stripping (5/8).** The decomposer ripped claims out
   of their qualifiers: the "**v1 stub**" context (modulators ×2), the
   "in this step" phase qualifier (atoms.rs), the header's own exception
   clause (field_engine: "zero domain logic *except from_recipe*" → adjudicated
   as if no exception), depth-framing read as its own contradiction (brief.rs).
   Fix: decomposition prompt must carry conditions/qualifiers INTO the claim
   statement (the `conditions[]` field existed but wasn't populated or used).
2. **Verifier polarity flips (3/8).** Verify reasons that *confirm* the header
   (checkpoint.rs, literary.rs, reconciliation) still returned `refuted=false`.
   Fix: structured/constrained output (llguidance) instead of free JSON, or a
   final deterministic cross-check that the reason and the boolean agree
   (an entailment-direction check).
3. **Evidence scoping (1/8, overlaps #2).** `mod.rs` headers describe the whole
   module; file-scoped evidence can't see sibling files (`Split` lives in
   `oplog.rs`). Fix: module-level claims get module-scoped evidence.

Also fixed during the run (kept for the record): the v1 verify phase framed the
bare header claim as "the finding," so the verifier judged the wrong
proposition and killed ALL 28 candidates including the real ones
(`verify.json.v1-buggy-framing`). The corrected framing + real source at cited
lines flipped section_cache/multi.rs/watch.rs to shipped.

## Caveats

- Header-age analysis is polluted: `git log -L1,N` attributes every header to
  a 2026-06-09 repo-wide commit (SPDX line). A real age signal needs blame on
  the `//!` lines only. Not done in this spike.
- One-shot single-model verdicts; no majority vote. Precision numbers are for
  ONE pass of one 35B.

## What this means for productionizing

The value hypothesis stands: real drift exists at ~2.7% of headers and 100% of
the verified findings are act-on-able. The blocker is machine precision (43%),
and the phantom taxonomy says most of it is prompt/harness, not model
capability: context-carrying decomposition (+ its `conditions[]` field),
polarity-safe constrained verify, module-scoped evidence. A reasonable v2
target before any production surface: re-run this same spike with those three
fixes and require G2 to pass end-to-end (0 phantoms shipped, no manual layer).
Human-in-the-loop remains a legitimate interim shape: at 28 candidates per
225 headers, the review burden is minutes, not hours.
