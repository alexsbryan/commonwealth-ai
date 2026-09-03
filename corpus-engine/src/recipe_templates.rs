// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology-v1 recipe templates — the worked declarations from
//! `sovereign/docs/specs/ONTOLOGY_PRIMITIVES.md` §1 as complete recipes.
//!
//! `svrn recipe new --ontology <name>` scaffolds one of these. They are
//! data, not code: each lives at
//! `sovereign-recipes/_templates/ontology-v1/<name>/recipe.toml` and is
//! vendored into `OUT_DIR` by `build.rs` exactly like the bundled recipes
//! (`recipe_builtin.rs`), so there is one checked-in copy and no
//! repo-relative `include_str!` path. Adding a template is adding a
//! directory there and a row in [`BUILTINS`]; `tests/main/recipe_templates.rs`
//! parses and validates every row and pins its derived facets.
//!
//! One template per §1 user, in §1 order (§1.1 numismatics … §1.10
//! product-support).

use crate::error::{Error, Result};

/// `(name, recipe.toml)` for one vendored template directory.
macro_rules! template {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!(
                env!("OUT_DIR"),
                "/recipes/_templates/ontology-v1/",
                $name,
                "/recipe.toml"
            )),
        )
    };
}

/// `(name, recipe.toml)` for every shipped template, in catalog order.
const BUILTINS: &[(&str, &str)] = &[
    template!("numismatics"),
    template!("governance"),
    template!("patient-community"),
    template!("contracts"),
    template!("engineering-org"),
    template!("due-diligence"),
    template!("literary"),
    template!("research-notebook"),
    template!("materials-lab"),
    template!("product-support"),
];

/// The placeholder the templates use for the fields an author must fill
/// (`corpus.id`, `corpus.name`, `acquire.path`). `recipe new --id` replaces
/// the id and name lines; the path is always the author's.
pub const PLACEHOLDER: &str = "REPLACE_ME";

/// Every template name, in catalog order.
pub fn list_builtin_names() -> Vec<&'static str> {
    BUILTINS.iter().map(|(n, _)| *n).collect()
}

/// The recipe TOML for a template, or an error naming every template there
/// is (mirrors `enrich_cmd/templates::load_builtin`; ARCH §4, unknown id loud).
pub fn load_builtin(name: &str) -> Result<&'static str> {
    BUILTINS
        .iter()
        .find_map(|(n, body)| (*n == name).then_some(*body))
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "no built-in ontology template '{name}' (available: {})",
                list_builtin_names().join(", ")
            ))
        })
}

/// Instantiate a template: the `id = "REPLACE_ME"` and `name = "REPLACE_ME"`
/// lines take `id`; nothing else changes, so the template's comments survive.
/// Pass `None` to get the template verbatim.
pub fn instantiate(template: &str, id: Option<&str>) -> String {
    let Some(id) = id else {
        return template.to_string();
    };
    template
        .lines()
        .map(|line| {
            let t = line.trim_start();
            if t.starts_with(&format!("id = \"{PLACEHOLDER}\""))
                || t.starts_with(&format!("name = \"{PLACEHOLDER}\""))
            {
                let key = t.split_whitespace().next().unwrap_or("id");
                let indent = &line[..line.len() - t.len()];
                format!("{indent}{key} = \"{id}\"")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if template.ends_with('\n') { "\n" } else { "" }
}

/// The [`crate::enrichment::ontology::OntologyPolicies`] a shipped template
/// declares — the one derive from a template name to the policies retrieval,
/// resolution and the prompt builders actually read.
///
/// This is the surface every ontology test uses in place of a hand-rolled
/// declaration, so no test can pass against an ontology nobody ships. Before
/// 2026-09-03 six fixtures each re-typed a subset of `numismatics` in Rust,
/// and one of them invented a `label` the template does not carry.
pub fn policies(name: &str) -> Result<crate::enrichment::ontology::OntologyPolicies> {
    Ok(crate::recipe::Recipe::from_toml(load_builtin(name)?)?
        .custom_atlas_spec()
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "built-in template '{name}' declares no [enrichment.ontology] block"
            ))
        })?
        .policies())
}

/// The shipped `numismatics` declaration, for the crate's own ontology tests.
/// Integration tests call [`policies`] directly.
#[cfg(test)]
pub(crate) fn numismatics_policies() -> crate::enrichment::ontology::OntologyPolicies {
    policies("numismatics").expect("numismatics is a shipped ontology template")
}
