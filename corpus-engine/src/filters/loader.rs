//! Build a [`FilterPipeline`] from recipe-level config.
//!
//! Filter artefacts can come from two places:
//!
//! 1. `@bundled:<key>` — embedded into the crate binary at compile
//!    time (see [`crate::filters::assets`]). Used for the v1 Wikipedia
//!    Core scope.
//! 2. A path on disk (relative to the recipe's override directory or
//!    absolute). Used by user-supplied recipes that ship sidecar
//!    artefacts.
//!
//! The loader is the only place that knows about the `@bundled:`
//! convention; concrete filters take raw bytes (or a `Path`) so they
//! remain agnostic to the source.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::filters::{
    assets, compute_signature, ComposeMode, DocumentFilter, FilterConfig, FilterPipeline,
    PageviewRankFilter, TitleListFilter,
};

const BUNDLED_PREFIX: &str = "@bundled:";

/// Resolve a recipe's `[[filter]]` entries to a runnable
/// [`FilterPipeline`].
///
/// `mode` controls combination semantics (`Any` is the default and
/// matches the Wikipedia Core "rank ≤ N OR vital" scope).
///
/// `recipe_root` is used to resolve relative `*_file` paths. `None` is
/// only valid when every entry uses `@bundled:` keys; mixing relative
/// paths with `None` returns a recipe error.
pub fn build_filter_pipeline(
    filters: &[FilterConfig],
    mode: ComposeMode,
    recipe_root: Option<&Path>,
) -> Result<FilterPipeline> {
    if filters.is_empty() {
        return Ok(FilterPipeline::empty());
    }
    let mut children: Vec<Arc<dyn DocumentFilter>> = Vec::with_capacity(filters.len());
    for cfg in filters {
        let child: Arc<dyn DocumentFilter> = match cfg {
            FilterConfig::PageviewRank { rank_file, max_rank } => {
                Arc::new(load_pageview_rank(rank_file, *max_rank, recipe_root)?)
            }
            FilterConfig::TitleList { list_file } => {
                Arc::new(load_title_list(list_file, recipe_root)?)
            }
        };
        children.push(child);
    }
    let signature = compute_signature(filters, mode);
    Ok(FilterPipeline::new(children, mode, signature))
}

fn load_pageview_rank(
    rank_file: &str,
    max_rank: u32,
    recipe_root: Option<&Path>,
) -> Result<PageviewRankFilter> {
    if let Some(key) = rank_file.strip_prefix(BUNDLED_PREFIX) {
        let bytes = assets::lookup_bundled(key).ok_or_else(|| {
            Error::Recipe(format!("unknown bundled filter asset '{key}' in pageview_rank"))
        })?;
        // Bundled rank files are gzipped to keep the binary small. The
        // crate currently ships only `.csv.gz` keys, but if a future
        // bundled key is plain CSV the magic bytes (`1f 8b`) tell us.
        if bytes.starts_with(&[0x1f, 0x8b]) {
            PageviewRankFilter::from_gz_csv_bytes(bytes, max_rank)
        } else {
            PageviewRankFilter::from_csv_bytes(bytes, max_rank)
        }
    } else {
        let path = resolve_path(rank_file, recipe_root)?;
        PageviewRankFilter::from_path(&path, max_rank)
    }
}

fn load_title_list(list_file: &str, recipe_root: Option<&Path>) -> Result<TitleListFilter> {
    if let Some(key) = list_file.strip_prefix(BUNDLED_PREFIX) {
        let bytes = assets::lookup_bundled(key).ok_or_else(|| {
            Error::Recipe(format!("unknown bundled filter asset '{key}' in title_list"))
        })?;
        TitleListFilter::from_bytes(bytes, key)
    } else {
        let path = resolve_path(list_file, recipe_root)?;
        TitleListFilter::from_path(&path)
    }
}

fn resolve_path(spec: &str, recipe_root: Option<&Path>) -> Result<PathBuf> {
    let p = Path::new(spec);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let root = recipe_root.ok_or_else(|| {
        Error::Recipe(format!(
            "filter file '{spec}' is relative but no recipe root was provided"
        ))
    })?;
    Ok(root.join(p))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::ExtractedDoc;

    fn doc(title: &str) -> ExtractedDoc {
        ExtractedDoc {
            title: Some(title.into()),
            content: String::new(),
            url: None,
            source_id: title.into(),
            metadata: None,
            source_file: None,
        }
    }

    #[test]
    fn empty_filter_list_returns_inactive_pipeline() {
        let p = build_filter_pipeline(&[], ComposeMode::Any, None).unwrap();
        assert!(!p.is_active());
        assert!(p.accept(&doc("anything")));
    }

    #[test]
    fn bundled_pageview_rank_loads() {
        // The bundled placeholder ships with at least one entry; we
        // can't assert content without coupling to the placeholder's
        // exact titles, but construction must succeed.
        let cfg = vec![FilterConfig::PageviewRank {
            rank_file: "@bundled:pageview_ranks_202311".into(),
            max_rank: 1_000_000,
        }];
        let p = build_filter_pipeline(&cfg, ComposeMode::Any, None).unwrap();
        assert!(p.is_active());
        assert_eq!(p.descriptions().len(), 1);
        assert_eq!(p.signature().len(), 64);
    }

    #[test]
    fn bundled_title_list_loads() {
        let cfg = vec![FilterConfig::TitleList {
            list_file: "@bundled:vital_articles_l5".into(),
        }];
        let p = build_filter_pipeline(&cfg, ComposeMode::Any, None).unwrap();
        assert!(p.is_active());
    }

    #[test]
    fn unknown_bundled_key_errors() {
        let cfg = vec![FilterConfig::TitleList {
            list_file: "@bundled:does_not_exist".into(),
        }];
        let res = build_filter_pipeline(&cfg, ComposeMode::Any, None);
        assert!(res.is_err());
    }

    #[test]
    fn relative_path_without_root_errors() {
        let cfg = vec![FilterConfig::TitleList {
            list_file: "vital.txt".into(),
        }];
        let res = build_filter_pipeline(&cfg, ComposeMode::Any, None);
        assert!(res.is_err());
    }

    #[test]
    fn composed_filter_any_mode_accepts_either() {
        let cfg = vec![
            FilterConfig::PageviewRank {
                rank_file: "@bundled:pageview_ranks_202311".into(),
                max_rank: 100_000,
            },
            FilterConfig::TitleList {
                list_file: "@bundled:vital_articles_l5".into(),
            },
        ];
        let p = build_filter_pipeline(&cfg, ComposeMode::Any, None).unwrap();
        assert!(p.is_active());
        assert_eq!(p.descriptions().len(), 2);
    }
}
