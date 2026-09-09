# PRE-REG: prompt-climbing the obsidian enrichment golden

Written 2026-09-09, before any climb data. Pattern:
`research/agentic-variation/README.md` — pre-registration is the honesty
mechanism; the bars below are fixed before the first run.

## Claim under test

Prompt climbing — the generalized solver over prompt-overlay files with
the enrichment golden as a `counts:` checker — moves a production
enrichment number that manual tuning left on the table, WITHOUT
overfitting the golden (transfer arm holds). Secondary: the climb's
receipts name which golden entries resisted, which is the steering
surface for the per-recipe product later.

## Why this golden (room to climb)

Baseline 2026-09-09, `sovereign/bench/obsidian/baselines/golden/latest.json`:

| axis | matched/expected | named misses |
|---|---|---|
| concept | **6/12** | tragedy of the commons, common-pool resource, spread pricing, beyond GDP, … |
| opposition | **2/4** | towers-in-a-park vs short blocks, competitive balance vs dynasty |
| evidence | 3/5 | Ohio $224.8M, Pruitt-Igoe |
| mechanism | 4/6 | spread pricing, land value tax |
| named_position | 3/4 | capture thesis |
| concession | 2/3 | PBMs do provide |
| person (guardrail) | 4/4 | — saturated; must not regress |

Headroom on every typed axis; a saturated axis as the no-regression
guardrail; misses already named by the scorer (the rung feedback
exists for free).

## Arms

- **Climb**: obsidian golden (above).
- **Transfer (overfit detector)**: `literary/bk-book-1` — event 2/5,
  state 2/3 have room; climb prompts must NOT be shared with the
  literary pipeline, so the transfer arm measures whether obsidian
  gains come from generic extraction skill or from fitting the bank.
  Bar below.
- **No-touch**: wessex-hoard ontology-reach (`truth.json`) — the
  per-recipe product demo, NOT the climb target: its acceptance
  passed, so it is likely near ceiling (exactly the "golden without
  room" trap). Step 0 verifies; if it is NOT at ceiling it becomes the
  v2 ontology-climb target (ontology as artifact).

## Design

- **Artifact / workdir**: the argumentative pipeline's prompt-overlay
  directory as its own git repo (`SOVEREIGN_PROMPT_DIR` already
  selects overlays at enrich time). Candidate snapshots are
  prompt-file-only — cheap. `literary_atlas` is peer-hot right now;
  v1 touches only the obsidian-serving prompts.
- **Checker**: one adapter script, `counts:`-prefixed:
  `SOVEREIGN_PROMPT_DIR=<candidate> sovereign enrich build obsidian-vault
  && sovereign enrich eval obsidian-vault --golden …` → per-golden-entry
  `PASS obsidian.concept.<name>` / `FAIL <name>` lines; every
  `forbidden_hit` a `FAIL guardrail.forbidden` entry; over-extraction
  rungs (`unmatched_count ≤ N` descending). Guardrails vacuous-pass
  red-when-violated, rungs red at baseline — the probe's ladder
  discipline verbatim.
- **Serial candidates** (Playwright profile precedent): the corpus
  store under `~/.svrnmesh` is shared mutable state OUTSIDE the
  candidate snapshot — parallel candidates race it. K=3, serial.
- **Regime**: nightly/AVO shape. Full-corpus re-enrich per candidate;
  round-0 wall-clock is measured and if a round exceeds 45 min the
  pilot drops to a chunk-subset climb (adapter then scores only
  golden entries whose chunks are in the subset — named, not silent).

## Bars (fixed now)

1. **Primary**: concept ≥ 9/12 AND forbidden_hit = 0 AND person stays
   4/4 AND no climbed axis below its baseline.
2. **Transfer**: bk-book-1 event ≥ 3/5 AND no bk axis more than 1
   below its baseline. Failing transfer with primary passed = the
   climb overfit the bank — the result is reported as such, not as a
   win.
3. **Honesty**: any stall ships its receipts (which entries resisted,
   what the candidates tried). No silent bar adjustment.

## Named risks

- **Keyword-gaming**: the golden matcher scores name +
  `description_keywords_any` — a climb can learn to sprinkle keywords.
  Detectors: the transfer arm, forbidden entries, and the
  over-extraction rungs. If receipts show keyword-stuffing shapes,
   that is a finding about the MATCHER, not the model (§18: validate
  the instrument).
- **Corpus-store races**: serial candidates mitigate; if a round's
  artifacts look interleaved (impossible scores), suspect the store.
- **Live-golden drift**: the obsidian golden tracks a real vault;
  a mid-pilot vault edit can move entries. Freeze the vault for the
  pilot's duration or re-baseline before judging.

## Step 0 (before any climb)

Confirm: (a) which pipeline/schema serves the obsidian axes
(argumentative via `enrich eval` — verify the prompt dir name),
(b) wessex-hoard's current truth score (ceiling check), (c) round-0
wall-clock on the full vault.
