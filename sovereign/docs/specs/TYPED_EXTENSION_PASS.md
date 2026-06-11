# Spec: Typed-Extension LLM Pass over RAPTOR Cluster Summaries

**Status:** SHIPPED (2026-05-24, same push as the vault tiered
port — commit a0535d88). Implementation:
`sovereign-tools/src/typed_extension/` (manifest + two-pass
orchestrator + tests), wired into
`FolderTieredProvider::finalize_corpus` after `run_vault_synthesis`;
operator re-run surface `sovereign atlas typed-extension <corpus>`
(`sovereign-cli-llm/src/atlas_cmd/typed_extension.rs`). The
2026-06-07 obsidian bench baseline scores all five argumentative
axes non-zero against its `atoms.json`. Spec retained per the
lifecycle clause below for the rationale's forensic value.
(Status banner added 2026-06-10 — the spec sat at "design only"
for two weeks after the code shipped, and downstream docs kept
promising the pass as future work.)
**Targets:** v2 of the vault tiered port
(`sovereign notes --query vault-tiered-port-2026-05-24`; plan at
`~/.claude/plans/let-s-get-into-raptor-wise-bachman.md`).
**Lifecycle:** When shipped, the runtime surface lands in
[`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md); this spec
either retires here with a Shipped status banner (forensic value
of the rationale) or moves to `docs/archive/` (low forensic value).
Decision at ship time.

**Prerequisites for reading:** [`../TIERED_RETRIEVAL.md`](../TIERED_RETRIEVAL.md)
(tier architecture); [`PROGRESSIVE_ENRICHMENT.md`](PROGRESSIVE_ENRICHMENT.md)
(RAPTOR + GLiNER layering); [`CONV_TIERED_PORT.md`](CONV_TIERED_PORT.md)
(conv-tiered persistence schema).

## Why this exists

When the vault tiered port shipped (2026-05-24), it replaced the legacy `obsidian_atlas` Phase-1+ pipeline with the per-source-doc RAPTOR + GLiNER + vault-wide synthesis surface. The replacement was a documented **Path-C** trade: 5 of 9 obsidian bench axes (`mechanism`, `named_position`, `evidence`, `opposition`, `concession`) dropped to ~0 F1 because the tiered output writes to SQLite sidecars (`conv_raptor_nodes`, `chunk_entities`, `vault_themes`), not to the literary-pipeline `atoms.json` that the bench scorer reads.

The user accepted the trade in exchange for faster ingest, milestone-based UX, incremental updates, and avoiding the doubled LLM spend of running both pipelines. The trade was explicitly **reversible via this spec**: a typed-extension LLM pass over RAPTOR cluster summaries that produces atom-shaped output compatible with the bench's `atoms.json` reader, at materially lower cost than literary_atlas's per-section calls.

**The core insight:** RAPTOR cluster summaries are already concentrated, distilled prose at one LLM call per leaf. Running one *more* LLM call per leaf to type-extract atoms from that summary is roughly **3× cheaper** than literary_atlas's per-section extraction over the raw text (which makes ~5 calls per section across Phases 1, 1b, 3, 5, 6). The summary is a better-fitting input for typed extraction than raw prose — it's already paraphrased a section's argument into one passage.

## Where it fits in the tiered paradigm

T1 (embed) → T2 (GLiNER + per-note entity graph) → T3 (per-note RAPTOR + motifs) → **cross-source synthesis** (vault_themes; this spec extends).

The pass runs inside `FolderTieredProvider::finalize_corpus`, *after* `run_vault_synthesis` produces `vault_themes`. Both layers depend on `conv_raptor_nodes`; both write to corpus-scoped sidecars; neither blocks per-source enrichment (which is the load-bearing retrieval surface). The execution order matters because the typed-extension pass uses `vault_themes` as Pass-B input (cross-leaf scope).

```
FolderTieredProvider::finalize_corpus:
  1. run_vault_synthesis     →  vault_themes              [shipped 2026-05-24]
  2. run_typed_extension     →  atoms.json                [this spec]
```

No new `AssetState` variant. Typed atoms are a bench-side concern; no chat-side surface (briefing, rerank) reads them in v2. Adding a T4 state would force every consumer to be aware of a transition with no chat-path payoff. The atoms.json file materialises when ready; the bench scorer reads-or-skips on file presence.

## Two-pass extraction shape

A single grammar over a single LLM call per leaf doesn't fit all five atom kinds. Mechanism/position/evidence are leaf-local — they live in one passage's argument. Opposition and concession are cross-leaf — "X vs Y" spans clusters; "the author grants Z but then narrows it" spans a position and its qualifier. Forcing both shapes into one input either over-calls (per-pair leaves) or under-recalls (one collapsed summary).

**Pass A — per-leaf extraction**, fires once per level-0 RAPTOR leaf:
- Input: `conv_raptor_nodes.summary + primary_entities` for one leaf
- Output: `{mechanisms: [...], named_positions: [...], evidence: [...]}`
- Volume: ~70 calls on a 23-non-tiny-note vault (avg ~3 leaves/note)

**Pass B — per-vault-theme extraction**, fires once per `vault_themes` row:
- Input: `vault_themes.summary + member_source_doc_ids` (with optional per-member leaf summaries for grounding)
- Output: `{oppositions: [...], concessions: [...]}`
- Volume: ~3-20 calls per vault

**Total v2 cost:** ~80-90 LLM calls per vault vs literary_atlas's ~250. Per-leaf input is small (one summary, ~500 tokens) so prefill is cheap; output schema is small so generation is short (~200-400 tokens per call). Wall-clock estimate at Slow slot ~10s/call: **~15 min for a 50-note vault**, ~5x faster than literary_atlas full Phase 1+ on the same input.

Tiny-bucket notes (synthetic single-node RAPTOR) are skipped from Pass A. They carry no real cluster summary — their "summary" is the chunk title. Extraction would mostly produce nothing.

## Atom shapes

Mirrors the golden's expected_*_atoms semantics (`sovereign/bench/obsidian/golden.toml`). All five kinds use llguidance Lark grammars (project pattern per [[project_llguidance_readoption_plan]]) so the output is structurally valid before scoring.

```rust
struct MechanismAtom {
    name: String,              // "spread pricing", "salary cap"
    domain: Vec<String>,       // ["economics", "pharmacy"]
    description: String,
    supports_position: Option<String>,  // optional ref to named_position.name
}

struct NamedPositionAtom {
    name: String,              // "tragedy of the commons thesis"
    content: String,           // load-bearing paraphrase
    proponent: Option<String>, // "Hardin", "the essay"
    stance: Stance,            // Endorse | Rebut | Neutral
}

struct EvidenceAtom {
    label: String,             // "$1.4B FTC PBM spread income"
    content: String,
    kind: EvidenceKind,        // Figure | CaseStudy | HistoricalExample
    supports: Vec<String>,     // refs to mechanism/position names
}

struct OppositionAtom {
    left: String,              // "markets"
    right: String,             // "regulation"
    axis: String,              // "governance / commons allocation"
}

struct ConcessionAtom {
    content: String,           // "PBMs do provide some intermediation value"
    addresses: String,         // ref to position name being conceded
    outcome: Outcome,          // Intact | Narrowed | Abandoned
}
```

The `supports` / `addresses` cross-references are *string names*, not atom IDs. Resolution happens at score-time inside the bench, matching how literary_atlas's atoms.json already works. Forward-reference is fine — `evidence.supports="spread pricing"` doesn't need the mechanism atom to exist yet; the bench resolves via name fuzzy-match.

## Grammar — one envelope per pass

Single envelope per pass, NOT one grammar per atom kind. Per-kind grammars would need 3-5 LLM calls per leaf (one per kind) → 3-5× the cost with no precision gain. One envelope per leaf lets the model decide which kinds to populate (often only 1-2 are present).

Pass A envelope:
```lark
start: leaf_atoms
leaf_atoms: %json {
    "type": "object",
    "properties": {
        "mechanisms":      {"type": "array", "items": #MechanismAtom},
        "named_positions": {"type": "array", "items": #NamedPositionAtom},
        "evidence":        {"type": "array", "items": #EvidenceAtom}
    },
    "additionalProperties": false
}
```

Pass B envelope: same shape but `oppositions` and `concessions` only.

Each call returns valid JSON (llguidance enforces); empty arrays are valid; the parser drops empty kinds before persisting.

## Persistence — reuse `atoms.json` path

Write to `~/.sovereign/indexes/{corpus_id}/atlas/atoms.json` in the literary_atlas-compatible shape. Two reasons:

1. **Bench scorer needs no changes.** `sovereign enrich eval` reads `{corpus_id}/atlas/atoms.json`. The new path produces a file the existing scorer reads correctly.
2. **Atlas viewer (desktop) works unchanged.** Same path, same JSON shape; the desktop's atlas browser stays compatible.

The alternative — a new `vault_typed_atoms` SQLite table — doubles the persistence surface for one consumer (the bench) and forces parallel maintenance of two atom stores. Filesystem keeps everything aligned with literary's convention.

Atom IDs are **content-addressed**: `blake3({kind}\n{canonical_name}\n{summary_fragment[:64]})`. Matches the user's recent `migrate_atlas_ids` decision (content-hash IDs, not sequential). Dedupe across re-runs is automatic; if a leaf's summary doesn't change, the typed atoms it produces get the same IDs and the next write is a no-op.

The atoms.json file carries a sidecar manifest at `atoms.meta.json`:
```json
{
  "schema_version": 1,
  "produced_by": "tiered_typed_extension_v1",
  "raptor_nodes_hash": "<blake3 over all (node_id, summary_embedding_hash) leaves>",
  "vault_themes_hash": "<blake3 over theme summaries>",
  "extracted_at_unix": 1779636073,
  "pass_a_calls": 67,
  "pass_b_calls": 3,
  "atoms_per_kind": { "mechanism": 21, "named_position": 14, "evidence": 8, "opposition": 4, "concession": 6 }
}
```

The manifest gates re-extraction. `finalize_corpus` reads it; if `raptor_nodes_hash` matches the current set, skip extraction (already done for this RAPTOR state). On any per-note incremental re-enrich, the hash changes → re-extract.

## Idempotency + incremental re-enrich

The watched-folder sweeper hooks (per the vault tiered port) call `engine.reindex_changed_sources_tiered(corpus_id, changed_source_doc_ids)` after `apply_watched_diff`. That re-runs per-doc enrichment for changed notes, then calls `finalize_corpus`, which calls `run_typed_extension`.

Re-extraction strategies, ordered by cost vs freshness:

- **Full re-extract (current spec):** on any change, the manifest's `raptor_nodes_hash` differs → extract every leaf again. Simple, slow for small edits on big vaults.
- **Per-leaf delta (v3+):** maintain per-leaf hash; only re-extract leaves whose summary changed. ~5× cheaper for typo-style edits.

v2 ships the full re-extract path. v3 adds per-leaf delta when the cost actually hurts.

## Failure modes

**RAPTOR summarisation strips load-bearing vocab.** The Conrad failure mode flagged in `TIERED_RETRIEVAL.md` §"On HippoRAG 1 vs 2" — `frail` becomes `psychologically fragile` in the cluster summary — limits what typed-extraction can recover. The mechanism is "spread pricing", but if RAPTOR summarised it as "the practice of buying drugs cheap and billing payers more", `name="spread pricing"` won't extract. Mitigation: tune the RAPTOR per-leaf summarisation prompt to preserve distinctive verbatim phrases. This is a cross-cutting fix that benefits other consumers too (motif extraction, vault_themes naming).

**Cross-leaf concessions split.** A position in one leaf, its concession in another → Pass A sees only half. Pass B over vault_themes catches some (themes span notes by construction); the residual gap is real. Per-note overview as an *additional* Pass-A variant could help — operating on the whole-note summary rather than per-leaf — but adds ~46 more LLM calls. Defer to v3 if bench shows the concession axis stays below 50%.

**Per-leaf extractor over-recalls common nouns.** "Markets" as a position name vs "markets" as a noun in a different argument. Mitigation: the grammar's `name` field uses a regex disallowing 1-2-word generic phrases UNLESS they're capitalised proper nouns. Calibration via golden's forbidden_named_position_atoms anti-tests.

**Massive vaults.** 500-note vault → ~1500 leaf calls × 10s = ~4h on Slow slot. Mitigations: (1) route Pass A to Fast slot (loses some precision; A/B per-vault); (2) batch multiple leaves per call when their summaries fit in one prompt; (3) gate behind `SOVEREIGN_TIERED_TYPED_EXTRACTION=1` env var for opt-in on large vaults. The current vault (46 notes, 67 leaves) is well below the threshold where this matters.

## Bench impact prediction

Predicted per-axis F1 vs literary_atlas baseline (aggregate 86.7% on 2026-05-24 capture):

| Axis | Literary baseline | Tiered v1 (today) | Typed-ext v2 predicted | Why |
|---|---:|---:|---:|---|
| person | 100% | 100% (GLiNER) | 100% | unchanged |
| event | 80% | 80% (GLiNER) | 80% | unchanged |
| concept | 66.7% | 66.7% (RAPTOR primary_entities) | 75-85% | leaf summary names concepts more often than primary_entities array |
| mechanism | 80% | 0% | **75-85%** | leaf summary directly names mechanisms |
| named_position | 85.7% | 0% | **70-80%** | leaf paraphrases positions, but proponent attribution may suffer |
| evidence | 75% | 0% | **55-70%** | dollar figures + dates need to survive RAPTOR's summariser |
| opposition | 66.7% | 0% | **50-65%** | cross-leaf; Pass B helps but loses leaf-level grounding |
| concession | 80% | 0% | **40-60%** | hardest; needs both halves of "X, but Y" in same input |

**Aggregate v2 prediction: 65-75%.** Won't fully match literary 86.7% — leaves are summaries (lossy) vs literary's raw-section extraction — but recovers most of the lost ground at ~⅓ the LLM cost. The gap between v2 and literary is the upper bound on what RAPTOR's summarisation strips out, which is itself an optimisation surface.

## Open design questions

1. **Pass A on per-note overview instead of per-leaf?** Per-note (one summary per note, ~46 calls) catches concessions within a note (both halves in one input) at the cost of mechanism specificity (multiple mechanisms collapsed into one note-level summary). Pass A could be ~50/50: per-leaf for `mechanism + evidence`, per-note for `named_position + concession`. Costs an extra ~46 calls.

2. **Should typed atoms surface in chat briefing?** v2 says no (bench-side only). v3 could surface mechanism/position atoms as a "Vault arguments" briefing block alongside vault_themes when the user asks about specific mechanisms ("what does my vault say about regulatory capture?"). Tracked under "Chat-surface typed atoms" as future work.

3. **Fast vs Slow slot for Pass A?** Slow gives best precision; Fast cuts cost ~3× but typed extraction precision matters more than mechanism's typical Fast-slot use case (e.g. router). Default Slow; expose `SOVEREIGN_TYPED_EXT_SPEED=fast` for operators with cost ceilings.

4. **Skip Pass B entirely if vault_themes is empty?** vault_themes synthesis skips below `MIN_LEAVES_FOR_SYNTHESIS=8` (sparse vaults). When themes are empty, Pass B has nothing to run on → opposition + concession axes stay 0. Acceptable for v2 — these axes don't matter on a sparse vault anyway.

## Reuses (do NOT redo)

- `conv_raptor_nodes` table — Pass A input source
- `vault_themes` table — Pass B input source
- llguidance grammar pattern — structured output, prior art in RAPTOR summarisation
- `~/.sovereign/indexes/{id}/atlas/atoms.json` path — matches literary_atlas, bench scorer reads it
- `FolderTieredProvider::finalize_corpus` — natural insertion site
- `build_atlas_artifacts_with_checkpoint` patterns — input-hash-based idempotency (adapt for Pass A/B manifest)
- Content-addressed IDs — per `migrate_atlas_ids` decision in sovereign notes

## What v2 does NOT need to add

- New SQLite tables (`vault_typed_atoms` is unnecessary — write filesystem `atoms.json`)
- New `AssetState` variant (T4 — typed atoms are bench-side, no chat-side state need)
- New CLI surface (atoms.json appears in conventional path; `sovereign enrich eval` already reads it)
- New retrieval rerank (chat path doesn't read typed atoms in v2)
- New briefing block (chat synthesis path stays on per-note signposts + vault_themes only)

## Reference implementation files (v2 scope, ~700 LOC estimate)

| File | Purpose | Estimated LOC |
|---|---|---:|
| `sovereign-tools/src/typed_extension/mod.rs` | Module entry — `run_typed_extension(corpus_id, store, inference) -> Result<TypedExtractionReport>` | ~200 |
| `sovereign-tools/src/typed_extension/grammar.rs` | Lark/llguidance grammars + JSON schema definitions for Pass A and Pass B envelopes | ~150 |
| `sovereign-tools/src/typed_extension/atoms_writer.rs` | Project typed atoms onto literary_atlas atoms.json shape + write to `{index_dir}/atlas/atoms.json` + `atoms.meta.json` sidecar | ~100 |
| `sovereign-tools/src/conv_tiered_provider.rs` | Extend `FolderTieredProvider::finalize_corpus` with Pass A + Pass B invocations | ~80 |
| `corpus-engine/src/enrichment/tiered.rs` | (no changes — `finalize_corpus` hook already exists) | 0 |
| Tests | per-axis grammar correctness, idempotency via manifest hash, write-then-read round-trip | ~200 |

## Verification

Gating check: re-run `sovereign bench obsidian` against the post-v2 corpus. Acceptance:

- **5 argumentative axes recover to ≥ 50% F1** (was 0% post Path-C; literary baseline was 66.7-85.7%). Aggregate target: ≥ 65%.
- **Surviving axes (person/event/concept) stay ≥ baseline − 0.05.** Don't regress what already worked.
- **`forbidden_*` axes stay at 0 FPs.** Path-C's non-negotiable bound holds — the typed-extension grammar must reject "the author" as a Person, "2024" as a date-shaped Person, etc.

Bench A/B captured under `baselines/obsidian-vault/typed-ext-post-{git-sha}.json` for comparison against the v1 capture at `baselines/obsidian-vault/synth-post-vault-port.json` (sources 75%, judge facts 68%).

Wall-clock target: typed extraction ≤ 50% of the vault's per-note enrichment time (already ~6 min for the user's 50-note vault). Total finalize_corpus budget ≤ ~9 min.

## Decision log

| Date | Decision | Why |
|---|---|---|
| 2026-05-24 | Single envelope per pass, not per-kind grammars | 3-5× cheaper; output validity unchanged |
| 2026-05-24 | Two passes (per-leaf A + per-theme B) instead of single per-leaf pass | Concession + opposition span clusters; per-leaf alone misses them by construction |
| 2026-05-24 | Write to filesystem `atoms.json` not new SQLite table | Zero changes to bench scorer + atlas viewer; matches literary_atlas convention |
| 2026-05-24 | Content-addressed atom IDs via blake3 | Idempotent re-runs; aligns with `migrate_atlas_ids` content-hash decision |
| 2026-05-24 | No T4 state variant | Bench-side concern; chat-side consumers unaffected; avoids state-machine plumbing |
| 2026-05-24 | Default Slow slot for Pass A | Precision-bound; Fast slot is opt-in for cost-ceiling operators |

## Related

- Spec inheritance: extends `CONV_TIERED_PORT.md` (per-source RAPTOR) and `PROGRESSIVE_ENRICHMENT.md` (GLiNER + RAPTOR layering)
- Vault port plan: `~/.claude/plans/let-s-get-into-raptor-wise-bachman.md`
- Vault port memory: `~/.claude/projects/.../memory/project_vault_tiered_port_2026_05_24.md`
- v1 baseline scores: `baselines/obsidian-vault/{retrieval,synth}-post-vault-port.json`
- HippoRAG vocab-fidelity discussion: `sovereign/docs/TIERED_RETRIEVAL.md` §"On HippoRAG 1 vs 2"
- Literary atoms.json shape (reference): `corpus-engine/src/enrichment/pipeline/pipelines/literary_atlas.rs` writers
