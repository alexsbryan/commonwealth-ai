//! `sovereign enrich sep-ingest <category>` — scaffold a per-article
//! SEP enrichment corpus from the cached parquet.
//!
//! ## Flow
//!
//!   1. Read the SEP parquet at
//!      `~/.sovereign/indexes/_downloads/sep.parquet` (downloaded
//!      via `sovereign corpus acquire sep`).
//!   2. Filter rows to the chosen `<category>` slug and render as
//!      plaintext with `## Section NNN` markers (groups
//!      `paragraphs_per_section` paragraphs per section).
//!   3. Write the markdown to
//!      `~/.sovereign/corpora/sep/articles/<category>.md`.
//!   4. Delegate to `enrich_cmd::init::cmd_init` with the rendered
//!      file as the source and `pipeline = philosophy_atlas`.
//!
//! The result is an enrichment corpus named `sep-<category>` ready
//! for `sovereign enrich build sep-<category>`. The split between
//! this helper and `enrich init` keeps section detection, config
//! writing, and manifest scaffolding in one place.

use std::fs;
use std::path::PathBuf;

use corpus_engine::enrichment::sep::{list_categories, load_article};

use crate::util::dirs::{sovereign_indexes, sovereign_root};
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich sep-ingest",
    summary: "Scaffold an enrichment corpus for one SEP article, ready for `enrich build`.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich sep-ingest <category-slug> \
             [--paragraphs-per-section N] [--parquet <path>] [--list] [--force]",
        ),
        HelpSection::Flags(&[
            (
                "--paragraphs-per-section N",
                "Group N parquet paragraphs into one atlas section (default: 10). \
                 Tuning note: the compatibilism smoke at N=5 produced 19 sections = \
                 19 Phase-1 LLM calls; N=10 halves that while keeping enough internal \
                 structure for the trajectory + configuration phases to have signal.",
            ),
            (
                "--parquet <path>",
                "Override the SEP parquet path (default: \
                 ~/.sovereign/indexes/_downloads/sep.parquet).",
            ),
            (
                "--list",
                "List every category slug in the parquet with its paragraph count.",
            ),
            (
                "--force",
                "Overwrite an existing `~/.sovereign/corpora/sep/articles/<slug>.md` \
                 and re-init the enrichment corpus.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich sep-ingest compatibilism",
                "Scaffold `sep-compatibilism` with 5 paragraphs per section.",
            ),
            (
                "sovereign enrich sep-ingest recursive-functions --paragraphs-per-section 10",
                "Coarser sectioning for long articles (542 paragraphs → ~55 sections).",
            ),
            (
                "sovereign enrich sep-ingest --list | head -20",
                "Pick a category slug from the parquet.",
            ),
        ]),
        HelpSection::Notes(
            "Requires the SEP parquet cached locally. Acquire it with \
             `sovereign corpus acquire sep` (downloads ~1 GB from HuggingFace).",
        ),
    ],
};

pub async fn cmd_sep_ingest(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let parquet_path = parsed
        .parquet_override
        .unwrap_or_else(default_parquet_path);
    if !parquet_path.exists() {
        eprintln!(
            "error: SEP parquet not found at {}.\n       \
             Run `sovereign corpus acquire sep` to download it (~1 GB).",
            parquet_path.display()
        );
        return 1;
    }

    // `--list` mode: dump category slugs + paragraph counts and exit.
    if parsed.list {
        match list_categories(&parquet_path) {
            Ok(cats) => {
                println!(
                    "  {} article(s) in {}",
                    cats.len(),
                    parquet_path.display()
                );
                for (slug, n) in &cats {
                    println!("    {:>4}  {}", n, slug);
                }
                return 0;
            }
            Err(e) => {
                eprintln!("error: listing categories: {e}");
                return 1;
            }
        }
    }

    let Some(slug) = parsed.slug else {
        eprintln!("error: missing <category-slug> (or pass --list to see options)");
        eprintln!();
        help::print(&HELP);
        return 2;
    };

    // Defensive slug clamp. The slug becomes part of the corpus
    // id (`sep-<slug>`) and the file name at
    // `~/.sovereign/corpora/sep/articles/<slug>.md`. The cached
    // parquet's `category` column is mostly lowercase ASCII +
    // digits + hyphens but includes a few mixed-case names
    // (e.g. `18thGerman-preKant`, `emotion-Christian-tradition`).
    // Accept either case here so `--list` and the consumer agree.
    if !is_valid_sep_slug(&slug) {
        eprintln!(
            "error: invalid SEP slug `{slug}`: slugs must match `[A-Za-z0-9-]+` \
             (ASCII letters, digits, or hyphens only, 1-64 chars)."
        );
        eprintln!();
        eprintln!(
            "Hint: run `sovereign enrich sep-ingest --list` to see valid slugs."
        );
        return 2;
    }

    // Load the article from the parquet.
    let article = match load_article(&parquet_path, &slug) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!(
                "Hint: run `sovereign enrich sep-ingest --list` to see available slugs."
            );
            return 1;
        }
    };
    let section_count = article.section_count(parsed.paragraphs_per_section);
    println!(
        "  loaded `{}` — {} paragraph(s) → {} section(s) at {} paragraphs/section",
        slug,
        article.paragraphs.len(),
        section_count,
        parsed.paragraphs_per_section
    );

    // Write the rendered markdown to `~/.sovereign/corpora/sep/articles/<slug>.md`.
    let articles_dir = sovereign_root().join("corpora").join("sep").join("articles");
    if let Err(e) = fs::create_dir_all(&articles_dir) {
        eprintln!(
            "error: creating {}: {e}",
            articles_dir.display()
        );
        return 1;
    }
    let article_path = articles_dir.join(format!("{slug}.md"));
    if article_path.exists() && !parsed.force {
        eprintln!(
            "error: {} already exists. Re-run with --force to overwrite.",
            article_path.display()
        );
        return 1;
    }
    let markdown = article.render_markdown(parsed.paragraphs_per_section);
    if let Err(e) = fs::write(&article_path, &markdown) {
        eprintln!(
            "error: writing {}: {e}",
            article_path.display()
        );
        return 1;
    }
    println!("  ✓ wrote {}", article_path.display());

    // Delegate to `enrich init` to scaffold config + manifest. The
    // chapter regex matches `## Section NNN` — the shape the SEP
    // render_markdown emits. `philosophy_atlas` is the locked-in
    // pipeline for SEP per the recipe.
    let corpus_id = format!("sep-{slug}");
    let mut init_args = vec![
        corpus_id.clone(),
        "--source".into(),
        article_path.to_string_lossy().into_owned(),
        "--pipeline".into(),
        "philosophy_atlas".into(),
        "--chapter-regex".into(),
        r"(?m)^## Section \d+$".into(),
    ];
    if parsed.force {
        init_args.push("--force".into());
    }
    let init_code = super::init::cmd_init(&init_args).await;
    if init_code != 0 {
        eprintln!("error: `enrich init` failed with exit code {init_code}");
        return init_code;
    }

    // Stash the article metadata next to the corpus so a future
    // `enrich sep-refresh` can re-extract from the parquet without
    // re-asking the operator for the slug.
    let metadata_path = super::paths::enrichment_root(&corpus_id).join("sep_article.json");
    let metadata = SepArticleMetadata {
        slug: article.slug.clone(),
        url: article.url.clone(),
        paragraph_count: article.paragraphs.len(),
        paragraphs_per_section: parsed.paragraphs_per_section,
        section_count,
        parquet_path: parquet_path.clone(),
    };
    if let Err(e) = fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&metadata).unwrap_or_else(|_| "{}".into()),
    ) {
        // Not fatal — the corpus is scaffolded, this is just a
        // re-ingest convenience.
        eprintln!(
            "warning: could not write {}: {e}",
            metadata_path.display()
        );
    }

    println!();
    println!(
        "  Next: sovereign enrich build {corpus_id}"
    );
    0
}

/// Same validator as the desktop's `is_valid_sep_slug`; kept in
/// both places so the CLI path can refuse malformed input without
/// depending on the desktop crate (the CLI is the lower layer).
fn is_valid_sep_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > 64 {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '-')
}

fn default_parquet_path() -> PathBuf {
    // `sovereign corpus acquire sep` drops the parquet here. See
    // `recipes/sep/recipe.toml` for the URL.
    sovereign_indexes().join("_downloads").join("sep.parquet")
}

/// Sidecar metadata written to
/// `~/.sovereign/enrichment/sep-<slug>/sep_article.json`. Not read
/// by the pipeline itself — purely an operator aid for
/// post-hoc inspection and future `sep-refresh` workflows.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SepArticleMetadata {
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub paragraph_count: usize,
    pub paragraphs_per_section: usize,
    pub section_count: usize,
    pub parquet_path: PathBuf,
}

#[derive(Debug)]
struct ParsedSepIngest {
    slug: Option<String>,
    paragraphs_per_section: usize,
    parquet_override: Option<PathBuf>,
    list: bool,
    force: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedSepIngest, String> {
    let mut slug: Option<String> = None;
    let mut paragraphs_per_section: usize = 10;
    let mut parquet_override: Option<PathBuf> = None;
    let mut list = false;
    let mut force = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--paragraphs-per-section" => {
                let v = args.get(i + 1).ok_or_else(|| {
                    "--paragraphs-per-section requires an integer".to_string()
                })?;
                paragraphs_per_section = v
                    .parse()
                    .map_err(|e| format!("--paragraphs-per-section: {e}"))?;
                if paragraphs_per_section == 0 {
                    return Err(
                        "--paragraphs-per-section must be ≥ 1".into(),
                    );
                }
                i += 2;
            }
            "--parquet" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--parquet requires a path".to_string())?;
                parquet_override = Some(PathBuf::from(v));
                i += 2;
            }
            "--list" => {
                list = true;
                i += 1;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if slug.is_none() {
                    slug = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!(
                        "unexpected positional argument: {other}"
                    ));
                }
            }
        }
    }
    Ok(ParsedSepIngest {
        slug,
        paragraphs_per_section,
        parquet_override,
        list,
        force,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_defaults_are_sensible() {
        let p = parse_args(&["compatibilism".into()]).unwrap();
        assert_eq!(p.slug.as_deref(), Some("compatibilism"));
        // 10 paragraphs/section is the tuned default — compatibilism
        // becomes ~10 sections (from 97 paragraphs), keeping Phase 1
        // LLM calls cheap while preserving internal argument
        // structure. Changing this needs a paired update in the SEP
        // recipe toml.
        assert_eq!(p.paragraphs_per_section, 10);
        assert!(!p.list);
        assert!(!p.force);
        assert!(p.parquet_override.is_none());
    }

    #[test]
    fn parse_args_accepts_all_flags() {
        let p = parse_args(&[
            "recursive-functions".into(),
            "--paragraphs-per-section".into(),
            "10".into(),
            "--parquet".into(),
            "/tmp/sep.parquet".into(),
            "--force".into(),
        ])
        .unwrap();
        assert_eq!(p.slug.as_deref(), Some("recursive-functions"));
        assert_eq!(p.paragraphs_per_section, 10);
        assert_eq!(
            p.parquet_override.as_deref(),
            Some(std::path::Path::new("/tmp/sep.parquet"))
        );
        assert!(p.force);
    }

    #[test]
    fn parse_args_list_mode_doesnt_need_slug() {
        let p = parse_args(&["--list".into()]).unwrap();
        assert!(p.list);
        assert!(p.slug.is_none());
    }

    #[test]
    fn parse_args_rejects_zero_paragraphs_per_section() {
        let err = parse_args(&[
            "x".into(),
            "--paragraphs-per-section".into(),
            "0".into(),
        ])
        .unwrap_err();
        assert!(err.contains(">= 1") || err.contains("≥ 1"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["x".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn sep_slug_validator_accepts_real_plato_slugs() {
        // Sampled from the parquet's `category` column — these
        // are the shapes the validator needs to pass. Includes
        // the 5 mixed-case slugs found in the upstream parquet
        // (validator was strict-lowercase originally, but `--list`
        // and the consumer disagreed → 1770 ingest had 5 failures).
        assert!(is_valid_sep_slug("compatibilism"));
        assert!(is_valid_sep_slug("recursive-functions"));
        assert!(is_valid_sep_slug("abner-burgos"));
        assert!(is_valid_sep_slug("18thGerman-preKant"));
        assert!(is_valid_sep_slug("emotion-Christian-tradition"));
        assert!(is_valid_sep_slug("equivME"));
        assert!(is_valid_sep_slug("physics-Rpcc"));
        assert!(is_valid_sep_slug("statphys-Boltzmann"));
    }

    #[test]
    fn sep_slug_validator_rejects_path_traversal_and_weird_chars() {
        // The slug becomes part of a file path; rejecting these
        // shapes avoids reasoning about symlinks / traversal in
        // the downstream writer.
        assert!(!is_valid_sep_slug("../etc/passwd"));
        assert!(!is_valid_sep_slug("slug with spaces"));
        assert!(!is_valid_sep_slug("slug/with/slashes"));
        assert!(!is_valid_sep_slug("unicode-ümlaut"));
        assert!(!is_valid_sep_slug(""));
        assert!(!is_valid_sep_slug(&"a".repeat(65)));
    }
}
