# Ontology migration — five axes, wired from recipe to inspector

Status: **plan** (2026-09-01). Executes
[`ONTOLOGY_PRIMITIVES.md`](ONTOLOGY_PRIMITIVES.md) (the surface) over
the seams mapped in
[`CUSTOM_ONTOLOGIES_AS_BUILT.md`](CUSTOM_ONTOLOGIES_AS_BUILT.md). Eight
phases, each shippable on its own, each with a gate that can fail.
Nothing here is built.

## 0. Rules the plan obeys

**Additive, opt-in by version.** `[enrichment.ontology] version`
selects the declaration language; absent means 0, today's prose block.
The pipeline never reads the version: it reads `OntologyPolicies`, one
struct per axis, and a version is an `OntologyLanguage` impl that
parses TOML into them (primitives §0.1). Three rules keep this honest
(ARCH §18.3, never silently substitute): version-1 keys in a block
without `version = 1` fail `recipe validate` naming the line to add; a
version the binary does not know is refused naming the highest it
supports; and a version-1 block with no declarations yields the same
policies as version 0, so adding the version line is always safe.
Readers keep every shipped version; writers (the interview, the
scaffold) emit the latest; `svrn recipe migrate --ontology-version N`
rewrites a recipe as a reviewable diff. Every atlas records the
policies that produced it (`EnrichConfig.ontology` in `config.json`,
`_summary.json`), and every bench baseline notes the version. Five
invariants, each pinned by a test before the phase that could break it
lands:

- **I1** A version-0 recipe, and a version-1 recipe with no
  declarations, compose byte-identical Phase 1 and Phase 6 prompts
  before and after (snapshot test on `maple-house`, both ways).
- **I2** Prebuilt genres send no different bytes to a model (the
  `genre.rs` precedent: "this refactor changes no bytes sent to a
  model").
- **I3** Atoms written today stay readable. The v2 store keeps a
  lossless `payload` JSON per atom beside its projected columns
  (`atlas/store.rs:205-240`), so the two additive envelope fields need
  no store migration; `tests/main/atoms_schema_back_compat.rs` pins it.
- **I4** Old TOML loads. `#[serde(default)]` on every new field;
  `tests/main/recipe_back_compat.rs` gains a fixture per phase.
- **I5** The three prebuilt-genre corpora — SEP (`philosophy_atlas`,
  1,770 atlases), Wikipedia (structural atlas plus link graph) and
  Enron (`referential_atlas` plus reconciliation) — declare nothing,
  keep their pinned genres, are never re-extracted by this plan, and
  their benches are gates: `bench/sep` and `bench/wikipedia` are HARD
  lanes in `sovereign-ci-bench.sh`, `bench/enron` B³ is named in P3.
  Movement there is a leak, not a result. Porting any of them to
  declarations is a separate decision with its own re-extraction cost
  and its own bench.

**One decider for the recipe shape.** `recipe.rs` is the AST;
`SCHEMA.md` regenerates from it under a CI gate. Two more hand copies
exist today and drift: the recipe-author tool schema
(`studio/crates/sovereign-recipe-author/src/recipe_schema.rs:395-430`)
and the descriptor menu (`sovereign-recipes/schema/recipe_schema_descriptor.json`,
which has no enrichment section at all). Phase 1 adds a test that both
agree with the AST, because a June note records the tool grammar
silently blocking the agent from emitting `ontology` when it lagged.

**Derived facets print what they inferred.** The clock, the tension
selector, the merge policy, identity defaults and question shapes all
appear in `recipe validate` and the build report.

**Two knobs change retrieval bytes and get an A/B and a
`DEFAULTS_LEDGER.md` row:** attributes in the embedded atom text, and
declared claim types in the ANN bag. Everything else is version-gated.

**A version 2 lands beside version 1.** `OntologyLanguage` impls live
in a registry keyed by version (ARCH §4: open set, registry). A new
version adds an impl, a `SCHEMA.md` section and fixtures; it touches a
version-1 code path only if a policy struct gains a field, and that
field carries a default. The version-1 fixtures in `recipe_back_compat`
are what make the claim checkable.

**Names are converged before they are minted** (`sovereign code
converge noun TypeDecl`, then the concept gate). **Docs land in the
same commit** as each phase: `SYSTEM_OVERVIEW.md` §Enrichment,
`corpus-engine/ENRICHMENT.md`, `SCHEMA.md`. **Each phase is one work
order** under `.sovereign/features/`, with the gate as its definition
of done; the gate is the two scripts plus `pre-push.sh`, then the
benches named below, bars pre-registered in the order before the run.

## 1. The wire

Where each axis lands, stage by stage. Column three names the axis the
stage carries; a stage with none is plumbing.

| Stage | Owner today | Change | Axis |
|---|---|---|---|
| Parse | `recipe.rs:627` `OntologyConfig`; `:791` `EntityTypeDecl` | `OntologyLanguage` registry dispatched on `version` (0 = today's `OntologyConfig`, 1 = `types` with `kind` plus `voices`, `change`, `tension`, `derive`); both yield `OntologyPolicies` | all |
| Validate | `testing.rs:896-1020` `validate_recipe` (ignores the ontology block) | refs, `specializes`, `role_of`, endpoints, `subject`, `same` resolve; unknown keys under the ontology warn; derived facets printed | all |
| Materialize | `sovereign-enrichment-catalog/src/config.rs:120` `EnrichConfig.ontology: CustomAtlasSpec` | the spec carries the declarations; `enrich init` prints what it detected (`init.rs:184`) | — |
| Resolve pipeline | `enrich_cmd/pipeline_resolve.rs:30` | unchanged | — |
| Compose Phase 1 | `configurable_atlas.rs:211` leaves `compose_phase1` at default; schema `literary_atlas.rs:1920-2105` | `CustomOntology::compose_phase1` generates the per-kind schema from declarations, reusing `investigation/extract.rs:182 response_schema`'s shape; voices and `must_not` rendered | Shape, Assertion |
| Parse Phase 1 | `literary_atlas.rs:1449` drops `Other(_)` | declared types accepted; attributes validated by family; anchorless declared claims rejected; voices enforced | Shape, Assertion |
| Resolve (Phase 3) | `resolution.rs:645` `resolve_step_3b`; reconciler | `ref` attributes snap to atoms; endpoints checked; `role_of` becomes a `State` on the rigid atom; identity keys primary, fallback judged; merges reified as `same_as` claims; `subject` resolved | Shape, Identity |
| Derive (Phases 6, 8, patterns) | `tensions.rs:75` strategy; `custom_phase6_classifier_system.md`; `governance.rs:280` `derive_active`; `investigation/patterns.rs` | `same` as the candidate filter; `not_conflicts` and the deontic normal form in the template; `supersedes` folds on its clock for any listed type; patterns run over atlas edges; Phase 8 and arguments opt-in | Change, Derivation |
| Write | `atlas/writer.rs`, `store.rs` (`subtype` column, `payload`) | `subtype` carries the declared type; `schema_validation.rs:477` counts per declared type, reports identity criteria and merges | — |
| Backfill | `context.rs:1264` `build_persistent_ann_seed_table`, called only by `atlas backfill-ann` / `migrate-all` | called at the end of `enrich build` and desktop Build & enrich; `atlas_context_manager.rs:355` warns instead of returning `false` silently | — |
| Retrieve | `context.rs:1346` `render_atom_entry`; `atom_enum.rs:149,314`; `atlas_traversal` plans; `retrieval_pipeline.rs:719` governance step; `boosts.rs` meta classifier | enum and subtype compare from declared types; enumerate and aggregate plans by declared type; governance reads `subject` and `claim_kind`; attributes in embed text behind the A/B | Shape, Assertion, Derivation |
| Inspect | `atlas_view/subgraph.rs:94`; `AtlasCorpusView.svelte:60-79`; `AtomDetail.svelte:183`; `ConflictsPanel.svelte:94`; `governance_commands.rs:97` | pills and census by declared type; generic attribute rows and a `subject` link; bodies for Position, Opposition, Asset; labels from `label`; build report card | all |
| Author | `sovereign/modes/recipe-author/skill.toml:282-344`; `recipe_schema.rs:395`; `RecipeValidationCard.svelte` | five-question interview; tool schema gated to the AST; validate output rendered | all |

## 2. Phases

### P0 — Ground on build

*As-built M0. Size: small. No axis; unblocks every measurement below.*

- `enrich_cmd/build.rs`: call `build_persistent_ann_seed_table` after
  the final atlas write; the desktop build command does the same.
- `atlas_context_manager.rs:355`: when `atoms.json` exists and the ANN
  table does not, log at warn with the command that fixes it.
- Gate: a corpus enriched from scratch by `enrich build` has
  `atoms_ann.lance`, and a `tracing=debug` run of one question shows
  `apply_atlas_grounding` seeding from it. `sovereign-ci-bench.sh --quick`
  neutral on sep and wikipedia (both already backfilled, so any delta
  is a bug).

### P1 — Shape in the recipe

*Axis 1 parsed, validated, materialized, authorable. No extraction
change yet. Size: medium.*

- `recipe.rs`: the `OntologyPolicies` structs (six, one per axis plus
  prose) and the `OntologyLanguage` trait with a registry keyed by
  version; `V0` wraps today's `OntologyConfig` and fills `prose` and
  labels; `V1` parses `TypeDecl { name, kind, description, attributes,
  specializes, role_of, from, to, participants, source, label, identity,
  identity_fallback, force, deontic, subject, grades, anchors, scope }`
  under `[[enrichment.ontology.types]]` plus the corpus blocks
  `voices`, `change`, `tension`, `derive`, and the version-0 fields
  `guidance` and `must_not`. Unknown version → a parse error naming the
  highest supported. Everything downstream takes `&OntologyPolicies`. `EntityTypeDecl` / `RelationshipTypeDecl`
  become the investigation path's view of the same struct (converge
  first). Attribute families: text, quantity, time, ref.
- `testing.rs`: `validate_recipe` covers the block (rule list in the
  primitives note §4), prints derived facets and question shapes.
- `config.rs`: `CustomAtlasSpec` carries declarations; `init.rs` prints
  "custom ontology: medieval-numismatics (4 types, 1 claim type)".
- `recipe_schema.rs`: the tool schema gains the same fields; new test
  `tool_schema_matches_recipe_ast` parses every property the tool
  offers through `recipe.rs` and fails on a field the AST has that the
  tool lacks. The descriptor json gains an enrichment section from the
  same source.
- `svrn recipe new --ontology <template>` scaffolds; `svrn contract
  census` sees the new verb.
- `SCHEMA.md` regenerated; a `recipe_back_compat` fixture with the old
  `guidance`-only block; the I1 snapshot test written now.
- `svrn recipe migrate --ontology-version 1` adds the version line and
  leaves everything else; the diff is the whole change.
- `EnrichConfig.ontology` and the atlas `_summary.json` carry the
  policies; the desktop build report and the inspector read them to
  decide whether pills come from kinds or declarations.
- Gate: parse and validate tests including the three version rules and
  `v1_without_declarations_equals_v0` on the policies;
  `recipe_schema_is_fresh` with a section per version;
  `tool_schema_matches_recipe_ast`; contract census green for `recipe
  new` and `recipe migrate`.

### P2 — Extraction

*Shape and Assertion reach the model. Size: large. The phase the
probe test was written for.*

- Lift `response_schema`'s shape into a shared
  `schema_from_types(kind, &[TypeDecl])` and use it from both the
  investigation path and `CustomOntology::compose_phase1`. The custom
  schema per kind is the declared names plus the six generic ones, so
  an undeclared corpus regresses nowhere.
- `pipeline/atlas.rs` sketches and `atoms.rs` envelopes gain
  `attributes: BTreeMap<String, AttrValue>` on Entity, Relation, Claim,
  Event, and `Claim.subject: Option<AtomId>`; both `#[serde(default)]`.
  `AttrValue` is the four families. `stable_key.rs` excludes attributes
  from keys (identity from essence, not from extracted detail).
- `CustomOntology::parse_phase1`: declared types survive; attributes
  validated by family, unknown ones dropped with a debug line;
  anchorless claims of a declared type rejected; `not_entities` names
  never become atoms; `must_not` and voices rendered into the prompt.
- `force` → `discourse_act`, `strength` per instance →
  `epistemic_status`, `deontic` normal form → `claim_kind`, `scope` →
  `ClaimScope`.
- Prompt budget: measure composed Phase 1 tokens on the numismatics
  fixture against the literary baseline; cap attributes per type and
  types per kind in `validate` if the schema grows past what the fast
  slot's input gate accepts (as-built: the 6,000-character FastShort
  gate exists for a reason).
- Gate: the probe becomes the pinned test (`coin` survives on the
  custom path); `atoms_schema_back_compat`; I1 and I2 snapshots
  byte-identical; the numismatics fixture (`sovereign-recipes/_templates/numismatics/`,
  three sections, a truth file) produces atoms whose `subtype` is
  `coin` and `sceatta`, with `weight` and `struck` populated;
  `bench/literary` golden (bk-book, 10/10 person atoms) unchanged.

### P3 — Resolution and identity

*Shape completes; Identity lands. Size: medium.*

- `resolution.rs`: `ref` attributes snap to atom ids with the existing
  fuzzy resolver; `from` / `to` / `participants` checked against
  declared types, mismatches logged and dropped; `specializes` resolved
  so a `sceatta` is also a `coin` for enumeration; `role_of` resolves
  the mention to the rigid atom and records a `State` with a
  `Transition` when the role changes.
- Reconciler: `identity` keys merge strictly; `identity_fallback` keys
  go through the judged path; for a corpus with declarations, every
  non-strict merge writes a `Claim` with `claim_kind: "same_as"`, both
  ids, a grade and the anchor that justified it, and a `Grounding`
  edge to each side. Undeclared corpora keep today's silent merge, so
  a future Enron re-enrichment does not change its atom count; making
  reification always-on is a later `DEFAULTS_LEDGER.md` decision with
  the B³ lane as its gate.
- `schema_validation.rs`: coverage per declared type;
  `coverage:zero:<type>` signatures; identity criterion per type;
  merge count.
- Gate: fixture asserts Acme is one `organization` atom with a `party`
  state, and a fuzzy merge shows up as a claim; `bench/enron` B³ lane
  unchanged (no declarations there); `enrich schema-report` on the
  fixture lists the new rows.

**As built (2026-09-02).** Four departures from the paragraphs above,
each because the code said so:

- `resolution.rs` keeps `resolve_step_3b` and
  `resolve_entities_and_events` as shims; the declared passes live in a
  sibling `atlas/resolution_ontology.rs` and read a `ResolutionPolicy`.
  Nothing is re-plumbed for a corpus that declares nothing.
- A `ref` attribute snaps by NAME and the resolved atom's type is NOT
  checked against the `of`. A second, stricter gate would refuse a
  correct snap whenever Phase 1 typed the target as one of the generic
  six, which is the common case on a first extraction. `of` still earns
  its keep in the prompt and in `recipe validate`.
- Every merge in a declared corpus is reified, not only the non-strict
  ones — one rule is cheaper to hold than two, and the strict merges are
  the ones a reader most wants to see. The grade is `external` or
  `signal_gated`; the primitives note calls the second "judged" and it
  is not, because no judge is wired into `svrn enrich reconcile`.
  Grading a merge "judged" when nothing judged it is the well-formed
  false claim §18.3 forbids; when a judge lands, its grade is a third
  value.
- The reified merges attach `Involves` + `Grounds`, NOT `Grounding` —
  that is the cross-corpus edge family, and a merge inside one corpus is
  not a cross-corpus link. (The paragraph above says `Grounding`.)

### P4 — Change and derivation

*Axes 4 and 5. Size: medium. The phase governance already proves.*

- `governance.rs`: `derive_active` generalized to fold any claim type
  named in `change.supersedes`, on the named clock; the governance oplog
  path is unchanged and stays the only writer of adjudications.
- `tensions.rs`: `same` is the candidate filter (defaults to `subject`
  plus the type's clock); the selector is derived from corpus shape
  and printed; `not_conflicts` and the deontic interdefinition fill
  `custom_phase6_classifier_system.md`; `between` restricts candidate
  pairs to the listed claim types.
- `investigation/patterns.rs` detectors run over atlas edges when
  `patterns` is declared; `derive.configurations` and `derive.arguments`
  become `AtlasGenre` opt-ins (as-built M2), with Phase 8 off for
  declared corpora until a neutral prompt exists.
- Gate, pre-registered in the order: Maple House detector lane holds
  recall ≥ 0.83 and precision ≥ 0.42 in the harness, and a new fixture
  with "must not host after 10pm" and "must end hosting by 10pm" yields
  one rule, not two and not a conflict; governance QA lane holds all
  four bars; `bench/sep` unchanged.

### P5 — Retrieval

*The declared types reach answers. Size: medium, mostly measurement.*

- `atom_enum.rs`: the classifier's enum is the declared types plus the
  six; the compare at `:314` walks `specializes`. `atlas_traversal`:
  enumerate and a new aggregate plan take a declared type; the
  classifier's known-name list includes labels.
- `retrieval_pipeline.rs` governance step reads `subject` and the
  normalized `claim_kind`, so the desktop's deontic column is
  populated on the live path.
- `boosts.rs`: `classify_articulation` maps a declared type to its
  kind's default articulation instead of the Wikipedia prose fallback.
- Two A/Bs, each a `DEFAULTS_LEDGER.md` row: attributes appended to
  `render_atom_entry` text for declared corpora; declared claim types
  included in the backfill filter. Default off until the numbers say
  otherwise.
- Gate: `sovereign-ci-bench.sh --quick` neutral on sep and wikipedia
  (HARD lanes; no declarations, so any move is a leak); governance QA
  holds; a tiny numismatics lane where "which sceattas are in the
  hoard" enumerates all of them and "which mints struck for Offa" hits
  the declared relation. Bars written before the run.

### P6 — Desktop

*The user sees their own nouns. Size: medium. The authoring half
depends only on P1.*

- `atlas_view/subgraph.rs`: census and filters keyed on `subtype` for
  declared corpora, falling back to kind; `AtlasCorpusView.svelte`
  pills from the declaration; `AtomDetail.svelte` renders `attributes`
  as rows and `subject` as a link; `PositionBody`, `OppositionBody`,
  `AssetBody` so no kind renders blank; `ConflictsPanel` labels from
  `label`.
- `BuildEnrichCard.svelte` shows the build report: per-type coverage,
  zero-coverage types, identity criteria, merges, inferred facets, and
  the backfill status from P0.
- `skill.toml`: the five-question interview replaces the twelve;
  `RecipeValidationCard.svelte` renders `validate` output including
  derived facets; the tool schema is already gated (P1).
- Gate: `svelte-check` and vitest; the Federalist starter walked
  through Playwright (`tests/e2e/real/`) ends with pills named from the
  starter's declared types; `governance.real.spec.ts` holds.

### P7 — Migrate the shipped ontologies

*Size: small in code, one re-enrichment per corpus.*

- `maple-house`, `proxy-company` and the desktop Federalist starter
  gain declarations; their `guidance` paragraphs stay as the
  explanation.
- `GOVERNANCE_ONTOLOGY_GUIDANCE` moves to
  `sovereign-recipes/_templates/governance/recipe.toml` and the desktop
  "Rules & decisions" template reads it; recipes take it by
  `ontology = "governance"` (as-built M3). The numismatics template
  from P2 ships beside it.
- `conversation_atlas`'s seven voice rules become a `voices` template
  so KnowledgeView imports can adopt declarations later without a
  genre.
- Re-enrich the three corpora; re-baseline the governance lanes with
  the reason in the baseline's `notes`.
- Docs: `SYSTEM_OVERVIEW.md` §Enrichment and `ENRICHMENT.md` rows for
  the custom genre; `CUSTOM_ATLAS.md` marked shipped and pointed here;
  the human page and glossary from the as-built doc moves; `SCHEMA.md`
  descriptions for every facet, including the one dead vocabulary knob
  now removed.

## 3. Order and parallelism

```
P0 ──────────────────────────────────────────────┐
P1 ─▶ P2 ─▶ P3 ─┬─▶ P4 ─┐                        │
                └─▶ P5 ─┴─▶ P6 (viewers) ─▶ P7 ◀─┘
P1 ─────────────────────▶ P6 (authoring half)
```

P0 can land today, alone. P1 is the contract and goes first. P2 is the
long pole. P4 and P5 are independent of each other once P3 is in; the
authoring half of P6 needs only P1 and can run alongside P2. Three
workers at once is the cap: P0 plus P1 plus the docs half of P7 is a
sensible first fan-out; P2 is one worker's whole session.

## 4. Risks named before they bite

- **Grammar cost.** The investigation path already compiles a
  per-corpus JSON schema into a grammar per request; the custom path
  will do the same. Measure compile time on the numismatics fixture
  before assuming it is free.
- **Prompt growth.** Each declared type adds schema bytes to every
  Phase 1 call. The cap lives in `validate`, not in the model's
  patience; the enrich turbo arc showed prompt tokens are the cost.
- **Small-model attribute quality.** A 4B extractor filling `weight`
  and `struck` may hallucinate values. The fixture's truth file scores
  attribute fields separately from atom presence, so the number is
  visible before anyone relies on it.
- **Atom counts move for declared corpora.** Reified merges and role
  states add atoms. Only benches over declared corpora re-baseline;
  SEP, wikipedia and enron do not re-extract and must not move.
- **Two copies of the recipe shape remain** until the tool schema and
  descriptor are generated rather than gated. The gate is the floor;
  generation is the follow-up.
- **`recipe validate` warning on unknown keys** is a warning because
  `recipe.rs` has no `deny_unknown_fields` and community recipes must
  keep loading. A misspelled ontology key is now loud; it is still not
  fatal.

## 5. Definition of done, per phase and overall

Per phase: the two scripts and `pre-push.sh` exit 0; the named benches
hold their pre-registered bars; the phase's `recipe_back_compat`
fixture loads for both version 0 and version 1; the docs named in §0
are in the same commit; the work order carries the note ids for any
decision taken. Overall: a new
recipe written only from the five interview questions produces an
atlas whose inspector pills, build report, enumeration answers and
conflict labels use the author's nouns, with no Rust touched; the three
prebuilt-genre benches did not move while it happened; and a version-2
declaration language could be added afterwards as one parser impl,
without editing a version-1 code path.
