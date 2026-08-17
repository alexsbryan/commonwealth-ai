# Bank v1 — the report-class mint

**Minted 2026-08-14, order `deep-research-t1c` (serves `dr-local-loop`).**
One report-class question, 16 coverage keys, an 11-body source deck, the
vendored operator's report as ground truth. The mint mirrors bank v0's
discipline (bank/README.md): keys authored NWCI from the operator's exemplar,
numeric bars PROPOSED in the mint commit and operator-ratified at this
order's approval — no arm result exists when these numbers are written.

## Contents

- `source-report.pdf` — the operator's exemplar vendored verbatim (ground
  truth for the claims + the per-claim source spine).
- `seeds.md` — the question, the 16 keys (verbatim from the order), the NWCI
  record, the per-key pinning + arbiter journal.
- `harvest-audit.md` — every fetch of the bounded one-time harvest.
- `deck/` — the frozen source deck (deck.toml + 11 body files).

## The frozen deck

sha256 (sorted per-file hashes) =
`e63a14499d849301f3f0bbd00024c178609c5899b97d5b6ec0a6ee5b1e88c5ee`.
Recipe: `(cd deck && sha256sum deck.toml $(ls *.md | sort) | sha256sum)`.
Run, never edit.

## The numeric bars — PROPOSED at mint, operator-ratified at this order's approval

| bar | proposal | clause |
|---|---|---|
| P4 | **≥58 of 72** v0 keys AND **≥12 of 16** v1 keys cleared | restated absolute K/N per bank, scored per the bank README semantics below |
| P3 | **≥10 of 13** questions paired-pass | round-2 fetch count < 20% of round-1 with the coverage key not worse, journaled per question |
| P5 | **6/6** flights | the t1b poisoned-drill battery, no noise band |
| R-12 | **≥10 of 12** v0 seeds | gap-set strict shrinking; the v1 question's convergence journaled (reported, not gated) |
| Two-arm lift | pooled attribution density: loop ≥ one-shot + **0.10**; v1-question density: loop ≥ one-shot + **0.15**; honesty not worse | attribution density = the fraction of numeric claims in the output that trace to deck sources, measured by the deterministic checker over both outputs; the honesty leg = fabrication rate (numeric claims NOT supported by the window) |

## Scoring semantics (pre-registered shape, before any arm run)

- **"Cleared"** = the deterministic structured-match checker's verdict over
  the answer+evidence artifacts (the v0 README semantics): the answer names
  the key's subject with the key's figure (or the deck-supported corrected
  figure, arbiter-journaled), and the run's evidence window supports it.
  Partial = gap (all-of rule). C-class deterministic, never an LLM judge.
- **The K2 conflict clause** (v1-only, deterministic): a claim asserting the
  superlative ("NYC leads at 0.5469") is not covered while 0.57s sit in the
  same report unless the answer names the discrepancy or renders
  could-not-judge; conflicting figures across sources must render
  could-not-judge or a named discrepancy, never a synthesized pass.
- **Honesty is scored separately, never blended into coverage**
  (DEEP_RESEARCH.md P2): the loop's gate verdicts (passed / could-not-judge /
  failed / never-ran), the corroboration floor's caps, and the fabrication
  rate are REPORTED per run; coverage is the bank's scorer.
- **Verdicts:** every leg reports the four-verdict set (§18.2): passed /
  failed / could-not-judge / never-ran.

## The same-commit substitution (named, never silent — §18.3)

The dr-local-loop bar amendment names the v1 mint by its **deck sha256**
rather than the mint's commit hash, because the amendment lands in the SAME
commit as the mint (the order's §1.1 one-commit rule) — a commit hash cannot
be cited by its own commit. The deck sha256 IS the frozen-mint identity
(content-addressable; the v0 bar's "b28c72b7" was a commit hash, cited a
commit later). The substitution is declared in the pre-registration append.

## Bars context at mint

dr-compass stays the 12 v0 seeds; the v1 question's convergence is reported,
not gated (named in the amendment note). The two-arm control (loop vs
one-shot RAG on the same deck, same daemon, same model, same retrieval —
only the loop differs) is the product claim for T1: does the loop beat
asking once, on the class of question the product serves, by a
pre-registered margin.
