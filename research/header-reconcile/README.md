# Header-reconcile spike — the fractal derived-vs-asserted hypothesis

*Protocol drafted 2026-07-09, gates pre-registered before any adjudication runs.
Sibling of `research/orientation-bench/` (which banked the input pairs).
Results land in FINDINGS.md.*

## The falsifiable hypothesis

Rust `//!` file headers are micro-specs: intent written at authorship time,
read by default on every file open (by humans and, dominantly here, by LLM
agents assembling context), trusted because they're attached to the code — and
validated by nothing. The hypothesis: **reconciling each header's claims
against pinned per-function evidence surfaces real, act-on-able drift and
silent growth** — the spec↔code loop at file granularity.

The null: this repo's headers don't actually rot (they're small, close to the
code, edited in the same commits), so the reconcile is a smoke detector in a
house that doesn't burn — preventive-only value, no findings worth acting on.
The 6 most claim-rich pairs eyeballed on 2026-07-09 were all corroborated,
so the null is live.

## Scope and input

All corpus-engine files with a non-empty `//!` header (~245; enumerated from
the orientation-bench file-node set). Per file:

- **Asserted:** the `//!` header block (same extraction as orientation-bench).
- **Evidence:** the body-hash-pinned per-function summaries from the code-intel
  cache (`name`, `line`, `summary`) — NOT the derived rollup summary. A verdict
  citing LLM-derived node text against LLM-decomposed header text would be
  noise judging noise; every verdict must cite child symbols by name+line.

## Pipeline (one file = one unit, checkpointed)

1. **Decompose** (LLM): header → claims, each
   `{statement, claim_class, conditions[]}` with
   `claim_class ∈ {capability, rationale, history, cross_cutting, reference}`.
   Only `capability` claims (what the file does / contains / guarantees
   observable in its functions) proceed. Rationale ("we chose X because"),
   history, cross-cutting constraints ("never hold the lock across await"),
   and doc references are recorded but NOT adjudicated — forcing them to
   verdicts is where phantoms come from.
2. **Adjudicate** (LLM, one call per file, all capability claims at once):
   claims + child evidence → per-claim verdict
   `corroborated | contradicted | not_evidenced`, each citing supporting or
   contradicting child symbols (`name`, `line`). `not_evidenced` is a legal
   outcome and NEVER ships as drift — absence of evidence in one-line
   summaries is not contradiction.
   Same call also proposes at most ONE **silent-growth** candidate under a
   conservative cluster rule: a capability implemented by ≥3 child functions
   that the header does not mention at all.
3. **Adversarial verify** (LLM, only on `contradicted` + silent-growth
   candidates — expected few): a second pass prompted to REFUTE the finding
   given the same evidence. Findings that don't survive are dropped and
   logged. This is the capability-reconcile drift-judge discipline; drift
   ships precision-biased or not at all.
4. **Report:** `out/report.md` — every shipped finding with header excerpt,
   claim, cited child evidence `file:line`, and the refutation-pass verdict;
   plus the full base-rate table (claims by class, verdicts by kind).
   Glassbox: `out/claims.json`, `out/verdicts.json` hold everything,
   including what was gated out and what the verifier killed.

## Pre-registered gates

| # | Gate | Threshold | On failure |
|---|---|---|---|
| G1 | Value / base rate | ≥5 shipped findings (drift or silent-growth) that pass the codeowner's ten-second "I'd act on this" test | Park the reconcile; keep only the deterministic no-header coverage report |
| G2 | Precision | 0 phantom drifts among shipped drift findings (every one eyeballed against actual source) | Phantom source diagnosed (decomposition vs adjudication vs verify) before any production build |
| G3 | Claim-class discipline | Spot-check 10 decompositions: no rationale/history claim leaked into adjudication | Fix the decomposition prompt, re-run decompose (cached adjudications invalidate) |

Base-rate numbers (claims/file, % corroborated / contradicted / not_evidenced,
silent-growth rate) get reported regardless of gate outcomes — they are the
measurement this spike exists to take.

Pre-registered interpretation notes:
- Fresh headers (recently written under review discipline) are expected to
  corroborate; the interesting population is old/unloved files. Report
  findings-vs-header-age (git blame on the header lines) so a "no findings"
  result can distinguish "headers don't rot" from "this repo's headers are
  simply young."
- Silent-growth findings are worth-a-look grade, not drift grade — a header
  is not obligated to be complete. They count toward G1 only if the
  ten-second test says the omission genuinely misleads a reader.

## v2 protocol addendum (pre-registered 2026-07-09, before the v2 run)

v1 result: hypothesis survived (6 real source-verified drifts) but G2 failed —
8/14 machine-shipped findings were phantoms (43% precision). v2 applies the
three fixes from the v1 phantom taxonomy and re-runs end-to-end into `out-v2/`:

1. **Context-carrying decomposition** — claim statements must carry every
   qualifier the header attaches (stub status, phase scope, exception clauses);
   a rule and its exception are ONE claim.
2. **Polarity-safe verify** — no "refuted" double negation. The verifier answers
   `header_accurate: true|false` after separately stating what the code shows
   and what the header says; a second cheap classifier reads the reason text
   alone and must AGREE with the boolean (disagreement → one retry → kill +
   logged as `polarity_conflict`).
3. **Module-scoped evidence** — `mod.rs`/`lib.rs` headers are adjudicated
   against evidence from the whole module directory, not the single file.

### v2 gates (machine output judged as-is; manual eyeball is measurement only)

| # | Gate | Threshold | On failure |
|---|---|---|---|
| V1 | Precision | 0 phantoms among machine-shipped findings | Diagnose stage; do NOT productionize; consider human-in-loop shape only |
| V2 | Recall (regression set) | ≥5/5 v1 known-reals ship again: section_cache sha256, section_cache lookup, multi.rs todo-except, watch.rs ownership, tensions.rs signal count. (v1 #6 QuoteSpan borderline may go either way.) | A fix over-suppressed; diagnose which stage dropped it |
| V3 | Report `polarity_conflict` count | informational — each is a phantom prevented by the guard | — |

## Cost model

~245 decompose + ~245 adjudicate + (few) verify calls on `primary`
(Qwen3.6-35B-A3B-MTP) ≈ 500-550 calls ≈ 60-90 min single local box,
checkpointed and resumable. Steady-state (if productionized) rides the same
(header_hash, children_hash) incrementality as the rollup: a handful of
re-adjudications per commit.
