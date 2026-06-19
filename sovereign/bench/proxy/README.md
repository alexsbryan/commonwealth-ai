# Proxy Voting Corpus — Q&A bench (AC-4 / AC-5)

The chaos two-red-line path over an installed `proxy-cik…` corpus. Because
the sealed corpus is in the `proxy-cik` family, the live turns take the
`GateSurface::ProxyArgument` cite-or-abstain gate automatically.

```
# install the frozen fixture filing (reproducible — pinned accession):
scripts/setup-proxy-corpus.sh XOM --accession 0001193125-26-147614
sovereign enrich init  proxy-cik0000034088 --from-corpus proxy-cik0000034088
sovereign enrich build proxy-cik0000034088 --full

# run the bench + gate vs the committed baseline:
sovereign bench proxy qa proxy-cik0000034088 --out target/proxy-qa/results.jsonl
sovereign bench gate proxy-qa --report target/proxy-qa/results.jsonl
# recapture the baseline (only with a justified reason):
sovereign bench gate proxy-qa --report target/proxy-qa/results.jsonl --update-baseline
```

Fixture: Exxon Mobil 2026 DEF 14A, accession `0001193125-26-147614`, corpus
`proxy-cik0000034088`. Bank + gates: `exxon/{bank.toml,manifest.toml}`.
Baseline: `baselines/exxon_qa/`.

## The two red lines

- **RL-2 (both sides, cited)** — `present` rows over a *shareholder* proposal
  (Items 5, 6). A correct answer surfaces the **proponent's** side (the one at
  risk of being dropped — the board's side is always printed); the gold_keyword
  + supporting_quote witness it. **This is the load-bearing gate metric**
  (`competence`).
- **RL-1 (no confabulated opposition)** — `absent_adjacent` rows over a
  *management* item (Items 1–3): the "case against" an item the filing argues
  only FOR. The honest move is to decline; **manufacturing an against-case is
  the cardinal sin** the whole "legibility, never editorialize" contract exists
  to prevent.

## Why RL-1's `honesty`/`hallucination_rate` numbers read low — a known
## judge-fit artifact, NOT a model defect

The first baseline shows `competence 0.75` (RL-2, pass) but `honesty 0.33` /
`hallucination_rate 0.67` (RL-1). That RL-1 number is a **scoring-instrument
artifact**, not the model fabricating:

- The model does the right thing simply — it answers, e.g., *"the filing does
  not present a case against ratifying the auditors; the board recommends FOR."*
  That is a **correct, honest, informative decline** with **no fabricated
  opposition** (the true RL-1 invariant), and it is what FR-7/AC-4 explicitly
  allows: *"abstains **or** states only the board's recommendation is present."*
- The chaos `absent_adjacent` scorer is a binary **answer-vs-abstain** judge. It
  credits only *silent* abstention; an *informative* decline that adds the
  board's FOR context reads to it as "answered" → counted as a hallucination on
  an absent question. So a correct response is scored as a miss.

We deliberately do **not** chase this number by forcing the model into terse
silence — that would make the product worse to satisfy an arbitrary instrument.
The true no-fabrication invariant is enforced where it matters: the
`ProxyArgument` grounding gate rejects any opposition claim with no supporting
passage (verified live), and RL-2 remains a clean, meaningful gate.

A proper RL-1 instrument — a judge that flags only a *substantive fabricated
against-argument*, not an informative decline — is tracked future work. Until
then: **read `competence` (RL-2) as the live signal; treat `honesty` /
`hallucination_rate` here as anchored-for-regression-only, with this caveat.**
The gate still catches a real regression (an actual fabrication would push
`hallucination_rate` well past tolerance).
