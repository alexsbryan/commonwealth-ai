# Defaults Ledger — capabilities shipped dark, and what flips them

**The failure mode this file exists to stop:** an initiative proves a
capability works ("zero delta on both banks!"), ships it behind a flag
or non-default mode "until X", and then X never happens — the work
withers, provably good code sits dark forever, and six months later
nobody remembers the flip condition or whether it was ever met.
(Poster child: the cluster-score blend shipped 2026-05-22 at
`cluster_weight=0.0` "pending bench plan" and sat dark for ten weeks
before this ledger existed.)

**The contract:**

1. Any push that ships a capability default-off or dark **adds a row
   in the same commit** — with a falsifiable flip condition, which
   plan item or run settles it, and a review-by date.
2. When the condition is met (or refuted), the row **moves** to
   Graduated or Rejected — it never silently disappears.
3. A row past its review-by date is not noise: it is the signal. Any
   session touching that area raises it to the operator — flip it,
   kill it, or re-date it with a reason. "Still waiting" without a
   named blocker is not a valid state.

Cross-references: env flag defaults live in `quality/env-flags.toml`
(this ledger records *why* a default is what it is and what changes
it, not the mechanics); decisions with full context live in the notes
store (ids cited per row).

---

## DARK — proven or plausible, awaiting a named condition


### Corpus relevance prefilter — `SOVEREIGN_CORPUS_PREFILTER_TOPK` (unset)
- **Shipped:** dark, pre-2026-08; row added 2026-08-05 on first real
  measurement.
- **What it does:** on an UNSCOPED turn, prunes the eligible corpus set
  to the top-K by query↔centroid cosine before the fan-out.
- **Proof so far:** measured at `K=5` on
  `bench sep/summarize --prod-pipeline` (14 questions, 420 chunks,
  deterministic): off-topic evidence 11.0% → **10.2%**, no change to
  fact recall (0.7500 / 0.7833). The centroid ranking is genuinely
  discriminating — sep 0.59 and wikipedia 0.59 against
  conversations-anthropic 0.39 — so the mechanism works.
- **Why it under-delivers, and this is the actionable part:** the trace
  shows `kept=9` at `top_k=5` — five corpora earned a slot on relevance
  and **four more were admitted by the "always keep `personal_scope`
  regardless of score" carve-out**. Those four are the entire residual
  (31 of 43 off-topic chunks). The prefilter cannot fix what it is
  required to exempt.
- **Flip condition:** a run on a bank that contains BOTH reference-corpus
  and personal-corpus questions shows top-K pruning holding personal
  recall flat while cutting off-topic share. Flipping it on today's
  evidence would be tuning against a bank that can only see one side.
- **Settled by:** the personal-corpus bench bank (see
  `docs/RETRIEVAL_AUDIT_2026-08-04.md` §D1-residual) — unowned. If no
  tranche claims that bank by the review date, kill this row rather than
  re-dating it a third time.
- **Review by:** 2026-09-05.
- **Notes:** `8758759a`, `c9aa59c6`.
### Multi-quote citation contract (`SOVEREIGN_CITATION_MULTIQUOTE`) → **GRADUATED 2026-08-05, default ON**
- **Moved to GRADUATED the same day it shipped dark.** The row stays
  here rather than in the Graduated section only because its whole
  argument is the DARK row below it; read the two together. Flip
  landed in `grounding/config.rs::citation_multiquote_enabled`.
- **What settled it** (matched control — same HEAD, same day, same
  local topology, `saltgrass_compound` n=7, **0 extraction failures in
  both arms**, `=1` vs `=0`):

  | metric | `=0` (legacy) | `=1` (shipped) |
  |---|---|---|
  | citation releases | 0 | **3** |
  | competence-when-present | 0.14 (1/7 correct) | **0.43 (3/7)** |
  | misses attributed to gate | 4 | **2** |
  | blatant-confab-rate | 0.00 | **0.00** |

  Both halves of the flip condition are met: releases > 0, and confab
  did not regress. **The known risk did not materialise** — it did not
  trade a full correct legacy answer for a partial one. It RECOVERED
  two turns the legacy ladder was abstaining away
  (`compound-sentence-then-inn`, `compound-constable-then-finder`:
  Abstained → `citation_grounded`), which is the "kills 3-4 correct
  drafts per run" cost the dark row predicted, now measured at 4 → 2.
- **Caveat carried forward, not hidden:** n=7 on one bank. The
  competence delta is 1/7 → 3/7 — direction is unambiguous and the
  mechanism is understood, but this is not a CI-separated result and
  the n≈20 compound bank should re-confirm it.
- **Shipped:** 2026-08-05, dark.
- **Proof so far:** the defect is measured and deterministic, the cure
  is not yet. Quote-first citation grounding releases on **0 of 14**
  compound probes (chaos-monkey `saltgrass_compound`, n=7 × 2
  independent runs, 2026-08-04): every probe ends `ANSWER: NONE`
  because the prompt demands the ONE sentence answering the whole
  question, and a two-part question has none. In
  `compound-inn-and-innkeeper` the model copied the correct sentence
  for part one and still answered NONE. Consequence: `cites_a_source`
  is 0/7 structurally, and all 14 fall through to the legacy ladder,
  which then kills 3–4 correct drafts per run.
- **Flip condition:** an arm-C chaos + situated run vs the arm-A
  baseline shows citation releases > 0 on the compound bank AND no
  regression in blatant-confab-rate (currently 0.00) — i.e. the
  partial releases are grounded, not padded. Overlapping CIs on the
  situated dimensions do NOT settle it either way at n=7; the bank
  grows first (see below).
- **Settled by:** P4 arm C (this initiative's A/B), then the n≈20
  compound bank.
- **Known risk this must clear:** unlike `SOVEREIGN_CITATION_BROAD`
  this is *not* purely additive — it converts a legacy-ladder turn
  into a partial citation release, so it could in principle replace a
  full correct legacy answer with a grounded-half-plus-named-gap. The
  arm measures exactly that trade.
- **Review by:** 2026-09-05.


### Next-edit consult gates `fanout_insert` + `param_insert` (detected, declined)
- **Shipped:** 2026-08-06, dark — `next_edit_model::should_consult`
  returns `Consult::No { skipped: "fanout_insert_deferred" }` /
  `"param_insert_deferred"`. Detection is unchanged, so both stay
  visible in the admission table; only the consult is withheld. Joins
  `casing_deferred`, deferred the same way in v1.
- **Why:** scored **per admitting gate** rather than per bank shape on
  the golden set (`gym/next-edit/golden/`, 1,098 cases, note
  `2c22ec10`), the three consult reasons are three different bets:
  `multiline_fanout` 17 useful / 1 wrong (94.4%), `fanout_insert` 2/17
  (10.5%), `param_insert` 2/6 (25.0%). `fanout_insert` was also the
  path by which 7 `neg_literal_trap` wrong fires reached the model.
- **Cost of off:** 4 useful edits, measured — paired, deterministic
  pipeline, same 1,098 cases. Bought 23 fewer wrong fires; all 27
  changed cases moved one way, none regressed. System goes 36.0%
  useful / 21.0% wrong-fire → 35.4% / 15.2%, which is a LOWER
  wrong-fire than disabling the model lane entirely (33.1% / 15.8%).
  Model-lane p95 1748ms → 9ms.
- **Flip condition:** a candidate model scores ≥60% useful on the
  `fanout_insert`-admitted slice (n=41) and `param_insert`-admitted
  slice (n=19) of the golden set, with ≤1 wrong fire on the negatives
  each gate admits. Re-measure with `--force-consult` + `compare_runs.py`;
  the admission counts those gates still log are the denominator.
- **Settled by:** the next bakeoff arm scored on the golden set —
  zeta-2 and instinct have never been run against it. Until one is,
  this is a property of sweep-1.5b only.
- **Review by:** 2026-09-06.

### EvidenceCheck frame + evidence-shape early-decline
- **Shipped:** 2026-07-21, dark.
- **Proof so far:** top_cosine established as TOPIC signal, not
  answer-containment (~0.75 in-topic-but-thin) — the floor needs
  calibration before the frame can gate anything.
- **Flip condition:** floor calibration soak separates
  "in-topic-thin" from "answerable" without raising false declines.
- **Settled by:** unowned — no current T1 item covers it. If no
  tranche claims it by review date, kill or re-scope.
- **Review by:** 2026-08-14.

### Cross-encoder reranker slot
- **Shipped:** dark (note `10a1b08d`). **Wired into the daemon-server
  and desktop Runtimes 2026-08-03** (T1 A2) — until then the `svrn
  chat` CLI was the only surface that installed one, so both shipping
  surfaces ran baseline fusion and `SOVEREIGN_PPR_EXPAND` logged "lane
  dark" for want of the same `rerank_fn`. Still opt-in via
  `SOVEREIGN_RERANK_MODEL_PATH`; the row stays DARK until the A/B.
- **Cost of on:** the ~500MB / ~1.7s-per-query figures below are
  SUPERSEDED and were measured on the broken jina GGUF. SP4
  (2026-07-31, note `d43fb03b`) adopted the official
  `Qwen3-Reranker-0.6B-Q8_0` GGUF: 639MB, **22.7ms/pair batched**
  (~470ms for top-20), 2.57ms/pair on short titles. The
  `jina-reranker-v3-Q8_0.gguf` finding that read as "rerankers are
  unusable" was a conversion defect in that one artifact — it dropped
  the scoring head — not a property of the capability.
- **Superseded cost figures:** ~500MB resident, ~1.7s/query at k=50,
  OICP wire work for peer routing.
- **Flip condition:** residual contribution (+1 SEP source, +5 wiki
  sources, +12 wiki facts) survives after cap-N chunks-per-article +
  vector-distance dedup are measured *combined* — the cheap fixes
  must fail to close the gap before the expensive slot earns it.
- **SETTLED 2026-08-04 — the flip condition PASSED on quality and the
  slot was REJECTED ANYWAY, on latency.** See the REJECTED section
  below; this row is kept here only so the flip condition and its
  answer sit together. Notes `6a957b47`, `f4150097`.
- **Review by:** closed.

### Hardened `sovereign-server` — `dev-routes` + `net-tools` (both default **ON**)
- **Shipped:** `dev-routes` 2026-08-02, `net-tools` 2026-08-03. Both
  default ON. The *hardened* build is the opt-in one:
  `cargo build -p sovereign-server --no-default-features`.
- **What is dark:** not a capability — a *posture*. Default-on keeps
  every existing build (desktop, mobile host, dev workstation)
  byte-identical, so the row records why the safe configuration is the
  one you have to ask for.
- **`dev-routes` gates PRIVILEGE:** `POST /v1/solve` + `/v1/cycle/bdd`
  (client-supplied `test_command` reaches `sh -c` inside the
  *authenticated* router — any tenant key is a shell);
  `POST /v1/documents/upload` + `/v1/corpora/upload` (ingest an
  absolute server-side path — any tenant can read any file the process
  can, including the config holding every other tenant's key);
  `/mcp`, `/mcp/message`, `/mcp/stats` (outside the auth layer, gated
  only by `ip.is_loopback()`, which a same-host reverse proxy
  satisfies for every remote caller); `ShellTool`.
- **`net-tools` gates EGRESS**, and it exists because an audit found
  three agent tools reaching the open internet on **ordinary chat
  turns** with no config key, no env var, and no removal by
  `--no-default-features`: the `search` tool's web fallback
  (DuckDuckGo → Google → DuckDuckGo Lite, fired whenever the top LOCAL
  retrieval score is thin), `web_fetch` (any URL the model emits,
  scheme-only validation, 5 redirects), and `wikipedia_fetch`. They sit
  three lines below `ShellTool`, which *is* gated. `Permission::Network`
  is not a control: it is consulted at exactly one call site, in the
  plan executor, and the chat path calls `tool.execute()` directly.
  Under `--no-default-features`, `search` survives built local-only.
- **Why they are two flags, not one:** privilege and egress are
  unrelated decisions. One flag for both would make neither name true
  (§10.6, one decider one name).
- **Proof so far:** both configurations compile clean; under
  `--no-default-features` the dead-code count drops 47 → 2, confirming
  the modules are excluded from the binary rather than merely
  unreachable. `acceptance.sh` check 0c enumerates `GET /v1/tools` on
  the running box and fails if either egress tool is present *or* if
  `search` went missing with them.
- **Flip condition (falsifiable):** `dev-routes` is **deleted and the
  hardened surface becomes unconditional** once all four are true —
  (a) upload routes path-jail to a per-tenant root instead of taking
  an absolute server path; (b) `/mcp` moves inside the auth layer and
  stops trusting peer address; (c) `test_command` is an allowlist, not
  free text; (d) `ShellTool` registration is gated on an explicit
  config key. `net-tools` flips only when the three tools gain a
  runtime allowlist that the chat path actually consults — a cargo
  feature is the wrong granularity for a product capability, and is
  here only because no runtime control exists.
- **Settled by:** the on-prem pilot
  (`sovereign/deploy/onprem/PLAN.md`). If the pilot does not proceed,
  the items above are the standing debt regardless — this crate is
  reachable from the desktop's embedded host too.
- **Review by:** 2026-09-15.

### Headless OCR in the daemon — the `ocr` cargo feature (default **OFF**)
- **Shipped:** 2026-08-03, off by default
  (`sovereign-cli-daemon/Cargo.toml`, `ocr = ["sovereign-tools/paddle-ocr"]`).
- **What is dark:** the daemon can install an `OcrCtx` at boot so
  `svrn corpus watch --ocr` reads scanned PDFs headlessly. Without the
  feature, a scanned PDF lands in `WatchedFolderState.failed_files`
  with reason `scanned_no_text` — reported, not silent, but the
  document does not enter the index.
- **Cost of on:** pulls `ort` + `ndarray` + `imageproc` + `i_overlay`
  into every daemon build, and the runtime needs ~20 MB of staged
  assets (`det.onnx` + `rec.onnx` + `dict.txt` = 12.6 MB, `libpdfium`
  = 7.6 MB) that a default install does not fetch. Off-by-default
  keeps dev builds and the standard release set unchanged;
  `sovereign/deploy/onprem/package.sh` turns it on.
- **Flip condition (falsifiable):** default-on when (a) the added
  clean-build wall time for `-p sovereign-cli-daemon` is measured at
  under 60 s, **and** (b) the OCR assets ship in the standard release
  artifact so the feature is not compiled-in-but-unusable — a build
  that has the code and no models fails `build_engine` at ingest,
  which is worse than not having it.
- **Settled by:** the on-prem pilot's `package.sh`; the general
  release path (`scripts/release-cli-local.sh`) does not stage OCR
  assets today.
- **Review by:** 2026-09-15.

### Comaintainer director (M0 supervised) + review seat — script-invoked, no env flag
- **Shipped:** 2026-08-06, M0. The role is `gym/comaintainer/CHARTER.md`;
  the seat is `scripts/co-review.sh` (advisory: exit 0 always, no hook,
  no gate, verdicts append to `~/.sovereign/comaintainer/verdicts.jsonl`);
  supervision records land via `scripts/co-directive-log.sh`
  (`--stats` = the per-kind edit rate). Vision `docs/COMAINTAINER.md`.
- **What it does:** a trained, measured role between operator and agent
  pool. At M0 every directive it drafts (order/steer/review/briefing)
  passes an operator approve/edit before reaching a worker; the
  (draft, final) delta is the disengagement signal, self-driving style.
- **What is dark:** any autonomy. M0 supervision is charter-enforced
  (remembered, not structural) — acceptable only because the operator
  is in the loop by construction; from M1 on, sends must route through
  the helper with an explicit per-kind operator-ack flag (§7).
- **Proof so far:** the gym (`gym/comaintainer/`, 301 episodes, tier-A
  holdout 72) with noise floor, charter-less baseline and charter v1
  scored against a predeclared margin — numbers in
  `gym/comaintainer/README.md` §Results.
- **Flip condition (M1, per directive kind):** over the trailing ≥30
  directives of that kind, the operator edit rate is at or below a
  threshold SET FROM M0 DATA (never invented — §18.4), AND the charter
  meets its predeclared gym margin on the tier-A holdout.
- **Settled by:** `~/.sovereign/comaintainer/directives.jsonl`
  (`co-directive-log.sh --stats`) + the gym.
- **Review by:** 2026-09-06.

## OWED A ROW — dark capabilities with no flip condition (audit 2026-08-05)

**How this section came about.** Cross-referencing
`quality/env-flags.toml` against this file found **31 retrieval flags, 12
default-off or `status = experiment`, and only 2 with a ledger row**. The
contract in the preamble says a dark ship adds a row in the same commit; these
predate the contract, so nobody broke it — but they are exactly the withering
this file exists to stop, and they were invisible until someone counted.

Stripping out what is not ledger material — `SOVEREIGN_FORENSIC` (debug),
`SOVEREIGN_COMPACTION_DISABLE` (escape hatch), and `ATOM_ENUM_RANK` / `_POOL` /
`DECOMP_DECAY` (tuning params, not on/off capabilities) — **six genuine dark
capabilities are owed a row**. They are listed here rather than given
fabricated flip conditions: a row whose "proof so far" was invented is worse
than no row, because it reads as settled.

Each needs one measurement before it can graduate to a real row. The instrument
already exists and is deterministic (~9 min per arm):
`svrn bench all --bench-root sovereign/bench --filter <bank> --prod-pipeline`.

| flag | capability | measurement owed |
|---|---|---|
| `SOVEREIGN_ATOM_ENUM` | entity-typed atom enumeration for enumeration-class questions | an A/B on a bank with enumeration questions ("which X were involved") — the Enron counterparty case its doc comment cites |
| `SOVEREIGN_ATOM_ENUM_RELATIONS` | relation atoms in the same path | same bank, as a second arm on top of `ATOM_ENUM=1` |
| `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND` | wikipedia graph-neighbour expansion | A/B on a wikipedia bank; watch corpus-mix drift, not just recall |
| `SOVEREIGN_META_BRIDGE` | meta-atlas bridge boost | A/B on a multi-corpus bank |
| `SOVEREIGN_QUERY_DECOMP` | question decomposition + fan-out | A/B on a multi-hop bank; cost is extra retrieval round-trips |
| `SOVEREIGN_TITLE_EXPAND` | LLM question→title expansion | A/B on any retrieval bank; cost is one LLM call per turn |

**One live inconsistency found in the same audit, and it is not cosmetic.**
`SOVEREIGN_ATOM_ENUM` is default-**off** with `status = experiment`, while
`SOVEREIGN_ATOM_ENUM_OVERVIEW` — a sibling path in the same module, reached
through the same `enumerate_typed_atom_chunks` entry point — is default-**on**
and runs in production on every overview-shaped question. That is how audit D1
happened: a production path whose parent feature is nominally an experiment,
so nobody was measuring it. Either the overview path is a shipped capability
(then `ATOM_ENUM`'s `experiment` status is wrong and its own row is overdue) or
it is an experiment (then it should not be default-on). Resolve it with the
`SOVEREIGN_ATOM_ENUM` measurement above.

- **Review by:** 2026-09-05 — the whole section. If a flag has no measurement
  by then, the right move is to DELETE the capability, not re-date it. Six
  unmeasured flags is a labyrinth; six measured ones is a feature set.

## REJECTED — measured no; do not re-litigate without new evidence

### GLiNER2 as the vault/conversation extractor — `SOVEREIGN_GLINER_MODEL_ID` (stays `gliner_small-v2.1`)
- **Shipped and settled the same day, 2026-08-03.** The row was written
  in the morning against a flip condition — "holds the vault bar at a
  lower time-to-enriched AND no per-label typing regression" — and the
  afternoon's run refuted **both halves**. Recording it rather than
  deleting it, because the seam it rode in on is staying.
- **Verdict, on all 3,175 obsidian vault chunks**, both backends through
  the production `LabeledEntityExtractor` seam
  (`sovereign-gliner/examples/typing_audit.rs`, artifact
  `research/enrichment-spikes/findings/typing_audit_obsidian.json`):
  - **Time: 881.9 s v1 vs 893.2 s GLiNER2 — no speedup, marginally
    slower.** The 2.52× was real but is a property of the chunk-length
    distribution, not the model (sep p50 761 chars; vault p50 1,808).
    v1's gline-rs stack batches 8 texts per call and amortises; GLiNER2
    is one graph call per text. Note `dc2e4b5d`.
  - **Typing: worse, not fixed.** Mention-level Person accuracy 96.9%
    (v1) vs 81.8% (GLiNER2) on the vault oracle; 99.7% vs 67.3% on sep.
    `Ostrom` — the vault's anchor entity — is `Person` ×6 /
    `Organization` ×6 under GLiNER2. `Work` becomes a catch-all for
    ordinary noun phrases: 16,053 `Work` mentions to v1's 632, 47% of
    its entire output. Note `f42cf7ec`.
- **What is NOT rejected:** the residency finding (GLiNER2 is ~4.8×
  lighter, note `3f47d12e`) and the seam itself. The knob stays — it is
  how anyone re-tests this — and P2.1's steps (b)–(d) were never
  evaluated.
- **Re-open only if:** a GLiNER2 checkpoint or label/threshold
  configuration demonstrably stops `Work` absorbing common noun phrases,
  scored **per mention** on `bench/gliner/` oracles; or a target corpus
  with sep-shaped chunk lengths makes the throughput win real AND typing
  holds. Both halves, not either.

### Cross-encoder reranker slot — `SOVEREIGN_RERANK_MODEL_PATH` (stays unset)
- **Verdict:** 2026-08-04. **The flip condition PASSED and the slot is
  still rejected** — it was a quality condition, and quality was never
  the binding constraint. Rejected on TTFT plus a fourth resident model
  slot. Notes `6a957b47`, `f4150097`; artifacts
  `target/overnight/20260803-225051/block1/`.
- **The condition, answered:** 180-question paired bank on
  `conversations-anthropic` via `eval run --prod-pipeline`. The cheap
  fix measured alone (`dedup-only`, per-article dedup, no model) moved
  the number a lot and still LOST to the cross-encoder 42–89
  (p=0.0000). Gap not closed ⇒ by the letter of the condition, earned.

  | arm | mean RR | both@10 | src ratio | **search p50** |
  |---|---|---|---|---|
  | baseline | 0.2631 | 26.7% | 0.744 | **557 ms** |
  | dedup-only | 0.3362 | 50.6% | 0.856 | **1,240 ms** |
  | reranker | 0.3968 | 75.6% | 0.903 | **4,566 ms** |

- **What killed it:** corpus search runs SYNCHRONOUSLY inside the turn,
  so retrieval latency lands on TTFT. The median turn goes 0.56 s →
  4.6 s **before the model emits a token**. The reranker's margin over
  free dedup is +18% mean RR / +25pp both@10 for **+2.8 s of TTFT** —
  and it needs a 4th resident slot on a daemon already at ~29 GB
  (35B + 2B + embed + a 7.85M-edge wiki graph). `RERANK_EXPERIMENT.md`
  §"Resident-weight cost" predicted exactly this in May.
- **And it is fragile, not merely slow:** the same arm cost 4.3 s/query
  on a quiet box and **>280 s/query** the next day under memory
  pressure (~5 GB free, compressor holding ~5.4 GB of RAM). A ~60×
  degradation with headroom is not a knob you ship behind a default.
- **What shipped instead:** `[retrieval] dedup_by_source = true` on
  `conversations-anthropic` (measured) and `conversations-chatgpt`
  (same shape, inferred — labelled as such in the recipe). ~60% of the
  quality gain for ~20% of the latency, no model, no slot, no VRAM.
  This is `RERANK_EXPERIMENT.md`'s own pre-registered call — "the big
  win is the dedup… don't add the slot, add the diversifier" — decided
  by the arm that doc asked for.
- **NOT rejected:** per-article dedup itself, and the reranker as an
  OFFLINE/batch tool where TTFT is irrelevant (bench scoring, index
  build). The rejection is specifically *a resident slot on the
  interactive path*.
- **Re-open only if:** retrieval moves off the critical path (streamed
  or speculative retrieval), OR a rerank pass lands somewhere TTFT
  cannot see it, OR an `x:rerank` peer capability serves it from a node
  with headroom — the OICP route `RERANK_EXPERIMENT.md` §"Mesh contract
  surface" sketched. Not on a faster GGUF alone: 610 MB was never the
  problem, the 4th slot and the synchronous path were.
- **Code NOT deleted, deliberately** — unlike the cluster-score row
  below. The rerank stack has live non-interactive consumers (the bench
  param-loop drives `SOVEREIGN_RERANK_DEDUP_*` via
  `scaffolding_param.rs::RerankSettings::set_env` +
  `promote.rs:389`, and `bench enrichment-ablate --rerank` scores it),
  and the dedup path that DID ship shares that code. Deleting the slot
  would take the diversifier with it. What must not persist is the
  *expectation* that this becomes a default — hence this row.

### Conversation entity PPR — `SOVEREIGN_CONV_PPR_WEIGHT` (0.25 → **0.0**)
- **Verdict:** 2026-08-04. Default flipped OFF. Notes `6a957b47`,
  `f4150097`; artifact
  `target/overnight/20260803-225051/block1/VERDICT-with-ppr0.txt`.
- **Measured, on the corpus where it actually fires:** 180-question
  paired bank on `conversations-anthropic`, `eval run --prod-pipeline`,
  two-sided sign test on reciprocal rank. Alone: 49–31 vs the off arm,
  **p=0.0567**. Under the strongest retrieval config: 64–43,
  **p=0.0527**. Neither reaches p<0.05. The arm was NOT vacuous — it
  changed ordering on 146/180 questions — so this is "measured and did
  not separate", not "never engaged". (An earlier 2026-07 attempt WAS
  vacuous: it ran on SEP, where this path never fires.)
- **Why the ceiling is low, structurally:** it re-ranks in place and
  never adds a document. `B-in-pool` (87.8%) and `source_ratio`
  (0.9028) were **identical to four decimals** with it on and off —
  only the ordering moved. `bench/conversation-bridge/GATE_FINDINGS.md`
  predicted exactly this before the run ("PPR re-ranks in place and
  never adds"), which is also why that doc pre-registered this A/B.
- **Cost of on:** a per-conversation entity graph rebuilt from SQL on
  EVERY query, plus — because it reads `chunk_entities` on the query
  path — it is the sole reason the GLiNER NER pass must complete
  eagerly at ingest before a corpus is fully useful. Turning it off is
  what makes deferred/late NER safe (`PROGRESSIVE_ENRICHMENT.md`).
- **CODE KEPT, NOT DELETED — operator call 2026-08-04.** ~1,325 lines
  (`conv_entity_graph.rs` + `rerank_conv_chunks_via_ppr` + 23 unit
  tests) were sized for removal and deliberately retained: the code is
  correct and tested, the measurement says *marginal*, not *wrong*, and
  a one-line default is cheaper to reverse than a deletion is to
  rebuild. This is a deliberate departure from the cluster-score row
  below, which was deleted — that one had a measured **0.0000** delta;
  this one has a real-but-unprovable effect.
- **What a user loses:** the "bridge" badge on promoted sources
  (`ppr_seed` / `ppr_mass_norm` → `SourceAttribution.svelte`,
  `EpistemicFooter.svelte`) simply never fires. The UI degrades
  silently and correctly; no dead controls.
- **Re-open only if:** a bank shows it separating at p<0.05 — most
  plausibly one built on *cross-conversation* questions where in-pool
  reordering is the whole game, since this bank's own headroom analysis
  showed 66% of target conversations were already in the pool. Set a
  non-zero `SOVEREIGN_CONV_PPR_WEIGHT`; nothing else is needed.

### Cluster-score blend — `SOVEREIGN_DOC_CLUSTER_WEIGHT` (stays 0.0)
- **Verdict:** 2026-07-31, per this row's own settling condition — the
  T1 P0.4 knob matrix (`bench enrichment-ablate`, 3 sep banks × 3
  reps, artifact `sovereign/bench/ablation/2026-07-31-sep-knob-matrix.json`)
  reports the banks CANNOT separate it: Δ = 0.0000 on every bank,
  zero rep spread. In fact NO knob separated — even
  `SOVEREIGN_RAPTOR_GROUNDING=0` moved only −0.0125 on summarize,
  under the 0.02 floor. Dark since 2026-05-22; settled in one night
  once the lane existed.
- **Honest scope note:** the sep banks do not exercise the
  attached-document search path the blend lives in — this is "the
  current banks can't see it", not "the blend does nothing". Both
  readings route the same way:
- **Re-open only if:** P3.1 golden authoring (T2) produces a bank that
  exercises attached-doc retrieval with cluster-structured answers —
  the same routing as the demand-plan rejection.
- **CODE DELETED 2026-08-01.** Both env vars, the blend branch in
  `attached_document_search.rs`, and the now-unreachable
  `blend_by_cluster_score` / `min_max_normalize` helpers with their 10
  tests are gone; the registry entries in `quality/env-flags.toml` are
  replaced by a tombstone pointing here. A Rejected verdict that leaves
  the code running is the withering pattern this ledger exists to stop —
  the verdict and the deletion belong in the same week, not the same
  hypothetical future tranche. Enrichment knob count **12 → 10**, the
  first movement on the `ENRICHMENT_ROADMAP.md:348` complexity ratchet.
  Recovery for the re-open case: `git show <this commit>^` — the
  rationale survives in `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`.

### Demand-plan fan-out — `SOVEREIGN_DEMAND_PLAN_FANOUT` (off)
- **Verdict:** 2026-07-19 A/B — net-neutral answer quality at 2–3x
  retrieval latency. Flag stays off; `env-flags.toml` records it.
- **Re-open only if:** a bank exists that separates multi-hop recall
  (P3.1 golden-authoring, T2). A flat-recall bank cannot exonerate it.

### Claim-search ladder — `SOVEREIGN_GATE_CLAIM_SEARCH_LADDER` (stays off)
- **What it did:** used a batched triage judge to decide which claims
  skip the per-claim corpus fan-out. Worth wanting: that fan-out is one
  hybrid search per allowed corpus per claim and measured 25% of
  wall-clock on `bench sep/summarize --synth`; the ladder measured
  **−6.8%** turn wall while skipping ~half the fan-out.
- **Verdict:** 2026-08-05 — **it destroys 23% of the rescues.** Over 78
  claims on the `sep-summarize-slowtail` scratch bank (SHADOW=1,
  LADDER=0, so every claim is still searched and the true rescue set is
  observable): 26 real rescues, 41 claims the ladder would skip, and
  **6 of those skipped claims were real rescues.** Batch-vs-calibrated
  agreement is 86% (67/78), and the 14% disagreement lands exactly where
  it costs most. Trading 23% of the anti-fabrication rescue mechanism
  for 6.8% latency is not a trade this system makes.
- **Why the safety argument failed:** it claimed losslessness *by
  construction* — a rescue fails without re-search, so it must have
  stage-1 `vp >= tau` and always reach stage 2. Sound only while stage 1
  is the CALIBRATED per-claim judge. Stage 1 is the batched text A/B, a
  different instrument with different tau semantics, so their agreement
  is empirical. The claim was withdrawn by its author before this run;
  the run measured what the withdrawal predicted.
- **Both known stage-1 instruments now fail.** A calibrated per-claim
  stage 1 measured net-NEGATIVE (+5.0s wall — a restored pinned prefix
  does not make a forced-choice free; note `a4be8afd`). A batched stage 1
  is fast but lossy, above. Any re-open needs a THIRD instrument, not a
  retuned threshold.
- **Re-open only if:** a stage-1 triage exists whose disagreement with
  the calibrated judge is measured at ~0 on skipped claims — the gate is
  `lost_rescue == 0` summed over a bank, from the `claim_search_shadow`
  event. Note `3850a896`-adjacent; instrument lives in
  `grounding/mod.rs`.
- **Kept, not deleted:** the flag and its shadow instrument stay so the
  next attempt inherits the measurement harness rather than rebuilding
  it. The fan-out it targets is still 25% of wall-clock and still worth
  attacking.

### Acquisition gate armed at 0.45
- **Verdict:** 2026-07-20 — `import_conversations` is a top-1
  attractor at that threshold; arming it misroutes.
- **Re-open only if:** the attractor is fixed and the threshold
  recalibrated against the post-fix distribution.

### Speculative decoding (classic draft)
- **Verdict:** 2026-05-12 — net-negative on this hardware; KV-rollback
  hand-port costed at 2–4 days for nothing the llama-server harness
  doesn't provide.
- **Re-open triggers:** recorded in
  `sovereign/docs/archive/SD_EXPERIMENT.md` §closure.

## INTENTIONAL OPT-IN — off is the designed end state, not a debt

### RSS hard limit — `SOVEREIGN_RSS_HARD_LIMIT_MB` (off)
- Self-SIGTERM is only safe under a supervisor that restarts the
  daemon (2026-07-18). Soft-warn is on. This row exists so nobody
  "fixes" the default.

## GRADUATED — the pipeline completing, for the record

### `SOVEREIGN_SKIP_MOTIFS` / `vault-report --no-motifs` → **deleted**
- **Lifespan: 2026-08-02 to 2026-08-02.** Shipped dark in the morning
  as an ablation arm; the code it ablated was deleted the same day. The
  knob is gone with it — this row is the record, not a live default.
- **What it proved.** Motif extraction was **22.3m of a 52m03s cold
  vault build — 42.8% of time-to-enriched** (330 notes,
  `~/.sovereign/bench-runs/vault-report/1785678945/`), and its output
  table `conv_motifs` had one INSERT, two DELETEs and **no reader
  anywhere in the workspace**. The briefing-signposts claim at
  `conv_tiered_provider.rs:232` traced to `CONV_TIERED_PORT.md:385`,
  which is future tense and was never built for the conv/vault side.
- **The measured result** (three cold builds + `eval run
  --prod-pipeline`, obsidian vault, sweeper paused):

  | config | wall | speedup | facts | sources |
  |---|---|---|---|---|
  | motifs + GLiNER | 52m03s | 1.00x | 58/68 | 8/12 |
  | **motifs off, GLiNER on** | **29m32s** | **1.76x** | **58/68** | **8/12** |
  | motifs off, GLiNER off | 14m15s | 3.65x | 58/68 | 5/12 |

  Motifs-off matched the full build **per question exactly** on facts
  (6,6,5,3,4,6,5,4,3,6,6,4). Run-to-run variance on one build was zero.
- **Resolution: deletion, not a flip.** `build_folder_artifacts` now
  calls `build_raptor_nodes_with_checkpoint`, which has no motif
  concept in its return type — the pass cannot be re-enabled by
  setting anything. `save_conv_motifs` and `ConvMotifRow` are deleted.
  The `conv_motifs` table and its purge DELETE are retained so existing
  databases still shed their legacy rows.
- **Untouched:** the attached-document path keeps its motifs
  (`asset_motifs`, read by `list_asset_motifs` for the doc briefing).
- **Notes:** `3f47d12e` (the result), `e10bf96e` (the no-reader
  census), `de25ebe9` (why the confirmation arm was cancelled),
  `0b8b6cae` (sweeper contamination), `d39af2dc` (the 68/68 correction).

### Extractive summary mode default for memory corpora (T1 P1.1)
- **Flip condition met 2026-07-31, same day it was written:** the
  production seam held parity on the sep banks — both arms rebuilt
  through `enrich raptor` at identical 14-article scope, |B−A| =
  −0.0125 on summarize (band ±0.025), 0.0000 on obscure (band
  ±0.0167), r1–r3 deterministic, rawindex guard 0.0000 both banks
  (`research/enrichment-spikes/runs/prodAB/`).
- **Default flipped:** memory corpora (vault notes, imported
  conversations via `build_folder_tiered_provider`; memory-pool trees
  via `mem_atlas`; the vault-wide theme synthesis) now build
  EXTRACTIVE trees. Attached documents keep abstractive — now
  verifier-gated (T1 P1.2, same push). `enrich raptor` CLI default
  remains abstractive with explicit `--summary-mode`.
- **Registry/env:** no env flag — the default is code-level policy at
  the memory-corpus construction sites, provenance-stamped per node.

### Caller-directed prefix-cache pin — `SOVEREIGN_PREFIX_STATE`
- **Default ON 2026-08-03**, opt out with `=0`. Genuinely flipped this
  time — `env_enabled()` now defaults true.
- **This row was FALSE for thirteen days and that is the lesson.** It
  claimed "default-on 2026-07-21" when the flip had never happened:
  `BATCHED_GATE_VERIFY.md` *recommended* flipping after two hardenings,
  those hardenings landed, and the row recorded the recommendation as
  executed. A false GRADUATED row is worse than no ledger, because it
  is trusted. Nothing parses this file's review-by dates (T1 B2 is the
  gate that would have caught it).
- **Earned by:** controlled A/B through the production answer path,
  `svrn bench enrichment-ablate sovereign/bench/obsidian/questions.toml
  --prefix-state --reps 2`, on `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`:

  | arm | reps | mean wall | fact ratio |
  |---|---|---|---|
  | off | 901.7s, 835.2s | 868.4s | 0.4736 |
  | on  | 671.1s, 667.0s | 669.0s | 0.4597 |

  **1.30x, −199s per rep**, against an OFF-arm spread of 66.5s — the
  delta is 3x the noise. Arms proven distinct by pin telemetry: OFF
  `LEARNED=0 HIT=0`, ON `LEARNED=28 HIT=86`. Reproduces the 2026-07-21
  result (1.35x, 786.3s → 584.5s) on HEAD.
- **The earlier "worth ≈0" result was never a contradiction.** The
  2026-07-12 A/B measured ONE synthesis prefill; the pin's only
  consumer is the grounding gate, which issues ~35 judge calls per turn
  each re-prefilling the same evidence. Two workloads, not two answers.
- **Open caveat, stated rather than buried:** the quality delta is
  −0.0139 mean fact ratio (~1 fact in 60). That is below the ablation's
  0.02 separation floor and reports as NOT SEPARABLE, but it was
  IDENTICAL in both reps — a small reproducible difference, not noise.
  If restore is bit-exact it should be zero. Settle it by checking
  restore bit-exactness, not by adding reps (the eval is deterministic
  per arm, so more reps of the same config cannot move it).
- **Model scope:** measured on `qwen35moe`. The pin's value scales with
  prefill cost, and `prefix_cache_gate` vetoes ordinary partial-KV
  reuse on both `qwen35moe` and dense `qwen35`, so on those the pin is
  the ONLY caching available. On a small primary the win will be
  smaller and the ~64KB/token state cost proportionally larger; the
  byte-capped LRU (`_MAX_MB`, default 2048) is what bounds it.
- **Instrument:** `svrn bench enrichment-ablate --prefix-state` is
  committed and is the template for any daemon-side knob. The original
  harness (`scratchpad/arm_runner.py`) never was.

### RAPTOR grounding — `SOVEREIGN_RAPTOR_GROUNDING`
- Default **on**, status shipped (`env-flags.toml`). Summary nodes as
  virtual chunks earned the default.
