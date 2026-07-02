// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generates `sovereign-recipes/SCHEMA.md` from the recipe types' AST and
//! gates it against drift.
//!
//! The recipe schema is defined once, in Rust (`src/recipe.rs` + the filter
//! config types). This test parses those source files with `syn`, walks every
//! `Deserialize`-deriving struct/enum, and renders a Markdown reference from
//! the field names, `#[serde(...)]` wire keys, types, and the `///` doc
//! comments the authors already wrote. A new recipe field therefore cannot
//! ship undocumented: the rendered output won't match the committed
//! `SCHEMA.md` and this test fails.
//!
//! Regenerate after an intentional schema change:
//!
//! ```text
//! UPDATE_RECIPE_SCHEMA=1 cargo test -p corpus-engine --test recipe_schema
//! ```
//!
//! Dev-only: `syn`/`quote` are dev-dependencies; the recipe types carry no
//! doc-gen derives and there is no runtime cost.

use quote::ToTokens;
use std::path::PathBuf;
use syn::{Attribute, Fields, Item};

/// Source files parsed for recipe-facing config types, in render order.
const SOURCES: &[&str] = &[
    "src/recipe.rs",
    "src/filters/mod.rs",
    "src/filters/boilerplate.rs",
    "src/filters/knowledge_density.rs",
];

/// Deserialize-deriving types that are NOT recipe-TOML surface (runtime
/// state, internal helpers). Excluded from the reference.
const SKIP_TYPES: &[&str] = &["ResolvedParameters", "ParameterValue"];

const HEADER: &str = "# Recipe schema reference\n\
\n\
> **Generated** from `corpus-engine/src/recipe.rs` (+ the filter config types) by\n\
> the `recipe_schema` test. Do not edit by hand — regenerate with\n\
> `UPDATE_RECIPE_SCHEMA=1 cargo test -p corpus-engine --test recipe_schema`.\n\
>\n\
> This is the authoritative field list. The strings in the **TOML key** columns\n\
> are exactly what a recipe author writes. See `GETTING_STARTED.md` for a\n\
> walkthrough and `_templates/` for a copy-paste starting point.\n\
\n\
A recipe is a TOML file with these top-level sections, threaded through the\n\
acquire → extract → filter → chunk → embed → index pipeline:\n\
\n\
- `[corpus]` — identity + catalog metadata (`CorpusMeta`)\n\
- `[acquire]` — where the raw bytes come from (`AcquirerConfig`, tagged by `type`)\n\
- `[extract]` — raw bytes → documents (`ExtractorConfig`, tagged by `type`)\n\
- `[[filter]]` — optional document filters (`FilterConfig`, tagged by `type`)\n\
- `[chunk]` — documents → chunks (`ChunkerConfig`, tagged by `type`)\n\
- `[index]` — FTS + vector index settings (`IndexConfig`)\n\
- `[enrichment]` — optional atlas/field-model enrichment (`EnrichmentConfig`)\n\
- `[prebuilt]`, `[update]`, `[catalog]`, `[parameters]` — optional advanced blocks\n\
\n\
---\n";

#[test]
fn recipe_schema_is_fresh() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut md = String::from(HEADER);

    for rel in SOURCES {
        let path = manifest.join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let file =
            syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        for item in &file.items {
            match item {
                Item::Struct(s) if is_deserialize(&s.attrs) && !skipped(&s.ident.to_string()) => {
                    md.push_str(&render_struct(s));
                }
                Item::Enum(e) if is_deserialize(&e.attrs) && !skipped(&e.ident.to_string()) => {
                    md.push_str(&render_enum(e));
                }
                _ => {}
            }
        }
    }

    let out_path = manifest.join("../sovereign-recipes/SCHEMA.md");
    if std::env::var("UPDATE_RECIPE_SCHEMA").is_ok() {
        std::fs::write(&out_path, &md).expect("write SCHEMA.md");
        eprintln!("wrote {}", out_path.display());
        return;
    }

    let committed = std::fs::read_to_string(&out_path).unwrap_or_default();
    if committed != md {
        let (cl, gl) = (committed.lines().count(), md.lines().count());
        panic!(
            "sovereign-recipes/SCHEMA.md is stale ({cl} committed lines vs {gl} generated).\n\
             The recipe types changed — regenerate with:\n  \
             UPDATE_RECIPE_SCHEMA=1 cargo test -p corpus-engine --test recipe_schema\n\
             {}",
            first_diff(&committed, &md)
        );
    }
}

fn skipped(name: &str) -> bool {
    SKIP_TYPES.contains(&name)
}

fn is_deserialize(attrs: &[Attribute]) -> bool {
    derives(attrs).iter().any(|d| d == "Deserialize")
}

fn derives(attrs: &[Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for a in attrs {
        if a.path().is_ident("derive") {
            let _ = a.parse_nested_meta(|m| {
                if let Some(id) = m.path.get_ident() {
                    out.push(id.to_string());
                }
                Ok(())
            });
        }
    }
    out
}

/// Concatenated `///` doc text, collapsed to a single line for table cells.
fn doc(attrs: &[Attribute]) -> String {
    let mut parts = Vec::new();
    for a in attrs {
        if a.path().is_ident("doc") {
            if let syn::Meta::NameValue(nv) = &a.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    parts.push(s.value().trim().to_string());
                }
            }
        }
    }
    let joined = parts.join(" ");
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// All `#[serde(...)]` attribute bodies of an item, as one token string.
fn serde_str(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("serde"))
        .map(|a| a.to_token_stream().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn capture<'a>(re: &str, hay: &'a str) -> Option<String> {
    regex::Regex::new(re)
        .ok()?
        .captures(hay)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn type_str(ty: &syn::Type) -> String {
    let raw = ty.to_token_stream().to_string();
    let no_space: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    no_space.replace(',', ", ")
}

fn esc(cell: &str) -> String {
    cell.replace('|', "\\|")
}

/// CamelCase variant ident → serde `rename_all` wire form (subset that recipe
/// enums actually use).
fn rename_all(ident: &str, rule: &str) -> String {
    match rule {
        "lowercase" => ident.to_lowercase(),
        "UPPERCASE" => ident.to_uppercase(),
        "snake_case" | "kebab-case" | "SCREAMING_SNAKE_CASE" => {
            let mut s = String::new();
            for (i, ch) in ident.chars().enumerate() {
                if ch.is_uppercase() && i != 0 {
                    s.push('_');
                }
                s.push(ch.to_ascii_lowercase());
            }
            match rule {
                "kebab-case" => s.replace('_', "-"),
                "SCREAMING_SNAKE_CASE" => s.to_uppercase(),
                _ => s,
            }
        }
        _ => ident.to_string(),
    }
}

struct FieldRow {
    key: String,
    ty: String,
    required: bool,
    default: String,
    doc: String,
}

fn field_rows(fields: &Fields) -> Vec<FieldRow> {
    let mut rows = Vec::new();
    let named = match fields {
        Fields::Named(n) => &n.named,
        _ => return rows,
    };
    for f in named {
        let sd = serde_str(&f.attrs);
        // Skip fields removed from the wire (`skip` / `skip_deserializing`),
        // but keep `skip_serializing_if` (still deserialized).
        if regex::Regex::new(r"skip[ ,)]").unwrap().is_match(&sd)
            && !sd.contains("skip_serializing_if")
        {
            continue;
        }
        let ident = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
        let key = capture(r#"rename = "([^"]+)""#, &sd).unwrap_or(ident);
        let tystr = type_str(&f.ty);
        let optional = tystr.starts_with("Option<");
        let has_default = sd.contains("default");
        let default = if let Some(f) = capture(r#"default = "([^"]+)""#, &sd) {
            format!("`{f}()`")
        } else if has_default {
            "type default".to_string()
        } else if optional {
            "—".to_string()
        } else {
            "—".to_string()
        };
        rows.push(FieldRow {
            key,
            ty: tystr,
            required: !optional && !has_default,
            default,
            doc: doc(&f.attrs),
        });
    }
    rows
}

fn render_table(rows: &[FieldRow]) -> String {
    if rows.is_empty() {
        return "_No fields._\n\n".to_string();
    }
    let mut t = String::from("| TOML key | Type | Required | Default | Description |\n");
    t.push_str("|---|---|---|---|---|\n");
    for r in rows {
        t.push_str(&format!(
            "| `{}` | `{}` | {} | {} | {} |\n",
            esc(&r.key),
            esc(&r.ty),
            if r.required { "**yes**" } else { "no" },
            esc(&r.default),
            esc(&r.doc),
        ));
    }
    t.push('\n');
    t
}

fn render_struct(s: &syn::ItemStruct) -> String {
    let mut out = format!("## `{}`\n\n", s.ident);
    let d = doc(&s.attrs);
    if !d.is_empty() {
        out.push_str(&d);
        out.push_str("\n\n");
    }
    out.push_str(&render_table(&field_rows(&s.fields)));
    out
}

fn render_enum(e: &syn::ItemEnum) -> String {
    let sd = serde_str(&e.attrs);
    let tag = capture(r#"tag = "([^"]+)""#, &sd);
    let ra = capture(r#"rename_all = "([^"]+)""#, &sd);

    let mut out = format!("## `{}`", e.ident);
    if let Some(t) = &tag {
        out.push_str(&format!(" (select with `{t} = \"…\"`)"));
    }
    out.push_str("\n\n");
    let d = doc(&e.attrs);
    if !d.is_empty() {
        out.push_str(&d);
        out.push_str("\n\n");
    }

    let all_unit = e.variants.iter().all(|v| matches!(v.fields, Fields::Unit));
    if tag.is_none() && all_unit {
        // Simple value enum: allowed-values list.
        out.push_str("Allowed values:\n\n");
        for v in &e.variants {
            let vsd = serde_str(&v.attrs);
            let key = capture(r#"rename = "([^"]+)""#, &vsd)
                .unwrap_or_else(|| rename_all(&v.ident.to_string(), ra.as_deref().unwrap_or("")));
            let vd = doc(&v.attrs);
            if vd.is_empty() {
                out.push_str(&format!("- `{key}`\n"));
            } else {
                out.push_str(&format!("- `{key}` — {vd}\n"));
            }
        }
        out.push('\n');
        return out;
    }

    // Tagged / data-bearing enum: one sub-section per variant.
    let tagname = tag.as_deref().unwrap_or("type");
    for v in &e.variants {
        let vsd = serde_str(&v.attrs);
        let key = capture(r#"rename = "([^"]+)""#, &vsd)
            .unwrap_or_else(|| rename_all(&v.ident.to_string(), ra.as_deref().unwrap_or("")));
        out.push_str(&format!("### `{tagname} = \"{key}\"`\n\n"));
        let vd = doc(&v.attrs);
        if !vd.is_empty() {
            out.push_str(&vd);
            out.push_str("\n\n");
        }
        out.push_str(&render_table(&field_rows(&v.fields)));
    }
    out
}

/// First differing line, for a readable failure message.
fn first_diff(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!(
                "first diff at line {}:\n  committed: {x}\n  generated: {y}",
                i + 1
            );
        }
    }
    String::from("(content is a prefix/length mismatch)")
}
