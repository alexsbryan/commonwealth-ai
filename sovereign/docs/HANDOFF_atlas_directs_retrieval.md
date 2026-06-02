# HANDOFF — Atlas-directs-retrieval (Enron) + atom-enum enumeration

_Written 2026-06-02 by the prior session. Goal: next dev hits the ground running, not from scratch._

## Mission (user's directive, verbatim intent)

> "Ultimately the enrichment system MUST be an atlas for retrieval — it guides what is retrieved, that is fundamental."

Concretely: for **enumeration-class questions** ("who were the executives", "which energy companies were counterparties") on a personal/email corpus, embedding-retrieval collapses onto the one dominant member of the set. The atlas (the corpus's own typed entity atoms) should **direct** retrieval to each entity's evidence chunk. Test corpus: `enron-sample-multi-wide` (6049 chunks). Bench: `sovereign/bench/enron/qa_demo.toml` (5 prosecutor questions).

## TL;DR status

- ✅ **The Enron atlas is now a retrieval atlas** (the foundational win). Fresh `referential_atlas` build on the 35B: **3785 typed atoms**, **100% preview→chunk resolution**, all demo entities present (incl. previously-sparse Calpine/El Paso/Williams).
- ✅ **6 fixes committed** (branch `tech-debt/pr1-sweep`): see Commits below.
- ❌ **OPEN**: the atom-enum **enumeration** (gated `SOVEREIGN_ATOM_ENUM=1`, off by default) now classifies + fetches the right evidence chunks, but they **do not survive into the synthesis `retrieved` set** (0 survival, 0 QA delta). This is the **injection→synth-snapshot boundary** — the one remaining piece. The user chose: **"one targeted diagnostic"** to pinpoint it (see Next step).

## Commits (branch `tech-debt/pr1-sweep`)

NOTE: branch is `tech-debt/pr1-sweep` (started as `atlas-directs-retrieval`; two unrelated tech-debt commits `a22d566b`/`57f322fb` are interleaved — not from this work). The atlas-work commits, oldest→newest:

| hash | what |
|---|---|
| `80b4fdae` | **build_corpus real-id contract** — `enrich_cmd/corpus_io.rs`: corpus-mode `build_corpus` now emits one `ChunkRecord` per REAL LanceDB chunk (real id), via shared `fetch_enrichment_chunks` helper. (Turned out tangential — modern pipelines key on section_id+preview, not ChunkRecord.id. Harmless/correct, not the lever.) |
| `d2a1e821` | **shape-aware atom-enum fetch** — `runtime/retrieval.rs`: numeric chunk_id → `fetch_chunk_by_id`; section-shaped id ("sec_0001") → FTS the `passage_preview`. |
| `af95dc51` | **MTP FastShort auto-skip** (proper fix, user-requested, validated live) — `sovereign-inference/embedded.rs` ~5057: skip FastShort when `is_recurrent_arch(arch) \|\| (model_id contains "mtp" && !SOVEREIGN_MTP_DISABLE)`. Was only checking recurrent-arch; the 4B-MTP (qwen35 arch, "mtp" in name) slipped through → `Decode Error -3` on every continuous-batched call. No env hatch needed now. |
| `7b92ca3b` | **promote-after-retry** — `enrich_cmd/build.rs` ~594: `build --chapters` only promoted subset→cache when first extract pass `first_code==0`; a single parse_drift triggered auto-retry → promote skipped → cluster died "questions cache is missing". Now promotes explicitly after retry resolves. |
| `84592ff4` | **graph-aware corpus selection + diagnostics** — `runtime/retrieval.rs`: `enumerate_typed_atom_chunks` used `provider.loaded_corpus_ids()` (loaded embedding-bag CONTEXTS) ∩ enabled_corpora. A freshly re-enriched atlas has stale embeddings cache → context skipped → `corpus_ids=[]` → no enumeration. Now uses `enabled_corpora` directly (`provider.graph(id)` serves the graph regardless of context-load). Added `atom_enum_empty`/`atom_enum_nofetch` audit events. |

All builds clean. Atom-enum is **gated off** (`SOVEREIGN_ATOM_ENUM` default off) — production unaffected.

## THE OPEN PROBLEM (what the next dev should fix first)

**Symptom:** `SOVEREIGN_ATOM_ENUM=1` → `enumerate_typed_atom_chunks` classifies enumerate correctly (exec_cast→person, counterparty→institution), fetches real evidence chunks (the `atom_enum directed-fetch` audit event fires), returns `Some(chunks)`. But the chunks **never appear in the result's `retrieved` set** (survival-by-`metadata.source=="atom-enum"` = 0 on all questions), and QA scores are identical ON vs OFF.

**Where it's injected:** `prepare_knowledge_query_plan` (`handlers/knowledge_query.rs:268`). My injection block is **post-noise-floor** (after `drop_no_overlap_chunks`), logging `"KnowledgeQuery: atom-enum virtual chunks injected"` at **knowledge_query.rs:567** (+ a mirror in DeepQuery at `retrieval.rs:2796`). It does `chunks.extend(atom_chunks)`.

**Downstream of injection (knowledge_query.rs):** `reweight_by_query_relevance` (~613) → `cap_chunks_per_article` (~641) → title-expand `reserve_chunks_per_entity` (~660) → `chunks.truncate(KQ_MERGED_LIMIT)` (~670) → `post_merge` audit (~703).

**Two hypotheses (the targeted diagnostic distinguishes them):**
1. **Injected-but-dropped**: `reweight_by_query_relevance` overwrites the atom-enum chunks' score by query-token overlap; the entity-evidence chunk has low overlap with the *question* → sinks below `KQ_MERGED_LIMIT` at truncate. (This is what killed the earlier virtual-chunk version — score clobbered by reweight.)
2. **Snapshot-separate**: the synth/`retrieved` list is captured at a point my post-floor injection doesn't flow into.

**THE DIAGNOSTIC (user picked this) — one run:**
```sh
RUST_LOG=warn,retrieval_audit=info,sovereign_core=info \
SOVEREIGN_ATOM_ENUM=1 SOVEREIGN_ATOM_ENUM_TOPK=16 SOVEREIGN_TITLE_EXPAND=1 SOVEREIGN_DECOMP_DECAY=0.6 \
./target/debug/sovereign-cli-llm eval run --bank sovereign/bench/enron/qa_demo.toml --synth --isolate \
  --chat-model fast --max-tokens 100 --output /tmp/diag.json 2> /tmp/diag.log
```
Then grep `/tmp/diag.log`:
- `"atom-enum virtual chunks injected" count=N` — confirms the caller injected N (this log is `sovereign_core` info, which the prior run's `RUST_LOG=warn` SUPPRESSED — that's why "injected" looked like 0; it almost certainly fired).
- `retrieval_audit: post_merge ... by_corpus=...` — if atom-enum chunks are in post_merge but not in `retrieved` → snapshot issue (hypothesis 2). If injected N>0 but NOT in post_merge → dropped at reweight/truncate (hypothesis 1).
- atom-enum chunks have `corpus_id = enron-sample-multi-wide` (real source corpus, from fix 84592ff4) + `metadata.source="atom-enum"`.

**LIKELY FIX (try first):** mirror the **title-expand reservation** at `knowledge_query.rs:660-668` — add a `reserve_chunks_per_entity`-style block that PINS the atom-enum chunks before `truncate`, immune to reweight/sort. (Title-expand had the same "upstream made an intentional source selection the cross-corpus sort shouldn't demote" problem and solves it with a reservation.) Alternatively exempt `metadata.source=="atom-enum"` chunks from `reweight_by_query_relevance`'s clobber. DeepQuery path needs the analogous reservation (`retrieval.rs` ~2760 reweight).

## Key file:line map

| thing | location |
|---|---|
| `enumerate_typed_atom_chunks` (the whole atom-enum: classify gate + degree-rank + shape-aware fetch) | `sovereign-core/src/runtime/retrieval.rs:572` |
| KQ injection point (post-floor) + "injected" log | `handlers/knowledge_query.rs:559-575` (log at 567) |
| DeepQuery injection point | `retrieval.rs` ~2780-2800 (log at 2796) |
| `prepare_knowledge_query_plan` (synth chunk pipeline) | `handlers/knowledge_query.rs:268` |
| reweight / cap / reserve / truncate | `knowledge_query.rs` ~613 / 641 / 660 / 670 |
| `fetch_chunk_by_id` (numeric → LanceDB row) | `retrieval.rs:1232` |
| `search_corpus_indexes_with_overrides` (FTS w/ empty embedding) | `retrieval.rs:1534` |
| MTP FastShort skip | `sovereign-inference/src/embedded.rs` ~5057 (search `fast_short_unsafe`) |
| MTP detection at slot load | `embedded.rs:735` (`mtp_by_name`/`mtp_by_arch`) |
| `build_corpus` (corpus-mode ChunkRecord) | `enrich_cmd/corpus_io.rs:249` |
| `promote_subset_to_cache` + `find_latest_run` | `enrich_cmd/build.rs:635` |

## Env knobs (atom-enum, all gated)

- `SOVEREIGN_ATOM_ENUM=1` — enable atom-enum enumeration (off by default).
- `SOVEREIGN_ATOM_ENUM_TOPK` — top-K atoms by degree (default 16).
- `SOVEREIGN_ATOM_ENUM_SCORE` — seed score for enum chunks (default 0.04; reweight clobbers it — part of the open problem).
- Companions: `SOVEREIGN_TITLE_EXPAND=1`, `SOVEREIGN_DECOMP_DECAY=0.6` (the committed query-expansion win, separate).

## Daemon / model state (IMPORTANT gotchas)

- `~/.sovereign/config.toml`: **primary=27B** (Qwopus3.6-27B-MTP, restored to normal), **fast=4B** (Qwopus3.5-4B-MTP). Watcher lint/test still disabled in workspace `.sovereign/sovereign.toml` (restore via `.with-watchers`).
- Daemon binary was rebuilt with the MTP fix (`af95dc51`) — `sovereign daemon restart` loads it; **no `SOVEREIGN_FAST_SHORT_DISABLE` env needed** (auto-skip works; verified live: log "skipped — fast slot model is MTP or recurrent" with no env).
- **4B-MTP is generation-bound, NOT faster than the 35B here** (~56s/ch vs 35B's ~28s/ch) and has ~11% parse-fail on atlas extract. **Use the 35B (`FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L`, the original Enron-atlas model) for enrichment** — cleaner + faster. To enrich on the 35B, swap `config.toml primary` → the 35B path + restart (the atlas extract phase pins to `primary`, ignores `--chat-model`). 35B is also the pinned JUDGE model (`sovereign-eval/src/judge.rs:10`).
- 64GB box: can't hold 35B + 4B + 27B resident. The fix-validated setup is primary=27B (on-demand), fast=4B (pinned ~5GB).

## Enrich-tooling gotchas (cost me hours — avoid re-discovering)

- `enrich build --chapters <subset>` is for ITERATING on extraction, **not building a bounded atlas** cleanly: it RE-EXTRACTS each run (no clean checkpoint resume), and subset extract doesn't reliably promote to cache (bugs `7b92ca3b` + `find_latest_run` sorts by NAME so `questions-terse-retry-*` wins over `questions-subset-*`).
- **How the resolvable atlas was actually built** (repro): extract 249 demo-relevant chapters on the 35B, then **manually** `cp runs/questions-subset-001.json cache/questions.json` and run **downstream only**: `enrich build enron-sample-multi-wide --chapters <ids> --skip seed --skip extract` → cluster→name→resolve→tensions→gaps → atoms.json. The 249-chapter set is the chapters whose chunks mention the demo entities (script logic in the session; rare terms full, common terms capped) — list was in `/tmp/demo_chapters.txt`.
- Too-short sections (<40 words) skip → counted as "failure" → `extract` exits 1 → build halts. Exclude them from `--chapters`.
- The atlas pipeline is `referential_atlas` (NOT the corpus's recorded `structural_atlas`, which is gone from the registry). `referential_atlas` keys `first_appearance` on `section_id` + `passage_preview` (resolves via FTS), numeric path unused.

## Artifacts on disk

- **New resolvable atlas**: `~/.sovereign/indexes/enron-sample-multi-wide/atlas/atoms.json` (2.2MB, 3785 atoms).
- **OLD atlas backup** (revertible — 18,833 flat Entity atoms, the reconciliation-demo source): `~/.sovereign/indexes/enron-sample-multi-wide/atlas.pre-resolv.bak/`. **NOTE the tradeoff**: the new bounded atlas reconciles to only 11 merges (1102→1091 entities) vs the old's 935 merges (the `CAPABILITY_BRIEF`/runbook reconciliation demo). The new atlas is RESOLVABLE; the old had the big reconciliation spectacle. For BOTH, a full-corpus 35B re-enrich (~24h) would be needed.
- Demo artifacts: `sovereign/bench/enron/{qa_demo.toml, DEMO_RUNBOOK.md, CAPABILITY_BRIEF.md, qa.toml}`.
- Result JSONs from validation runs: `/tmp/enron_fixed_on.json` (atom-enum firing), `/tmp/enron_newatlas_on.json` (atom-enum bugged-off = baseline).

## Verify the atlas is resolvable (sanity check, no daemon)

```python
import json, lance, re
a=json.load(open('/Users/alexsbryan/.sovereign/indexes/enron-sample-multi-wide/atlas/atoms.json'))
items=a if isinstance(a,list) else a['atoms']
ds=lance.dataset('/Users/alexsbryan/.sovereign/indexes/enron-sample-multi-wide/chunks.lance')
CORP=[re.sub(r'\s+',' ',(c or '').lower()) for c in ds.to_table(columns=['content']).to_pydict()['content']]
def res(pv):
    t=[x for x in re.findall(r'[a-z0-9]+',pv.lower()) if len(x)>3]
    return bool(t) and max(sum(x in c for x in t)/len(t) for c in CORP)>=0.7
pvs=[(x.get('data',x).get('first_appearance') or {}).get('passage_preview') for x in items if x.get('atom_type')=='Entity']
pvs=[p for p in pvs if p][:40]
print(sum(res(p) for p in pvs), '/', len(pvs))   # was 40/40
```

## Scope / restraint notes

- Atom-enum is GATED OFF — committing the in-progress enumeration is safe; un-gating needs (a) the synth-boundary fix above, (b) cross-corpus re-validation (it was reverted on wikipedia before; title-expand history).
- The atom-enum **degree-ranking** premise was disproven for predicate questions (counterparty's top-degree institutions = the custodian's ego-network: Howard U, Rice, AEI — not trading counterparties). The resolvable atlas + relevance (preview/FTS) is the better signal — but only matters once chunks land in synth.
- Reconcile decision pending: keep the resolvable atlas (retrieval) vs restore the backup (reconciliation demo). They're mutually exclusive without a full re-enrich.

## Suggested first 30 minutes for the next dev

1. `git log --oneline -8` on `tech-debt/pr1-sweep`; read commits `80b4fdae`→`84592ff4`.
2. Run the DIAGNOSTIC above; grep `injected count=` + `post_merge`. Classify hypothesis 1 vs 2.
3. If hypothesis 1 (dropped at truncate): add the atom-enum reservation at `knowledge_query.rs:660` (mirror title-expand), rebuild `sovereign-cli-llm`, re-run the bench, check survival>0 + QA Δ.
4. A/B: `/tmp/enron_fixed_on.json` (ON) vs an OFF run; the win to look for is **counterparty** (now has Calpine/El Paso/Williams as resolvable atoms) and **exec_cast** (Lay/Skilling/Fastow).
