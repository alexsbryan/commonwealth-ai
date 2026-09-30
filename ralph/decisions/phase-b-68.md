<!-- ledger -->

**phase-b-68 · 2026-09-29 · pb-meshapp-rest · director** — this commit
- Needed: pb-meshapp-rest stopped at its census with no code written. The door it moves into code passes corpus-engine's grammar lookup to two lanes, code may not name corpus-engine, and phase-b-53 had ruled out a distribution supplier. Its falsifier ("then the grammar lookup needs another home") had fired.
- Chose: package option (a). `sovereign_code::face::CodeParts` gains `grammar: Option<GrammarLookup>`; the stock binary supplies corpus-engine's `language_for_extension` through a new declared face item; standalone `svrn code` passes `None`, and the lanes that need a grammar report it absent. The row names the `[[distribution]] stock` widening (hard rule §7) and carries the trial. FIVE_PROGRAMS §2c gains one sentence.
- Because:
  - Extend, never re-own (§2c; principle 8). One registry stays in corpus-engine and one supplier supplies it; the daemon still stops naming it, so pb-ingest-dial-daemon's premise holds. phase-b-53's "second supplier" objection assumed the door deleted the need; it carries it.
  - (b) loses the ts, tsx, js and go lanes on the stock install (a behaviour change the row does not state, operator-only) and adds a second grammar table in code. (c) admits a new shared leaf (operator-only) and is past this row's lift.
  - Stock behaviour is unchanged. Standalone `svrn code` gains a door it never had, degraded by name rather than guessed (principle 6).
  - Boundary gate: 19 violations, EXIT=1 at e3403ebe0 (`cargo xtask boundary-gate`, corpus-engine/), unchanged with the trial applied. This commit changes no Rust.
- REVIEW-AFTER: pb-meshapp-rest lands. The charter covers false premises and landing by the ladder, but not in so many words a new `[[distribution.face]]` item overriding a prior director ruling.

<!-- appendix -->

## phase-b-68 · 2026-09-29 — code's next-edit door takes its grammar lookup as a CodeParts input; the stock binary supplies ingest's registry

<details><summary>reasoning, evidence, package</summary>

Reproduced at e3403ebe0:
- sovereign-daemon/src/routes_edit_predictions.rs:48-54 `grammar_for` over `corpus_engine::extractors::code::language_for_extension`; passed at :175 (`SyntaxOracle::parse`) and :601 (`next_edit_symbols::navigate`).
- corpus-engine/src/extractors/code/mod.rs `all_languages()`: rust, typescript (ts), typescript (tsx, the TSX grammar), javascript, go, python. `git grep language_for_extension` outside corpus-engine: the daemon's door only.
- code-next-edit/src/grammar.rs: `GrammarLookup = fn(&str) -> Option<Grammar>`, injected at "the route shell".
- code-facts/src/facts.rs `lang_packs()`: rust and python only.
- quality/ARCH_LAYERS.toml: corpus-engine is `[[package]] ingest`; sovereign-code and sovereign-cli-dev are `[[package]] code`; `[[distribution]] stock` has a code face (`face::compose`, `face::CodeParts`, `face::NotesRail`) and an ingest face (sovereign-enrichment-catalog).

Trial at e3403ebe0, reverted: the CodeParts field, `pub use code_next_edit::grammar::{Grammar, GrammarLookup}` in face.rs, code-next-edit as a sovereign-code dep, `grammar_for` + `grammar: Some(grammar_for)` + a `corpus-engine` (treesitter) dep in sovereign-stock, `grammar: None` in cli-dev project_cmd/serve.rs, and the two face-row edits. `cargo check -p sovereign-stock -p sovereign-cli-dev --features corpus-engine/treesitter` exit 0 (1m25s). boundary-gate 19 violations, the same 19 as HEAD's boundary.log, no distribution finding. layer-gate pass. Raw diff: target/ralph/phase-b/trials/t-meshapp-rest-grammar.diff.

Not checked: that the moved symbol-lane e2e runs green on grammar dev-deps. code-next-edit's own tests already use that shape.

What would falsify this:
- A distribution face may not name a program's non-port item (the gate or the operator reads §2c that way). Then the lookup needs a port in corpus-index or option (c), and both go to the operator.
- The moved e2e cannot build its lookup without corpus-engine. Then the tests stay in the stock binary's test tree.
- Standalone `svrn code` has a user who needs the ts/go lanes without ingest. Then option (c), a shared grammar leaf, is owed, and that is the operator's call.

</details>
