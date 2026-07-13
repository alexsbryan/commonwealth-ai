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

fn capture(re: &str, hay: &str) -> Option<String> {
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

// ── Recipe variant-catalog descriptor ───────────────────────────────────────
//
// The authoring JSON Schema (`sovereign-tools/.../recipe_author/recipe_schema.rs`)
// needs the discriminator strings + required fields of the `AcquirerConfig` /
// `ExtractorConfig` / `ChunkerConfig` / `FilterConfig` / `PatternDecl` /
// `Comparison` tagged enums so its grammar can't drift behind the real types.
// That catalog used to be extracted by a `sovereign-tools/build.rs` that reached
// *across the crate boundary* to parse `corpus-engine/src/recipe.rs` — a
// source-tree path no package split survives. It now lives here, next to the
// SCHEMA.md generator that already parses the same source, and is emitted as a
// checked-in artifact `sovereign-recipes/schema/recipe_schema_descriptor.json`
// that the authoring tool `include_str!`s. corpus-engine owns the catalog (it
// owns the types); the authoring tool owns the schema shape + overlays.

/// Regenerate + drift-gate the recipe variant-catalog descriptor. Same
/// `UPDATE_RECIPE_SCHEMA=1` bless as the SCHEMA.md gate above.
#[test]
fn recipe_schema_descriptor_is_fresh() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let recipe_file = descriptor::parse(&manifest.join("src/recipe.rs"));
    let filters_file = descriptor::parse(&manifest.join("src/filters/mod.rs"));

    // Keys inserted in ALPHABETICAL order on purpose. serde_json's object
    // ordering depends on the `preserve_order` feature: a scoped `-p
    // corpus-engine` build (no preserve_order) sorts keys via BTreeMap, while a
    // `--workspace` build unifies in `serde_json/preserve_order` and keeps
    // insertion order. Inserting alphabetically makes both configs emit
    // byte-identical output, so the bless command's feature set can't skew the
    // gate. (The array values below are Vec — insertion order always.)
    let desc = serde_json::json!({
        "acquire":    descriptor::variants_with_required(&recipe_file, "AcquirerConfig"),
        "chunk":      descriptor::variant_keys(&recipe_file, "ChunkerConfig"),
        "comparison": descriptor::variant_keys(&recipe_file, "Comparison"),
        "extract":    descriptor::variants_with_required(&recipe_file, "ExtractorConfig"),
        "filter":     descriptor::variant_keys(&filters_file, "FilterConfig"),
        "pattern":    descriptor::variant_keys(&recipe_file, "PatternDecl"),
    });
    // Pretty-print + a trailing newline (checked-in-file hygiene). The consumer
    // parses with `serde_json::from_str`, which ignores trailing whitespace.
    let generated = serde_json::to_string_pretty(&desc).unwrap() + "\n";

    let out_dir = manifest.join("../sovereign-recipes/schema");
    let out_path = out_dir.join("recipe_schema_descriptor.json");
    if std::env::var("UPDATE_RECIPE_SCHEMA").is_ok() {
        std::fs::create_dir_all(&out_dir).expect("create schema dir");
        std::fs::write(&out_path, &generated).expect("write descriptor");
        eprintln!("wrote {}", out_path.display());
        return;
    }

    let committed = std::fs::read_to_string(&out_path).unwrap_or_default();
    if committed != generated {
        let (cl, gl) = (committed.lines().count(), generated.lines().count());
        panic!(
            "sovereign-recipes/schema/recipe_schema_descriptor.json is stale \
             ({cl} committed lines vs {gl} generated).\n\
             The recipe config enums changed — regenerate with:\n  \
             UPDATE_RECIPE_SCHEMA=1 cargo test -p corpus-engine --test recipe_schema\n\
             {}",
            first_diff(&committed, &generated)
        );
    }
}

/// Descriptor extraction — the recipe variant catalog for the authoring JSON
/// Schema. Ported verbatim from the retired `sovereign-tools/build.rs` so the
/// emitted descriptor is byte-identical to what the build script produced.
mod descriptor {
    use std::collections::{HashMap, HashSet};
    use syn::{Attribute, Fields, Item, ItemEnum, Type, Variant};

    pub fn parse(path: &std::path::Path) -> syn::File {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    fn find_enum<'a>(file: &'a syn::File, name: &str) -> &'a ItemEnum {
        file.items
            .iter()
            .find_map(|it| match it {
                Item::Enum(e) if e.ident == name => Some(e),
                _ => None,
            })
            .unwrap_or_else(|| panic!("enum `{name}` not found"))
    }

    /// Just the wire-form discriminator strings of a tagged enum.
    pub fn variant_keys(file: &syn::File, name: &str) -> Vec<String> {
        let e = find_enum(file, name);
        let (_, kv) = serde_meta(&e.attrs);
        let rename_all = kv.get("rename_all").map(String::as_str);
        e.variants.iter().map(|v| wire_key(v, rename_all)).collect()
    }

    /// Wire key + required named fields per variant. A field is required iff it
    /// is non-`Option` and carries no `#[serde(default)]` / `skip`.
    pub fn variants_with_required(file: &syn::File, name: &str) -> Vec<serde_json::Value> {
        let e = find_enum(file, name);
        let (_, kv) = serde_meta(&e.attrs);
        let rename_all = kv.get("rename_all").map(String::as_str);
        e.variants
            .iter()
            .map(|v| {
                let required: Vec<String> = match &v.fields {
                    Fields::Named(n) => n
                        .named
                        .iter()
                        .filter(|f| field_required(f))
                        .filter_map(field_wire_name)
                        .collect(),
                    _ => Vec::new(),
                };
                serde_json::json!({ "key": wire_key(v, rename_all), "required": required })
            })
            .collect()
    }

    fn wire_key(v: &Variant, rename_all: Option<&str>) -> String {
        let (_, kv) = serde_meta(&v.attrs);
        if let Some(r) = kv.get("rename") {
            return r.clone();
        }
        apply_rename_all(&v.ident.to_string(), rename_all)
    }

    fn field_required(f: &syn::Field) -> bool {
        let (keys, _) = serde_meta(&f.attrs);
        !is_option(&f.ty)
            && !keys.contains("default")
            && !keys.contains("skip")
            && !keys.contains("skip_deserializing")
    }

    fn field_wire_name(f: &syn::Field) -> Option<String> {
        let base = f.ident.as_ref()?.to_string();
        let (_, kv) = serde_meta(&f.attrs);
        Some(kv.get("rename").cloned().unwrap_or(base))
    }

    fn is_option(ty: &Type) -> bool {
        matches!(ty, Type::Path(tp) if tp.path.segments.last().is_some_and(|s| s.ident == "Option"))
    }

    /// Collect a `#[serde(...)]` attribute's flag keys and `key = "string"` pairs.
    fn serde_meta(attrs: &[Attribute]) -> (HashSet<String>, HashMap<String, String>) {
        let mut keys = HashSet::new();
        let mut kv = HashMap::new();
        for a in attrs {
            if !a.path().is_ident("serde") {
                continue;
            }
            let _ = a.parse_nested_meta(|m| {
                if let Some(id) = m.path.get_ident() {
                    let k = id.to_string();
                    keys.insert(k.clone());
                    // Consume `= <value>` so sibling metas keep parsing; capture
                    // the value when it's a string literal.
                    if m.input.peek(syn::Token![=]) {
                        let v = m.value()?;
                        let expr: syn::Expr = v.parse()?;
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = expr
                        {
                            kv.insert(k, s.value());
                        }
                    }
                }
                Ok(())
            });
        }
        (keys, kv)
    }

    fn apply_rename_all(ident: &str, rule: Option<&str>) -> String {
        match rule {
            Some("lowercase") => ident.to_lowercase(),
            Some("UPPERCASE") => ident.to_uppercase(),
            Some("snake_case") | Some("kebab-case") | Some("SCREAMING_SNAKE_CASE") => {
                let mut s = String::new();
                for (i, ch) in ident.chars().enumerate() {
                    if ch.is_uppercase() && i != 0 {
                        s.push('_');
                    }
                    s.push(ch.to_ascii_lowercase());
                }
                match rule {
                    Some("kebab-case") => s.replace('_', "-"),
                    Some("SCREAMING_SNAKE_CASE") => s.to_uppercase(),
                    _ => s,
                }
            }
            _ => ident.to_string(),
        }
    }
}
