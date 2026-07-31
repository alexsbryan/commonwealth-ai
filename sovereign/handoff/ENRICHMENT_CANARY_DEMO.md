# Enrichment Canary Demo — quality regressions can no longer ship silently

The user-facing win (T1 plan §Demonstrating value, step 1): before this
week, our enrichment quality score was a number frozen in April — the
corpus it "gated" could not even rebuild, and nothing was red. Now a
broken extractor turns the build red in about three minutes.

> The demo is the run itself: break the entity resolver on purpose,
> watch every named character in *The Brothers Karamazov* vanish from
> the knowledge graph, and watch the lane exit non-zero. Then undo the
> break and watch it stay green. That is the whole product claim —
> a regression the user would have experienced as silently worse
> answers is now a red build.

---

## The two-run protocol

```
scripts/enrichment-canary.sh --control    # run FIRST: proves green is attributable
scripts/enrichment-canary.sh              # perturbed: proves the lane CAN fail
```

Both runs force the resolver to actually re-run (the enrichment
pipeline caches every step — see "what the canary found" below), build
into a scratch target dir so the deployed binary is untouched, and
restore the atlas afterwards whatever happens.

| Run | Perturbation | Person atoms matched (golden: 10) | Lane verdict |
|---|---|---|---|
| Control | none (forced re-resolve) | 7/10 — same as committed baseline | green, exit 0 |
| Perturbed | `ENTITY_MERGE_LEVENSHTEIN 2→100` + `ENTITY_MERGE_COSINE 0.85→0.35` | **0/10** — Fyodor Pavlovitch, Dmitri, Ivan, Alyosha, Zossima, all ten gone | **regressed, exit 1** |

The perturbation opens the resolver's rule-2 merge gate so wide that
distinct entities with description embeddings collapse into each other
— exactly the failure mode a bad prompt, model swap, or resolver edit
would produce. Full reports: `target/canary/canary-report-{control,perturbed}.json`.

Cost: ~3 minutes warm (scratch debug build + 27s corpus rebuild; the
expensive extract/cluster/name LLM steps stay cached, only
resolve + configure re-run).

One calibration detail worth repeating to anyone re-deriving the
canary: perturbing the cosine constant alone is a **no-op** on this
corpus — rule 2's cosine sits behind a Levenshtein ≤ 2 syntactic
pre-gate that empties its candidate set (byte-identical atlas,
measured 2026-07-31). The canary perturbs both constants and the
script header documents why.

## What the canary found before it ever passed

The acceptance test paid for itself: three real ways the old lane was
fake-green, all fixed in this push.

1. **The weekly rebuild tier could never gate.** `bench all --rebuild`
   scored from discovery-time corpus state, so a rebuild that repaired
   a missing atlas still scored `Stale`. Fixed: post-rebuild
   `inspect_corpus_state` re-probe in `bench_cmd/all.rs::run_one`.
2. **Resolver changes were invisible.** The enrich-build pipeline
   caches the resolve step entirely (`atoms.json` exists → skip), so
   a perturbed — or genuinely broken — resolver never executed. The
   canary now deletes `atlas/atoms.json` to force it.
3. **The gated corpus had been unable to rebuild since ~April.** Its
   enrichment config pinned a model deleted months ago
   (`Qwen3.5-9B.Q8_0.1`, 503 on phase 8) and nothing was red — the
   lane only ever re-read a static `atoms.json`. Repointed to the
   primary slot alias (backup at `config.json.pre-canary-bak`).

## Why this matters downstream

P0.1 is the go/no-go for the rest of the T1 trust tranche: the
faithfulness lane (P0.3), extractive-only summaries (P1.1), and the
chaos double-gate (P1.4) all report through this same bench lane. A
lane that cannot fail would have made every one of those numbers
decorative. It reds on demand now; the tranche builds on that.

Reproduce: `scripts/enrichment-canary.sh --help` (header documents
mechanics, cost, and the restore guarantees). Weekly cadence:
`scripts/sovereign-ci-bench.sh --rebuild` on a workstation with a live
daemon.
