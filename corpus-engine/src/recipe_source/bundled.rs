// SPDX-License-Identifier: AGPL-3.0-or-later
//! The default recipe and asset source: the `corpus-engine-recipes` data
//! crate, compiled in. `recipe_toml` answers `RECIPES` only, never
//! `UNCATALOGED` — the registry fallback resolves any id it is handed.

use corpus_engine_recipes::{RECIPES, TEMPLATES};

pub use corpus_engine_recipes::{
    REGISTRY_TOML, VITAL_ARTICLES_L1, VITAL_ARTICLES_L2, VITAL_ARTICLES_L3, VITAL_ARTICLES_L4,
    VITAL_ARTICLES_L5,
};

use super::{AssetSource, RecipeSource};

pub(super) struct Bundled;

fn lookup<T: Copy>(table: &[(&str, T)], key: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

impl RecipeSource for Bundled {
    fn registry_toml(&self) -> &'static str {
        REGISTRY_TOML
    }

    fn recipe_ids(&self) -> Vec<&'static str> {
        RECIPES.iter().map(|(id, _)| *id).collect()
    }

    fn recipe_toml(&self, id: &str) -> Option<&'static str> {
        lookup(RECIPES, id)
    }

    fn template_names(&self) -> Vec<&'static str> {
        TEMPLATES.iter().map(|(name, _)| *name).collect()
    }

    fn template_toml(&self, name: &str) -> Option<&'static str> {
        lookup(TEMPLATES, name)
    }
}

/// The `@bundled:<key>` table, keys as filter configs spell them.
const ASSETS: &[(&str, &[u8])] = {
    use corpus_engine_recipes as data;
    &[
        ("vital_articles_l1", data::VITAL_ARTICLES_L1),
        ("vital_articles_l2", data::VITAL_ARTICLES_L2),
        ("vital_articles_l3", data::VITAL_ARTICLES_L3),
        ("vital_articles_l4", data::VITAL_ARTICLES_L4),
        ("vital_articles_l5", data::VITAL_ARTICLES_L5),
    ]
};

impl AssetSource for Bundled {
    fn bundled_asset(&self, key: &str) -> Option<&'static [u8]> {
        lookup(ASSETS, key)
    }
}
