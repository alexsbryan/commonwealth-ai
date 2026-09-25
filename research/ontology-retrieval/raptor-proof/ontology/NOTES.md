# literary-composed.recipe.toml — what it declares, what it could not, how to build it

Custom navigation rows ARE legal: `OntologyV1.navigation` (understanding-vocab/src/ontology/decl.rs:264-268), key
admitted by `V1_KEYS` (corpus-engine/src/recipe_ontology/language.rs:174), path `[enrichment.ontology.navigation.<kind>]`
(recipe_ontology/docs/v1.md:54-70). `Summary` is a legal seed kind; a bad kind, edge or source refuses at load (watched).
Exemplars are optional, but a row without them can never be classified onto (navigation.rs:212-217).

## Constructs
- Types: entity `character` (+`station`), `setting`, `theme`; relation `bond` (+`tie`); event `plot_event` (+`place`,
  `consequence`) and `turning_point` (specializes it); claims `motive` (commissive), `conviction` (assertive), both
  `scope = "in_work"`, `subject = "character"`; states `inner_state`, `bond_state`.
- `change.clock = "narrative"`; `voices.not_entities`; `derive.configurations = true` (Phase 8, off by default once
  types are declared); `tension` between the two claim types.
- Navigation: `trajectory`, `tension`, `thematic` each seed `Summary` (quota 8, per row: ground.rs:633-638) with
  `summary_sources = ["atoms","raptor"]`; `lookup` and `enumeration` carry none.

## What the syntax could not express

Three of the five were fixed on 2026-09-21 by the change this file provoked; the
strikethroughs are kept because the recipe above still cites them.

1. ~~Declared STATE types are inert in extraction: no prompt text, no schema slot~~ —
   FIXED. `of` routes a state type to `entities_developed` or `relations_developed`, and
   both facets carry a `state_type` enum through the prompt, the schema, the reader and
   onto the State atom. A state type now MUST name `of` and may NOT declare attributes
   (the State atom has no attribute bag); both refuse at load.
2. ~~A state `of` a PAIR: `of` names one type~~ — FIXED by the same routing, and it is
   the same form this file already used: `bond_state of = "bond"`. The relation is the
   pair, so there is no second construct; what was missing was the type reaching Phase 1.
3. ~~Conflict BETWEEN characters: an empty `tension.same` means subject + clock~~ —
   FIXED as proposed here: `same` is `Option<Vec<String>>` and `same = []` is a real
   no-criterion. The validator also accepts the reserved `clock` field, which it had
   rejected — so the documented default could not be written out.
4. STILL OPEN. No cause edge: no build emits `Causes`; setup/payoff rides on
   `plot_event.consequence` (free text).
5. STILL OPEN. `seed.entity_types` is unchecked (`EntityType::Other` takes any string):
   a typo seeds nothing, silently.

## Seen in the model-free dry-run (`svrn enrich extract <id> --chapters sec_0001 --dry-run`)
- The one worked attribute example takes the FIRST type with attributes and prints it entity-shaped; with `bond` first
  it drew a relation with `canonical_name`. `character.station` is first for that reason.
- Declared block: 2,794 chars before schema growth; soft budget 3,000, warns, never refuses (ontology_schema.rs:30).
- `enrich init` finds the recipe via `config.toml`'s `[data] dir` and swallows a missing config.toml (init.rs:143-153):
  under a bare `SOVEREIGN_DATA_DIR` it printed `pipeline = literary`, which `build` refuses.

## Build on a pod (mirrors run-proof.sh; C=raptor-pilot-and-his-wife-composed, R=this recipe, BOOK=books/pilot-and-his-wife.txt)
    svrn recipe validate "$R" && svrn corpus install "$R" --wait
    rm -rf "$INDEX_DIR/atlas"; svrn enrich reset "$C" --full --yes || true
    svrn enrich init "$C" --source "$BOOK" --force     # MUST print `pipeline = custom_atlas`; --source keeps the baseline's 32 chapters
    svrn enrich extract "$C" --full --resume; svrn enrich extract "$C" --finalize
    svrn enrich build "$C" --skip extract              # keep `tensions`: the tension row walks Tension edges
    svrn enrich raptor "$C" --doc-type narrative --force --daemon "$SOVEREIGN_DAEMON_URL"
    svrn enrich summary-atoms "$C"
