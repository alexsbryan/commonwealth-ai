// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in philosophy templates for `sovereign enrich init
//! --from-template <name>`.
//!
//! Each template is a self-contained TOML fixture: meta block plus a
//! list of `[[chapters]]`. When `enrich init` is invoked with
//! `--from-template`, the loader materialises the chapters into a
//! single plaintext file with `## <title>` headings and feeds it to
//! the existing init flow. From the rest of the pipeline's
//! perspective, a template-seeded corpus is indistinguishable from
//! one initialised against a real plaintext source.
//!
//! Why TOML over Rust constants: per ARCH §6 ("data ≠ program"),
//! corpus templates and golden sets are data — checked in next to the
//! consumer, easy to diff and edit, and the same files double as
//! reference fixtures for `enrich eval`.
//!
//! Schema:
//!
//! ```toml
//! [meta]
//! name = "free-will-debate"
//! description = "..."
//! domain = "philosophy"
//! pipeline_id = "philosophy_atlas"
//!
//! [[chapters]]
//! title = "Introduction"
//! body = """
//! Long prose body...
//! """
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub meta: TemplateMeta,
    #[serde(default)]
    pub chapters: Vec<TemplateChapter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_domain")]
    pub domain: String,
    #[serde(default = "default_pipeline_id")]
    pub pipeline_id: String,
}

fn default_domain() -> String {
    "philosophy".to_string()
}

fn default_pipeline_id() -> String {
    "philosophy_atlas".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateChapter {
    pub title: String,
    pub body: String,
}

/// Built-in template registry. Adding a template is a two-step
/// commit: drop `<name>.toml` next to this module and append a row
/// here. `load_builtin(name)` parses on demand.
const BUILTINS: &[(&str, &str)] = &[
    ("free-will-debate", include_str!("free-will-debate.toml")),
    (
        "virtue-ethics-fragments",
        include_str!("virtue-ethics-fragments.toml"),
    ),
    ("stoicism-mini", include_str!("stoicism-mini.toml")),
    ("bk-book-1", include_str!("bk-book-1.toml")),
    ("dubliners-3", include_str!("dubliners-3.toml")),
];

/// Section-detection regex paired with the chapter materialiser. A
/// line whose first non-whitespace tokens are `##` followed by
/// whitespace marks the start of a chapter. Body prose contains no
/// such lines (verified in unit tests).
pub const CHAPTER_REGEX: &str = r"(?m)^##\s+";

pub fn list_builtin_names() -> Vec<&'static str> {
    BUILTINS.iter().map(|(n, _)| *n).collect()
}

pub fn load_builtin(name: &str) -> Result<Template, String> {
    let body = BUILTINS
        .iter()
        .find_map(|(n, body)| if *n == name { Some(*body) } else { None })
        .ok_or_else(|| {
            format!(
                "no built-in template '{name}' (available: {})",
                list_builtin_names().join(", ")
            )
        })?;
    parse_template(body).map_err(|e| format!("parse {name}.toml: {e}"))
}

pub fn load_from_path(path: &std::path::Path) -> Result<Template, String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_template(&body).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn parse_template(body: &str) -> Result<Template, toml::de::Error> {
    toml::from_str::<Template>(body)
}

/// Materialise a parsed template into the plaintext shape `enrich
/// init`'s downstream pipeline expects. Each chapter becomes
/// `## <title>\n\n<body>\n\n`; the header line matches `CHAPTER_REGEX`.
pub fn materialise_to_plaintext(t: &Template) -> String {
    let mut out = String::new();
    for ch in &t.chapters {
        out.push_str("## ");
        out.push_str(ch.title.trim());
        out.push_str("\n\n");
        out.push_str(ch.body.trim());
        out.push_str("\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_will_debate_parses() {
        let t = load_builtin("free-will-debate").expect("free-will-debate.toml should parse");
        assert_eq!(t.meta.name, "free-will-debate");
        assert_eq!(t.meta.pipeline_id, "philosophy_atlas");
        assert!(
            t.chapters.len() >= 5,
            "free-will-debate must have at least 5 chapters"
        );
    }

    #[test]
    fn virtue_ethics_fragments_parses() {
        load_builtin("virtue-ethics-fragments").expect("virtue-ethics-fragments.toml should parse");
    }

    #[test]
    fn stoicism_mini_parses() {
        load_builtin("stoicism-mini").expect("stoicism-mini.toml should parse");
    }

    #[test]
    fn bk_book_1_parses() {
        let t = load_builtin("bk-book-1").expect("bk-book-1.toml should parse");
        assert_eq!(t.meta.pipeline_id, "literary_atlas");
        assert_eq!(
            t.chapters.len(),
            5,
            "bk-book-1 must have 5 chapters (Book I of Brothers Karamazov)"
        );
    }

    #[test]
    fn dubliners_3_parses() {
        let t = load_builtin("dubliners-3").expect("dubliners-3.toml should parse");
        assert_eq!(t.meta.pipeline_id, "literary_atlas");
        assert_eq!(
            t.chapters.len(),
            3,
            "dubliners-3 must have 3 stories (Sisters / Araby / Eveline)"
        );
    }

    #[test]
    fn unknown_template_lists_known_names() {
        let err = load_builtin("nonexistent").unwrap_err();
        assert!(err.contains("free-will-debate"), "err: {err}");
    }

    /// Count lines of `text` whose first non-empty content is `## `.
    /// This is the runtime semantics of `CHAPTER_REGEX` without taking
    /// a regex dep into this crate.
    fn count_chapter_headers(text: &str) -> usize {
        text.lines().filter(|l| l.starts_with("## ")).count()
    }

    #[test]
    fn materialised_text_starts_each_chapter_with_marker() {
        let t = load_builtin("free-will-debate").unwrap();
        let text = materialise_to_plaintext(&t);
        assert_eq!(
            count_chapter_headers(&text),
            t.chapters.len(),
            "every chapter title should appear exactly once at the start of a line"
        );
    }

    #[test]
    fn no_chapter_body_contains_chapter_marker() {
        // If a chapter body included a line beginning with `## `, the
        // section detector would split that chapter mid-body. Catch
        // any such authoring slip here so adding a new template can
        // never silently corrupt the section count.
        for name in list_builtin_names() {
            let t = load_builtin(name).unwrap();
            for ch in &t.chapters {
                assert_eq!(
                    count_chapter_headers(&ch.body),
                    0,
                    "template {} chapter {:?} body contains a `## ` marker — \
                     would split the section",
                    name,
                    ch.title
                );
            }
        }
    }
}
