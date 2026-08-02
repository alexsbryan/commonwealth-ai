# Enrichment complexity ratchet — the seven numbers, measured

**What this is.** `ENRICHMENT_ROADMAP.md:348-351` declares seven "Today"
numbers and makes them a gate: *"a tranche that leaves any of them
higher than it found them has failed its gate, whatever features it
shipped."* Those numbers were authored as estimates in the roadmap's
§4.0 table and **never enumerated**, so until this file existed the gate
could not be checked — a tranche could claim movement without anyone
being able to show it wrong.

This file is the document of record. Each number gets a **predicate**
(so it is falsifiable), an **enumeration** (so it is auditable), and a
**value**. Re-measure at every tranche exit and append a row; never
rewrite a past measurement.

Companion documents: `ENRICHMENT_ROADMAP.md` §4.0 (the end state each
number is walking toward), `DEFAULTS_LEDGER.md` (why a given default is
what it is), `quality/env-flags.toml` (the env registry).

---

## Headline: two of the seven were undercounted at authoring

Measuring for the first time turned up three corrections. None of them
is a regression — they are the roadmap's estimates meeting a census.

| # | Roadmap said | Measured | Why the gap |
|---|---|---|---|
| 2 | ~9 stores | **11** | `mem_raptor_nodes`, `asset_motifs`, `conv_motifs` are live and were not in the roadmap's enumeration |
| 5 | ~12 env knobs | **26** | the roadmap counted registered flags only; 11 more are read by code and sit in the grandfathered `quality/baselines/env_unregistered.txt` |
| 6 | "2 + an unwired flag" | **2 + a wired flag** | `SOVEREIGN_ATLAS_INCREMENTAL` is read *and* load-bearing at `newsworthy_host.rs:318-340` — it gates the refresh-role rebuild fallback |

**Consequence for T1's exit claim.** T1's commit note recorded the knob
population moving "12 → 10". Against the predicate in §5 the registered
count moved **16 → 15** (two deleted, one added) and the total
population moved **27 → 26** — a net of **−1**, not −2, against a
baseline more than twice the size the roadmap estimated. The direction
is right and the deletion was real; the magnitude was overstated because
the baseline it was measured against had never been enumerated. That is
the failure this file exists to stop, and it is why every number below
carries a predicate.

---

## 1. Enrichment systems — **4**

**Predicate:** distinct values of `EnrichmentConfig.enrichment_type`
dispatched during or after ingest.

| System | `enrichment_type` | Dispatch |
|---|---|---|
| Tiered (RAPTOR + GLiNER) | `"tiered"` | `corpus-engine/src/engine/ingest.rs:1738`, sub-branching folder vs conv at `:1749-1755` |
| Investigation | `"investigation"` | `ingest.rs:1785` — **skipped at ingest**, explicit CLI verb only |
| Atlas (v2) | `"atlas"` | `ingest.rs:1808` — **skipped at ingest**, `enrich init` / `enrich build` only |
| Field model (System 1) | fallthrough (schema default, `recipe.rs:885`) | `ingest.rs:1817` |

Plus the naming collision the roadmap flags: "atlas" means both the
`enrichment_type` above and the atom-graph artifact directory.

**Unchanged by T1.** Target: 1 pipeline with per-corpus profiles.

## 2. Knowledge-artifact stores — **11** (roadmap said ~9)

**Predicate:** a distinct persistent location holding enrichment output.
The atom-graph files count as **one family** (they are one store with
four serializations), matching the roadmap's own convention.

SQLite (`sovereign-store/src/migrations.rs`), each verified live —
at least one write site and one read site:

1. `raptor_nodes`
2. `conv_raptor_nodes`
3. `mem_raptor_nodes` — **not in the roadmap's enumeration**
4. `conv_skeletons` — roadmap calls this vestigial; it has 1 write and
   8 read sites, so verify before treating it as deletable
5. `chunk_entities` (+ the `chunk_entity_progress` sidecar, not counted
   separately)
6. `vault_themes`
7. `asset_motifs` — **not in the roadmap's enumeration**
8. `conv_motifs` — **not in the roadmap's enumeration**

On disk:

9. atom-store family — `atoms.json`, `edges.json`, `atoms.lance`,
   `edges.csr`
10. `raptor_summaries.lance`
11. `field_skeleton.json`

**Unchanged by T1** (which deleted no stores, by design — T1 is the
demolition permit). Target: 3.

## 3. Entity-extraction paths — **5**

**Predicate:** a distinct code path that produces entity records.
All five are live; none is vestigial.

1. GLiNER v1 — `sovereign-gliner/src/gliner_ner.rs:319`, dispatched
   `corpus-engine/src/enrichment/tiered.rs:317`
2. Slow-LLM lark fallback — `sovereign-tools/src/document_asset.rs:1851-1866`,
   triggered on GLiNER miss/empty/error at `:1810-1830`
3. Phase-1 LLM enumeration — two distinct implementations:
   `corpus-engine/src/enrichment/pipeline/runner.rs:673` (System 2
   atlas) and `corpus-engine/src/enrichment/entity_extraction.rs:349`
   (System 1, slated for deletion with `field_engine.rs` under D4)
4. SCIP walk — `corpus-engine/src/enrichment/atlas/strategies/code_walk.rs`
5. Tabular — `corpus-engine/src/extractors/tabular_atoms.rs`

**Unchanged by T1.** Target: 2 (encoder schemas + structural).

## 4. Trust model — **moved: folklore → a rule with two exceptions**

**Predicate:** can an engineer state, without tribal knowledge, which
persisted artifacts may contain unverified generated prose?

**This is the one number T1 moved.** Before: per-artifact folklore.
After:

- Memory corpora (vault notes, imported conversations, memory-pool
  trees, vault-wide theme synthesis) build **extractive** trees by
  default — verbatim quotes, structurally unable to fabricate
  (`sovereign-tools/src/enrichment_bootstrap.rs:60-72`, stamped
  `summarizer_model = "extractive"` at `raptor_atlas.rs:1372-1373`).
- Attached documents stay abstractive but are **verifier-gated** —
  nothing persists without a verdict (T1 P1.2).
- Evidence is provenance-aware: factual claims verify against Leaf text,
  summaries support thematic claims only (T1 P1.4,
  `runtime/grounding/mod.rs:1735-1753`).

**Residual folklore, named honestly:** the `enrich raptor` CLI still
defaults abstractive; a verifier-driven abstractive→extractive fallback
is distinguishable only by `summarizer_model == "extractive"`, with no
per-node boolean (deferred to D3). Target: one rule, no exceptions.

## 5. Env knobs on the enrichment paths — **26** (roadmap said ~12)

**Predicate:** an env var that gates the production or consumption of an
enrichment-produced artifact — RAPTOR nodes, atlas atoms/edges, GLiNER
extraction, PPR over an entity/atom graph, or graph-neighbour expansion.
Deliberately excludes grounding-gate knobs that do not touch an
enrichment artifact (`SOVEREIGN_CITATION_*`, `SOVEREIGN_SUFFICIENCY_CHUNKS`,
`SOVEREIGN_COVERAGE_PROBE`) and pure retrieval-merge knobs
(`SOVEREIGN_MERGE_SELECT`, `SOVEREIGN_META_BRIDGE`,
`SOVEREIGN_DEMAND_PLAN`).

**Registered in `quality/env-flags.toml` — 15:**

`SOVEREIGN_GATE_EXCLUDE_RAPTOR`, `SOVEREIGN_GATE_SUMMARY_EVIDENCE`,
`SOVEREIGN_ATLAS_GROUNDING`, `SOVEREIGN_ATOM_ENUM`,
`SOVEREIGN_ATOM_ENUM_OVERVIEW`, `SOVEREIGN_ATOM_ENUM_TOPK`,
`SOVEREIGN_ATOM_ENUM_SCORE`, `SOVEREIGN_RAPTOR_GROUNDING`,
`SOVEREIGN_RAPTOR_LATE`, `SOVEREIGN_RAPTOR_TOP_M`,
`SOVEREIGN_RAPTOR_MIN_LEVEL`, `SOVEREIGN_RAPTOR_DEDUPE`,
`SOVEREIGN_PPR_EXPAND`, `SOVEREIGN_CONV_PPR_WEIGHT`,
`SOVEREIGN_GRAPH_NEIGHBOR_EXPAND`

**Read by code but unregistered — 11**, all sitting in the
grandfathered `quality/baselines/env_unregistered.txt` (167 entries
total across the workspace):

`SOVEREIGN_ATLAS_INCLUDE_CLAIMS`, `SOVEREIGN_ATLAS_INCLUDE_DEPTHS`,
`SOVEREIGN_ATLAS_INCREMENTAL`, `SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS`,
`SOVEREIGN_CONCEPT_OBLIGATIONS`, `SOVEREIGN_DOC_PPR`,
`SOVEREIGN_DOC_PPR_BOOST`, `SOVEREIGN_ENRICH_AUTO_RETRY`,
`SOVEREIGN_ENRICH_SKIP_INDEX`, `SOVEREIGN_GLINER_MODEL_DIR`,
`SOVEREIGN_RERANK_ATLAS_WEIGHT`

**T1's movement:** deleted `SOVEREIGN_DOC_CLUSTER_WEIGHT` and
`SOVEREIGN_DOC_CLUSTER_POOL` with their code and 10 tests (tombstone at
`quality/env-flags.toml:305`); added `SOVEREIGN_GATE_SUMMARY_EVIDENCE`
(`:296`). So registered went **16 → 15** and the total **27 → 26** —
net **−1**. Real, and half the size the commit note claimed.

**Follow-up this census creates:** registering the 11 grandfathered vars
is a prerequisite for the target of ≤4, since a var nobody has
registered cannot be deleted with a decision attached. That work is not
scheduled; it belongs to T2's D2.

Target: ≤ 4, each with a committed A/B behind its default.

## 6. Incremental mechanisms — **2, plus a wired flag**

**Predicate:** a distinct mechanism for updating enrichment output
without a full rebuild.

1. `apply_atom_delta` — `corpus-engine/src/enrichment/atlas/atoms_delta.rs`
2. `extract_delta_for_corpus` — the GLiNER chunk-entity delta,
   `corpus-engine/src/enrichment/tiered.rs:558-580`

**Correction to the roadmap:** `SOVEREIGN_ATLAS_INCREMENTAL` is
described as "read today and unused" (`ENRICHMENT_ROADMAP.md:466-468`,
`SIZING:263-265`). It is read **and load-bearing** at
`sovereign-mesh/src/newsworthy_host.rs:318-340`: it gates the
refresh-role rebuild fallback, and portal-role corpora deliberately
bypass it because the legacy full-rebuild path collapses a portal page
into a useless single-Entity atlas. Anyone planning P2.3(a) should read
that comment first.

**Unchanged by T1.** Target: 1 (content-hash deltas), no flag.

## 7. "Explain enrichment" — **a page plus a warning section**

**Predicate:** the shortest honest description of the enrichment system
a new engineer must hold.

Unchanged. `ENRICHMENT.md` plus the naming-collision warning. Target:
the single paragraph at `ENRICHMENT_ROADMAP.md:315-320`.

---

## Measurements

| Date | Tranche | Systems | Stores | Extraction paths | Trust model | Knobs (reg / total) | Incremental | Explain |
|---|---|---|---|---|---|---|---|---|
| 2026-08-01 | T1 exit | 4 | 11 | 5 | rule + 2 exceptions | 15 / 26 | 2 + wired flag | page + warning |
| — | (pre-T1, reconstructed) | 4 | 11 | 5 | folklore | 16 / 27 | 2 + wired flag | page + warning |

**T1 verdict against the ratchet: PASS.** No number rose. One fell
(knobs, −1) and one improved qualitatively (trust model — the tranche's
actual product). Stores, systems, and extraction paths are flat **by
design**: T1 is the measurement fabric that makes T2's subtraction safe,
and its own Deletes ledger said so up front.

## T1 exit gates — verdicts

The ratchet is one gate; `ENRICHMENT_ROADMAP_SIZING.md` §4 Tranche 1
names four. Recorded here so the tranche has one close-out artifact.

**Gate 1 — the canary proves the enrichment lane can fail. MET.**
Demonstrated, not asserted: control run green (7/10 person atoms, same
as baseline), perturbed run **0/10 with `regressed`, exit 1**, ~3 min
warm. Protocol and calibration notes at
`sovereign/handoff/ENRICHMENT_CANARY_DEMO.md`; script at
`scripts/enrichment-canary.sh`.

**Gate 2 — faithfulness rate reported per corpus. PARTIAL.**
The lane is wired into CI as a TRACKED run plus a HARD gate twin
(`scripts/sovereign-ci-bench.sh:360-397`), and one baseline is committed
(`sovereign/bench/faithfulness/baselines/chaos-secret-agent/latest.json`,
unsupported-claim rate 0.4848). But it runs for **one** corpus and is
skipped entirely when no RAPTOR tier exists, so "per corpus" is not yet
true. The rate is also computed to stdout only — no persisted,
daemon-readable copy exists, so no user or CLI surface can show it.
Carried forward, not silently closed.

**Gate 3 — chaos double-gate. RESTATED, then MET within tolerance.**
The original gate (competence ≥ 0.71 AND honesty ≥ 0.82) was retired by
the operator on 2026-08-01: the 0.71 figure came from a retired
24-probe bank, from the contaminated arm of the 2026-06-17 A/B, on an
unrecorded model. The replacement bar is the manifest red-line, 0.60.

Measured on the model actually in production
(`sa_Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`): honesty **1.00**, hallucination
**0.00**, grounding fidelity **0.917**, competence **0.59375** (19/32).

Competence sits **0.00625 below a bar whose own recorded `tolerance` is
0.15** — i.e. the gate is being decided two orders of magnitude inside
its measurement's noise floor, where a single probe flips the verdict.
**Recorded as met-within-tolerance.** Spending a bench night to buy one
probe would be measuring the sampling error, not the pipeline. The real
follow-up is bank resolution, not tuning — routed to T2's Step 0.

**Gate 4 — both workspace gates exit 0.** Held at each T1 landing
(last: lint `--full` 0 errors, tests 8936 pass / 0 fail).

### P1.4 verification — a negative result worth recording

A suspected defect was investigated and **did not exist**. The concern:
`runtime/handlers/synthesis_common.rs:125` passes an empty
`chunk_sources`, which the gate reads as all-Leaf — which would make
P1.4's Leaf/Summary policy inert wherever that builder is used.

It is not. All four retrieval-shaped answer paths thread
`gate_evidence_with_sources`: `runtime/streaming.rs:1423` (KQ),
`streaming.rs:2631` (Deep), `handlers/knowledge_query.rs:1542`,
`handlers/simple.rs:180`. The empty-vec builder is
`transcript_gate_evidence` (`synthesis_common.rs:111`), and its only two
callers — `handlers/complex_task.rs:305` and `handlers/attached_doc.rs:662`
— pass **tool-result transcripts, not retrieved chunks**, where all-Leaf
is correct by construction and documented as such at the definition.

P1.4 is live on every path it should be. Recorded so the next reader of
that empty `Vec::new()` does not re-open the same question.

## How to re-measure

Every number above is a `grep`-level census against a stated predicate.
Re-run at tranche exit and append a row — do not edit prior rows.

```bash
# 1. systems — enrichment_type dispatch arms
grep -n 'enrichment_type ==' corpus-engine/src/engine/ingest.rs

# 2. stores — SQLite side
grep -oE "CREATE TABLE IF NOT EXISTS [a-z_]+" \
  sovereign/crates/sovereign-store/src/migrations.rs | sort -u

# 5. knobs — registered, then the grandfathered remainder
grep -n 'name = "SOVEREIGN_' quality/env-flags.toml
grep -iE "raptor|atlas|cluster|ppr|enrich|gliner|concept" \
  quality/baselines/env_unregistered.txt
```

A number that cannot be produced by a command belongs in this file with
the command that produces it, or it is an estimate again.
