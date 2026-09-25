// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-definition ports (five-programs-52).
//!
//! corpus-engine reads bundled recipes, templates, the registry catalog and
//! `@bundled:` filter assets through these two traits; [`default_source`] and
//! [`default_assets`] are the one accessor each. Today both answer from the
//! `corpus-engine-recipes` data crate via [`bundled`], the only file in
//! `src/` that names it (the structural test below holds that).

pub(crate) mod bundled;

/// Where recipe definitions come from.
pub trait RecipeSource: Send + Sync {
    /// The registry catalog TOML.
    fn registry_toml(&self) -> &'static str;
    /// Every id [`RecipeSource::recipe_toml`] answers, in catalog order.
    fn recipe_ids(&self) -> Vec<&'static str>;
    /// The recipe TOML for a catalogued id; `None` for any other id.
    fn recipe_toml(&self, id: &str) -> Option<&'static str>;
    /// Every ontology template name, sorted.
    fn template_names(&self) -> Vec<&'static str>;
    /// The recipe TOML of an ontology template.
    fn template_toml(&self, name: &str) -> Option<&'static str>;
}

/// Where `@bundled:<key>` filter data comes from.
pub trait AssetSource: Send + Sync {
    fn bundled_asset(&self, key: &str) -> Option<&'static [u8]>;
}

/// The recipe source every reader in this crate asks.
pub fn default_source() -> &'static dyn RecipeSource {
    &bundled::Bundled
}

/// The asset source every reader in this crate asks.
pub fn default_assets() -> &'static dyn AssetSource {
    &bundled::Bundled
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read_dir src") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Principle 10: the data crate is reached through the ports, so only
    /// `recipe_source/bundled.rs` may name it.
    #[test]
    fn only_bundled_names_the_data_crate() {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowed = src.join("recipe_source").join("bundled.rs");
        let this = src.join("recipe_source.rs");
        let needle = ["corpus_engine", "_recipes"].concat();
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        let offenders: Vec<_> = files
            .iter()
            .filter(|p| **p != allowed && **p != this)
            .filter(|p| std::fs::read_to_string(p).is_ok_and(|s| s.contains(&needle)))
            .map(|p| p.strip_prefix(&src).unwrap().display().to_string())
            .collect();
        assert!(
            offenders.is_empty(),
            "{needle} named outside recipe_source/bundled.rs: {offenders:?}"
        );
    }

    #[test]
    fn default_source_answers_catalogued_ids_only() {
        let source = super::default_source();
        for id in source.recipe_ids() {
            assert!(source.recipe_toml(id).is_some(), "{id} has no toml");
        }
        assert!(source.recipe_toml("maple-house").is_none());
        assert!(source.recipe_toml("does-not-exist").is_none());
    }

    #[test]
    fn default_assets_answer_every_vital_tier() {
        let assets = super::default_assets();
        for tier in 1..=5 {
            let key = format!("vital_articles_l{tier}");
            assert!(
                assets.bundled_asset(&key).is_some_and(|b| !b.is_empty()),
                "{key}"
            );
        }
        assert!(assets.bundled_asset("vital_articles_l6").is_none());
    }
}
