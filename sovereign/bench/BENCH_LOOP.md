# Bench loop — process theory for tuning typed-atom extraction

This document is the reproducible-process companion to `HISTORY.md`. Where
HISTORY records per-campaign findings, this document records the **loop
discipline** that produces those findings: how to set up a bench against a
routed-Phase-1 corpus, how to iterate prompts and scorers without overfitting
to the golden, and how to recognise when a number is a real signal versus a
measurement artefact.

The obsidian-vault bench (closed 2026-05-14 at typed-aggregate F1 51.3 → 77.5
across four iterations) is the worked example throughout. The same shape
applies to any future bench that scores routed Phase-1 typed extensions —
literary, philosophy, engineering, recipe, etc. — once that corpus has a
routed pipeline.

## 1. Why this exists

The literary atlas pipeline tuned cleanly on long-form narrative prose
(*Brothers Karamazov*, *Dubliners*). Heterogeneous personal vaults — essays,
journals, project notes, poetry, recipes — broke it: argumentative essays
saturated the 10-claim cap, journal entries collapsed Narrative + Reflective
into one label, zettel cards extracted as low-density entity blobs.

The fix shipped in the May 2026 MECE rewrite was structural:

- **Phase 0 classifier** emits a *vector* over three orthogonal MECE axes
  (Discourse Mode / Epistemic Posture / Temporal Frame) plus optional
  Audience, not a single label.
- **Phase 1 dispatcher** fans out one chat call per discourse mode whose
  weight clears `DISCOURSE_ROUTING_THRESHOLD = 0.25`. Sections with hybrid
  shape (argumentative + narrative) extract under both schemas instead of
  collapsing into a `Mixed` bucket nobody routes.
- **Per-mode typed extensions** carry atom shapes the literary base schema
  can't express: Mechanism, Position, Evidence, Opposition, Concession
  (argumentative); Definitions, PropertyClaims (descriptive); Tasks,
  Decisions, Blockers (procedural); etc.

The bench loop's job is to keep this machinery honest as prompts and
schemas iterate. Without a measurement substrate, prompt tuning becomes
folklore.

## 2. Architecture the bench measures

```
            ┌─────────────────────────┐
section ───▶│ Phase 0: vector classify│──▶ classifications.json
            │  (4 axes, weights)      │     (one record per section)
            └─────────────────────────┘
                          │
                          ▼
            ┌─────────────────────────┐
            │ Dispatcher: fan-out over│
            │ active_modes ≥ 0.25     │
            └─────────────────────────┘
                          │
                  ┌───────┼───────┬───────┬─────────┬──────┐
                  ▼       ▼       ▼       ▼         ▼      ▼
              arg.rs  narr.rs desc.rs  refl.rs proc.rs lyric.rs
                  │       │       │       │         │      │
                  └───────┴───────┴───┬───┴─────────┴──────┘
                                      ▼
                          questions.json (section_extraction.type_extensions)
                                      │
                                      ▼
                          ┌───────────────────────────┐
                          │ enrich eval — typed scorers│
                          │  mech / position / evid / │
                          │  opposition / concession  │
                          └───────────────────────────┘
                                      │
                                      ▼
                                golden.toml
```

Each surface has a corresponding bench axis. The golden TOML declares
`expected_*_atoms` per axis with substring matchers + optional
discriminators. The scorer walks the Phase 1 cache, finds matching atoms,
and reports precision / recall / F1 per axis plus a notes column for soft
signals.

## 3. The loop, concretely

The measurement cycle has four steps. Each can iterate independently.

```
   ┌─▶ (1) classify  ──▶ (2) extract-typed  ──▶ (3) bench  ──▶ (4) diagnose
   │                                                                │
   └────────────────── adjust prompt / scorer / golden ◀────────────┘
```

### Step 1 — classify (one chat call per section, ~6s on Qwen3.6-35B)

```bash
sovereign enrich classify <corpus> --full [--force]
```

Writes `cache/section_classifications.json` (schema v2: vectors with
`discourse_mode.primary`, `primary_weight`, `secondaries`,
`epistemic_posture`, `temporal_frame`). Idempotent on `content_hash`.

### Step 2 — extract-typed (fan-out one chat call per active mode)

```bash
sovereign enrich extract-typed <corpus> --full [--force]
# or per-section:
sovereign enrich extract-typed <corpus> --chapters sec_00002,sec_00021,...
```

Budget discipline: `TYPED_BUDGET_INITIAL = 4096`, retried at
`TYPED_BUDGET_RETRY = 8192` on parse drift. The retry pattern is what
makes the tight default safe — long sections that overrun the initial
budget get a second attempt without manual intervention. Per-section
output is annotated `mode=N↑` when the retry budget was used.

### Step 3 — bench

```bash
sovereign bench obsidian --report /tmp/<run-id>.json
```

Thin wrapper around `enrich eval` that defaults to the in-repo golden
and pre-flights `cache/questions.json` existence. The bench is what
shows up in `git diff` between iterations — the JSON report is the
diffable artefact.

### Step 4 — diagnose, then loop

This is where the discipline lives. Sections 4–6 below.

## 4. Iteration discipline — what generalises versus what coaches

The single biggest failure mode in bench-driven prompt tuning is
*coaching to the test*: adding a clause to the prompt that lifts F1 by
mentioning the specific atoms the golden expects, rather than teaching
a pattern that surfaces those atoms naturally.

Two heuristics to keep iterations honest.

### Heuristic A — Does the change name golden strings or domain patterns?

| Naming pattern | Generalises | Coaches |
|---|---|---|
| "Positions can be endorsed OR rebutted; lift both stances symmetrically" | ✓ | |
| "When a section contains 'Hardin' and 'tragedy', emit `tragedy of the commons` as a position" | | ✗ |
| "Oppositions and the items inside them co-occur — lift both layers" | ✓ | |
| "Look for 'towers in a park' vs 'short blocks' in urbanism sections" | | ✗ |
| "Each collection captures content the others can't; don't trade between them under budget pressure" | ✓ | |

The generalising rows describe *cognitive patterns* the model can apply
to any argumentative section. The coaching rows describe specific
extractions the golden happens to expect.

When in doubt: state the rule abstractly, illustrate with one example
across two domains (essay AND poetry, or law AND sport). If the second
domain example feels forced, the rule was probably overfit to the first.

### Heuristic B — Does the scorer change soften a real ambiguity, or open a hole?

Three scorer relaxations landed during the obsidian loop. Each one
addressed a documented LLM-extraction ambiguity, not a golden bug:

1. **Mechanism `domain` filter → informational note**. Rule 10b-18 is
   reasonably tagged as "law", "regulation", or "finance" — the
   choice depends on which face of the rule the section emphasises.
   Gating on domain converts every domain-tag disagreement into a
   miss. Fix: keep `domain_contains_any` accepted in the golden but
   only emit a notes-column flag when it diverges.

2. **Position `content_contains_any` → informational note**. The
   LLM paraphrases position content. Golden's `["overgrazing",
   "inevitable", "common-pool", "ruin"]` didn't substring-match the
   model's `"Common property is doomed by the rationality of its
   users…"` — same idea, different vocabulary. Name + stance gate
   F1; content paraphrasing reports as a note.

3. **Concession `outcome` → informational note**. The same passage
   can defensibly read as `intact` (thesis unchanged) or `narrowed`
   (claim scope reduced) depending on how the reader weighs the
   surrounding section. Outcome judgment requires reading
   beyond the concession itself; gating on it produces false misses
   on real concessions.

The pattern across all three: **gate on what's load-bearing for the
atom's identity, surface everything else as soft signal**. For
entity-shaped atoms the load-bearing field is the name. For
oppositions it's the (left, right) pair, order-independent. For
concessions it's the content. Discriminators (stance, kind) gate
because they distinguish atom *types* — a `rebut`-stance Hardin atom
is a different thing from an `endorse`-stance Hardin atom.

## 5. Prompt iteration patterns that landed

Three teachings, each one short paragraph in the system prompt.

### Pattern 1 — endorse/rebut symmetry

> **Endorse and rebut are symmetric — extract both.** A common
> failure mode is to lift only the views the section pushes BACK
> against (the targets of critique) while collapsing the section's
> own NAMED endorsed view into a mechanism or a concept atom. If
> the section advances a stance you can name — the X thesis, the Y
> view, the Z framing, the W principle — lift it as a position with
> `stance: endorse` even when the author voices it. Test: if a
> reader could ask "what view does this section ultimately defend?"
> and you can name the view in 3-5 words, that name is a Position.

Why it worked: Qwen3.6-35B's default behaviour is to flag the
*targets* of critique as positions (they're explicitly named in
opposition to the author's voice) while folding the author's own
view into the mechanism collection (where it shows up as machinery
rather than as a stance). The paragraph corrects the asymmetry. Pre-
test it generalises by checking it works for both an essay that
endorses something widely accepted (the section's view is
defaultish) and an essay that endorses something contested (the
section's view is the controversial one) — if both produce
endorsed-position atoms, the pattern is sound.

### Pattern 2 — opposition + concept co-occurrence

> **Oppositions and the items inside them co-occur — extract both
> layers.** When a section names two approaches / styles / strategies
> in structural contrast (a planning style vs another planning style;
> a league-design lever vs its absence; one valuation discipline vs
> another), the binary IS the load-bearing argumentative structure
> even if each side is also a Concept the section uses elsewhere.

Why it worked: without this paragraph the model treats opposition
and concept as alternative slots — once it puts "towers in a park"
in concepts, it won't also frame "towers in a park vs short blocks"
as an opposition. The paragraph licenses the dual representation.

### Pattern 3 — collection independence (budget guard)

> **The five collections are independent — fill each on its own
> content, not in trade with the others.** Lifting a strong named
> position does not reduce how many mechanisms or evidence pieces
> the section carries.

Why it worked: tight prompt budgets push the model toward filling
the *newer* collections (whichever the prompt most recently
emphasised) at the expense of the rest. After Patterns 1 and 2
landed, the model started spending budget on positions and
oppositions, dropping mechanisms and concessions. The reminder
prevents the seesaw without naming any specific atom.

## 6. Cost discipline — budget + retry

The argumentative prompt's `max_output_tokens` was 8192 in v1. Most
sections produce 10-25 typed atoms across the five collections, which
fits in 2-3 KB of JSON. The 8192 default wasted budget on long
generations that mostly trailed off in repetition or `<think>`
preamble. Two changes cut latency 2-3x without losing recall:

1. **Tight initial budget** — `TYPED_BUDGET_INITIAL = 4096`. Covers
   ~95% of sections comfortably. Per-call latency drops from 25-35s
   to 8-15s on Qwen3.6-35B.

2. **Retry on parse drift** — `TYPED_BUDGET_RETRY = 8192`. When the
   initial response trails off mid-JSON, the parser returns an
   error and the dispatcher retries the same prompt at the wider
   budget. The retry rate on the 40-section obsidian corpus was <5%
   after the prompt-density work in §5 landed.

This is the standard "tight default, expand on failure" pattern. The
key implementation detail is that the failure detection has to be a
*real* failure (parse error, not zero-atom output), or the retry
loop fires on legitimately-empty sections and doubles the cost
without any signal lift.

## 7. Reproduction recipe

Setup a new typed-atom bench against a routed-pipeline corpus
(estimated effort: 4-6 hours for a fresh corpus, 1 hour for an
existing routed corpus):

```bash
# 0. Daemon running with a primary chat slot (Qwen3.6-35B or peer).
sovereign daemon status

# 1. Corpus initialised + base Phase 1 extracted.
sovereign enrich init <corpus-id> --source <path> --pipeline <pipeline-id>
sovereign enrich build <corpus-id>

# 2. Phase 0 vector classify.
sovereign enrich classify <corpus-id> --full

# 3. Phase 1 routed fan-out.
sovereign enrich extract-typed <corpus-id> --full

# 4. Author golden TOML at bench/<corpus>/golden.toml.
#    Start with 10-15 entries per typed axis, sampled from the
#    sections with richest extracted atoms. The atoms-as-authoring-
#    seed strategy keeps the golden grounded.
$EDITOR sovereign/bench/<corpus>/golden.toml

# 5. Run bench.
sovereign bench <corpus> --report /tmp/<corpus>-bench-v1.json

# 6. Diagnose misses (§8 below). Iterate ONE thing per cycle:
#    prompt OR scorer OR golden. Never all three at once.

# 7. Commit the golden + prompt + scorer changes together. The git
#    log becomes the bench history.
```

Per-iteration time on Qwen3.6-35B on Apple M4 Max (64 GB unified
memory):

- Phase 0 classify on 40 sections: ~4 minutes
- Phase 1 typed fan-out on 40 sections: ~25 minutes (parallelisable
  pending multi-slot dispatcher work)
- Bench (read-only): <2 seconds
- Total round-trip for one iteration: ~30 minutes

The bench itself is fast enough to re-run on every scorer or golden
change without re-extracting. Re-extract is needed only when prompts
change.

## 8. Diagnostic playbook

Per-axis miss patterns and what they mean.

### "Atom not present in the corpus at all"

Grep the Phase 1 cache for the substring before assuming a prompt
miss:

```bash
cat ~/.sovereign/enrichment/<corpus>/cache/questions.json | \
  python3 -c "import json,sys; d=json.load(sys.stdin); ..."
```

If 0 matches anywhere, the golden expectation is *out-of-corpus* —
the essay the entry was authored against isn't in the indexed vault
snapshot. Either widen the corpus (`enrich init --source` includes
the file) or annotate the golden entry with `note = "out of subset"`
and proceed.

### "Atom is in the corpus but classified into the wrong collection"

The LVT example: golden expected `land value tax` as a mechanism;
the actual extraction emitted it as a position. The genre mismatch
is real signal — usually it means the golden was authored against
how a different essay would have framed the same concept. Move the
golden entry to the right axis.

### "Extracted atom matches semantically but mismatches the golden's
specific vocabulary"

The Ostrom example: golden expected `"third pattern"`; model
extracted `"Ostrom's third-way view"`. Two paths:

- **Broaden the golden's `name_contains_any` with semantic
  variants** — accept the model's natural phrasing alongside the
  canonical academic term. This is *not* coaching as long as the
  variants are obviously the same concept.
- **Add a soft-match scorer** — token-level fuzzy match with a
  threshold. Not implemented today; would help if a corpus has
  >20% of misses falling into this bucket. Tracked as a future
  scorer improvement.

### "Extracted atom is faithful to the section; golden was wrong"

The Jacobs example: golden expected `towers in a park vs short
blocks` (the textbook urbanist binary); model extracted `parks
uplift neighbourhoods vs neighbourhoods generate parks` (the actual
binary the section sets up). The model is right; the golden was
authored from memory rather than from the section text.

Update the golden to reflect what's in the section, not what the
genre canon says should be there.

### "Atom count regression on a re-extraction with no prompt change"

LLM noise. Re-run extraction. If it persists, look for cache
contamination, daemon model swap (peer mesh routing), or a silent
schema change in the typed-extension parser.

## 9. Bench scaffold for a new corpus

To set up a typed-extension bench for, say, a recipe corpus:

1. **Pick the discourse modes the corpus exercises.** Recipes are
   mostly procedural with some descriptive — fan-out will mainly
   hit `procedural.rs` and `descriptive.rs`. No need for narrative
   or argumentative scoring lanes.

2. **Pick the typed axes from `AXIS_CATALOG` that matter most for
   the corpus's value delivery.** v1 of the catalog ships the five
   argumentative axes (mechanism / named_position / evidence /
   opposition / concession). When procedural / descriptive / lyric
   axes land (each one is `corpus-engine/src/enrichment/atlas/
   axis_catalog.rs::AXIS_CATALOG` + a `resolve_type_extensions`
   arm), pick the relevant subset. Lyric / opposition / concession
   aren't relevant to recipes.

3. **Author 10-15 golden entries per relevant axis.** Two paths:
   - **Scaffold from atoms (recommended once a corpus has been
     enriched):** `sovereign bench scaffold <corpus-id> --output
     sovereign/bench/<group>/<id>.toml` samples 10 entries per
     populated atom kind. Review, prune, tighten needles, add
     forbidden_* blocks.
   - **Hand-author from source:** read 10-15 sections, write
     expectations directly. Higher quality but slower; reserve for
     corpora that haven't been enriched yet or where the atlas's
     atom inventory is itself under test.

   The first run's scores aren't what matters — what matters is
   whether the per-axis numbers *move* when you change the prompt,
   scorer, or golden.

4. **Run the eval.** `sovereign enrich eval <corpus-id> <golden-
   toml>` reads the catalog and scores every axis the golden carries
   expectations for. No per-corpus shim required. (When Move 3 —
   `sovereign bench scaffold <corpus-id>` — lands, step 3 will draft
   the golden TOML from a freshly-extracted atoms.json.)

5. **Run the loop.** See §3.

### Adding a new typed axis to the catalog

When a discourse mode's `resolve_type_extensions` arm grows to
project a new atom shape (e.g. lyric Motif → qualified Claim atom),
land the bench-side axis in the same PR:

1. Add a `TypedAxis` const entry to `AXIS_CATALOG` in `corpus-
   engine/src/enrichment/atlas/axis_catalog.rs`. Pick the right
   `AtomKind` variant (qualified or direct), declare gating fields
   (Name is always there; layer Stance / Kind / Opposition as the
   semantics require), and list informational fields the golden
   may carry.
2. Extend `axis_expectations` in `sovereign-cli/src/enrich_cmd/
   eval.rs` with the match arm that pulls the new axis's golden
   entries into the uniform view. (Until Move 2/3 lands a canonical
   TOML shape, this is where the named-field bridge lives.)
3. Add the corresponding informational-note shape to `emit_
   informational_notes` if the axis has supplementary fields beyond
   the gates.
4. Add `expected_<axis>_atoms` / `forbidden_<axis>_atoms` named
   fields to `GoldenSet` (Vec of a new `Expected*` struct, mirroring
   `ExpectedMechanism` etc.).

Drift between projection and catalog is silent: atoms land in
`atoms.json` but the bench has no axis to score. The catalog row
makes the contract explicit.

## 10. Open questions for the next pass

These didn't land in v1 of the obsidian loop and are flagged for
follow-up campaigns:

- **Modulator wiring.** The Epistemic + Temporal axes from Phase 0
  flow through `apply_modulators` post-extraction as a no-op today.
  When the atom shapes grow `normative_marker` / `counterfactual` /
  `target_state` fields, the modulators will tag them. Will need a
  scorer pass that reads the modulator tags.

- **Gap B — resolver projection.** **Landed 2026-05-14.** Typed
  extensions now project from Phase 1 cache → resolved atoms.json
  via `resolve_type_extensions`. Brief assembly + retrieval see the
  full surface. Bench reads moved to atoms.json with ±0 F1 drift.

- **Typed-axis catalog — Move 1.** **Landed 2026-05-15.** Five
  hand-coded `score_*_atoms` functions collapsed into a single
  catalog-driven driver. See §9 "Adding a new typed axis".

- **Golden scaffolder — `sovereign bench scaffold <corpus-id>`.**
  **Landed 2026-05-15.** Reads `atoms.json`, samples per catalog
  axis + base atom kinds, emits a draft golden TOML. Cuts new-bench
  authoring from hours to minutes; verified 100% F1 self-match.

- **Cross-bench rollup — `sovereign bench all`.** **Landed
  2026-05-15.** Single command discovers every enrichment golden +
  every retrieval question bank under `sovereign/bench/`, scores
  each against a recorded baseline, renders two-pane scoreboards +
  cross-corpus matrices. The standard tuning loop is now:
  *iterate prompts → `sovereign bench all` → read cross-corpus
  matrix → identify lever owner (resolver / scorer / classifier /
  retrieval ranker)*. Baselines live at
  `sovereign/bench/<group>/baselines/<bench-id>/latest.json`
  (symlink → dated snapshot). `--update-baseline` retargets after a
  validated improvement. `--rebuild` re-extracts the enrichment-
  lane atlas before scoring (sequential, GPU-bound).

- **Cross-corpus calibration.** Each bench today tunes against one
  corpus's golden. Whether the same prompt iterations transfer
  cleanly across corpora (does endorse/rebut symmetry help a recipe
  corpus? Does collection independence matter when the corpus
  always fits in budget?) is open. The pattern catalogue in §5 is
  the place to record cross-bench observations as they accumulate.

- **Held-out golden authoring.** The current obsidian golden is
  author-tuned by the same person who owns the vault. A peer-
  authored subset would catch unconscious-bias drift. Tracked in
  the obsidian README; equivalent caveat applies to any bench whose
  golden author is also the corpus author.

## 11. References

- `sovereign/bench/HISTORY.md` — pre-MECE bench findings.
- `sovereign/bench/obsidian/README.md` — worked-example bench.
- `corpus-engine/src/enrichment/pipeline/section_classifier_axes_prompt.md` — Phase 0 vector prompt.
- `corpus-engine/src/enrichment/pipeline/typed_schemas/argumentative_phase1_system.md` — argumentative typed-extension prompt (carries the three iteration patterns from §5).
- `corpus-engine/src/enrichment/atlas/axis_catalog.rs::AXIS_CATALOG` — typed-axis registry the bench dispatches on.
- `sovereign/crates/sovereign-cli/src/enrich_cmd/eval.rs::score_axis` — catalog-driven scorer.
- `sovereign/crates/sovereign-cli/src/enrich_cmd/extract_typed.rs` — fan-out dispatcher with budget + retry.
