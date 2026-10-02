// SPDX-License-Identifier: AGPL-3.0-or-later
//! The differential falsifier for `EmbeddedDaemon`'s services sum
//! (`quality/TOPOLOGY.md` §4, "Root construction" row).
//!
//! The claim Phase 2 of daemon-convergence makes is two-directional, and a
//! unit test inside `daemon_services.rs` can only check one half of it:
//!
//! > **Sound** — every variant is constructed by some live path. A variant
//! > nobody builds is a representable-but-dead configuration, which is the
//! > defect the 17 `RwLock<Option<T>>` slots had 2¹⁷ of.
//! >
//! > **Complete** — every live path names a variant. A host that could reach a
//! > runtime posture the type does not name is back to punching dependencies
//! > in afterwards.
//!
//! Completeness is enforced by the type system: `EmbeddedDaemon::new` takes a
//! `DaemonServices` by value, so there is no way to construct a daemon without
//! naming one. **Soundness is not**, and cannot be — `sovereign-mesh` cannot
//! link its own hosts. So this test reads the hosts' source.
//!
//! ## Rewritten 2026-08-25, because Phase 4b moved where the naming happens
//!
//! Hosts no longer name a variant. They hand `LaunchParts` to
//! `sovereign_daemon::assemble` — the one exhaustive match over `Launch` — and
//! that match names the variant at its arm. The two halves of the claim did
//! not change; the place each one is checkable did:
//!
//! - **Soundness** is now checkable IN-CRATE and behaviourally: drive
//!   `assemble` with each `Launch` and see which variant comes back. Those
//!   tests live beside it in `daemon_services.rs` and run rather than grep.
//! - **The host half** is what still needs source: does each live host reach
//!   the arm it is supposed to? That is what remains here, and it is now the
//!   shape of the parts it supplies (`LaunchParts::Admin`,
//!   `Serving { headless: None }`, `Serving { headless: Some(..) }`) rather
//!   than a variant name it spells itself.
//!
//! The failing inputs are correspondingly sharper than before: flip the
//! desktop to `headless: Some(..)` and it fails naming the file, where the old
//! spelling could only notice a variant name going missing entirely.
//!
//! It is a census over first-party source, not a lint: it fails loudly if a
//! file it expects to find moves, rather than passing on an empty scan. A
//! check with no failing input you can name is not a check (ARCH §18.1); the
//! failing inputs here are "add a fourth variant and build it nowhere" and
//! "delete the last host that builds `Desktop`".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[path = "shared/repo_root.rs"]
mod repo_root;
use repo_root::repo_root;

/// Every first-party file that commissions an `EmbeddedDaemon`, the variant it
/// must end up with, and the `LaunchParts` shape that is the only way to reach
/// that variant through the assembler. Listed rather than globbed so that a
/// host which stops constructing a daemon fails here instead of silently
/// shrinking the census.
///
/// **`Desktop` came off this list on 2026-09-11 (sv-surface svt-3a), and the
/// census did exactly what it was built to do on the way.** Its own header
/// names "delete the last host that builds `Desktop`" as one of the two
/// failing inputs it exists to catch; `sovereign-desktop`'s `state.rs` stopped
/// calling `EmbeddedDaemon::new` and this went red naming the row. Removing
/// the row is therefore the OWED half of that deletion, not a way around it —
/// and the claim the row used to carry does not evaporate, it moves to
/// [`the_desktop_variant_has_no_first_party_host`] below.
const LIVE_CONSTRUCTION_SITES: &[(&str, &str, &str)] = &[
    (
        "sovereign/crates/sovereign-daemon/src/daemon_cmd/boot.rs",
        "Headless",
        "headless: Some(",
    ),
    // The setup wizard's join (moved out of cli-daemon's terminal.rs by
    // fp-cond2-c: the wizard spawns this launch instead of building one).
    (
        "sovereign/crates/sovereign-daemon/src/daemon_cmd/admin_join.rs",
        "MeshAdmin",
        "LaunchParts::Admin",
    ),
];

/// Variant names parsed out of the enum itself, so adding one without a host
/// fails rather than being invisible to a hand-maintained list.
fn declared_variants() -> Vec<String> {
    // `daemon_services.rs` moved to `sovereign-daemon` at dm-daemon-mesh-edge
    // (2026-09-17); this census reads it by repo-relative path.
    let src = std::fs::read_to_string(
        repo_root().join("sovereign/crates/sovereign-daemon/src/daemon_services.rs"),
    )
    .expect("daemon_services.rs is readable");
    let body_start = src
        .find("pub enum DaemonServices {")
        .expect("DaemonServices enum is declared in daemon_services.rs");
    let body = &src[body_start..];
    let body_end = body.find("\n}\n").expect("enum body terminates");
    body[..body_end]
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.starts_with("///") || t.starts_with("//") || t.ends_with('{') {
                return None;
            }
            // `MeshAdmin,` or `Desktop(Box<DesktopServices>),`
            let name: String = t
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let starts_upper = name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
            (!name.is_empty() && starts_upper).then_some(name)
        })
        .collect()
}

/// Soundness, host side: every declared variant has an ARM in the assembler.
///
/// The behavioural half — that the arm returns what it claims — is in
/// `daemon_services.rs`'s own tests, which can call `assemble`. This half
/// catches the thing those cannot: a variant added to the enum with no arm
/// constructing it, which the compiler permits (a `match` must cover every
/// input, not produce every output).
#[test]
fn every_variant_is_constructed_by_the_assembler() {
    // The assembler moved to `sovereign-daemon` with the file (2026-09-17).
    let src = std::fs::read_to_string(
        repo_root().join("sovereign/crates/sovereign-daemon/src/daemon_services.rs"),
    )
    .expect("daemon_services.rs is readable");
    let start = src
        .find("pub fn assemble(")
        .expect("the assembler is declared in daemon_services.rs");
    let body = &src[start..];
    // The assembler ends where the next top-level item begins.
    let end = body.find("\n}\n").expect("assemble terminates");
    let body = &body[..end];

    let variants = declared_variants();
    assert!(
        variants.len() >= 2,
        "parsed {variants:?} from the enum — the parser has drifted from the source"
    );
    for variant in &variants {
        let constructed = body.contains(&format!("DaemonServices::{variant}"))
            || body.contains(&format!("DaemonServices::{}(", snake_case(variant)));
        assert!(
            constructed,
            "DaemonServices::{variant} is declared but `assemble` has no arm that \
             constructs it — a representable-but-dead configuration (TOPOLOGY §4, \
             soundness). Either give it an arm or delete the variant."
        );
    }
}

/// `MeshAdmin` -> `mesh_admin`. The variant's canonical `pub(crate)`
/// constructor is the snake_case of its name, and since Phase 7 the assembler
/// calls THAT rather than naming the variant — `MeshAdmin` carries a private
/// witness, so `DaemonServices::MeshAdmin` is no longer a expression anyone,
/// including this crate, writes by hand.
///
/// This used to be `variant.to_lowercase()`, which is right only for
/// single-word variants: it maps `MeshAdmin` to `meshadmin` and would have
/// silently stopped matching the moment the assembler switched to the
/// constructor. It never fired because every variant was matched by the FIRST
/// clause until Phase 7 landed.
fn snake_case(variant: &str) -> String {
    let mut out = String::with_capacity(variant.len() + 2);
    for (i, c) in variant.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Source with comment lines removed.
///
/// **Found by sabotage, 2026-08-25.** The first version of the host-half check
/// below matched raw file text, and the desktop's own explanatory comment
/// contains the literal `headless: None` — so breaking the CODE left the gate
/// green. That is the §18.1 shape "a guard asserting on a field the subject
/// supplies or echoes back": prose about an invariant satisfied the check for
/// the invariant. Comments are stripped before matching, and this test was
/// then watched to fail on the same edit.
fn code_only(body: &str) -> String {
    body.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The host half: each live path reaches the arm it is supposed to.
///
/// A host supplies parts, not a variant name, so what is checkable in its
/// source is the SHAPE of those parts — and that shape is exactly what the
/// assembler matches on. `headless: None` versus `headless: Some(..)` is the
/// whole difference between the two serving variants, which is the point of
/// the type: the distinction a reader has to know is the one written down.
#[test]
fn each_live_path_supplies_the_parts_for_its_variant() {
    let root = repo_root();
    for (rel, expected, parts) in LIVE_CONSTRUCTION_SITES {
        let body = code_only(
            &std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("live construction site {rel} is unreadable ({e})")),
        );
        assert!(
            body.contains("EmbeddedDaemon::new("),
            "{rel} is listed as a live construction site but no longer calls \
             EmbeddedDaemon::new — update LIVE_CONSTRUCTION_SITES rather than \
             leaving the census claiming coverage it does not have"
        );
        assert!(
            body.contains("assemble("),
            "{rel} commissions a daemon without going through \
             `sovereign_daemon::assemble` — the one exhaustive match over Launch \
             (TOPOLOGY §10, Falsifier 3)"
        );
        assert!(
            body.contains(parts),
            "{rel} must reach DaemonServices::{expected}, which the assembler \
             produces only for parts shaped `{parts}` — that literal is not in \
             this file, so either the host changed shape or the assembler did"
        );
    }
}

/// `DaemonServices::Desktop` is REPRESENTABLE and has no first-party host.
///
/// The desktop was the only one. It commissioned a daemon whenever its boot
/// concluded `Local` — loading the GGUFs, claiming the data root's `RunLock`
/// and serving `:9741` from the window's own process — and svt-3a deleted
/// that: a client does not become the thing it is a client of (ARCH principle
/// 12).
///
/// The variant is deliberately NOT deleted with its host. `sv-surface`'s K3
/// kill-bar keeps in-process hosting as a DECLARED mode with a named owner,
/// and iOS is the standing case — the App Store forbids fork/exec, so a phone
/// running on-device weights cannot use a sidecar and must host in-process.
/// This test is what keeps that reservation honest in the meantime: the moment
/// a first-party file reaches the arm again, it fails and asks for the
/// declaration rather than letting a host appear by habit.
///
/// Watched to fail at landing: `headless: None` re-added to `state.rs` inside
/// a `LaunchParts::Serving`, red naming the file, reverted.
#[test]
fn the_desktop_variant_has_no_first_party_host() {
    let root = repo_root();
    // Scanned rather than listed, because the claim is an ABSENCE and a list
    // of places it is absent from proves nothing (ARCH principle 5). Both host
    // crates are excluded: `daemon_services.rs` (now in `sovereign-daemon`)
    // declares the arm and its tests exercise it, which is what a reserved
    // variant looks like, and `sovereign-mesh` is where the census itself
    // lives.
    let mut hosts: Vec<String> = Vec::new();
    let mut stack = vec![
        root.join("sovereign/crates"),
        root.join("commonwealth/crates"),
    ];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            // `daemon_services.rs` declares the arm and its tests exercise it —
            // both crates are the assembler's home since dm-daemon-mesh-edge
            // (2026-09-17) moved the host cluster out of `sovereign-mesh`.
            if rel.starts_with("sovereign/crates/sovereign-mesh/")
                || rel.starts_with("sovereign/crates/sovereign-daemon/")
            {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let body = code_only(&body);
            if body.contains("LaunchParts::Serving") && body.contains("headless: None") {
                hosts.push(rel);
            }
        }
    }
    assert!(
        hosts.is_empty(),
        "these files reach `DaemonServices::Desktop` — in-process daemon hosting \
         outside `sovereign-mesh`: {hosts:?}. sv-surface's K3 bar makes that a \
         DECLARED mode with a named owner and a `sovereign/DEFAULTS_LEDGER.md` \
         row, never a fallback a surface grows back into. If this is the iOS \
         case the variant is reserved for, add the row and the host to \
         LIVE_CONSTRUCTION_SITES in the same commit."
    );
}

/// The router delta this phase dissolves. Before 2026-08-24 the desktop
/// installed 5 of 7 routers and the CLI daemon 7 of 7, and the difference was
/// a runtime fact nothing could report. Three of those routers are now built
/// by the daemon from its own `Weak<Self>`, so no host installs them — which
/// is why no host can differ on them.
#[test]
fn no_host_installs_a_router_on_the_daemon() {
    let root = repo_root();
    for (rel, _, _) in LIVE_CONSTRUCTION_SITES {
        let body = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("live construction site {rel} is unreadable ({e})"));
        for forbidden in [
            ".install_mesh_http_router(",
            ".install_admin_http_router(",
            ".install_reading_http_router(",
            ".install_project_http_router(",
            ".install_knowledge_view_http_router(",
            ".install_corpus_watch_http_router(",
            ".install_solve_http_router(",
            ".set_corpus_engine(",
            ".set_inference_provider(",
            ".set_state_store(",
            ".set_setup_config(",
            ".set_mcp(",
            ".set_provider_factory(",
            ".set_mesh_store(",
            ".set_convergence_recorder(",
            ".set_embed_model_info(",
        ] {
            assert!(
                !body.contains(forbidden),
                "{rel} calls {forbidden} — post-construction wiring is what \
                 daemon-convergence Phase 2 removed; name it in DaemonServices instead"
            );
        }
    }
}

/// The construction census (pb-distribution; FIVE_PROGRAMS §12 "Done",
/// phase-b-30): `EmbeddedDaemon::new` is called in non-test code only by the
/// process entries `LIVE_CONSTRUCTION_SITES` names. The list above was the
/// whole claim until now and a list cannot see a site it does not name, so
/// this scan decides the file set and the list must equal it.
///
/// A grep cannot pass (26 files name the type in comments) and a line filter
/// cannot tell a `#[cfg(test)] mod tests;` file from production, so the scan
/// PARSES: it walks every workspace member's module tree from its lib and bin
/// roots, skips `#[cfg(test)]` items and `#[test]` fns, and matches the call
/// in the token stream, where comments and doc strings are already gone.
#[test]
fn only_the_process_entries_construct_a_daemon() {
    let root = repo_root();
    let mut walk = ModWalk::default();
    for krate in workspace_member_dirs(&root) {
        for entry in target_roots(&krate) {
            let dir = entry
                .parent()
                .expect("a target root has a dir")
                .to_path_buf();
            walk.file(&root, &entry, &dir);
        }
    }
    assert!(
        walk.visited.len() > 1000,
        "the census parsed only {} files; the module walk has drifted from the workspace",
        walk.visited.len()
    );
    assert!(
        walk.broken.is_empty(),
        "the census could not read these modules, so it cannot claim them: {:?}",
        walk.broken
    );
    let listed: BTreeSet<String> = LIVE_CONSTRUCTION_SITES
        .iter()
        .map(|(rel, _, _)| rel.to_string())
        .collect();
    let found: BTreeSet<String> = walk.hits.keys().cloned().collect();
    assert_eq!(
        found, listed,
        "`EmbeddedDaemon::new` in non-test code ({:?}) differs from \
         LIVE_CONSTRUCTION_SITES. A daemon is constructed only by the stock/svrn \
         process entry: spawn that process instead, or, if this is a new entry, \
         list it with the variant it reaches",
        walk.hits
    );
}

/// Directories of the root `Cargo.toml`'s `[workspace] members`.
fn workspace_member_dirs(root: &Path) -> Vec<PathBuf> {
    let manifest: toml::Value = std::fs::read_to_string(root.join("Cargo.toml"))
        .expect("root Cargo.toml")
        .parse()
        .expect("root Cargo.toml parses");
    manifest["workspace"]["members"]
        .as_array()
        .expect("[workspace] members")
        .iter()
        .map(|m| root.join(m.as_str().expect("member is a path")))
        .collect()
}

/// A crate's non-test target roots: its lib, its bins and its build script.
/// Tests, benches and examples are not production and are not walked.
fn target_roots(krate: &Path) -> Vec<PathBuf> {
    let manifest: toml::Value = std::fs::read_to_string(krate.join("Cargo.toml"))
        .unwrap_or_else(|e| panic!("{}/Cargo.toml: {e}", krate.display()))
        .parse()
        .expect("member Cargo.toml parses");
    let path_of = |t: &toml::Value| {
        t.get("path")
            .and_then(|p| p.as_str())
            .map(|p| krate.join(p))
    };
    let mut roots = vec![
        krate.join("src/lib.rs"),
        krate.join("src/main.rs"),
        krate.join("build.rs"),
    ];
    roots.extend(manifest.get("lib").and_then(path_of));
    for bin in manifest
        .get("bin")
        .and_then(|b| b.as_array())
        .into_iter()
        .flatten()
    {
        roots.extend(path_of(bin));
    }
    if let Ok(entries) = std::fs::read_dir(krate.join("src/bin")) {
        for e in entries.flatten() {
            let p = e.path();
            roots.push(if p.is_dir() { p.join("main.rs") } else { p });
        }
    }
    roots.retain(|p| p.extension().is_some_and(|e| e == "rs") && p.is_file());
    roots.sort();
    roots.dedup();
    roots
}

#[derive(Default)]
struct ModWalk {
    visited: BTreeSet<PathBuf>,
    /// repo-relative file -> the items in it that construct a daemon
    hits: BTreeMap<String, Vec<String>>,
    broken: Vec<String>,
}

impl ModWalk {
    /// Parse `file`, whose out-of-line child modules resolve under `mod_dir`.
    fn file(&mut self, root: &Path, file: &Path, mod_dir: &Path) {
        if !self.visited.insert(file.to_path_buf()) {
            return;
        }
        let parsed = std::fs::read_to_string(file)
            .map_err(|e| e.to_string())
            .and_then(|s| syn::parse_file(&s).map_err(|e| e.to_string()));
        match parsed {
            Ok(ast) => {
                let dir = file.parent().expect("a source file has a dir");
                self.items(root, file, &ast.items, dir, mod_dir, false);
            }
            Err(e) => self.broken.push(format!("{}: {e}", file.display())),
        }
    }

    fn items(
        &mut self,
        root: &Path,
        file: &Path,
        items: &[syn::Item],
        file_dir: &Path,
        mod_dir: &Path,
        inline: bool,
    ) {
        for item in items {
            match item {
                syn::Item::Mod(m) if !is_test_only(&m.attrs) => {
                    let name = m.ident.to_string();
                    let name = name.trim_start_matches("r#");
                    let path_attr = path_attr(&m.attrs);
                    if let Some((_, inner)) = &m.content {
                        let child = mod_dir.join(path_attr.as_deref().unwrap_or(name));
                        self.items(root, file, inner, file_dir, &child, true);
                        continue;
                    }
                    // rustc: a `#[path]` outside an inline block is relative to
                    // the declaring file's dir, inside one to the module dir; a
                    // `#[path]` or `mod.rs` file owns its own dir for children.
                    let (target, child_dir) = match path_attr {
                        Some(p) => {
                            let t = if inline {
                                mod_dir.join(p)
                            } else {
                                file_dir.join(p)
                            };
                            let d = t.parent().expect("module file has a dir").to_path_buf();
                            (t, d)
                        }
                        None if mod_dir.join(format!("{name}.rs")).is_file() => {
                            (mod_dir.join(format!("{name}.rs")), mod_dir.join(name))
                        }
                        None => (mod_dir.join(name).join("mod.rs"), mod_dir.join(name)),
                    };
                    if target.is_file() {
                        self.file(root, &target, &child_dir);
                    } else {
                        self.broken.push(format!(
                            "{}: `mod {name};` resolves to no file ({})",
                            file.display(),
                            target.display()
                        ));
                    }
                }
                syn::Item::Mod(_) => {}
                syn::Item::Impl(imp) if !is_test_only(&imp.attrs) => {
                    for inner in &imp.items {
                        let attrs = match inner {
                            syn::ImplItem::Fn(f) => &f.attrs,
                            syn::ImplItem::Const(c) => &c.attrs,
                            _ => continue,
                        };
                        if !is_test_only(attrs) {
                            let label =
                                format!("impl {}", quote::ToTokens::to_token_stream(&imp.self_ty));
                            self.scan(root, file, inner, label);
                        }
                    }
                }
                other => {
                    if !item_attrs(other).is_some_and(is_test_only) {
                        let label = item_label(other);
                        self.scan(root, file, other, label);
                    }
                }
            }
        }
    }

    fn scan(&mut self, root: &Path, file: &Path, node: &impl quote::ToTokens, label: String) {
        // Token spacing is the printer's: `EmbeddedDaemon :: new (`.
        let tokens = node.to_token_stream().to_string();
        if tokens.contains("EmbeddedDaemon :: new (") {
            let rel = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .to_string();
            self.hits.entry(rel).or_default().push(label);
        }
    }
}

/// `#[cfg(test)]` (or a `cfg` that needs `test` and does not negate it), and
/// `#[test]` / `#[tokio::test]`: compiled only into a test binary.
fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let last = a.path().segments.last().map(|s| s.ident.to_string());
        if last.as_deref() == Some("test") {
            return true;
        }
        if !a.path().is_ident("cfg") {
            return false;
        }
        let syn::Meta::List(list) = &a.meta else {
            return false;
        };
        let cfg = list.tokens.to_string();
        let idents: Vec<&str> = cfg
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .collect();
        idents.contains(&"test") && !idents.contains(&"not") && !idents.contains(&"any")
    })
}

fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.meta {
        syn::Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            _ => None,
        },
        _ => None,
    })
}

fn item_attrs(item: &syn::Item) -> Option<&[syn::Attribute]> {
    Some(match item {
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => return None,
    })
}

fn item_label(item: &syn::Item) -> String {
    match item {
        syn::Item::Fn(i) => format!("fn {}", i.sig.ident),
        syn::Item::Const(i) => format!("const {}", i.ident),
        syn::Item::Static(i) => format!("static {}", i.ident),
        syn::Item::Trait(i) => format!("trait {}", i.ident),
        _ => "item".to_string(),
    }
}
