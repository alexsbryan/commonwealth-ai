// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scans the whole workspace for `covers:` doc tags and generates one
//! `quality/conformance/<crate>.toml` per crate that carries any.
//!
//! ```text
//! UPDATE_CONFORMANCE_TAGS=1 cargo test -p kernel-types --test conformance_tags
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
//! # ONE scanner, N manifests
//!
//! The first version of this lived in `sovereign-core/tests/main/` and scanned
//! only its own crate. The 130 requirements that already have a named passing
//! test are spread across **21 crates**, so that shape needed twenty-one copies
//! of one generator — a duplicated decider (ARCH §10.6) built to satisfy a
//! partitioning requirement that was never about the scanner.
//!
//! The partitioning is about the OUTPUT: separate manifests are what let agents
//! working in different crates avoid contending on one file. One scanner, many
//! manifests, satisfies that with less code than the per-crate shape had.
//!
//! It lives here because this crate already owns the requirement vocabulary
//! (`kernel_types::conformance`) and already reads the specification from the
//! repo root. `syn` and `quote` are dev-dependencies; the four-dep runtime
//! budget is untouched.
//!
//! # The join key is the thing most likely to be silently wrong
//!
//! A claim resolves against nextest's JUnit report by `classname::name`, where
//! `classname` is the BINARY id — `sovereign-core` for a lib test but
//! `sovereign-core::main` for one in `tests/main/`. Getting that wrong produces
//! keys that match nothing, and a claim that matches nothing reads as
//! `never-ran`, which looks exactly like honest absence. So the derivation is
//! unit-tested against real observed report keys (ARCH §18.4).
//!
//! # Two hard failures, both absences (ARCH §18.3)
//!
//! - **An unknown id** — a tag naming something not in `quality/requirements.toml`.
//! - **`claimed-unproven`** — a `covers:` over a body with nothing falsifiable in
//!   it. It fails at any count rather than being counted: a claim that cannot
//!   fail is the cheap repair a coverage ratchet rewards.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::{Attribute, Item};

/// Where the per-crate manifests live, relative to the repo root.
const OUT_DIR: &str = "quality/conformance";
/// The marker a manifest from the OTHER generator carries in its header.
///
/// Matched as a substring of the file rather than parsed: the header is
/// already required to name the command that regenerates it, so there is no
/// second thing to keep in sync — and a manifest that stops naming its
/// generator correctly reads as unowned and fails loudly rather than being
/// silently deleted.
const FOREIGN_GENERATOR: &str = "conformance-tags.mjs";
/// The registry every tag must resolve against.
const REGISTRY: &str = "quality/requirements.toml";
/// The command that regenerates everything this test gates.
const REGEN: &str = "UPDATE_CONFORMANCE_TAGS=1 cargo test -p kernel-types --test conformance_tags";

/// What one `covers:` tag claims.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Claim {
    requirement: String,
    /// `<junit classname>::<junit name>` — the key the runner joins on.
    test: String,
    file: String,
    line: usize,
    asserts: usize,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("kernel-types sits directly under the repo root")
        .to_path_buf()
}

// ─── Where a test's name comes from ─────────────────────────────────────────

/// The nextest binary id and module prefix for a source file.
///
/// | file | binary id | module prefix |
/// |---|---|---|
/// | `<crate>/src/quote_verification.rs` | `<crate>` | `quote_verification` |
/// | `<crate>/src/runtime/mod.rs` | `<crate>` | `runtime` |
/// | `<crate>/src/lib.rs` | `<crate>` | (none) |
/// | `<crate>/tests/main/foo.rs` | `<crate>::main` | `foo` |
/// | `<crate>/tests/binder_replay.rs` | `<crate>::binder_replay` | (none) |
fn binary_and_module(rel_to_crate: &Path, krate: &str) -> Option<(String, Vec<String>)> {
    let mut parts: Vec<String> = rel_to_crate
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let first = parts.first()?.clone();
    parts.remove(0);
    if let Some(last) = parts.last_mut() {
        *last = last.trim_end_matches(".rs").to_string();
    }
    match first.as_str() {
        "src" => {
            parts.retain(|p| p != "mod" && p != "lib");
            Some((krate.to_string(), parts))
        }
        "tests" => {
            // `tests/<bin>.rs` is its own binary; `tests/<bin>/x.rs` is a module
            // of the `<bin>` binary (the repo's `#[path]` convention).
            let bin = parts.first()?.clone();
            let module: Vec<String> = parts[1..].to_vec();
            Some((format!("{krate}::{bin}"), module))
        }
        // benches and examples are not nextest targets.
        _ => None,
    }
}

/// Nearest ancestor `Cargo.toml`'s `name`, and the directory holding it.
fn owning_crate(file: &Path) -> Option<(String, PathBuf)> {
    let mut dir = file.parent()?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            let text = std::fs::read_to_string(&manifest).ok()?;
            // The first `name = "..."` after `[package]`.
            let pkg = text.find("[package]")?;
            let name_at = text[pkg..].find("name = \"")? + pkg + "name = \"".len();
            let end = text[name_at..].find('"')? + name_at;
            return Some((text[name_at..end].to_string(), dir.to_path_buf()));
        }
        dir = dir.parent()?;
    }
}

// ─── Reading the tag ────────────────────────────────────────────────────────

/// Concatenated `///` doc lines. Same reader as
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
    let mut tagged = false;
    for line in doc_lines(attrs) {
        let Some(rest) = line.strip_prefix("covers:") else {
            continue;
        };
        tagged = true;
        out.extend(
            rest.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    tagged.then_some(out)
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

/// Is this a test the runner will execute?
///
/// Matches on the attribute's LAST path segment, so `#[tokio::test]` counts —
/// it is a test in every way that matters here, and the first version of this
/// check compared the whole path and refused a real async guard test as "not a
/// #[test]".
fn is_test_attr(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
}

/// Falsifiable exits in a rendered function body.
///
/// The property measured is FALSIFIABILITY, not the spelling `assert`.
/// `match v { Good => {}, v => panic!("…") }` is the idiom the grounding guard
/// tests actually use and it fails exactly as loudly as an `assert_eq!` — an
/// earlier version of this counter missed it and reported a real negative
/// control as `claimed-unproven`. A `#[should_panic]` test asserts by panicking.
fn assertion_count(body: &str, should_panic: bool) -> usize {
    let mut n = usize::from(should_panic);
    for token in [
        "assert !",
        "assert_eq !",
        "assert_ne !",
        "assert_matches !",
        "panic !",
        "unreachable !",
        // `.unwrap_err(` stays: it panics when the subject returned `Ok`, so it
        // is an assertion ABOUT the subject.
        //
        // `.expect(` was here and is deliberately gone. It is falsifiable in
        // the narrow sense, but in practice it is SETUP —
        // `tempdir().expect("tmp")`, `serde_json::from_str(..).expect(..)` —
        // so counting it let a body that asserts nothing about the requirement
        // satisfy this gate on its fixtures alone. That is precisely "the cheap
        // repair a coverage ratchet rewards" that the failure message below
        // names, and the gate was rewarding it (§18.1).
        ". unwrap_err (",
    ] {
        n += body.matches(token).count();
    }
    n
}

/// 1-indexed line of `fn <name>(`.
fn line_of(text: &str, ident: &str) -> usize {
    let needle = format!("fn {ident}(");
    text.lines()
        .position(|l| l.contains(&needle))
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// Record one `covers:`-tagged function as claims, wherever it was found.
///
/// Shared by the free-function and `impl`-method arms of [`collect`] so the
/// three guards below — must be a `#[test]`, must name a requirement, must
/// contain something falsifiable — cannot hold on one traversal path and not
/// the other. One decider (§10.6).
#[allow(clippy::too_many_arguments)]
fn record_fn(
    attrs: &[Attribute],
    ident: &syn::Ident,
    block: &syn::Block,
    module: &[String],
    binary: &str,
    rel_file: &str,
    text: &str,
    out: &mut Vec<Claim>,
) {
    let Some(ids) = covers(attrs) else {
        return;
    };
    assert!(
        is_test_attr(attrs),
        "{rel_file}: `{ident}` carries a covers: tag but is not a #[test]. \
         A claim no test runner executes proves nothing."
    );
    assert!(
        !ids.is_empty(),
        "{rel_file}: `{ident}` has an empty covers: tag"
    );
    let body = format!("{}", quote::ToTokens::to_token_stream(block));
    let asserts = assertion_count(&body, has_attr(attrs, "should_panic"));
    assert!(
        asserts > 0,
        "CLAIMED-UNPROVEN — {rel_file}: `{ident}` claims {ids:?} but its body contains \
         nothing falsifiable. A claim that cannot fail is worse than no claim: it is \
         the cheap repair a coverage ratchet rewards. Assert something, or remove \
         the tag."
    );
    let mut path = module.to_vec();
    path.push(ident.to_string());
    let line = line_of(text, &ident.to_string());
    for id in ids {
        out.push(Claim {
            requirement: id,
            test: format!("{binary}::{}", path.join("::")),
            file: rel_file.to_string(),
            line,
            asserts,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn collect(
    items: &[Item],
    module: &[String],
    binary: &str,
    rel_file: &str,
    text: &str,
    out: &mut Vec<Claim>,
) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let mut nested = module.to_vec();
                    nested.push(m.ident.to_string());
                    collect(inner, &nested, binary, rel_file, text, out);
                }
            }
            Item::Fn(f) => record_fn(
                &f.attrs,
                &f.sig.ident,
                &f.block,
                module,
                binary,
                rel_file,
                text,
                out,
            ),
            // A `covers:` tag inside an `impl` block used to hit the catch-all
            // below and vanish: the file passed the `contains("covers:")`
            // prefilter, syn parsed it, and zero claims came out — no error, no
            // manifest row, no diagnostic. The author reads a green build and
            // believes the requirement is covered. A scanner that silently sees
            // less than it claims to is the same defect class it exists to
            // catch, so the traversal has to reach every place a `#[test]` can
            // legally live.
            Item::Impl(i) => {
                for sub in &i.items {
                    if let syn::ImplItem::Fn(f) = sub {
                        record_fn(
                            &f.attrs,
                            &f.sig.ident,
                            &f.block,
                            module,
                            binary,
                            rel_file,
                            text,
                            out,
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Every tracked `.rs` file. `git ls-files` is the same primary source
/// `xtask`'s `SourceTree` uses for scope, and it costs one subprocess instead of
/// a hand-maintained ignore list that rots (arch-gate once reported 2,115
/// failures from four trees a stale skip-list had not heard of).
fn tracked_rust_files(root: &Path) -> Vec<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "-z", "*.rs"])
        .current_dir(root)
        .output()
        .expect("git ls-files");
    assert!(
        out.status.success(),
        "git ls-files failed in {}",
        root.display()
    );
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| root.join(s))
        .collect()
}

/// crate name → its claims, sorted.
fn scan(root: &Path) -> BTreeMap<String, Vec<Claim>> {
    let mut by_crate: BTreeMap<String, Vec<Claim>> = BTreeMap::new();
    for path in tracked_rust_files(root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Prefilter: parsing every tracked file with syn is the expensive part
        // and almost none carry a tag.
        if !text.contains("covers:") {
            continue;
        }
        let Some((krate, crate_dir)) = owning_crate(&path) else {
            continue;
        };
        let rel_to_crate = path.strip_prefix(&crate_dir).expect("under its crate");
        let Some((binary, module)) = binary_and_module(rel_to_crate, &krate) else {
            continue;
        };
        let rel_file = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let file = syn::parse_file(&text)
            // A file the workspace compiles must parse; if syn cannot, tags in
            // it go missing rather than being reported.
            .unwrap_or_else(|e| panic!("syn could not parse {rel_file}: {e}"));
        let mut claims = Vec::new();
        collect(&file.items, &module, &binary, &rel_file, &text, &mut claims);
        by_crate.entry(krate).or_default().extend(claims);
    }
    for v in by_crate.values_mut() {
        v.sort();
    }
    by_crate.retain(|_, v| !v.is_empty());
    by_crate
}

fn render(krate: &str, claims: &[Claim]) -> String {
    let mut s = format!(
        "# Conformance tags in `{krate}` — GENERATED. DO NOT EDIT BY HAND.\n\
         #\n\
         #   {REGEN}\n\
         #\n\
         # Each claim maps a requirement id from research/clean-room/REQUIREMENTS.md\n\
         # to the test that proves it. `test` is `<junit classname>::<junit name>`, so\n\
         # `svrn conformance` can join a claim to a real per-test verdict without\n\
         # guessing.\n\
         #\n\
         # `asserts` counts falsifiable exits in the test body. It is never zero: a\n\
         # covers: tag over a body that cannot fail is refused by the generator.\n"
    );
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
    let by_crate = scan(&root);

    // Every tag resolves against the registry, or it is a claim about nothing.
    let registry = std::fs::read_to_string(root.join(REGISTRY))
        .unwrap_or_else(|e| panic!("read {REGISTRY}: {e}"));
    let known: BTreeSet<&str> = registry
        .lines()
        .filter_map(|l| l.strip_prefix("id = \""))
        .filter_map(|l| l.split('"').next())
        .collect();
    assert!(
        known.len() > 600,
        "{REGISTRY} yielded only {} ids — the registry did not parse, and every tag would \
         validate against an empty set",
        known.len()
    );
    for claims in by_crate.values() {
        for c in claims {
            assert!(
                known.contains(c.requirement.as_str()),
                "{}:{} claims `{}`, which is not a requirement in {REGISTRY}",
                c.file,
                c.line,
                c.requirement
            );
        }
    }

    let dir = root.join(OUT_DIR);
    let wanted: BTreeMap<String, String> = by_crate
        .iter()
        .map(|(k, v)| (format!("{k}.toml"), render(k, v)))
        .collect();
    let update = std::env::var("UPDATE_CONFORMANCE_TAGS").is_ok();

    if update {
        std::fs::create_dir_all(&dir).expect("create quality/conformance/");
    }
    // A manifest whose crate no longer has any tag is a claim that outlived its
    // test — removed, not left to rot.
    //
    // OWNERSHIP IS DECLARED, NOT ASSUMED. This directory has more than one
    // writer: this scanner emits one manifest per Rust crate from `covers:`
    // doc tags, and the desktop app's
    // `tests/e2e/scripts/conformance-tags.mjs` emits `desktop.toml` from
    // Playwright `@REQ-ID` tags — the instrument the `cli`/`desktop`-class
    // requirements actually need, and one this Rust scanner cannot see.
    //
    // Until 2026-09-01 this reclaimed the WHOLE directory, so the first
    // foreign manifest to land was reported as debris with no tags backing
    // it, and the gate went red for a file that was perfectly current. Both
    // generators believed they owned the directory; neither said so
    // (ARCH §10.6). Each now reclaims only what it wrote, and a manifest
    // names its generator in its own header.
    let existing: BTreeSet<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    let owned_by_another = std::fs::read_to_string(e.path())
                        .map(|t| t.contains(FOREIGN_GENERATOR))
                        .unwrap_or(false);
                    !owned_by_another
                })
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".toml"))
                .collect()
        })
        .unwrap_or_default();
    for stale in existing.difference(&wanted.keys().cloned().collect()) {
        if update {
            std::fs::remove_file(dir.join(stale)).expect("remove stale manifest");
            eprintln!("removed {OUT_DIR}/{stale} (no tags left in that crate)");
        } else {
            panic!(
                "{OUT_DIR}/{stale} has no covers: tags backing it any more.\nRegenerate:\n  {REGEN}"
            );
        }
    }

    for (name, body) in &wanted {
        let path = dir.join(name);
        if update {
            std::fs::write(&path, body).expect("write manifest");
            continue;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("cannot read {OUT_DIR}/{name} ({e}).\nGenerate it:\n  {REGEN}")
        });
        if &committed != body {
            let first = committed
                .lines()
                .zip(body.lines())
                .position(|(a, b)| a != b)
                .map(|i| {
                    format!(
                        "first diff at line {}:\n  committed: {}\n  generated: {}",
                        i + 1,
                        committed.lines().nth(i).unwrap_or(""),
                        body.lines().nth(i).unwrap_or("")
                    )
                })
                .unwrap_or_else(|| "(length mismatch)".to_string());
            panic!("{OUT_DIR}/{name} is stale.\n{first}\nRegenerate:\n  {REGEN}");
        }
    }
    if update {
        let n: usize = by_crate.values().map(Vec::len).sum();
        eprintln!(
            "wrote {} manifest(s), {n} claim(s) across {} crate(s)",
            wanted.len(),
            by_crate.len()
        );
    }
}

// ─── The join key, pinned against report keys observed from nextest ─────────

/// These four spellings were read out of a real `target/nextest/default/junit.xml`.
/// The derivation is the single most silent thing that can be wrong here: a key
/// that matches nothing resolves to `never-ran`, which is indistinguishable from
/// honest absence.
#[test]
fn the_join_key_matches_what_nextest_reports() {
    let p = Path::new;
    assert_eq!(
        binary_and_module(p("src/quote_verification.rs"), "sovereign-core"),
        Some(("sovereign-core".into(), vec!["quote_verification".into()]))
    );
    assert_eq!(
        binary_and_module(p("src/runtime/grounding/sealed.rs"), "sovereign-core"),
        Some((
            "sovereign-core".into(),
            vec!["runtime".into(), "grounding".into(), "sealed".into()]
        ))
    );
    assert_eq!(
        binary_and_module(p("src/lib.rs"), "kernel-types"),
        Some(("kernel-types".into(), vec![]))
    );
    // The form the per-crate scanner got wrong: an integration test is its own
    // BINARY, and `tests/main/<x>.rs` is a module inside the `main` binary.
    assert_eq!(
        binary_and_module(p("tests/main/gate_release_census.rs"), "sovereign-core"),
        Some((
            "sovereign-core::main".into(),
            vec!["gate_release_census".into()]
        ))
    );
    assert_eq!(
        binary_and_module(p("tests/binder_replay.rs"), "sovereign-core"),
        Some(("sovereign-core::binder_replay".into(), vec![]))
    );
    // Not nextest targets.
    assert_eq!(binary_and_module(p("benches/x.rs"), "c"), None);
    assert_eq!(binary_and_module(p("examples/x.rs"), "c"), None);
}

/// `#[tokio::test]` is a test. Both arms, because a predicate only ever watched
/// saying "yes" cannot be told from one that always says yes.
#[test]
fn an_async_test_attribute_still_reads_as_a_test() {
    let f: syn::ItemFn = syn::parse_quote! { #[tokio::test] async fn t() {} };
    assert!(is_test_attr(&f.attrs));
    let g: syn::ItemFn = syn::parse_quote! { #[test] fn t() {} };
    assert!(is_test_attr(&g.attrs));
    let h: syn::ItemFn = syn::parse_quote! { #[inline] fn t() {} };
    assert!(
        !is_test_attr(&h.attrs),
        "a non-test attribute must not pass"
    );
}

/// Falsifiability, not the spelling `assert`. Both arms, because a counter only
/// ever watched saying "yes" cannot be told from one that always says yes.
#[test]
fn a_body_that_cannot_fail_is_refused() {
    assert_eq!(assertion_count("let x = 1 ; x", false), 0);
    assert!(assertion_count("assert_eq ! (x , 1)", false) > 0);
    assert!(
        assertion_count(
            "match v { Released => { } v => panic ! (\"got {v}\") }",
            false
        ) > 0,
        "a match arm that panics is an assertion"
    );
    assert!(
        assertion_count("do_the_thing ()", true) > 0,
        "a #[should_panic] test asserts by panicking"
    );
}
