# Custom ontologies, as built — in conversation with the enrichment stack

Status: **analysis + proposal** (2026-09-01). Companion to
[`CUSTOM_ATLAS.md`](CUSTOM_ATLAS.md) (the June design, all seven
increments shipped), [`../../../corpus-engine/ENRICHMENT.md`](../../../corpus-engine/ENRICHMENT.md)
(the canonical map of the enrichment systems) and
[`AUTHORING_HARNESS.md`](AUTHORING_HARNESS.md) (which scopes ontology
quality out). Nothing in section 3 is built.

The question people are excited about is "can I give this system my own
ontology?" This document answers it against what is actually running:
the prebuilt genres (SEP's philosophy atlas above all), the typed
investigation path, the tiered RAPTOR + GLiNER path, and the retrieval
consumers that decide whether any of it reaches an answer. Every claim
is cited to a file and line, a bench baseline, a working note, or a run
performed on this date.

## Bottom line

1. **There are five ontology mechanisms in the tree, and they do not
   share a declaration surface.** Atlas genres (prompt assets in Rust
   modules), the recipe custom ontology (a prose paragraph), the
   investigation path (typed TOML declarations compiled to a JSON schema
   and grammar), the discourse-mode typed schemas (positions,
   mechanisms, oppositions), and the tiered path (GLiNER's fixed six
   labels). Only atlas atoms reach chat.
2. **The prebuilt genres show what "customized" means here, and the
   custom path gets a fraction of it.** SEP's philosophy genre
   customizes thirteen prompt assets across five phases plus ten pieces
   of Rust; the custom path customizes one Phase 1 prompt and one
   Phase 6 classifier. Two of nine phases hear a recipe's ontology.
3. **The custom path's headline promise is broken on the live path.**
   The prompt says "you are NOT limited to these types." The schema
   allows six, the parser drops the rest, and the custom genre overrides
   neither. Reproduced today: a `coin` entity from the prompt's own
   example is dropped, 0 of 1 survive.
4. **Retrieval barely cares about type labels, so the loss is smaller
   than it looks — and the real risk is operational.** Well over ninety
   percent of measured retrieval-time value flows through four universal
   fields (name, aliases, description, claim content) and two structural
   signals (edge degree, Tension edges). But the query-time atlas bag is
   read from a table only `atlas backfill-ann` writes. A freshly
   enriched corpus, custom or not, grounds on nothing until an operator
   runs it, with no warning.
5. **Proof exists on one domain.** Community governance: planted-conflict
   recall 0.33 → 0.83, precision 0.10 → 0.42 harness, about 0.67 in
   production; Q&A competence 0.80, honesty 1.00, dead law 0.00. It runs
   entirely on universal fields and Tension edges.
6. **A non-coder cannot find the feature.** The two narrative contracts
   (`SYSTEM_OVERVIEW.md`, `ENRICHMENT.md`) have zero mentions of it. The
   best explanations are an agent's system prompt and a desktop tutorial.
7. **The generalization move reuses three things that already exist:**
   the investigation path's schema-from-declarations generator, the
   `AtlasGenre` seam landed 2026-08-31, and the `schema-report` /
   `schema-review` gap-signature protocol. Ranked moves in section 3.

## 0. The enrichment stack as it runs

### 0.1 Four systems, one selector, one that feeds chat

`ENRICHMENT.md` §"TL;DR" is the canonical statement: `[enrichment] type`
in the recipe selects one of three build-time systems, and code
intelligence is a fourth reached by a verb. Which outputs the chat
runtime reads:

| System | Selected by | Output | Read at answer time by |
|---|---|---|---|
| Atlas (v2) — `Pipeline` trait, six registered genres + the recipe custom genre | `type = "atlas"` | `atlas/atoms.json`, `atoms.lance` + `edges.csr`, `atoms_ann.lance` | atlas grounding, overview claims, anchoring, governance active set, meta-atlas (§0.5) |
| Investigation — recipe-declared types compiled to schema + grammar | `type = "investigation"` | `entities.json`, `relationships.json`, `pattern_findings.json` | nothing under `sovereign-core/src/runtime` |
| Tiered — RAPTOR tree + GLiNER entities, SQLite sidecars | `type = "tiered"` (and every attached document, vault, watched folder, no recipe) | `conv_raptor_nodes`, `chunk_entities`, `vault_themes`; for folder corpora a typed-extension `atlas/atoms.json` | RAPTOR grounding (default on); the PPR entity rerank is off since 2026-08-04; the typed-extension atoms are "bench-side only — no chat-side surface reads typed atoms in v2" (`sovereign-tools/src/typed_extension/mod.rs:17-19`) |
| Field model (v1) — Rust `Domain` modules | `type = "field_model"` | concerns, positions, fault lines, open questions | KnowledgeView digests; zero enabled recipes today |

Recipes by system, from every `sovereign-recipes/*/recipe.toml` (39
checked): atlas 16 (of which 4 legal recipes are `enabled = false`),
tiered 2, investigation 2, field model 2 (both disabled, both naming
domains deleted 2026-07-13), and 17 with no enrichment block.

### 0.2 The atlas genres are the ontologies that exist

Since 2026-08-31 an atlas genre is a twelve-method trait, `AtlasGenre`
(`corpus-engine/src/enrichment/pipeline/pipelines/genre.rs:47-125`),
whose module note states the design: "a genre is a Phase-1 ontology plus
a handful of strategy choices — not a pipeline." Literary, conversation,
engineering and the recipe custom ontology implement it. Philosophy and
referential are still full `impl Pipeline` wrappers over an inner
literary pipeline, which is precisely why they can reach the roughly
fifteen `Pipeline` hooks that `AtlasGenre` does not expose.

| Genre | Recipes running it | Prompt assets | Entity types in its prompt | Extra atom kinds | Seed / 1b / Phase 6 / Phase 8 | Rust it needed beyond prompts |
|---|---|---|---|---|---|---|
| `literary_atlas` (base) | arch-principles, system-overview, brothers-karamazov, gutenberg-work, the narrative-markdown template | 13 | person, concept, institution, work, place | — | LLM seed / on (env-gated) / per-pair graph classifier / on, "Character trajectories" | the base itself: schema, parser, seven facets |
| `philosophy_atlas` (SEP, 1,770 per-article atlases) | sep | 13 | the five, plus prose rules (Person + Work + Concept split; schools are Concepts; `defining_quote` on concepts) and nine discourse acts | **ArgumentReconstruction** (premises, conclusion, objections) | LLM seed with own prompt, schema and a 4096-token budget / composed but gated off after the 2026-05-07 ablation / per-pair OFF ("0/81 acceptance"), **holistic ON**, the only genre / on, Ricoeur-constrained | ten items, listed in §1.8 |
| `referential_atlas` (legal, reference) | us-code, scotus-opinions, olc-opinions, federal-register — **all four disabled**; wikipedia's 1.67M-atom atlas is structural, built by the `atlas` verb | 10 | the five plus `event`, which is not in the schema enum | — | none / own prompts / OFF / OFF | five per-facet Phase 3 JSON schemas and a bespoke parser (`referential_atlas.rs:455-577`) |
| `engineering_atlas` | none; driven by the drift orchestrator | 1 | none — a flat claims envelope with code anchors | — | none / off / — / inherits on with nothing to summarise | own compose, schema (`x-asciiExtended`) and parser (`engineering_atlas.rs:139-240`) |
| `conversation_atlas` | none by recipe; KnowledgeView imports | 1 (414 lines, the largest) | the five, with seven hard rules (the user and the assistant are never Person atoms; decisions are Claims with `discourse_act: "commit"`) | — | literary defaults; terse retry still runs under the literary prompt | **none** — a pure ontology change |
| `custom_atlas` (recipe) | maple-house, proxy-company; the desktop Federalist starter | 1 neutral Phase 1 + 1 Phase 6 classifier template | six in the schema; "any label" in the prompt (§1.5) | — | none / off / **embedding top-K + ontology-templated classifier**, unique to this genre / inherits literary on | none — and therefore none of §1.8 |

Two readings of this table. First, `conversation_atlas` is the existence
proof that a pure ontology change needs zero Rust: 414 lines of prompt,
three trait methods. Second, the depth of customization the system
already knows how to do is thirteen assets across seed, extraction,
naming, classification and configuration; the custom path reaches two.

### 0.3 Two more ontology substrates, both typed, neither feeding chat

**Investigation** (`corpus-engine/src/enrichment/investigation/`). A
recipe declares `[[enrichment.entity_types]]` with attribute keys,
`[[enrichment.relationship_types]]` with a `directional` flag, and
`[[enrichment.patterns]]` (circular flow, role overlap, threshold —
`recipe.rs:790-885`). `response_schema` at
`investigation/extract.rs:182-240` builds the JSON schema with
`"enum": entity_type_names` from the declarations, and that schema
drives grammar-constrained decoding. This is the one place in the tree
where a recipe author's own type names reach the model as a constraint.
Its output never reaches chat. Two recipes use it (`uap-blue-book`,
`uap-blue-book-scans`), the second also the only corpus emitting `Asset`
atoms.

**Discourse-mode typed schemas**
(`corpus-engine/src/enrichment/pipeline/typed_schemas/`). Seven
per-mode schema + prompt + parser triples (argumentative, descriptive,
lyric, narrative, procedural, reflective, source recovery), a "MECE
axis" orthogonal to domain. The argumentative one is the only live
member: it mints **Position** and **Opposition** atoms and the
`EvidenceFor` / `Concedes` / `OpposesIn` edges through
`resolve_type_extensions` (`atlas/resolution.rs:2515`). It runs in the
tiered typed-extension pass for folder corpora and in the
`enrich extract-typed` verb; the routed Phase 1 it was designed for
lived in the retired `obsidian_atlas`. Position and Opposition are not
emitted by any registered genre — `philosophy_atlas` hard-sets
`type_extensions: Vec::new()` (`literary_atlas.rs:1405-1411`).

So the eleven atom kinds partition by producer: seven from every genre's
Phase 1, ArgumentReconstruction from philosophy, Configuration from
Phase 8, Position and Opposition from the typed-extension pass, Asset
from the extractor. And by consumer: at answer time Position,
Opposition and Asset have no reader at all (§0.5).

### 0.4 Scoreboard — what each built ontology delivers

Numbers as recorded, with the file or note that carries them.

| Corpus | Ontology | What enrichment produced | Bench and metric | Before → after |
|---|---|---|---|---|
| SEP | `philosophy_atlas` | 250,818 atoms over 1,770 atlases: Entity 88,801, Claim 59,100, Question 33,949, State 23,984, Relation 19,807, ArgumentReconstruction 13,610, Event 6,750, Configuration 4,817 | `bench/sep/questions.toml`, 21 questions, 66 sources / 159 facts, essay-readiness judge out of 252 | sources **40/66 → 55/66** (pre-enrichment 2026-05-05 → canonical 57-article 2026-05-08, same limit); essay **180 → 224** across the ArgumentReconstruction arc (`memory/project_sep_argument_reconstruction.md`); latest prod-isolated 57/66, facts 151/159 (2026-08-10) |
| Wikipedia | structural atlas + tier-2 entity descriptions | 1,666,146 Entity atoms; 52 augmented descriptions at the default 200-char floor | `bench/wikipedia/questions.toml` | atlas-grounding A/B on `wikipedia-core-v2`: sources **50.0% → 79.3%**, facts 70.8% → 83.1%, 0 regressions; runtime path 82.8% (`memory/project_atlas_grounded_retrieval_wired.md`); contested-source marker +4 strict facts on contested questions (`memory/project_atlas_layer0_findings.md`) |
| Enron | `referential_atlas` over mail + Phase 4 reconciliation | 6,101 atoms: Entity 1,730, Question 1,677, Claim 800, State 782, Relation 651, Event 461; 35 cross-inbox merges | B³ entity resolution (`bench/enron/`) | F1 **0.6154 → 0.8350** (recall 0.444 → 0.717, precision 1.0); atlas-directed counterparty retrieval 1/5 → 3/5 (note `51c23280`) |
| Maple House | `custom_atlas` (governance) | 158 Claim atoms over 22 sections | detector lane + 4-gate Q&A (`bench/governance/`) | recall **0.33 → 0.83**, precision **0.10 → 0.42** harness (note `1e4cefdd`), ~0.67 prod (note `c15e132b`); detector `latest.json` f1 0.80; Q&A competence 0.80, honesty 1.00, hallucination 0.0, dead law 0.0 (2026-06-22) |
| Proxy statements | `custom_atlas` | Exxon 125 Entity / 148 Claim / 107 Question / 57 State / 46 Relation / 43 Event, 31 tensions; Boeing 4 tensions (note `705cf142`) | none — no truth set | — |
| Conversations | tiered | RAPTOR nodes, GLiNER entities | `bench/conversation/` is marked SCAFFOLD; `sovereign-ci-bench.sh` gates only sep and wikipedia | per-article dedup RR 0.263 → 0.336 (note `4029a298`); no working gate anywhere (note `d2af7720`) |
| Vault / attached books | tiered + skeleton | golden person atoms 10/10 (was 7/10) | `bench/literary/`, book-report bench | retrieval unchanged pre/post (12/12, 68/71 — additive); book-report 64% → 72% mechanical, judge 2.49 → 3.69; noise floor ±15 pts per question |
| Engineering docs | `engineering_atlas` | 659 Claim / 0 Entity (system-overview), 125 / 0 (arch-principles) | drift bench | **zero, structurally** — the matcher keeps only Entity atoms, the pipeline emits only Claims (note `4e074503`) |

Results that were negative, reverted, or inert, because they bound what
a new ontology can expect:

- RAPTOR on SEP question answering: **−14 points** sources (85% → 71%,
  reproduced three times); late injection recovered to 86 and is now the
  default. RAPTOR on Wikipedia: 95.0% off, 95.0% on, per-question
  identical; NO-GO on the full build.
- GLiNER: zero lift on answer quality (obsidian on 10/12 = off 10/12;
  conversations 11/13 = 11/13, facts identical); the lowercase-concept
  obligations A/B was exactly off = on. Kept because a vault-only seed
  path consumes it.
- SEP Phase 8 configurations injected into context: **224 → 219**;
  Phase 6 tensions: flat at 187; Phase 1b coverage: 224 → 225 with it
  off, default flipped, ~7 h saved per re-ingest.
- Entity-typed enumeration on Enron: net negative (17/24 → 16/24 →
  14/24 facts as top-K rises); stays gated off.
- Meta-atlas SEP↔Wikipedia bridge: +0.0 after the fix, structurally
  redundant; parked 2026-06-17.
- A stronger Phase 6 classifier model scored **worse** on governance
  precision (0.79 vs 0.89): it manufactures contrived conflicts. Every
  Tension edge scores 0.95–1.0 confidence, so confidence cannot filter.
- The 2026-07-31 SEP knob-matrix ablation: on the questions bank every
  arm is identical (fact 0.945, source 0.882) — the retrieval levers
  that still move are not enrichment knobs.

### 0.5 What retrieval actually consumes, and whether a custom corpus is served

Nineteen answer-time consumers were traced. The ones that matter, with
the literal keys they read and the verdict for a corpus enriched under a
recipe ontology (free-text guidance, the same eleven kinds):

| Consumer | Reads | Default | Custom corpus |
|---|---|---|---|
| `apply_atlas_grounding` (`sovereign-core/src/runtime/retrieval/atlas_grounding.rs:68-382`) via `atlas_navigate_ann` (`corpus-engine/src/enrichment/atlas/context.rs:1013`) | the embedded atom bag; BFS over edge **types** (Tension 1.0, Concedes 1.0, Grounds 0.8, EvidenceFor 0.8, Configures 0.6, Involves 0.5, Causes 0.3) | on | works — keys on kinds and edge types, not vocabulary. **But see the hazard below** |
| `render_atom_entry` (`context.rs:1346-1408`) — the one embed-text shape | Entity `canonical_name + aliases + description`; Claim `[act, status] content`; Configuration; ArgumentReconstruction. `None` for Event, State, Relation, Question, Position, Opposition, Asset | Entity only under the shipped backfill filter (`include_claims=false`) | works unchanged |
| `atom_verbatim_excerpt` → `[Atlas highlights]` (`context.rs:889-988`) | ArgumentReconstruction premises/objections; `Entity.defining_quote`; `Claim.quotable_excerpt` with a ` — contested` tag | on, "most atoms have neither field set" | assumes philosophy; the only place `EpistemicStatus::Contested` reaches an answer |
| `enumerate_typed_atom_chunks` (`runtime/retrieval/atom_enum.rs:69-718`) | classifier prompt enumerates exactly `person, institution, initiative, concept, work, place`; string-equal on subtype; edge degree | **off** (`SOVEREIGN_ATOM_ENUM`) | inert — a domain type is unreachable by construction |
| `enumerate_overview_claim_chunks` (`atom_enum.rs:735-1080`) | `Claim.content`, `quotable_excerpt`, `confidence`, `evidence[0]` | on | works unchanged; removing it costs 0.83 → 0.78 fact recall on `sep-summarize-obscure` |
| anchoring (`runtime/evidence_loop/anchoring.rs:31-437`) | raw `atoms.json` description / statement / passage preview / name; never looks at `atom_type` | off (`SOVEREIGN_AGENTIC_KQ`) | works unchanged — the most ontology-neutral consumer |
| governance active set + `GateSurface::Governance` (`runtime/retrieval_pipeline.rs:719-960`) | presence of `governance_oplog.jsonl`; rules from `AtomEnvelope::Claim` (`content`, `claim_kind`, `attributed_to`, `evidence[0]`); tensions from `EdgeType::Tension` | corpus-kind gate | works — the proof case; `RuleAtom.deontic` always empty (§1.5) |
| `meta_atlas_boost` (`runtime/retrieval/boosts.rs:31-125`) | `classify_articulation` switches on the six `EntityType`s; `Other(_)` falls to a prose classifier whose first marker is a Wikipedia opener; `Initiative` dropped from the index | registry present | degrades |
| RAPTOR grounding (`runtime/retrieval/raptor_grounding.rs:105-251`) | `conv_raptor_nodes`; never reads atoms | on, late-inject | works unchanged |
| desktop Atlas Inspector (`sovereign-desktop/src/lib/components/atlas/AtlasCorpusView.svelte:60-79`) | filter pills over 8 of 11 kinds; `entity_type` printed verbatim | always | degrades — Position, Opposition, Asset render blank; free-text types display fine |

**The silent-zero hazard.** The runtime bag is built from
`atoms_ann.lance` (`build_atlas_context_from_ann`, `context.rs:1484`),
and `AtlasContextManager::load_corpus` returns `false` when that table
is absent (`sovereign-tools/src/atlas_context_manager.rs:355`).
`build_persistent_ann_seed_table` (`context.rs:1264`) is called from
exactly three places: `atlas backfill-ann`, `atlas migrate-all`, and
`enrich atlas-patch-code`. Not from `enrich build`; the desktop's
`state.rs:834` says "No auto-backfill on launch." With no bag,
`atlas_navigate_ann` returns empty on `atlases.is_empty()`
(`context.rs:1023-1025`), so a freshly enriched corpus gets zero atlas
grounding and nothing says so. Every knob that decides which atoms enter
the bag (`SOVEREIGN_ATLAS_INCLUDE_CLAIMS`, `_MIN_DESCRIPTION_CHARS`,
`_INCLUDE_DEPTHS`) now bites at backfill time, not at query time.

**The fraction.** Under the shipped default the embedded bag is Entity
atoms only and the embed text is exactly name, aliases and description.
The two paths with measured lift, overview Claims and the Enron
enumeration, rank on `content`, `confidence`, `evidence` and edge
degree. The single largest enrichment-to-retrieval win, governance, runs
on `Claim.content`, `Claim.evidence[0].chunk_id` and `EdgeType::Tension`.
Genre-specific kinds reach an answer only through the sparse verbatim
head and the default-off enumeration classifier. A custom-ontology
corpus therefore loses very little of the value the benches have
measured; what it risks is the operational zero above and the three
sites that hard-code the six entity types (`atom_enum.rs:149-162`,
`context.rs:889-988`, `meta_atlas/classifier.rs:47-145`).

## 1. What the custom path is

### 1.1 One TOML block

All of it is `[enrichment.ontology]` plus the sibling `domain` key.

| Key | Default | Where it lands | Reaches |
|---|---|---|---|
| `ontology.guidance` | empty (disables the path) | appended under "Domain focus" to the neutral Phase 1 prompt (`configurable_atlas.rs:113-119`); interpolated again into the Phase 6 classifier (`literary_atlas.rs:1005-1013`) | model, twice |
| `domain` | corpus id | display name (`recipe.rs:2159-2164`); cosmetic once `guidance` is set | logs, UI |
| `vocabulary.position_term`, `tension_term` | "position", "tension" | Phase 6 classifier prompt; desktop Conflicts panel (`ConflictsPanel.svelte:94-95`) | model, UI |
| `vocabulary.concern_term`, `evidence_term` | "concern", "passage" | desktop payload only (`governance_commands.rs:109-121`) | UI |
| `vocabulary.absence_term` | "gap" | parsed, defaulted, copied (`types.rs:197`); never read | nothing |

Precedence — non-empty `guidance` beats a `pipeline` pin beats the
`domain` heuristic — is enforced in one resolver every CLI site calls
(`recipe.rs:2135-2141`, `enrich_cmd/pipeline_resolve.rs:30`,
`init.rs:180-189`). That is the one structural invariant in the feature.

### 1.2 What is fixed

- Eleven atom kinds, closed, "unknown atom type on disk is a bug"
  (`atlas/atoms.rs:950-1030`). Adding the eighth for SEP touched seven
  code sites and re-extracted 57 atlases (note `8ef909f8`).
- Six entity types in the schema (`literary_atlas.rs:1975-1978`);
  `Other(String)` exists in Rust (`pipeline/atlas.rs:203`) but see 1.5.
- Fourteen edge types, closed, no escape hatch (`atlas/edges.rs:45-89`).
- Per-facet field sets and caps: fifteen entities, ten claims per
  section (`literary_atlas.rs:1920-2105`).
- The custom Phase 6 selector constants `k: 10, floor: 0.5`, calibrated
  on Maple House (`configurable_atlas.rs:254-256`).

### 1.3 Which phases hear the ontology

| Phase | Aware | What runs for a custom corpus |
|---|---|---|
| 1 extraction, 1 terse retry | yes | neutral base + guidance (`genre.rs:53`, `configurable_atlas.rs:227-229`) |
| 1a seed, 1b coverage | skipped | opted out (`configurable_atlas.rs:236-244`) |
| 2, 4 clustering | n/a | HDBSCAN, no prompt |
| 3 naming | no | literary prompts ("extracted from a novel") |
| 5 resolution | no | literary "argument-through-narrative" prompt |
| 6 selection, 6 classifier | yes | embedding top-K; ontology-driven template, pinned by `custom_phase6_classifier_is_ontology_driven_not_literary` |
| 7 gaps | no | literary prompt |
| 8 configuration | no | unconditionally on (`literary_atlas.rs:744`); "Character trajectories" (`:778`) |

### 1.4 What was measured

Section 0.4 carries the numbers. Unit tests cover prompt assembly,
vocabulary defaults, TOML round-trip, precedence and the classifier
(`configurable_atlas.rs:157-205`, `recipe.rs:4120-4216`,
`literary_atlas.rs:2281-2300`, `governance_commands.rs:1191-1209`). No
integration test drives a custom recipe from TOML to atoms.

### 1.5 The contract break

The neutral Phase 1 prompt (`configurable_atlas_prompts/phase1_system.md:36-39`):

> `entity_type` — a short lowercase label. Common ones are `person`,
> `concept`, `institution`, `work`, `place`. **You are NOT limited to
> these** (a domain may need `coin`, `statute`, `reaction`,
> `instrument`); the schema accepts any label.

Its own example emits `"entity_type": "coin"` (line 143). The schema the
model is held to (`literary_atlas.rs:1975-1978`) pins
`"enum": ["person","concept","institution","work","place","initiative"]`,
which becomes a GBNF alternation (`sovereign-inference/src/json_grammar.rs:446`).
If the model escapes the grammar, `RawEntitySketch::into_sketch` drops
the atom on `Other(_)` (`literary_atlas.rs:1449-1462`). The custom genre
overrides neither `compose_phase1` nor `parse_phase1`
(`configurable_atlas.rs:211-300`).

**Reproduced 2026-09-01.** A temporary test built
`LiteraryAtlasPipeline::with_custom_ontology(&spec)` with numismatics
guidance and fed one `"entity_type": "coin"` entity through
`parse_phase1`:

```
assertion `left == right` failed: a domain entity_type the prompt invites
must survive the custom-atlas parse; got: []
  left: 0
 right: 1
```

Run in the `sovereign-vulkan` toolbox via
`scripts/sovereign-test.sh --package corpus-engine --filter <name>`
(build 138s, test under 1s); reverted; tree clean. The literary genre
pins the same behaviour as intended
(`parse_phase1_drops_unknown_entity_type_tag`, `literary_atlas.rs:2694`).

Either way the domain label never reaches the atlas: the 122 atoms for
`arch-principles` and the 125 Exxon entities are entities retyped into
the six. A second instance: both governance ontologies ask for each
rule's "deontic force"; the claim schema has no field for it and rejects
extra properties (`literary_atlas.rs:2030-2052`); `RuleAtom.deontic`
(`governance_view.rs:609`) is always empty on the live path and is
rendered in the desktop export regardless.

### 1.6 Where the seam is

`genre.rs` (commit `b7b6892cc`, 2026-08-31) defines what varies: the
Phase 1 prompt ("what a genre IS"), the terse retry, vocabulary, seed
and tension strategies, and optional overrides for Phase 1 composition
and parsing, each defaulting to literary. Custom, literary, conversation
and engineering implement it. The override hooks a structural ontology
needs already exist on the trait; the custom genre leaves both at
default.

### 1.7 What SEP needed that the custom path cannot express

The recipe surface is three fields folded into one `format!`
(`configurable_atlas.rs:103-128`). SEP could not have been a recipe:

1. ArgumentReconstruction needed a schema arm
   (`literary_atlas.rs:2058-2095`), a raw parser (`:1739-1795`), a
   resolver arm (`resolution.rs:1489-1520`), a stable key
   (`stable_key.rs:126`), a store byte (`store.rs:122`) and a span
   extractor (`atlas_traversal/spans.rs:169`). Guidance asking for
   premises and objections is silently ignored by
   `RawSectionExtraction::into_extraction`, which reads eight known keys.
2. `Objection` needed a hand-written `Deserialize` for two shapes
   (`atoms.rs:696-719`).
3. Custom entity types: §1.5.
4. The holistic Phase 6 lives on `Pipeline` (`trait_def.rs:407,416`),
   not on `AtlasGenre`; a custom ontology is locked into per-pair
   classification, the frame that scored 0/81 on philosophy.
5. `render_holistic_user_body` partitions entities into a school lexicon
   and a proponent lexicon by `EntityType::Concept` vs `Person`
   (`holistic_classifier.rs:90-160`). No prompt produces that body.
6. Phase 8: no `AtlasGenre` method; the Ricoeur constraint and the
   four philosophy patterns are Rust-reachable only; the tolerant
   four-shape parser (`literary_atlas.rs:2210`) likewise.
7. Phase 3 facet naming: five tuned prompts (`philosophy_atlas.rs:355-361`);
   `AtlasGenre` has no `compose_phase3_facet`.
8. Seed and 1b are hard-off for custom; SEP needed an LLM seed with its
   own prompt, schema and a 4096-token budget because one article opened
   with a 3,000-word paragraph (`philosophy_atlas.rs:314-321`).
9. Per-phase token budgets and response schemas are `ChatPrompt` builder
   calls, not recipe keys.
10. `is_position_suffix` — the `-ism` / `-ology` / " ethics" Person →
    Concept retype (`literary_atlas.rs:1504-1530`) — is a philosophy
    rule compiled into the shared parser and runs on every custom corpus.

The one thing the custom path has that no prebuilt genre uses:
`TensionStrategy::EmbeddingTopK` and the ontology-templated Phase 6
classifier (`configurable_atlas.rs:254-284`).

## 2. How we communicate it

### 2.1 What already says it well

- `sovereign/docs/GETTING_STARTED.md:21-23`: "My field has its own
  concepts and jargon a generic AI doesn't understand" … "the assistant
  interviews you about your domain (coins and mints; clauses and
  parties; symptoms and treatments) and writes that into the recipe as
  your ontology." It then sends the reader (line 64) to a spec marked
  "in progress."
- The Federalist tutorial
  (`sovereign-desktop/src/lib/components/recipe_author/tutorial/federalistTutorial.ts`)
  replays a deliberately bad first ontology and its refinement: "expect
  a pass or two — that's the normal rhythm."
- `maple-house/recipe.toml:1-20`: hypothesis, planted truth, decoy, and
  the four commands to reproduce.
- Vocabulary reaching the Conflicts panel (`governance_commands.rs:97-122`):
  the UI says "rule" and "conflict" because the recipe said so.
- `enrich_cmd/init.rs:201-207`: the one good error, naming the missing
  key, the corpus and the file.

### 2.2 The path a non-coder walks

**Desktop.** Workshop → Build → New project → interview with the agent
(`sovereign/modes/recipe-author/skill.toml:282-344`) → Build & enrich →
Use this corpus. Three caveats: it depends on the agent following its
script, and the script documents a mode where the fast model answers in
prose and writes no recipe; the PDF extractor is registered at daemon
start (`recipe.rs:1811-1821`) but absent from
`sovereign-recipes/schema/recipe_schema_descriptor.json`, the menu the
author and agent see; and the quick "add a folder" route accepts only
the three prebuilt atlases (`sovereign-tools/src/local_corpus/manager.rs:544-549`).
And after Build & enrich, §0.5: no backfill, no grounding.

**CLI.** Documented and misleading. `sovereign-recipes/GETTING_STARTED.md:113`
says "leave it off for your first recipe"; the annotated template
teaches `domain = "multi"` (`_templates/annotated/recipe.toml:85`),
deleted in July; `recipe validate` (`testing.rs:896-1020`) never looks
at the ontology block and `recipe.rs` has no `deny_unknown_fields`, so
`[enrichment.ontolgy]` is silently dropped; `enrich init` overrides
`--pipeline` silently (`init.rs:184-188`) while
`sovereign/docs/ENRICH_A_CORPUS.md:20-22` says to pass it; the
governance guide never names the ontology that makes it work.
`svrn recipe --help` offers `list`, `test`, `validate`, `publish`; there
is no `new`. "Ontology" appears nowhere in `svrn enrich --help`.

### 2.3 The narrative contract does not know the feature exists

`sovereign/SYSTEM_OVERVIEW.md` §Enrichment (line 1152) lists six atlas
pipelines and describes the investigation path under "Recipe-authoring
platform" (line 1249). It contains zero occurrences of `custom_atlas`,
`enrichment.ontology`, or "custom ontology." `corpus-engine/ENRICHMENT.md`,
"the canonical umbrella that reconciles all three," likewise contains
none. `corpus-engine/ATLAS.md:115` lists three selectable pipelines.
`ARCH_PRINCIPLES.md §1.1` makes the overview entry a contract landed in
the same commit as the code; for this feature it was not.

### 2.4 Surfaces that contradict the code

| Surface | Says | Code |
|---|---|---|
| `phase1_system.md:36-39`; `CUSTOM_ATLAS.md:31-33`; `recipe.rs:620-623` | entity types are open | enum of six; parser drops the rest |
| `skill.toml:419-423`; `_templates/annotated/recipe.toml:85` | field-model domains include science, policy, legal, community, engineering, multi | deleted 2026-07-13 (`domain_registry.rs:80-95`) |
| `SCHEMA.md:129` | atlas domains are literary, philosophy, referential, else refused | shipping ontology recipes use governance, political-theory, legal; `domain` is cosmetic once `guidance` is set |
| `ENRICH_A_CORPUS.md:20-22` | always pass `--pipeline` | ontology recipes override it silently |
| `CUSTOM_ATLAS.md:3` | in progress | CA1–CA7 shipped in June |
| `SCHEMA.md:151-161` | five vocabulary terms, descriptions empty | one dead, two reach a prompt, two reach the UI |
| `referential_atlas_prompts/phase1_system.md` | six entity types including `event` | `event` not in the schema enum |

### 2.5 Words nobody defines; one ontology in three copies

Atom, facet, sketch, atlas, tension, gap, configuration, trajectory,
seed, resolve, Phase 1–8, field model, investigation, governance view,
edge. `corpus-engine/ATLAS.md:20-41` is the glossary and
`ENRICH_A_CORPUS.md:75-78` shelves it as "engineering material." The
governance ontology exists as `maple-house/recipe.toml`, as a compiled
string constant (`governance_commands.rs:695-713`; smell §6.2), and as
the `proxy-company` variant.

## 3. How to generalize

Ranked by what each unlocks against what it costs, each naming the
compass principle. M0 and M1 are what I would land before telling anyone
the system supports custom ontologies in the sense they are picturing.

### M0. Make a freshly enriched corpus ground at all

*Never silently substitute (§18.3). Size: small.*

Run `build_persistent_ann_seed_table` at the end of `enrich build` and
the desktop's Build & enrich, and have `AtlasContextManager::load_corpus`
log at warn, not return `false` silently, when `atoms.json` exists and
`atoms_ann.lance` does not. Every measured retrieval gain in §0.4 is
unreachable for a corpus that has not been backfilled, custom or not.

### M1. Make declared types the schema, reusing the generator that exists

*Structural, not remembered (§7); inventory outranks the plan (§19);
one decider (§10.6). Size: medium.*

Let a recipe declare types, and generate the Phase 1 enum and the
parser's accept-set from the declaration. Both halves exist:
`EntityTypeDecl` / `RelationshipTypeDecl` (`recipe.rs:791-830`) and
`response_schema` (`investigation/extract.rs:182`), which already emits
`"enum": entity_type_names` and feeds the grammar. The move is to reuse
those in `CustomOntology`'s `compose_phase1` (declared names plus the
six generic ones, so nothing regresses) and `parse_phase1`, the two
`AtlasGenre` hooks it leaves at default.

```toml
[enrichment]
enabled = true
type    = "atlas"
domain  = "medieval-numismatics"

[[enrichment.ontology.entity_types]]
name = "coin"
description = "A struck coin: note mint, ruler, denomination, metal."

[[enrichment.ontology.entity_types]]
name = "hoard"
description = "A deposit of coins found together."

[[enrichment.ontology.relationship_types]]
name = "minted_by"
directional = true

[enrichment.ontology]
guidance = """Prefer the document's own canonical names ..."""
```

Then `guidance` becomes what it should be, the explanation rather than
the schema; the two customization systems finally share one declaration
surface; and the three sites that hard-code the six types (§0.5) have a
declared set to read instead. `SCHEMA.md` regenerates from the AST
(`UPDATE_RECIPE_SCHEMA=1`, note `f009e4d9`).

### M2. Widen `AtlasGenre` to what SEP needed, as data

*One decider (§10.6); config as data (§6); say it in TOML. Size: medium.*

§1.7 is the list. Add to the trait, each with a literary default:
Phase 3 facet prompts, Phase 8 opt-in and prompt, holistic Phase 6
opt-in, seed prompt and budget, per-phase token budgets. Prompts already
load through one path (`prompts::load_or_baked`), so a recipe can name
prompt assets or inline them. Migrate `philosophy_atlas` and
`referential_atlas` onto the trait so the wrapper shape is gone. At that
point a genre is a Phase 1 ontology, a vocabulary, a few strategy
choices and a set of prompt files — a recipe template in the catalog,
not a Rust module — and the 13-versus-2 gap closes without Rust per
domain. Until the neutral downstream prompts exist, the custom genre
should skip Phase 8 rather than ask a house-rules corpus about character
trajectories.

### M3. One governance ontology, shipped as a recipe template

*Smell §6.2; write for the next reader (§1). Size: small.*

Move `GOVERNANCE_ONTOLOGY_GUIDANCE` to
`sovereign-recipes/_templates/governance/recipe.toml`, have the
desktop's "Rules & decisions" template read it, and give recipes an
`ontology = "governance"` reference so `maple-house` and
`proxy-company` share it. Every future shipped ontology lands the same
way, visible to authors and covered by the schema gate.

### M4. Validate the block, and say what was detected

*Never silently substitute (§18.3). Size: small.*

`recipe validate` covers the ontology (non-empty guidance, well-formed
declarations, unknown keys rejected or warned — the investigation path
already has a plural-typo warning at `testing.rs:964-1019`); a lint for
guidance that asks for a field the schema cannot carry (the deontic
case); `enrich init` prints what it detected and which pipeline it
chose; remove or wire `absence_term`; a `recipe new --ontology`
scaffold emitting the numismatics example.

### M5. Measure the ontology with the instrument that exists

*Validate the instrument (§18.4); a gate you have not watched fail
(§18.1). Size: medium.*

`enrich schema-report` and `schema-review` (`atlas/schema_validation.rs`)
already compute eight dimensions and stable gap signatures, and mark a
signature present in two corpora as a schema-revision candidate. Extend
`build_extraction_coverage` to count per declared type and emit
`coverage:zero:<type>`; that is the first honest "is this the right
ontology" signal, and the semantic rung `AUTHORING_HARNESS.md:37-40`
scoped out. Add one integration test that drives a tiny custom recipe
from TOML to atoms and asserts a declared type survives, and a truth set
plus bench lane for every shipped ontology template (`maple-house` has
one; `proxy-company` and the Federalist starter do not).

### M6. Let declared relations become typed labels

*Closed sets are enums, open sets are registries (§9). Size: small,
after M1.*

Relation labels are already free text on the sketch; only `EdgeType` is
closed, and it describes the graph's mechanics, not the domain. Carry
the declared relationship name as the label, surface it in the inspector
and the enumeration path, and take the "0 relations, 0 events"
follow-up from June with it.

### M7. Decide the fate of the two typed substrates that do not feed chat

*One decider (§10.6). A decision, not a build.*

Investigation (typed declarations, patterns, `entities.json`) and the
discourse-mode typed schemas (Position, Opposition, bench-side atoms)
are each a second answer to "what is a typed extraction." After M1 the
declaration half of investigation is shared; what remains is whether
its pattern detectors and the typed-extension kinds become declared
atlas kinds, or stay bench-side tools. Either is defensible. Two
parallel ontologies that a reader has to reconcile (`ENRICHMENT.md`
§"The atlas name collision" already exists for a reason) is not.

### Documentation moves, in the order a reader hits them

1. Add the custom genre to `SYSTEM_OVERVIEW.md` §Enrichment and to
   `ENRICHMENT.md`'s system table and corpus matrix — the §1.1 contract.
2. One human page, in the voice of `sovereign/docs/GETTING_STARTED.md:21-23`:
   "Teach it your field's vocabulary." What you declare, what you get
   back, what stays fixed, what to run after enrichment, a worked
   numismatics example, and the interview questions.
3. A glossary lifted from `corpus-engine/ATLAS.md:20-41`, linked from
   every user doc that says atom, tension, or phase.
4. Fix the cheap contradictions in §2.4: the template's deleted domain,
   the skill prompt's deleted domain list, the enrichment guide's
   `--pipeline` advice, `CUSTOM_ATLAS.md`'s status, the referential
   prompt's `event` type.
5. A "writing good guidance" section generalizing the craft in the
   `maple-house` and `proxy-company` TOML: name the deontic force, list
   what is *not* a conflict, require "one concrete moment," never
   manufacture a side the source lacks.
6. Ship the numismatics example as a catalog template; give
   `maple-house` and `proxy-company` a README.

## 4. What to tell people now

> You can give the system your field's vocabulary today: you describe,
> in your own words, what kinds of things, relationships, claims and
> events matter, and the extractor looks for those and the interface
> uses your terms. The same machinery built the Stanford Encyclopedia of
> Philosophy atlas — a quarter of a million typed atoms that lifted
> source recall from 40 to 55 of 66 and essay quality from 180 to 224 of
> 252 — and it has been proven on a second domain, community governance,
> finding planted rule conflicts at 83% recall. What it does not yet do
> is carry your own type labels through to the graph, or tune the
> phases after extraction the way the philosophy genre does: today those
> take Rust. The fix is a declared-types block in the recipe, generated
> the way the investigation path already does it, over a seam that
> landed this week.

## Sources

- Mechanism: `corpus-engine/src/enrichment/pipeline/pipelines/configurable_atlas.rs`
  (spec, defaults, `AtlasGenre` impl at 211), `genre.rs` (trait, module
  note), `literary_atlas.rs:1975` (schema enum), `:1449` (parser drop),
  `:2694` (pinning test), `:744` and `:778` (Phase 8),
  `philosophy_atlas.rs`, `referential_atlas.rs`, `engineering_atlas.rs`,
  `conversation_atlas.rs`, prompt asset dirs beside each.
- Recipe schema: `corpus-engine/src/recipe.rs:627` (`OntologyConfig`),
  `:646` (vocabulary), `:790` (investigation declarations), `:2142`
  (precedence); `investigation/extract.rs:182` (`response_schema`).
- Atom model: `atlas/atoms.rs:950` (kinds), `:658` (ArgumentReconstruction),
  `:791` (Position), `:836` (Opposition), `:891` (Asset);
  `atlas/edges.rs:45`; `pipeline/atlas.rs:203` onward; `typed_schemas/`;
  `atlas/resolution.rs:2515`.
- Resolution and registries: `sovereign-cli-llm/src/enrich_cmd/pipeline_resolve.rs:30`,
  `init.rs:180`; `pipeline/registry.rs:29`; `enrichment/domain_registry.rs:24,80`.
- Retrieval consumers: `sovereign-core/src/runtime/retrieval/{atlas_grounding,atom_enum,boosts,raptor_grounding}.rs`,
  `runtime/evidence_loop/anchoring.rs`, `runtime/retrieval_pipeline.rs:719`,
  `corpus-engine/src/enrichment/atlas/context.rs:889,1013,1264,1346,1484`,
  `sovereign-tools/src/atlas_context_manager.rs:355`,
  `sovereign-cli-llm/src/atlas_cmd/{backfill_ann,migrate_all}.rs`,
  `sovereign-desktop/src-tauri/src/state.rs:834`.
- Governance: `governance_commands.rs:689`, `governance_view.rs:598-629`,
  `sovereign-tools/src/local_corpus/manager.rs:544`.
- Narrative contracts: `sovereign/SYSTEM_OVERVIEW.md:1152,1249`,
  `corpus-engine/ENRICHMENT.md`, `corpus-engine/ENRICHMENT_V2.md:68-87`,
  `corpus-engine/ATLAS.md:115`.
- Docs audited: `sovereign/docs/GETTING_STARTED.md:21`, `ENRICH_A_CORPUS.md:20`,
  `GOVERN_A_CORPUS.md`, `sovereign-recipes/{GETTING_STARTED,SCHEMA,README}.md`,
  `_templates/annotated/recipe.toml:85`, `schema/recipe_schema_descriptor.json`,
  `sovereign/modes/recipe-author/skill.toml:282,419`,
  `recipe_author/tutorial/federalistTutorial.ts`.
- Benches and baselines: `sovereign/bench/sep/baselines/questions/{pre-enrichment-v1_1,canonical-57-articles,latest}.json`,
  `questions-prod-isolated/latest.json`; `bench/wikipedia/baselines/questions/`;
  `bench/enron/baselines/`; `bench/governance/{manifest.toml,baselines}`;
  `bench/ablation/2026-07-31-sep-knob-matrix.json`; `bench/README.md`.
- Working notes: `61ed55d5` (mint), `1e4cefdd` (governance ladder),
  `c15e132b` (model-swap rejection, confidence), `705cf142` (proxy),
  `f009e4d9` (schema regeneration), `8ef909f8` (eighth atom),
  `51c23280` (Enron enumeration), `4029a298` (dedup), `d2af7720`
  (conversation gate), `4e074503` (engineering drift), `75e320c1`
  (GLiNER lift), `26cdb3bf` / `3cc279e4` / `c70a3627` (SEP levers).
  Memory files under the agent memory dir: `project_sep_argument_reconstruction`,
  `project_atlas_grounded_retrieval_wired`, `project_atlas_layer0_findings`,
  `project_raptor_retrieval_grounding`, `project_wikipedia_raptor_due_diligence`,
  `project_lowercase_concept_obligations`, `project_enrich_resource_benchmark`.
- Commits: `310624bed`, `bf41fda60`, `a47d30285`, `0ba48e839`, `b7b6892cc`.
- Probe: temporary test on the custom-atlas parse path, 2026-09-01,
  `sovereign-vulkan` toolbox, failed with `entities_introduced == []`;
  reverted.
