// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build-time extraction of the recipe variant catalog from
//! `corpus-engine/src/recipe.rs`, so the recipe-author tool's JSON Schema
//! (which drives the LLGuidance grammar) cannot drift behind the real
//! `AcquirerConfig` / `ExtractorConfig` / `ChunkerConfig` / `FilterConfig`
//! types.
//!
//! Parses the recipe source with `syn` and emits a small descriptor to
//! `OUT_DIR/recipe_schema_descriptor.json`:
//!
//! ```json
//! {
//!   "acquire":    [{"key":"http_api","required":["requests"]}, …],
//!   "extract":    [{"key":"jsonl","required":[]}, …],
//!   "chunk":      ["paragraph", "threaded_turns", …],
//!   "filter":     ["boilerplate", …],
//!   "pattern":    ["role_overlap", …],
//!   "comparison": ["greater_than", …]
//! }
//! ```
//!
//! `recipe_author/recipe_schema.rs` reads this and builds the grammar-shaped
//! JSON Schema from it (full coverage), layering hand-authored richness
//! (http_api pagination, etc.) on top. `cargo:rerun-if-changed` on the source
//! files means a new recipe variant invalidates this build.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use syn::{Attribute, Fields, Item, ItemEnum, Type, Variant};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // sovereign/crates/sovereign-tools/ → ../../../corpus-engine/src/
    let ce_src = manifest.join("../../../corpus-engine/src");
    let recipe_path = ce_src.join("recipe.rs");
    let filters_path = ce_src.join("filters/mod.rs");

    let recipe_file = parse(&recipe_path);
    let filters_file = parse(&filters_path);

    let descriptor = serde_json::json!({
        "acquire":    variants_with_required(&recipe_file, "AcquirerConfig"),
        "extract":    variants_with_required(&recipe_file, "ExtractorConfig"),
        "chunk":      variant_keys(&recipe_file, "ChunkerConfig"),
        "pattern":    variant_keys(&recipe_file, "PatternDecl"),
        "comparison": variant_keys(&recipe_file, "Comparison"),
        "filter":     variant_keys(&filters_file, "FilterConfig"),
    });

    let dest = out_dir.join("recipe_schema_descriptor.json");
    std::fs::write(&dest, serde_json::to_string_pretty(&descriptor).unwrap())
        .unwrap_or_else(|e| panic!("write {}: {e}", dest.display()));

    println!("cargo:rerun-if-changed={}", recipe_path.display());
    println!("cargo:rerun-if-changed={}", filters_path.display());
    println!("cargo:rerun-if-changed=build.rs");
}

fn parse(path: &std::path::Path) -> syn::File {
    let src =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
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
fn variant_keys(file: &syn::File, name: &str) -> Vec<String> {
    let e = find_enum(file, name);
    let (_, kv) = serde_meta(&e.attrs);
    let rename_all = kv.get("rename_all").map(String::as_str);
    e.variants.iter().map(|v| wire_key(v, rename_all)).collect()
}

/// Wire key + required named fields per variant. A field is required iff it is
/// non-`Option` and carries no `#[serde(default)]` / `skip`.
fn variants_with_required(file: &syn::File, name: &str) -> Vec<serde_json::Value> {
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
