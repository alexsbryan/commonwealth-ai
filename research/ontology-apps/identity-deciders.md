# Identity deciders on the atlas-resolve path (E5 inventory, 2026-10-10)

Order ontology-layer-16 step 2, committed before any change. Paths are under
`ingest/crates/corpus-engine/src/enrichment/`; line numbers at d8458602b, each checked by
`git show HEAD:<file> | grep -n`. Default path call order
(`sovereign-enrichment-build/src/atlas_resolve.rs`): metadata projection (`:249`), 3a
(`:330`), 3b (`:350`), type extensions (`:366`), then `atlas_resolve_documents::apply` (`:418`:
stamps, derive before RESOLVE, `resolve_declared_types`, derive after). 3a and 3b run for every
type, RESOLVE's included; RESOLVE then retires 3a's atoms of the types it decides and drops
references to them (`resolution_records.rs:290-304`).

Disposition: **R** = RESOLVE's own; **S** = structural, a declared key decides (all keys equal),
which is §Identity's "a sufficient field that agrees links"; **O** = overridden for RESOLVE's
types by retirement, still deciding for undeclared and version-0 types; **X** = cannot be folded
onto RESOLVE as RESOLVE stands (it needs an `identity_criterion`; these decide undeclared/generic
types and every version-0 corpus), and some hold a domain word.

| # | Decider | file:line | Decides, on what | Keys | Disp. |
|---|---|---|---|---|---|
| 1 | `project_source_atoms` / `sighting` | `atlas/resolution_sources.rs:149`, `atlas/resolution_sources/fields.rs:40` | one entity per folded identity value of a metadata-sourced type; `refs` by exact folded value | all keys | S (held the bundled `mailbox_providers` skip, `fields.rs:202`: step 4 moves it to a declaration) |
| 2 | `find_merge_target` (+ `merge_into_existing`) | `atlas/resolution.rs:794` | entity sketch vs entity across sections: exact folded name/alias, whole-word substring, first token + 2 shared tokens, Levenshtein ≤2 with cosine ≥.85, one long token with cosine ≥.92 | any rule | O, X (`fold` transliterates Cyrillic, `understanding-atlas/src/enrichment/atlas/fold.rs:43`; the first-token rule is patronymic logic) |
| 3 | `synthesize_entities_from_unresolved_event_participants` | `atlas/resolution.rs:258` | an unmatched participant name becomes a new `Person` | none | O, X (types every participant a person) |
| 4 | `dedup_typo_fragmented_entities` | `atlas/resolution.rs:392` | atom vs atom: same type, ≥8 chars, same first 4, Levenshtein ≤3 | one rule | O, X |
| 5 | `fold_source_atoms` | `atlas/resolution_sources.rs:471` | model atom into projected atom on the declared identity key (`ExternalIdSignal`, `reconciliation/identity_signals.rs:105`) | all keys | S (the key map at `identity_signals.rs:40` names `name`, `employer`, `role`: a domain word) |
| 6 | `find_event_merge_target` | `atlas/resolution.rs:1209` | event sketch vs event within ±2 sections: cosine ≥.93, or ≥.88 with a shared participant; no veto | similarity | O, X |
| 7 | `resolve_entity_id_fuzzy` | `atlas/resolution.rs:2003` | a name to an atom: exact fold, unique long token, per-token Levenshtein vote | first hit | O, X |
| 8 | `resolve_entity_id_with_salience` | `atlas/resolution.rs:2094` | #7, then a shortlist by first token and salience ≥2× | first hit | O, X |
| 9 | `resolve_within_declared_type` | `atlas/resolution_identity.rs:288` | #8 over one declared type's atoms (relation ends, non-RESOLVE claim subjects) | first hit | X |
| 10 | relation dedup, `relation_key` | `atlas/resolution.rs:2209` | relations equal when their participant sets are | participant set | X |
| 11 | `bind_claim_subject` | `atlas/resolution_identity/source_subject.rs:16` | a claim's subject: left to RESOLVE for its types; a metadata-sourced subject by all keys equal; otherwise #9 | all keys / #9 | R, S, X |
| 12 | `snap_ref_attributes` | `atlas/resolution_ontology.rs:480` | a `ref` value's name to an atom of the target type (#8) | first hit | X |
| 13 | `derive_section_context_refs` | `atlas/resolution_ontology.rs:381` | a target atom whose name a section title's breadcrumb contains, when exactly one | containment | X |
| 14 | `resolve_type_extensions` | `atlas/resolution.rs:2771` | argumentative extensions by exact folded name, containment | first hit | X (not verified that a version-1 recipe emits them) |
| 15 | statements and `local_subjects::assign` | `atlas/resolution_records/local_subjects.rs:45` | claims marking one span, or one local reference in one document, are one statement | within a document | R |
| 16 | `retire` | `atlas/resolution_records/retire.rs:19` | removes #2-#8's atoms of RESOLVE's types | — | R (the override) |
| — | `merge_permitted` (veto) | `atlas/resolution_identity.rs:64` | never across rigid types; any differing declared key forbids; fuzzy evidence on a keyed type needs an agreeing key | any key | veto on #2, #4, #5 |
| — | RESOLVE | `atlas/resolve_records.rs:465`, `resolve_records/weigh.rs:60`, `resolve_records/settle.rs` | sufficient key (any one), necessary values (veto when supplied, weighed when read), every other source weighed at corpus weights; held statements settled after the last document (E3) | any key / weighed | R |

Off the default path: `reconcile` (`reconciliation/multi_origin.rs:88`, `enrich reconcile` only)
with `NameSimilaritySignal` (a corporate-suffix list), `EmailHeaderSignal`, `OrgRoleSignal`,
`ExternalIdSignal`, `DescriptiveKeySignal`; `ThreadRootSignal` (no caller); `reify_merges` records
merges, decides none; `pipeline/atlas_clustering.rs` clusters themes, not particulars.

The three ways keys combine: all keys equal (#1, #5, #11 sourced), any key (`merge_permitted`,
RESOLVE's sufficient keys), weighed (RESOLVE).

Why step 3 stops here: RESOLVE decides a type only under its `identity_criterion` (§The core: a
question with no criterion is not RESOLVE). Deciders #2-#4 and #6-#14 are what decide every type
without one, which is every undeclared type, every generic type a version-1 recipe leaves open,
and every version-0 corpus. For RESOLVE's own types they are already overridden by #16. Folding
them means either RESOLVE deciding types with no criterion, or retiring the generic atlas's
entity resolution, and #2 and #3 cannot be stated without a domain word as they stand.
