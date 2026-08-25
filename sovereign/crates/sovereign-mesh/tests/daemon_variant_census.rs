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
//! `sovereign_mesh::assemble` — the one exhaustive match over `Launch` — and
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

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // .../sovereign/crates/sovereign-mesh -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("sovereign-mesh lives three levels under the repo root")
        .to_path_buf()
}

/// Every first-party file that commissions an `EmbeddedDaemon`, the variant it
/// must end up with, and the `LaunchParts` shape that is the only way to reach
/// that variant through the assembler. Listed rather than globbed so that a
/// host which stops constructing a daemon fails here instead of silently
/// shrinking the census.
const LIVE_CONSTRUCTION_SITES: &[(&str, &str, &str)] = &[
    (
        "sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/mod.rs",
        "Headless",
        "headless: Some(",
    ),
    (
        "sovereign/crates/sovereign-desktop/src-tauri/src/state.rs",
        "Desktop",
        "headless: None",
    ),
    (
        "sovereign/crates/sovereign-cli-llm/src/mesh_cmd.rs",
        "MeshAdmin",
        "LaunchParts::Admin",
    ),
];

/// Variant names parsed out of the enum itself, so adding one without a host
/// fails rather than being invisible to a hand-maintained list.
fn declared_variants() -> Vec<String> {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon_services.rs"),
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
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon_services.rs"),
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
            || body.contains(&format!("DaemonServices::{}(", variant.to_lowercase()));
        assert!(
            constructed,
            "DaemonServices::{variant} is declared but `assemble` has no arm that \
             constructs it — a representable-but-dead configuration (TOPOLOGY §4, \
             soundness). Either give it an arm or delete the variant."
        );
    }
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
             `sovereign_mesh::assemble` — the one exhaustive match over Launch \
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
