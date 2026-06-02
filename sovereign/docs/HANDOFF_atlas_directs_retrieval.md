# HANDOFF — Atlas-directs-retrieval (Enron): synth-boundary → corpus-selection → incremental atoms-delta

_Updated 2026-06-02. Supersedes the prior synth-boundary handoff (that issue is RESOLVED)._

## Mission (verbatim intent)

> "Ultimately the enrichment system MUST be an atlas for retrieval — it guides what is retrieved, that is fundamental."

Test: enumeration-class prosecutor questions on `enron-sample-multi-wide`. Bench
`sovereign/bench/enron/qa_demo.toml` (5 questions). The hard one: **counterparty_network**
("which energy companies appear as counterparties or competitors").

## TL;DR — the thesis is demonstrated end-to-end

The atlas now **directs retrieval to gold entities that pure embedding retrieval never
surfaces.** counterparty went **1/5 → 2/5 → 3/5**, with the 3/5 naming **Calpine + Pacific
Gas + El Paso** — Calpine/Pacific Gas were unreachable by base retrieval and the pre-delta
atlas. The chain: fix the synth-boundary (atlas chunks reach synth) → discover the real cap
is **corpus coverage** (counterparties were never ingested) → ingest them incrementally
(`corpus expand`) → extend the atlas incrementally (`enrich delta`, no 24h rebuild) → atom-enum
directs retrieval to the now-present gold.

## The journey + where each lever landed

1. **Synth-boundary RESOLVED.** atom-enum directed chunks died before synth via
   `vector_distance=None` sort-demotion (not the handoff's old reweight-clobber guess):
   fetched chunks have no query embedding → `cross_corpus_sort_cmp` (`retrieval_helpers.rs:239`,
   `(Some,None)=>Less`) sinks them below every base hit → `truncate(KQ_MERGED_LIMIT=20)` drops
   them. **6 gated levers** (commit `d29f5b48`, all behind `SOVEREIGN_ATOM_ENUM`, default off):
   pin (`reserve_atom_enum_chunks`) + cap-exempt, RRF rank (degree⊕cosine), collective-noun
   filter, relation-evidence candidates, route-override (enumeration→PrimarySynthesis),
   classifier prompt. Survival 0→16.
2. **27B finding (honest):** at the production 27B, atom-enum is fact_recall-**neutral** on the
   narrow gold (the model already enumerates well; the metric can't score breadth). atom-enum's
   real value is *answer quality* (clean sourced enumerations) + reaching entities base
   retrieval misses — not a global fact_recall lift. It **trades**: wins the enumeration
   question, displaces base hits on lookup questions (hence per-question gating is correct).
3. **The real cap was corpus selection.** counterparty stuck at 1-2/5 because the trade
   counterparties (Calpine/El Paso/Williams) were **never ingested** — the corpus is Lay+Skilling
   mailboxes only. Confirmed: the substrate lives in trader mailboxes (`symes-k`:
   Calpine 724, El Paso 600, Williams 337 mentions).
4. **Phase A — incremental chunk ingest (atlas-safe).** Added four `symes-k` trade-desk
   folders (`ctpy_contacts`/`confirms`/`deal_communication`/`power_marketer`, ~1280 msgs) as
   staging symlinks; `corpus expand` (daemon route `POST /internal/corpus/expand`, port 9742)
   appended them via the `source_doc_id` skipset → **6049 → 8829 chunks**, atlas untouched.
   Base-retrieval (atom-enum off) counterparty **1/5 → 2/5** (El Paso surfaced), answer now
   grounded in real trade confirmations. NOTE: `corpus install` does NOT pick up new staging
   files (static enumeration before resume-cursor read) AND triggers a full re-enrich — use
   `expand`, never `install`, on this corpus.
5. **Phase B — incremental atoms-delta (no rebuild).** New `enrich delta` /
   `enrich delta-manifest` commands (commits `5a57e6e3`, `b26904da`): mint `sec_NNNNN` for the
   appended chunks → migrate the live atlas to content-hash (idempotent, backed up) → subset
   extract/cluster/name → resolve into a **staging** dir → content-hash the staging atoms →
   additive `apply_atom_delta` → partial meta-atlas rebuild. The live atlas is mutated once,
   additively, after backup. Extended the real atlas **3777 → 6101 (+2324 atoms, +282
   relations), edges dropped 0.**
6. **atom-enum on the extended atlas directs retrieval to the gold.** Counterparty
   directed-fetch now selects `Calpine, Pacific Gas and Electric, El Paso, Dynegy, Sempra,
   Duke, …` (survival 16). At `max_tokens 800` synth names Calpine+PacificGas+El Paso →
   **counterparty 3/5** (best ever; gold base retrieval never reached).

## Final numbers (qa_demo, 35B-IQ4)

| q | base off (expanded) | atom-enum 400 | atom-enum 800 |
|---|---|---|---|
| exec_cast | 4/6 | 4/6 | 4/6 |
| ljm_fraud | 4/5 | 5/5 | 3/5 |
| dynegy | 4/4 | 3/4 | 3/4 |
| financial | 3/4 | 3/4 | 3/4 |
| **counterparty** | 2/5 | 1/5 | **3/5** |

## Model: Qwen3.6-35B-A3B-UD-MTP-IQ4_NL — fast, but disable MTP

A3B MoE (3B active) at IQ4 → **~10× the 4B** for atlas extract (~15-25s/ch vs 155s/ch),
clean (0 parse-fails). **GOTCHA:** with MTP on, sustained sequential extract hits
`MTP verify decode failed: Decode Error 1: NoKvCacheSlot` (~chapter 109) and the build halts.
Run with `SOVEREIGN_MTP_DISABLE=1` in the daemon env (pidfile-managed → inherits start-cmd
env: `SOVEREIGN_MTP_DISABLE=1 sovereign daemon start`). Slot loads `mtp=false`, no slot errors,
speed holds (MoE is fast natively). This is a real MTP bug worth reporting separately.

## Current daemon/atlas state (IMPORTANT)

- `~/.sovereign/config.toml`: **primary = Qwen3.6-35B-A3B-UD-MTP-IQ4_NL** (was 27B — TEMP for
  the delta; decide keep vs revert), fast = 4B. Daemon running with `SOVEREIGN_MTP_DISABLE=1`.
- `~/.sovereign/enrichment/enron-sample-multi-wide/config.json`: `chat_model = "primary"` (was
  "fast" — set so the delta used the 35B-IQ4).
- Real atlas: **content-hash ids now** (migrated from sequential), **6101 atoms**. The original
  3785-atom sequential atlas is backed up at `~/.sovereign/enron-atlas-backups/atlas.pre-delta.bak`
  (+ in-atlas `.delta-backup-*` dirs, + the handoff's `atlas.pre-resolv.bak` 18.8k-atom old atlas).
- chunks: 8829 (backup `~/.sovereign/enron-chunks-backups/chunks.lance.pre-expand`).
- `cache/questions.json` was overwritten with the 193-chapter subset (orig 249 at
  `cache/questions.json.orig-249.bak`).

## Open / next

- **Synth breadth, not retrieval, now caps counterparty.** The gold is *directed + survives*;
  the model names ~6 of 16 at 400 tokens, more at 800 (but lost Dynegy — the directed set's
  ordering/displacement). Levers: higher max_tokens, an "enumerate exhaustively" synth
  directive, or pin Dynegy. Williams isn't in the directed top-16 (rank/ctpy_contacts coverage).
- **Broader coverage:** only the 196 gold-term chapters were enriched (of 977 symes-k chapters,
  312 counterparty-term). `/tmp/broad_chapters.txt` has the 312-set for a richer pass.
- **Revert or keep** the 35B-IQ4 primary + MTP-disable + enron `chat_model=primary` (temp).
- **`enrich build` halts on a single unparseable chapter** (oversized newsletter → ctx-cap
  truncation → invalid JSON). `--skip-build` is the salvage; a `--tolerate-extract-failures`
  on the delta would be cleaner.
- atom-enum un-gating still needs cross-corpus ON validation (it displaces base hits).

## Key commits (branch `tech-debt/pr1-sweep`)

| hash | what |
|---|---|
| `d29f5b48` | 6 gated atom-enum levers (pin/RRF/filter/relation/route/classifier) + glassbox |
| `5a57e6e3` | `enrich delta` + `delta-manifest` (incremental referential atoms-delta) + refactors |
| `b26904da` | `enrich delta --skip-build` salvage flag |

## Repro (the working incremental flow)

```sh
# 1. add trader-folder symlinks to ~/.sovereign/corpora-staging/enron-multi-wide/, then:
curl -XPOST localhost:9742/internal/corpus/expand -d '{"corpus_id":"enron-sample-multi-wide"}'  # chunks
SOVEREIGN_MTP_DISABLE=1 sovereign daemon start                                                   # reliable 35B-IQ4
./target/debug/sovereign-cli-llm enrich delta-manifest enron-sample-multi-wide                   # mint sec_NNNNN
./target/debug/sovereign-cli-llm enrich delta enron-sample-multi-wide --chapters <ids> --yes     # extract→resolve→merge
# if build halts on a bad chapter: cp runs/questions-subset-NNN.json cache/questions.json
./target/debug/sovereign-cli-llm enrich delta enron-sample-multi-wide --skip-build --yes         # salvage
```
