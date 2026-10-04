// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bundled recipe definitions as static data (five-programs-52).
//!
//! Every list is hand-written, one line per directory; the tree test below
//! names any directory a list is missing, and any entry with no directory.

macro_rules! recipe {
    ($id:literal) => {
        ($id, include_str!(concat!("../", $id, "/recipe.toml")))
    };
}

macro_rules! template {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../_templates/ontology-v1/", $name, "/recipe.toml")),
        )
    };
}

/// The catalogued recipes: exactly the ids `registry.toml` lists.
pub const RECIPES: &[(&str, &str)] = &[
    recipe!("wikipedia"),
    recipe!("wikipedia-simple"),
    recipe!("stackexchange"),
    recipe!("stackexchange-knowledge"),
    recipe!("openalex"),
    recipe!("gutenberg"),
    recipe!("gutenberg-work"),
    recipe!("wikipedia-catalog"),
    recipe!("wikipedia-article"),
    recipe!("wikipedia-newsworthy"),
    recipe!("sep"),
    recipe!("crs_reports"),
    recipe!("federal-register-presidential"),
    recipe!("us-code"),
    recipe!("olc-opinions"),
    recipe!("scotus-opinions"),
    recipe!("conversations-anthropic"),
    recipe!("conversations-chatgpt"),
    recipe!("enron-sample"),
    recipe!("enron-sample-onemailbox"),
    recipe!("enron-sample-tiny"),
    recipe!("enron-sample-multi-tiny"),
    recipe!("enron-sample-multi-wide"),
    recipe!("uap-blue-book"),
    recipe!("uap-blue-book-scans"),
    recipe!("uap-blue-book-index"),
    recipe!("email-archive"),
    recipe!("brothers-karamazov-book-1"),
    recipe!("sec-filings-company"),
    recipe!("federalist-starter"),
];

/// Recipe directories outside the catalog. Kept apart from [`RECIPES`]: the
/// registry's offline fallback resolves any id it is handed, so bundling
/// these there would change what an offline lookup answers.
pub const UNCATALOGED: &[(&str, &str)] = &[
    recipe!("agent-sessions"),
    recipe!("agent-sessions-atlas"),
    recipe!("arch-principles"),
    recipe!("chaos-saltgrass"),
    recipe!("chaos-secret-agent"),
    recipe!("climb-ostrom"),
    recipe!("codebase"),
    recipe!("maple-house"),
    recipe!("proxy-company"),
    recipe!("sf-assessor-roll"),
    recipe!("system-overview"),
    recipe!("wessex-hoard"),
];

/// `<id>/truth.json` for every recipe that carries one.
pub const TRUTH: &[(&str, &str)] = &[
    ("maple-house", include_str!("../maple-house/truth.json")),
    ("wessex-hoard", include_str!("../wessex-hoard/truth.json")),
];

/// The `_templates/ontology-v1/` templates, sorted by name.
pub const TEMPLATES: &[(&str, &str)] = &[
    template!("contracts"),
    template!("due-diligence"),
    template!("engineering-org"),
    template!("governance"),
    template!("literary"),
    template!("materials-lab"),
    template!("numismatics"),
    template!("patient-community"),
    template!("product-support"),
    template!("research-notebook"),
];

/// The registry catalog.
pub const REGISTRY_TOML: &str = include_str!("../registry.toml");

pub const VITAL_ARTICLES_L1: &[u8] = include_bytes!("../wikipedia/data/vital_articles_l1.txt");
pub const VITAL_ARTICLES_L2: &[u8] = include_bytes!("../wikipedia/data/vital_articles_l2.txt");
pub const VITAL_ARTICLES_L3: &[u8] = include_bytes!("../wikipedia/data/vital_articles_l3.txt");
pub const VITAL_ARTICLES_L4: &[u8] = include_bytes!("../wikipedia/data/vital_articles_l4.txt");
pub const VITAL_ARTICLES_L5: &[u8] = include_bytes!("../wikipedia/data/vital_articles_l5.txt");

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn dirs_holding(parent: &Path, file: &str) -> BTreeSet<String> {
        std::fs::read_dir(parent)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", parent.display()))
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.join(file).is_file())
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn ids(list: &[(&str, &str)]) -> BTreeSet<String> {
        list.iter().map(|(id, _)| id.to_string()).collect()
    }

    fn assert_same(what: &str, listed: &BTreeSet<String>, on_disk: &BTreeSet<String>) {
        let missing: Vec<_> = on_disk.difference(listed).collect();
        let extra: Vec<_> = listed.difference(on_disk).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{what}: missing from src/lib.rs {missing:?}; listed with no directory {extra:?}"
        );
    }

    #[test]
    fn every_recipe_dir_is_listed_once() {
        let recipes = ids(RECIPES);
        let uncataloged = ids(UNCATALOGED);
        let both: Vec<_> = recipes.intersection(&uncataloged).collect();
        assert!(both.is_empty(), "in RECIPES and UNCATALOGED: {both:?}");
        let listed = recipes.union(&uncataloged).cloned().collect();
        assert_same(
            "RECIPES ∪ UNCATALOGED",
            &listed,
            &dirs_holding(&root(), "recipe.toml"),
        );
    }

    #[test]
    fn recipes_are_the_registry_catalog() {
        let registry: toml::Value = toml::from_str(REGISTRY_TOML).expect("registry.toml parses");
        let catalog = registry["recipes"]
            .as_array()
            .expect("[[recipes]]")
            .iter()
            .map(|r| r["id"].as_str().expect("recipe id").to_string())
            .collect();
        assert_same("RECIPES vs registry.toml", &ids(RECIPES), &catalog);
    }

    #[test]
    fn every_truth_json_is_listed() {
        assert_same("TRUTH", &ids(TRUTH), &dirs_holding(&root(), "truth.json"));
    }

    #[test]
    fn every_ontology_template_is_listed_sorted() {
        let names: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "TEMPLATES must be sorted by name");
        assert_same(
            "TEMPLATES",
            &ids(TEMPLATES),
            &dirs_holding(&root().join("_templates/ontology-v1"), "recipe.toml"),
        );
    }
}
