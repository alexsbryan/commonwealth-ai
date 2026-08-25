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

/// Every first-party file that commissions an `EmbeddedDaemon`, and the
/// variant it is expected to name. Listed rather than globbed so that a host
/// which stops constructing a daemon fails here instead of silently shrinking
/// the census.
const LIVE_CONSTRUCTION_SITES: &[(&str, &str)] = &[
    (
        "sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/mod.rs",
        "Headless",
    ),
    (
        "sovereign/crates/sovereign-desktop/src-tauri/src/state.rs",
        "Desktop",
    ),
    (
        "sovereign/crates/sovereign-cli-llm/src/mesh_cmd.rs",
        "MeshAdmin",
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

#[test]
fn every_variant_is_reachable_from_a_live_path() {
    let root = repo_root();
    let variants = declared_variants();
    assert!(
        variants.len() >= 2,
        "parsed {variants:?} from the enum — the parser has drifted from the source"
    );

    let sources: Vec<(String, String)> = LIVE_CONSTRUCTION_SITES
        .iter()
        .map(|(rel, _)| {
            let path = root.join(rel);
            let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("live construction site {rel} is unreadable ({e}) — if the host moved, update LIVE_CONSTRUCTION_SITES")
            });
            (rel.to_string(), body)
        })
        .collect();

    for variant in &variants {
        // `DaemonServices::MeshAdmin` or the `DaemonServices::desktop(` /
        // `::headless(` constructors that wrap the boxed payload.
        let needles = [
            format!("DaemonServices::{variant}"),
            format!("DaemonServices::{}(", variant.to_lowercase()),
        ];
        let built_by: Vec<&str> = sources
            .iter()
            .filter(|(_, body)| needles.iter().any(|n| body.contains(n.as_str())))
            .map(|(rel, _)| rel.as_str())
            .collect();
        assert!(
            !built_by.is_empty(),
            "DaemonServices::{variant} is declared but no live path constructs it — \
             a representable-but-dead configuration (TOPOLOGY §4, soundness). \
             Either wire a host to it or delete the variant."
        );
    }
}

#[test]
fn each_live_path_names_the_variant_it_is_expected_to() {
    let root = repo_root();
    for (rel, expected) in LIVE_CONSTRUCTION_SITES {
        let body = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("live construction site {rel} is unreadable ({e})"));
        assert!(
            body.contains("EmbeddedDaemon::new("),
            "{rel} is listed as a live construction site but no longer calls \
             EmbeddedDaemon::new — update LIVE_CONSTRUCTION_SITES rather than \
             leaving the census claiming coverage it does not have"
        );
        let names = body.contains(&format!("DaemonServices::{expected}"))
            || body.contains(&format!("DaemonServices::{}(", expected.to_lowercase()));
        assert!(
            names,
            "{rel} constructs an EmbeddedDaemon but does not name \
             DaemonServices::{expected}"
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
    for (rel, _) in LIVE_CONSTRUCTION_SITES {
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
