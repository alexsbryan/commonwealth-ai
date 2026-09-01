// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scans this crate for `covers:` doc tags and generates
//! `quality/conformance/sovereign-core.toml` — the map from a requirement id to
//! the test that proves it.
//!
//! ```text
//! UPDATE_CONFORMANCE_TAGS=1 cargo test -p sovereign-core --test main conformance_tags
//! ```
//!
//! # The tag
//!
//! ```ignore
//! /// covers: GR-19, GR-20
//! #[test]
//! fn a_quote_from_beyond_the_prompt_truncation_is_no_longer_demoted() { … }
//! ```
//!
//! # Why a generated sidecar and not a runtime scan
//!
//! `syn` is a dev-dependency. A runtime scanner would put a Rust parser in a
//! shipped crate to answer a question that only changes when the source
//! changes — so the answer is computed once, here, and committed. It is the
//! same generate-and-byte-gate contract as `corpus-engine`'s `recipe_schema`
//! and the desktop's `command_surface`, and the manifest it produces is what
//! the conformance runner reads.
//!
//! Partitioned per CRATE rather than per requirement family: a scanner can own
//! a crate unambiguously, one test may cover ids from two families, and
//! separate crates are what separate agents actually edit — which is the
//! collision this partitioning exists to prevent.
//!
//! # Two hard failures, both of them absences (ARCH §18.3)
//!
//! - **An unknown id.** A tag naming something that is not in
//!   `quality/requirements.toml` is a claim about nothing.
//! - **`claimed-unproven`.** A `covers:` over a body with no assertion is the
//!   cheap repair a coverage ratchet would otherwise reward, so it fails at any
//!   count rather than being counted. This is the `syn`-level analogue of
//!   `unproven_capabilities`' "mentioned but proves nothing" arm
//!   (`cli_contract_journeys.rs:543`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use syn::{Attribute, Item};

/// Generated manifest, relative to the repo root.
const OUT: &str = "quality/conformance/sovereign-core.toml";
/// The registry every tag must resolve against.
const REGISTRY: &str = "quality/requirements.toml";
/// nextest's binary id for this crate's lib tests — the JUnit `classname` the
/// runner joins on.
const LIB_BINARY: &str = "sovereign-core";

const HEADER: &str = "\
# Conformance tags in `sovereign-core` — GENERATED. DO NOT EDIT BY HAND.
#
#   UPDATE_CONFORMANCE_TAGS=1 cargo test -p sovereign-core --test main conformance_tags
#
# Each claim maps a requirement id from research/clean-room/REQUIREMENTS.md to
# the test that proves it. `test` is `<junit classname>::<junit name>`, so the
# runner can join a claim to a real per-test verdict without guessing.
#
# `asserts` is the number of assertion tokens in the test body. It is never
# zero: a `covers:` over an assertion-free body fails the generator, because a
# claim that cannot fail is the cheap repair a coverage ratchet rewards.
";

/// What one `covers:` tag claims.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Claim {
    requirement: String,
    test: String,
    file: String,
    line: usize,
    asserts: usize,
}

fn repo_root() -> PathBuf {
    // tests/main/ -> tests/ -> sovereign-core/ -> crates/ -> sovereign/ -> repo
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..3 {
        p = p.parent().expect("crate is nested under the repo").into();
    }
    p
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Concatenated `///` doc text. Lifted from
/// `corpus-engine/tests/main/recipe_schema.rs:129` — one doc reader, not two.
fn doc_lines(attrs: &[Attribute]) -> Vec<String> {
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
    parts
}

/// The ids a `covers:` line names, or `None` when the item carries no tag.
fn covers(attrs: &[Attribute]) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut seen_tag = false;
    for line in doc_lines(attrs) {
        let Some(rest) = line.strip_prefix("covers:") else {
            continue;
        };
        seen_tag = true;
        for id in rest.split(',') {
            let id = id.trim();
            if !id.is_empty() {
                out.push(id.to_string());
            }
        }
    }
    seen_tag.then_some(out)
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

/// Assertion tokens in a rendered function body.
///
/// A `#[should_panic]` test asserts by panicking, so it counts as one — it is
/// as falsifiable as an `assert!`, which is the property being measured.
fn assertion_count(body: &str, should_panic: bool) -> usize {
    let mut n = if should_panic { 1 } else { 0 };
    for token in [
        "assert !",
        "assert_eq !",
        "assert_ne !",
        "assert_matches !",
        ". expect (",
        ". unwrap_err (",
    ] {
        n += body.matches(token).count();
    }
    n
}

/// Walk `items`, tracking the Rust module path, and collect every tagged test.
fn collect(
    items: &[Item],
    module: &[String],
    file: &Path,
    text: &str,
    out: &mut Vec<Claim>,
    untagged_ids: &mut BTreeMap<String, usize>,
) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let mut nested = module.to_vec();
                    nested.push(m.ident.to_string());
                    collect(inner, &nested, file, text, out, untagged_ids);
                }
            }
            Item::Fn(f) => {
                let Some(ids) = covers(&f.attrs) else {
                    continue;
                };
                assert!(
                    has_attr(&f.attrs, "test"),
                    "{}: `{}` carries a covers: tag but is not a #[test]. A claim \
                     that no test runner executes proves nothing.",
                    file.display(),
                    f.sig.ident
                );
                assert!(
                    !ids.is_empty(),
                    "{}: `{}` has an empty covers: tag",
                    file.display(),
                    f.sig.ident
                );
                let body = format!("{}", quote::ToTokens::to_token_stream(&*f.block));
                let asserts = assertion_count(&body, has_attr(&f.attrs, "should_panic"));
                assert!(
                    asserts > 0,
                    "CLAIMED-UNPROVEN — {}: `{}` claims {:?} but its body contains no \
                     assertion. A claim that cannot fail is worse than no claim: it is \
                     the cheap repair a coverage ratchet rewards. Assert something, or \
                     remove the tag.",
                    file.display(),
                    f.sig.ident,
                    ids,
                );
                let mut path = module.to_vec();
                path.push(f.sig.ident.to_string());
                let test = format!("{LIB_BINARY}::{}", path.join("::"));
                let line = line_of(text, &f.sig.ident.to_string());
                for id in ids {
                    *untagged_ids.entry(id.clone()).or_insert(0) += 1;
                    out.push(Claim {
                        requirement: id,
                        test: test.clone(),
                        file: rel(file),
                        line,
                        asserts,
                    });
                }
            }
            _ => {}
        }
    }
}

/// 1-indexed line of `fn <name>`. `syn` spans need `proc-macro2`'s span-locations
/// feature to be reliable here, so this reads the text instead — the name is
/// unique within its file in every case this scans, and a wrong line is a
/// navigation nuisance, never a wrong verdict.
fn line_of(text: &str, ident: &str) -> usize {
    let needle = format!("fn {ident}(");
    text.lines()
        .position(|l| l.contains(&needle))
        .map(|i| i + 1)
        .unwrap_or(0)
}

fn rel(p: &Path) -> String {
    let root = repo_root();
    p.strip_prefix(&root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    files
}

fn scan() -> Vec<Claim> {
    let src = crate_root().join("src");
    let mut out: Vec<Claim> = Vec::new();
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    for path in rust_files(&src) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        // Cheap prefilter: parsing every file in this crate with syn is the
        // expensive part, and almost none carry a tag.
        if !text.contains("covers:") {
            continue;
        }
        let file = syn::parse_file(&text).unwrap_or_else(|e| {
            // A file this crate compiles must parse; if syn cannot, tags in it
            // are silently missing rather than reported.
            panic!("syn could not parse {}: {e}", path.display())
        });
        let module = module_path_of(&path, &src);
        collect(&file.items, &module, &path, &text, &mut out, &mut ids);
    }
    out.sort();
    out
}

/// `src/quote_verification.rs` → `["quote_verification"]`;
/// `src/runtime/mod.rs` → `["runtime"]`; `src/lib.rs` → `[]`.
fn module_path_of(path: &Path, src: &Path) -> Vec<String> {
    let rel = path.strip_prefix(src).unwrap_or(path);
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if let Some(last) = parts.last_mut() {
        *last = last.trim_end_matches(".rs").to_string();
    }
    parts.retain(|p| p != "mod" && p != "lib");
    parts
}

fn render(claims: &[Claim]) -> String {
    let mut s = String::from(HEADER);
    for c in claims {
        s.push_str("\n[[claim]]\n");
        s.push_str(&format!("requirement = \"{}\"\n", c.requirement));
        s.push_str(&format!("test = \"{}\"\n", c.test));
        s.push_str(&format!("file = \"{}\"\n", c.file));
        s.push_str(&format!("line = {}\n", c.line));
        s.push_str(&format!("asserts = {}\n", c.asserts));
    }
    s
}

#[test]
fn conformance_tags_are_fresh() {
    let root = repo_root();
    let claims = scan();

    // Every tag resolves against the registry, or it is a claim about nothing.
    let registry = std::fs::read_to_string(root.join(REGISTRY))
        .unwrap_or_else(|e| panic!("read {REGISTRY}: {e}"));
    let known: Vec<String> = registry
        .lines()
        .filter_map(|l| l.strip_prefix("id = \""))
        .filter_map(|l| l.split('"').next())
        .map(str::to_string)
        .collect();
    assert!(
        known.len() > 600,
        "{REGISTRY} yielded only {} ids — the registry did not parse, and every \
         tag would validate against an empty set",
        known.len()
    );
    for c in &claims {
        assert!(
            known.contains(&c.requirement),
            "{}:{} claims `{}`, which is not a requirement in {REGISTRY}",
            c.file,
            c.line,
            c.requirement
        );
    }

    let rendered = render(&claims);
    let out_path = root.join(OUT);
    if std::env::var("UPDATE_CONFORMANCE_TAGS").is_ok() {
        std::fs::create_dir_all(out_path.parent().expect("has a parent"))
            .expect("create quality/conformance/");
        std::fs::write(&out_path, &rendered).expect("write manifest");
        eprintln!("wrote {} ({} claim(s))", out_path.display(), claims.len());
        return;
    }
    let committed = std::fs::read_to_string(&out_path).unwrap_or_else(|e| {
        panic!(
            "cannot read {OUT} ({e}).\nGenerate it:\n  \
             UPDATE_CONFORMANCE_TAGS=1 cargo test -p sovereign-core --test main conformance_tags"
        )
    });
    if committed != rendered {
        let first = committed
            .lines()
            .zip(rendered.lines())
            .position(|(a, b)| a != b)
            .map(|i| {
                format!(
                    "first diff at line {}:\n  committed: {}\n  generated: {}",
                    i + 1,
                    committed.lines().nth(i).unwrap_or(""),
                    rendered.lines().nth(i).unwrap_or("")
                )
            })
            .unwrap_or_else(|| "(length mismatch)".to_string());
        panic!(
            "{OUT} is stale against this crate's covers: tags.\n{first}\n\
             Regenerate:\n  UPDATE_CONFORMANCE_TAGS=1 cargo test -p sovereign-core --test main conformance_tags"
        );
    }
}

/// The two refusals, watched failing rather than asserted in prose (ARCH §18.1).
#[test]
fn a_claim_over_a_body_that_cannot_fail_is_refused() {
    // The check the generator applies, exercised directly on both arms.
    assert_eq!(assertion_count("let x = 1 ; x", false), 0, "no assertion");
    assert!(assertion_count("assert_eq ! (x , 1)", false) > 0);
    assert!(
        assertion_count("do_the_thing ()", true) > 0,
        "a #[should_panic] test asserts by panicking"
    );
}

/// The junit join key is `classname::name`, and `name` is the test's path
/// within its binary — so the module path must be derived exactly, or every
/// claim resolves to a test nextest has never heard of.
#[test]
fn the_module_path_matches_what_nextest_reports() {
    let src = Path::new("/r/src");
    assert_eq!(
        module_path_of(Path::new("/r/src/quote_verification.rs"), src),
        vec!["quote_verification"]
    );
    assert_eq!(
        module_path_of(Path::new("/r/src/runtime/mod.rs"), src),
        vec!["runtime"]
    );
    assert_eq!(
        module_path_of(Path::new("/r/src/runtime/grounding/sealed.rs"), src),
        vec!["runtime", "grounding", "sealed"]
    );
    assert!(module_path_of(Path::new("/r/src/lib.rs"), src).is_empty());
}
