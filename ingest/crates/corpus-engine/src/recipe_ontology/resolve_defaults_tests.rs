// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::enrichment::ontology::validate_block;
use crate::recipe_ontology::OntologyBlock;

pub(crate) fn block(toml: &str) -> OntologyBlock {
    OntologyBlock {
        version: 1,
        body: toml::from_str(toml).unwrap(),
    }
}

/// 02-contracts n1, 05-incidents n2/n3, 06-tasks n4: a statement's reference
/// to the base kind `person`, which the recipe never declares.
pub(crate) const OWNER_IS_A_PERSON: &str = r#"
[[types]]
name = "contract"
kind = "entity"
description = "An agreement with a counterparty."
identity_criterion = "Two mentions are one contract when they name the same agreement."

[[types]]
name = "signed"
kind = "claim"
force = "assertive"
subject = "contract"
description = "A contract is signed."
attributes = [{ name = "owner", type = "ref", of = "person", description = "Who owns it on our side." }]
"#;

/// 07-inventory n3/n5: a movement's `from` and an issue's technician refer to
/// declared types with no identity criterion and no statement about them.
pub(crate) const MOVEMENT_FROM_A_WAREHOUSE: &str = r#"
[[types]]
name = "part"
kind = "entity"
description = "A stocked part."
identity_criterion = "Two mentions are one part when they give the same part number."

[[types]]
name = "warehouse"
kind = "entity"
description = "A place parts are kept."

[[types]]
name = "moved"
kind = "claim"
force = "assertive"
subject = "part"
description = "Parts move between warehouses."
attributes = [
  { name = "from", type = "ref", of = "warehouse", description = "Where they left." },
  { name = "to", type = "ref", of = "warehouse", description = "Where they arrived." },
]
"#;

/// Failing input, named: 02-contracts n1. `contract.owner` refers to the
/// undeclared base kind `person`; before the default no `person` type was
/// declared, RESOLVE decided none, and every owner reading was removed.
#[test]
fn a_referenced_base_kind_is_declared_and_decided_by_name() {
    let p = block(OWNER_IS_A_PERSON).policies().unwrap();
    let person = p.type_decl("person").expect("person is declared");
    assert_eq!(person.kind, TypeKind::Entity);
    assert_eq!(
        person.identity_criterion.as_deref(),
        Some("Two mentions are one person when they name the same person, however the name is written.")
    );
    assert!(decides(&TypeIndex::from_policies(&p), "person"));
    assert_eq!(
        block(OWNER_IS_A_PERSON).supplied_defaults().unwrap(),
        [SuppliedDefault::ByName {
            type_name: "person".into(),
            declared: false,
            referred_by: vec!["signed.owner".into()],
        }]
    );
}

/// Failing input, named: 07-inventory n3. `warehouse` is declared with no
/// criterion and no statement is about it; before the default it was no
/// type RESOLVE decides, and `moved.from` named no record.
#[test]
fn a_declared_type_nothing_records_is_decided_by_name() {
    let p = block(MOVEMENT_FROM_A_WAREHOUSE).policies().unwrap();
    assert!(decides(&TypeIndex::from_policies(&p), "warehouse"));
    assert_eq!(
        block(MOVEMENT_FROM_A_WAREHOUSE)
            .supplied_defaults()
            .unwrap(),
        [SuppliedDefault::ByName {
            type_name: "warehouse".into(),
            declared: true,
            referred_by: vec!["moved.from".into(), "moved.to".into()],
        }]
    );
}

/// A declaration the document reader does not read (no claim kind) gets no
/// default: the general extractor records every type it reads.
#[test]
fn a_declaration_the_reader_does_not_read_gets_no_default() {
    let toml = r#"
[[types]]
name = "warehouse"
kind = "entity"
description = "A place parts are kept."
attributes = [{ name = "manager", type = "ref", of = "person" }]
"#;
    let p = block(toml).policies().unwrap();
    assert!(p.type_decl("person").is_none());
    assert!(p
        .type_decl("warehouse")
        .unwrap()
        .identity_criterion
        .is_none());
}

/// A reference naming neither a declared type nor a base kind is refused by
/// validate as before, and nothing is declared for it.
#[test]
fn a_reference_to_an_unknown_name_is_still_refused() {
    let toml = OWNER_IS_A_PERSON.replace("of = \"person\"", "of = \"clerk\"");
    let p = block(&toml).policies().unwrap();
    assert!(p.type_decl("clerk").is_none());
    let v = validate_block(&block(&toml));
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("of = \"clerk\"` does not name a declared type")),
        "{:?}",
        v.errors
    );
}

/// Failing input, named: 02-contracts and 07-inventory validated with no word
/// of `person` or `warehouse` (07 a warning that they get no record). Validate
/// prints each derivation in place of the type's identity line.
#[test]
fn validate_prints_each_type_decided_by_name() {
    let v = validate_block(&block(OWNER_IS_A_PERSON));
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.notes.contains(
            &"identity: person → RESOLVE by name (no declaration; `signed.owner` refers to it)"
                .to_string()
        ),
        "{:?}",
        v.notes
    );
    assert!(!v
        .notes
        .iter()
        .any(|n| n.starts_with("identity: person → RESOLVE under")));

    let v = validate_block(&block(MOVEMENT_FROM_A_WAREHOUSE));
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.notes.contains(
            &"identity: warehouse → RESOLVE by name (no identity_criterion declared; \
              `moved.from`, `moved.to` refer to it)"
                .to_string()
        ),
        "{:?}",
        v.notes
    );
    assert!(
        !v.warnings.iter().any(|w| w.contains("warehouse")),
        "{:?}",
        v.warnings
    );
}
