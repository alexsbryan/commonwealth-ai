# GAP-2 — the corroboration floor (two-source rule as a verdict dimension)

Order deep-research-t1b. Spec: `sovereign/docs/specs/DEEP_RESEARCH.md`
("GAP-2 — Corroboration", lines ~414-426). FMEA row: F22 (near-duplicate
inflation). Gate bars: `dr-corroboration` in quality/initiative-bars.toml.

This note is the design record — written BEFORE the red-first test, per the
order's sequencing (design → red → implement → regenerate → pre-register →
execute).

## The rule

A claim may pass only if its supporting evidence spans **at least two
distinct provenance origins**. A claim whose support set has one origin —
one document, one planted source, five copies of one page — caps at
could-not-judge. Coverage counts distinct origins, never chunks (F22).

## Where the floor lives

`assess_claim` (audit.rs), the composed gate, **after the custody veto and
after the witness downgrade** — i.e. immediately before the fall-through to
Passed:

1. empty window → never-ran
2. judge → none → could-not-judge
3. violation (prob ≥ tau) → failed
4. containment witness
5. custody veto (R-3): all supporting chunks unknown provenance → refuse
6. witness downgrade: all specifics absent → could-not-judge
7. **corroboration floor: supporting chunks span <2 distinct origins →
   could-not-judge** (new)
8. passed, with C-class located citations

Ordering rationale: the floor only fires when the witness found support
(specifics present, custody known). It sits AFTER the witness downgrade so
the witness's own, more specific reason (all-absent / contradicted
negation) is never masked by the floor's generic one. It sits BEFORE the
pass so no single-origin claim can fall through to CitationGrounded.

## Origin extraction — C-class, deterministic

The origin of a supporting chunk is its `source_url` (`AuditChunk.source_url`).
Distinct origins = distinct source_urls among the supporting chunk ids.
No model, no embeddings — this is the C-class floor the spec demands
("origin extraction + counting, deterministic").

v1 note: the spec's "derivation DAG's distinct components" is the T2
refinement — in v1 the provenance component IS the source URL; the
derived-vs-primary tag and DAG-join shapes stay F5/F14/F15 Named (their
T2 wire is the enrichment regime, not this floor). A derived chunk in v1
enters the window with its own source_url, so it counts as its own origin
until the T2 lattice join exists. Recorded, not silent.

## The record — verdict-visible on the final claim

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CorroborationRecord {
    pub origins: Vec<String>,   // the distinct source_urls, sorted
    pub support_chunks: usize,  // how many chunks carried support
    pub floor: usize,           // 2 — the floor, constant, on the record
    pub passes_floor: bool,     // origins.len() >= floor
}
```

- `ClaimAudit.corroboration: Option<CorroborationRecord>` — the gate's
  accounting, on EVERY audit that reaches the floor (both the cap and the
  pass carry the record — the record is the gate's own answer, not a
  defect report).
- `ClaimVerdict.corroboration` (serde default) — flows to gap-list rows.
- `FinalClaim.corroboration` (serde default) — verdict-visible on the
  verdict-set rows, per spec ("verdict-visible on the final claim").
- New `GateAction::CorroborationFloor` → wire `"corroboration_floor"`.
  Deliberately NOT in `is_refusal()`: the floor is a cap, not a refusal —
  the R-3 reds (abstained_* / refused_*) stay untouched.
- Report flag for a floor-capped claim: "open question: single-origin
  support (corroboration floor)" — the reader sees WHY the claim is open,
  not a generic could-not-judge.

## Invariants

- **Downgrade-only.** The floor never upgrades a verdict. A claim whose
  support spans ≥2 origins passes exactly as it would have before; the
  record's `passes_floor: true` is added, nothing else changes.
- **Empty support is not corroborated.** If the witness located no
  supporting chunks, support set = 0 origins < 2 → the floor caps.
  (In practice the all-absent downgrade fires first with its more specific
  reason; the floor is the backstop, not the primary.)
- **The record is on the claim, not in the reason string.** The reason
  stays parseable; the structured record carries the counts.

## Gym deck — F22 flips to Watched

F22 is Named today ("watched by the GAP-2 corroboration fixtures"). With
the floor landed it becomes Watched, with a fixture `f22_corroboration_floor`
in FIXTURES (the f_table coverage test enforces the pairing):

- a deck whose plant surface is one origin, claim passes today →
  could-not-judge after the floor (single-origin downgrade);
- two chunks from one document → could-not-judge (chunk count is not
  corroboration — F22's exact shape);
- two chunks from two documents → passes unchanged (floor satisfied).

## Golden regeneration — deliberate

`run-meridian-1`'s goldens were recorded when single-chunk support passed.
The floor is a pure function of the recorded windows + evidence ids, so
the regeneration is deterministic (re-render, never re-run):

- verdict-set.json: 4 passed → could-not-judge (each cited 1 chunk,
  1 origin), each with a CorroborationRecord {origins: [1], support_chunks:
  1, floor: 2, passes_floor: false}; the 1 existing open stays open.
  passed 4 → 0, open 1 → 5.
- report.md: the 4 flags become the single-origin flag; re-render pinned.
- manifest.json: not_covered 1 → 5.
- golden_fixtures.rs assertions updated to the new counts; re-render
  byte-pins the new report.

run-reframe-1 / run-align-* are untouched: their windows are empty for the
claims that matter (round-2 reframe / pre-acquisition redirect) or the
golden re-render path reconstructs audits that stay could-not-judge —
verify, don't assume: any golden claim that previously PASSED with support
must be checked against the floor.

## Adversarial read — §18.6 pre-registration, then execution

The frozen instruments (sub-bank.jsonl + longform-negative.jsonl) run
against the changed gate. The declared shape: every fixture's window is a
single synthetic chunk → single origin → **every judge-supported claim
caps at could-not-judge**; positives that passed before the witness fix
stay capped; downgrades increase, upgrades stay 0. Pre-registration
appended to research/deep-research/adversarial/pre-registration.md BEFORE
any run; the execution read appended after, timestamped.

## Files touched

- sovereign/crates/sovereign-core/src/deep_research/icd.rs — GateAction
  variant, CorroborationRecord, ClaimVerdict + FinalClaim fields
- sovereign/crates/sovereign-core/src/deep_research/audit.rs — floor in
  assess_claim, ClaimAudit field, all constructions, red-first tests
- sovereign/crates/sovereign-core/src/deep_research/render.rs —
  final_claims carries the record, report flag, test constructions
- sovereign/crates/sovereign-core/src/deep_research/gym.rs — F22 Watched
  + f22_corroboration_floor fixture
- sovereign/crates/sovereign-core/tests/golden_fixtures.rs — regenerated
  assertions + reconstructions
- research/deep-research/notes/icd-schemas.md — the record on the wire
- research/deep-research/adversarial/pre-registration.md — §18.6 append

## Verification

- Red-first: `single_origin_support_caps_at_could_not_judge` (+ the
  two-origin pass twin) watched red before implementation.
- Full loop: `sovereign/crates/sovereign-core/tests/gym_deck.rs` —
  poisoned decks + clean twins; the poisoned deck's claims can no longer
  pass on a single planted source.
- Goldens: re-render byte-pins the regenerated meridian report.
- Gates: lint + test full, both exit 0 (toolbox).
