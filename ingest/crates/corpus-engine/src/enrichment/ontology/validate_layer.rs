// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate`'s warnings about the ontology layer's default path
//! (campaign ontology-layer, order 2): a type RESOLVE decides with no claim
//! kind about it, which `enrich extract` refuses, and the retired reader keys.
//! Split from `validate.rs` to keep it under arch-gate's 800-line band.

use super::OntologyPolicies;
use crate::recipe::OntologyBlock;

pub(super) fn layer_warnings(
    block: &OntologyBlock,
    policies: &OntologyPolicies,
    warnings: &mut Vec<String>,
) {
    let orphans = crate::enrichment::atlas::resolution_records::types_without_statements(policies);
    if !orphans.is_empty() {
        warnings.push(format!(
            "RESOLVE decides {} by its identity_criterion, but no claim kind names it as its \
             `subject`: `enrich extract` refuses to build this, since no statement of it would be \
             read. Declare a claim kind about it.",
            orphans
                .iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (key, instead) in crate::recipe_ontology::language::retired_keys(&block.body) {
        warnings.push(format!(
            "[enrichment.ontology] `{key}` is retired and ignored: {instead}. Remove the key."
        ));
    }
}
