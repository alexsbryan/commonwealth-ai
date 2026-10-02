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

Two stages (operator direction 2026-09-09: prove the mechanism with
the tightest possible iteration loop, then challenge it on the larger
holdout when confident). Stage bars are separate and the v0 result is
reported as mechanism-evidence, never as a quality claim.

### v0 — the tight loop (mechanism proof)

- **Slice, not vault**: one root essay (the Ostrom Summary — it
  carries the concept misses "tragedy of the commons" and
  "common-pool resource") re-ingested into a scratch corpus
  `climb-ostrom`. Golden-subset TOML = the existing obsidian golden's
  entries for that essay, copied verbatim — no re-authoring, no new
  judgment.
- **Round cost target**: one essay ≈ a handful of chunks; K=3 serial
  candidates ≈ minutes per round. If a round exceeds 10 minutes the
  slice is still too big — shrink, don't subsidize.
- **v0 bar (mechanism, not quality)**: at least one golden miss flips
  to matched via monotone promotion; zero forbidden hits; receipts
  show the winning candidate's prompt diff. Fail mode worth having:
  a stall whose receipts name the entry — that is the steering
  surface working, and gets reported as such.

### v1 — the vault challenge (only after v0)

- Full obsidian vault, the original bars below, bk-book-1 transfer
  arm, over-extraction rungs. v0's slice is NOT part of v1's
  measurement (it was climbed on).

### Common mechanics (both stages)

- **Artifact / workdir**: the argumentative pipeline's prompt-overlay
  directory as its own git repo (`SOVEREIGN_PROMPT_DIR` already
  selects overlays at enrich time). Candidate snapshots are
  prompt-file-only — cheap. `literary_atlas` is peer-hot right now;
  v1 touches only the obsidian-serving prompts.
- **Checker**: one adapter script, `counts:`-prefixed:
  `SOVEREIGN_PROMPT_DIR=<candidate> sovereign enrich build <corpus>
  && sovereign enrich eval <corpus> --golden …` → per-golden-entry
  `PASS obsidian.concept.<name>` / `FAIL <name>` lines; every
  `forbidden_hit` a `FAIL guardrail.forbidden` entry. Guardrails
  vacuous-pass red-when-violated, rungs red at baseline — the probe's
  ladder discipline verbatim.
- **Serial candidates** (Playwright profile precedent): the corpus
  store under `~/.svrnmesh` is shared mutable state OUTSIDE the
  candidate snapshot — parallel candidates race it. K=3, serial.
- **Regime**: nightly/AVO shape for v1; v0 is interactive.

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

## Wall ledger (v0, live runs 2026-09-09/10)

Fixed engine-side, committed: renderer/parser/prompt generalization
(3b4439b3e), budget split (55758db48), counts contract (1d7085a3c),
instrument profile serial+600s + artifact default target (2026-09-09
commits). Fixture-side, fixed: destructive-reset checker (cached
extraction masqueraded as ties — 31s runs vs 1:34 real).

**WALL 7 — OPEN, blocks the flip.** The model has drafted the correct
surname-resolution rule five times; no emission channel can land it on
a 308-line prompt file:

1. `write_file` refuses files >150 lines (anti-corruption threshold —
   right for code, wrong for prompt files).
2. Pathless `patch_lines`/`insert_before` route to `default_target` =
   alphabetically-first artifact (README.md) — a SILENT SUBSTITUTION
   (§18.3) that dies out-of-range. Wall-5's fallback was half right.
3. Patch actions accept a path but the trial prompt never documents
   it, so the model never sends one.

Repair (one coherent change): pathless patch/insert on artifact
workdirs REFUSES naming the fix instead of substituting; the
`src`/path field on patch actions is documented in trial_prompt.md;
and — separate hygiene bug from the same runs — a tie-promote landed
line-number-anchor content (`N:` prefixes) into a prompt file despite
the content check, so the anchor-prefix check must also run on
PROMOTED bodies, not only on apply-input.

Receipts for all three live in the 2026-09-10 session (job
93740be1…): the `<suite error>` tests_before in its final result is
the corrupted-terse base refusing to build — the checker working.

## Step 0 (before any climb)

- (a) **Pipeline — CONFIRMED**: `philosophy_atlas` serves the obsidian
  typed axes (argumentative shibboleth, philosophy_atlas.rs:656).
  Climb surface: `philosophy_atlas_prompts/` (14 files) — not
  peer-hot; `literary_atlas` untouched.
- (b) wessex-hoard ceiling check — deferred to v2 scoping (does not
  gate v0).
- (c) **Slice — CONFIRMED**: `~/Documents/Obsidian Vault/Ostrom
  Summary.md`, 6,340 bytes, local; carries the concept misses
  "tragedy of the commons" and "common-pool resource".
